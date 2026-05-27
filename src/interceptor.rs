use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{HoldWindow, InterceptorSettings, ReleasePacing, TrafficRule};
use crate::tui::{run_interceptor_dashboard, DashboardAction};

#[repr(C)]
#[derive(Clone, Copy)]
struct WinDivertAddress {
    timestamp: i64,
    flags: u32,
    reserved2: u32,
    data: [u8; 64],
}

impl Default for WinDivertAddress {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[link(name = "WinDivert")]
unsafe extern "C" {
    fn WinDivertOpen(
        filter: *const i8,
        layer: i32,
        priority: i16,
        flags: u64,
    ) -> *mut std::ffi::c_void;
    fn WinDivertClose(handle: *mut std::ffi::c_void) -> bool;
    fn WinDivertSetParam(handle: *mut std::ffi::c_void, param: i32, value: u64) -> bool;
    fn WinDivertShutdown(handle: *mut std::ffi::c_void, how: i32) -> bool;
    fn WinDivertRecv(
        handle: *mut std::ffi::c_void,
        p_packet: *mut u8,
        packet_len: u32,
        p_recv_len: *mut u32,
        p_addr: *mut WinDivertAddress,
    ) -> bool;
    fn WinDivertSend(
        handle: *mut std::ffi::c_void,
        p_packet: *const u8,
        packet_len: u32,
        p_send_len: *mut u32,
        p_addr: *const WinDivertAddress,
    ) -> bool;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(virtual_key: i32) -> i16;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

const WINDIVERT_LAYER_NETWORK: i32 = 0;
const WINDIVERT_PARAM_QUEUE_LENGTH: i32 = 0;
const WINDIVERT_PARAM_QUEUE_TIME: i32 = 1;
const WINDIVERT_SHUTDOWN_RECV: i32 = 1;
const MAX_HELD_PACKETS: usize = 4096;
const MAX_HELD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct Packet {
    data: Vec<u8>,
    addr: WinDivertAddress,
}

#[derive(Clone)]
pub enum LastEvent {
    Waiting,
    OpeningCapture,
    ThrottleOn {
        hold_secs: f64,
        matched: u64,
        queue: usize,
    },
    PacketQueued {
        bytes: u32,
        queue: usize,
        total: u64,
    },
    QueueLimitReached {
        queued: usize,
        bytes: usize,
        passed_through: u64,
    },
    NoTraffic {
        summary: String,
    },
    AutoBurst {
        hold_secs: f64,
        matched: u64,
        queue: usize,
    },
    Released {
        count: usize,
        remaining: usize,
    },
    ReleaseFailed {
        windows_error: u32,
        total_failures: u64,
    },
    ReleaseComplete {
        sent: u64,
    },
    ReleaseWithFailures {
        sent: u64,
        failed: u64,
    },
    StillQueued {
        remaining: usize,
    },
    Error(String),
}

pub struct State {
    pub throttling: bool,
    pub session_active: bool,
    pub throttle_start: Option<Instant>,
    pub hold_duration: Option<Duration>,
    matched_at_throttle_start: u64,
    pub matched_total: u64,
    pub queued_total: u64,
    pub burst_total: u64,
    pub release_failures: u64,
    pub passed_through_total: u64,
    pub last_event: LastEvent,
    queue: VecDeque<Packet>,
    queue_bytes: usize,
}

impl State {
    fn new() -> Self {
        Self {
            throttling: false,
            session_active: false,
            throttle_start: None,
            hold_duration: None,
            matched_at_throttle_start: 0,
            matched_total: 0,
            queued_total: 0,
            burst_total: 0,
            release_failures: 0,
            passed_through_total: 0,
            last_event: LastEvent::Waiting,
            queue: VecDeque::new(),
            queue_bytes: 0,
        }
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    fn can_hold(&self, packet_len: usize) -> bool {
        self.queue.len() < MAX_HELD_PACKETS
            && self.queue_bytes.saturating_add(packet_len) <= MAX_HELD_BYTES
    }

    fn drain_batch(&mut self, maximum: usize) -> Vec<Packet> {
        let mut batch = Vec::with_capacity(maximum.min(self.queue.len()));
        while batch.len() < maximum {
            match self.queue.pop_front() {
                Some(packet) => {
                    self.queue_bytes = self.queue_bytes.saturating_sub(packet.data.len());
                    batch.push(packet);
                }
                None => break,
            }
        }
        batch
    }
}

pub fn run(settings: InterceptorSettings) {
    if !settings.traffic_rule.has_port() {
        eprintln!("no port configured. open settings and choose a port before starting.");
        return;
    }

    let state = Arc::new(Mutex::new(State::new()));
    let stop_requested = Arc::new(AtomicBool::new(false));

    let trigger_state = Arc::clone(&state);
    let trigger_stop = Arc::clone(&stop_requested);
    let trigger_key = settings.trigger_key;
    let traffic_rule = settings.traffic_rule;
    let hold_window = settings.hold_window.clone();
    let release_pacing = settings.release_pacing.clone();
    let trigger_thread = thread::spawn(move || {
        listen_for_trigger(
            trigger_state,
            trigger_stop,
            trigger_key,
            traffic_rule,
            hold_window,
            release_pacing,
        );
    });

    match run_interceptor_dashboard(Arc::clone(&state), settings) {
        DashboardAction::ForceExit => std::process::exit(0),
        DashboardAction::BackToMenu => {
            stop_requested.store(true, Ordering::Relaxed);
            trigger_thread.join().ok();
            while state
                .lock()
                .map(|state| state.session_active)
                .unwrap_or(false)
            {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn is_invalid_handle(handle: *mut std::ffi::c_void) -> bool {
    // WinDivert can fail as NULL or INVALID_HANDLE_VALUE (-1).
    handle.is_null() || handle as isize == -1
}

fn listen_for_trigger(
    state: Arc<Mutex<State>>,
    stop_requested: Arc<AtomicBool>,
    trigger_key: i32,
    traffic_rule: TrafficRule,
    hold_window: HoldWindow,
    release_pacing: ReleasePacing,
) {
    let mut was_down = false;

    while !stop_requested.load(Ordering::Relaxed) {
        let is_down = unsafe { GetAsyncKeyState(trigger_key) } < 0;

        if is_down && !was_down {
            open_hold_window(
                &state,
                Arc::clone(&stop_requested),
                traffic_rule,
                &hold_window,
                &release_pacing,
            );
        }

        was_down = is_down;
        thread::sleep(Duration::from_millis(10));
    }
}

fn open_hold_window(
    state: &Arc<Mutex<State>>,
    stop_requested: Arc<AtomicBool>,
    traffic_rule: TrafficRule,
    hold_window: &HoldWindow,
    release_pacing: &ReleasePacing,
) {
    {
        let mut state = state.lock().unwrap();
        if state.session_active {
            return;
        }
        state.session_active = true;
        state.last_event = LastEvent::OpeningCapture;
    }

    let session_state = Arc::clone(state);
    let hold_window = hold_window.clone();
    let release_pacing = release_pacing.clone();
    thread::spawn(move || {
        hold_matching_traffic(
            session_state,
            stop_requested,
            traffic_rule,
            hold_window,
            release_pacing,
        )
    });
}

fn hold_matching_traffic(
    state: Arc<Mutex<State>>,
    stop_requested: Arc<AtomicBool>,
    traffic_rule: TrafficRule,
    hold_window: HoldWindow,
    release_pacing: ReleasePacing,
) {
    let expression = traffic_rule.compile_for_windivert();
    // i know why you're here. please do not make this a cheat.
    let handle = unsafe { WinDivertOpen(expression.as_ptr(), WINDIVERT_LAYER_NETWORK, 0, 0) };

    if is_invalid_handle(handle) {
        let error = unsafe { GetLastError() };
        finish_failed_session(
            &state,
            format!(
                "WinDivertOpen failed with Windows error {error}. run as admin and check filter"
            ),
        );
        return;
    }

    let hold_duration = random_hold_duration(&hold_window);
    if !configure_driver_queue(handle) {
        let error = unsafe { GetLastError() };
        unsafe {
            WinDivertClose(handle);
        }
        finish_failed_session(
            &state,
            format!("could not configure WinDivert queue (Windows error {error})"),
        );
        return;
    }

    let start = Instant::now();
    {
        let mut state = state.lock().unwrap();
        state.throttling = true;
        state.throttle_start = Some(start);
        state.hold_duration = Some(hold_duration);
        state.matched_at_throttle_start = state.matched_total;
        state.last_event = LastEvent::ThrottleOn {
            hold_secs: hold_duration.as_secs_f64(),
            matched: state.matched_total,
            queue: state.queue.len(),
        };
    }

    // Recv blocks inside the driver. Shutting down receive is what makes a quiet
    // connection finish its hold window on time instead of waiting for a packet.
    let shutdown_handle = handle as usize;
    let shutdown_stop = Arc::clone(&stop_requested);
    let shutdown_thread = thread::spawn(move || {
        let started = Instant::now();
        while started.elapsed() < hold_duration && !shutdown_stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            WinDivertShutdown(
                shutdown_handle as *mut std::ffi::c_void,
                WINDIVERT_SHUTDOWN_RECV,
            );
        }
    });

    let mut packet_buffer = vec![0u8; 65535];
    while start.elapsed() < hold_duration && !stop_requested.load(Ordering::Relaxed) {
        let mut recv_len: u32 = 0;
        let mut addr = WinDivertAddress::default();
        let success = unsafe {
            WinDivertRecv(
                handle,
                packet_buffer.as_mut_ptr(),
                packet_buffer.len() as u32,
                &mut recv_len,
                &mut addr,
            )
        };

        if !success || recv_len == 0 {
            continue;
        }

        let pass_through = {
            let mut state = state.lock().unwrap();
            state.matched_total += 1;
            let packet_len = recv_len as usize;
            if state.can_hold(packet_len) {
                state.queue.push_back(Packet {
                    data: packet_buffer[..packet_len].to_vec(),
                    addr,
                });
                state.queue_bytes += packet_len;
                state.queued_total += 1;

                if state.queue.len() == 1 || state.queue.len().is_multiple_of(25) {
                    state.last_event = LastEvent::PacketQueued {
                        bytes: recv_len,
                        queue: state.queue.len(),
                        total: state.queued_total,
                    };
                }
                false
            } else {
                state.passed_through_total += 1;
                state.last_event = LastEvent::QueueLimitReached {
                    queued: state.queue.len(),
                    bytes: state.queue_bytes,
                    passed_through: state.passed_through_total,
                };
                true
            }
        };

        if pass_through {
            let mut sent_len = 0u32;
            if !unsafe {
                WinDivertSend(
                    handle,
                    packet_buffer.as_ptr(),
                    recv_len,
                    &mut sent_len,
                    &addr,
                )
            } {
                let mut state = state.lock().unwrap();
                state.release_failures += 1;
                state.last_event = LastEvent::ReleaseFailed {
                    windows_error: unsafe { GetLastError() },
                    total_failures: state.release_failures,
                };
            }
        }
    }

    shutdown_thread.join().ok();

    {
        let mut state = state.lock().unwrap();
        let matched_during_hold = state.matched_total - state.matched_at_throttle_start;
        state.throttling = false;
        state.throttle_start = None;
        state.hold_duration = None;
        state.matched_at_throttle_start = state.matched_total;
        state.last_event = if matched_during_hold == 0 {
            LastEvent::NoTraffic {
                summary: traffic_rule.summary(),
            }
        } else {
            LastEvent::AutoBurst {
                hold_secs: hold_duration.as_secs_f64(),
                matched: matched_during_hold,
                queue: state.queue.len(),
            }
        };
    }

    let (failures_before_release, released_before_release) = {
        let state = state.lock().unwrap();
        (state.release_failures, state.burst_total)
    };
    release_queued_packets(handle, &state, &release_pacing, &stop_requested);
    unsafe {
        WinDivertClose(handle);
    }

    let mut state = state.lock().unwrap();
    state.session_active = false;
    let failed_this_release = state.release_failures - failures_before_release;
    let released_this_session = state.burst_total - released_before_release;
    if failed_this_release > 0 {
        state.last_event = LastEvent::ReleaseWithFailures {
            sent: released_this_session,
            failed: failed_this_release,
        };
    } else if released_this_session > 0 {
        state.last_event = LastEvent::ReleaseComplete {
            sent: released_this_session,
        };
    } else if !state.queue.is_empty() {
        state.last_event = LastEvent::StillQueued {
            remaining: state.queue.len(),
        };
    }
}

fn release_queued_packets(
    handle: *mut std::ffi::c_void,
    state: &Arc<Mutex<State>>,
    pacing: &ReleasePacing,
    stop_requested: &AtomicBool,
) {
    loop {
        // Menu navigation turns the release into an immediate drain so the UI returns promptly.
        let returning_to_menu = stop_requested.load(Ordering::Relaxed);
        let packets_per_batch = release_batch_size(pacing, returning_to_menu);
        let batch = state.lock().unwrap().drain_batch(packets_per_batch);
        if batch.is_empty() {
            break;
        }

        let mut sent = 0u64;
        let mut last_win_err: Option<u32> = None;
        for packet in &batch {
            let mut sent_len: u32 = 0;
            if unsafe {
                WinDivertSend(
                    handle,
                    packet.data.as_ptr(),
                    packet.data.len() as u32,
                    &mut sent_len,
                    &packet.addr,
                )
            } {
                sent += 1;
            } else {
                last_win_err = Some(unsafe { GetLastError() });
            }
        }

        let failed = batch.len() as u64 - sent;
        {
            let mut s = state.lock().unwrap();
            s.burst_total += sent;
            s.release_failures += failed;
            s.last_event = if failed > 0 {
                LastEvent::ReleaseFailed {
                    windows_error: last_win_err.unwrap_or(0),
                    total_failures: s.release_failures,
                }
            } else {
                LastEvent::Released {
                    count: sent as usize,
                    remaining: s.queue.len(),
                }
            };
        }

        if let Some(delay) = release_pause(pacing, stop_requested.load(Ordering::Relaxed)) {
            thread::sleep(delay);
        }
    }
}

fn release_batch_size(pacing: &ReleasePacing, returning_to_menu: bool) -> usize {
    if returning_to_menu {
        MAX_HELD_PACKETS
    } else {
        pacing.packet_batch_size()
    }
}

fn release_pause(pacing: &ReleasePacing, returning_to_menu: bool) -> Option<Duration> {
    if returning_to_menu {
        None
    } else {
        Some(Duration::from_millis(pacing.batch_pause_ms()))
    }
}

fn configure_driver_queue(handle: *mut std::ffi::c_void) -> bool {
    unsafe {
        WinDivertSetParam(handle, WINDIVERT_PARAM_QUEUE_LENGTH, 16384)
            && WinDivertSetParam(handle, WINDIVERT_PARAM_QUEUE_TIME, 8000)
    }
}

fn finish_failed_session(state: &Arc<Mutex<State>>, message: String) {
    let mut state = state.lock().unwrap();
    state.throttling = false;
    state.session_active = false;
    state.throttle_start = None;
    state.hold_duration = None;
    state.last_event = LastEvent::Error(message);
}

fn random_hold_duration(hold_window: &HoldWindow) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = nanos ^ nanos.rotate_left(13) ^ nanos.rotate_right(7);
    let min_ms = hold_window.min_ms();
    let max_ms = hold_window.max_ms();
    let span = max_ms.saturating_sub(min_ms);

    Duration::from_millis(min_ms + (mixed % (span + 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_queue_refuses_more_data_at_the_byte_limit() {
        let mut state = State::new();
        state.queue_bytes = MAX_HELD_BYTES;

        assert!(!state.can_hold(1));
    }

    #[test]
    fn held_queue_refuses_more_packets_at_the_packet_limit() {
        let mut state = State::new();
        for _ in 0..MAX_HELD_PACKETS {
            state.queue.push_back(Packet {
                data: Vec::new(),
                addr: WinDivertAddress::default(),
            });
        }

        assert!(!state.can_hold(1));
    }

    #[test]
    fn draining_packets_reduces_held_memory_accounting() {
        let mut state = State::new();
        state.queue.push_back(Packet {
            data: vec![0; 64],
            addr: WinDivertAddress::default(),
        });
        state.queue_bytes = 64;

        let drained = state.drain_batch(1);

        assert_eq!(drained.len(), 1);
        assert_eq!(state.queue_bytes, 0);
    }

    #[test]
    fn returning_to_menu_ignores_slow_custom_release_pacing() {
        let pacing = ReleasePacing::Custom {
            packets_per_batch: 1,
            pause_ms: 100,
        };

        assert_eq!(release_batch_size(&pacing, true), MAX_HELD_PACKETS);
        assert!(release_pause(&pacing, true).is_none());
    }
}
