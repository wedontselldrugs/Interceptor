use std::collections::HashMap;
use std::env;
use std::ffi::CString;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::updater::UpdateStatus;

pub const VK_F9: i32 = 0x78;
pub const STOCK_HOLD_MIN_MS: u64 = 1800;
pub const STOCK_HOLD_MAX_MS: u64 = 2500;
pub const SHORTEST_HOLD_MS: u64 = 100;
pub const LONGEST_HOLD_MS: u64 = 10_000;
pub const DEFAULT_RELEASE_BATCH_SIZE: usize = 30;
pub const DEFAULT_RELEASE_DELAY_MS: u64 = 4;
pub const MIN_RELEASE_BATCH_SIZE: usize = 1;
pub const MAX_RELEASE_BATCH_SIZE: usize = 250;
pub const MAX_RELEASE_DELAY_MS: u64 = 100;

#[derive(Clone)]
pub struct InterceptorSettings {
    pub trigger_key: i32,
    pub trigger_name: String,
    pub traffic_rule: TrafficRule,
    pub hold_window: HoldWindow,
    remembered_custom_hold: (u64, u64),
    pub release_pacing: ReleasePacing,
    remembered_custom_release: (usize, u64),
    pub always_on_top: bool,
    pub update_status: UpdateStatus,
}

impl Default for InterceptorSettings {
    fn default() -> Self {
        Self::with_update_status(UpdateStatus::Checking)
    }
}

impl InterceptorSettings {
    fn with_update_status(update_status: UpdateStatus) -> Self {
        Self {
            trigger_key: VK_F9,
            trigger_name: trigger_key_name(VK_F9).to_string(),
            traffic_rule: TrafficRule::default(),
            hold_window: HoldWindow::StockRandomized,
            remembered_custom_hold: (STOCK_HOLD_MIN_MS, STOCK_HOLD_MAX_MS),
            release_pacing: ReleasePacing::Default,
            remembered_custom_release: (DEFAULT_RELEASE_BATCH_SIZE, DEFAULT_RELEASE_DELAY_MS),
            always_on_top: false,
            update_status,
        }
    }

    pub fn load_saved() -> (Self, Option<String>) {
        let mut settings = Self::default();
        let path = match settings_file_path() {
            Ok(path) => path,
            Err(problem) => return (settings, Some(problem)),
        };
        if !path.exists() {
            return (settings, None);
        }
        let result = fs::read_to_string(&path)
            .map_err(|error| format!("could not read saved settings: {error}"))
            .and_then(|text| parse_saved_config(&text))
            .and_then(|values| settings.apply_saved_values(&values));
        match result {
            Ok(()) => (settings, None),
            Err(problem) => (settings, Some(problem)),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_file_path()?;
        let directory = path
            .parent()
            .ok_or_else(|| "could not determine settings directory".to_string())?;
        fs::create_dir_all(directory)
            .map_err(|error| format!("could not create settings directory: {error}"))?;
        fs::write(path, self.saved_values())
            .map_err(|error| format!("could not save settings: {error}"))
    }

    pub fn restore_defaults(&mut self) -> Result<(), String> {
        let update_status = self.update_status.clone();
        *self = Self::with_update_status(update_status);
        self.save()
    }

    pub fn switch_hold_window(&mut self) {
        self.hold_window = match self.hold_window {
            HoldWindow::StockRandomized => HoldWindow::Custom {
                min_ms: self.remembered_custom_hold.0,
                max_ms: self.remembered_custom_hold.1,
            },
            HoldWindow::Custom { min_ms, max_ms } => {
                self.remembered_custom_hold = (min_ms, max_ms);
                HoldWindow::StockRandomized
            }
        };
    }

    pub fn change_custom_minimum(&mut self, entered: &str) -> Result<(), SettingsError> {
        let min_ms = checked_hold_value(entered)?;
        let max_ms = self.remembered_custom_hold.1.max(min_ms);
        self.remembered_custom_hold = (min_ms, max_ms);
        self.hold_window = HoldWindow::Custom { min_ms, max_ms };
        Ok(())
    }

    pub fn change_custom_maximum(&mut self, entered: &str) -> Result<(), SettingsError> {
        let max_ms = checked_hold_value(entered)?;
        let min_ms = self.remembered_custom_hold.0.min(max_ms);
        self.remembered_custom_hold = (min_ms, max_ms);
        self.hold_window = HoldWindow::Custom { min_ms, max_ms };
        Ok(())
    }

    pub fn custom_min_ms(&self) -> u64 {
        self.remembered_custom_hold.0
    }

    pub fn custom_max_ms(&self) -> u64 {
        self.remembered_custom_hold.1
    }

    pub fn switch_release_pacing(&mut self) {
        self.release_pacing = match self.release_pacing {
            ReleasePacing::Default => ReleasePacing::Custom {
                packets_per_batch: self.remembered_custom_release.0,
                pause_ms: self.remembered_custom_release.1,
            },
            ReleasePacing::Custom {
                packets_per_batch,
                pause_ms,
            } => {
                self.remembered_custom_release = (packets_per_batch, pause_ms);
                ReleasePacing::Default
            }
        };
    }

    pub fn change_custom_batch_size(&mut self, entered: &str) -> Result<(), SettingsError> {
        let packets_per_batch = checked_batch_size(entered)?;
        self.remembered_custom_release.0 = packets_per_batch;
        self.release_pacing = ReleasePacing::Custom {
            packets_per_batch,
            pause_ms: self.remembered_custom_release.1,
        };
        Ok(())
    }

    pub fn change_custom_release_delay(&mut self, entered: &str) -> Result<(), SettingsError> {
        let pause_ms = checked_release_delay(entered)?;
        self.remembered_custom_release.1 = pause_ms;
        self.release_pacing = ReleasePacing::Custom {
            packets_per_batch: self.remembered_custom_release.0,
            pause_ms,
        };
        Ok(())
    }

    pub fn custom_batch_size(&self) -> usize {
        self.remembered_custom_release.0
    }

    pub fn custom_release_delay_ms(&self) -> u64 {
        self.remembered_custom_release.1
    }

    pub fn always_on_top_name(&self) -> &'static str {
        if self.always_on_top {
            "on"
        } else {
            "off"
        }
    }

    fn saved_values(&self) -> String {
        format!(
            concat!(
                "trigger_key={}\n",
                "trigger_name={}\n",
                "port={}\n",
                "protocol={}\n",
                "always_on_top={}\n",
                "hold_window={}\n",
                "custom_hold_min_ms={}\n",
                "custom_hold_max_ms={}\n",
                "release_pacing={}\n",
                "custom_batch_size={}\n",
                "custom_batch_delay_ms={}\n"
            ),
            self.trigger_key,
            self.trigger_name,
            self.traffic_rule
                .port
                .map(|port| port.to_string())
                .unwrap_or_default(),
            self.traffic_rule.protocol.saved_name(),
            self.always_on_top,
            self.hold_window.option_name(),
            self.remembered_custom_hold.0,
            self.remembered_custom_hold.1,
            self.release_pacing.option_name(),
            self.remembered_custom_release.0,
            self.remembered_custom_release.1,
        )
    }

    fn apply_saved_values(&mut self, saved: &HashMap<String, String>) -> Result<(), String> {
        self.trigger_key = saved_i32(saved, "trigger_key")?;
        self.trigger_name = saved_string(saved, "trigger_name")?.to_string();
        let port = saved_string(saved, "port")?;
        if port.is_empty() {
            self.traffic_rule.port = None;
        } else {
            self.traffic_rule
                .set_port_from_text(port)
                .map_err(|error| format!("saved port is invalid: {error}"))?;
        }
        self.traffic_rule.protocol = TrafficKind::from_saved_name(saved_string(saved, "protocol")?)
            .ok_or_else(|| "saved protocol is invalid".to_string())?;
        self.always_on_top = saved_bool(saved, "always_on_top")?;

        let min_ms = saved_u64(saved, "custom_hold_min_ms")?;
        let max_ms = saved_u64(saved, "custom_hold_max_ms")?;
        if !(SHORTEST_HOLD_MS..=LONGEST_HOLD_MS).contains(&min_ms) {
            return Err("saved custom hold minimum is out of range".to_string());
        }
        if !(SHORTEST_HOLD_MS..=LONGEST_HOLD_MS).contains(&max_ms) {
            return Err("saved custom hold maximum is out of range".to_string());
        }
        if min_ms > max_ms {
            return Err("saved custom hold minimum is above its maximum".to_string());
        }
        self.remembered_custom_hold = (min_ms, max_ms);
        self.hold_window = match saved_string(saved, "hold_window")? {
            "default" => HoldWindow::StockRandomized,
            "custom" => HoldWindow::Custom { min_ms, max_ms },
            _ => return Err("saved hold window mode is invalid".to_string()),
        };

        let batch_size: usize = saved_u64(saved, "custom_batch_size")?
            .try_into()
            .map_err(|_| "saved custom batch size is out of range".to_string())?;
        let pause_ms = saved_u64(saved, "custom_batch_delay_ms")?;
        if !(MIN_RELEASE_BATCH_SIZE..=MAX_RELEASE_BATCH_SIZE).contains(&batch_size) {
            return Err("saved custom batch size is out of range".to_string());
        }
        if pause_ms > MAX_RELEASE_DELAY_MS {
            return Err("saved custom batch delay is out of range".to_string());
        }
        self.remembered_custom_release = (batch_size, pause_ms);
        self.release_pacing = match saved_string(saved, "release_pacing")? {
            "default" => ReleasePacing::Default,
            "custom" => ReleasePacing::Custom {
                packets_per_batch: batch_size,
                pause_ms,
            },
            _ => return Err("saved release pacing mode is invalid".to_string()),
        };
        Ok(())
    }
}

fn settings_file_path() -> Result<PathBuf, String> {
    let app_data = env::var_os("APPDATA")
        .ok_or_else(|| "APPDATA is unavailable; settings not saved".to_string())?;
    Ok(PathBuf::from(app_data)
        .join("Interceptor")
        .join("settings.cfg"))
}

fn parse_saved_config(text: &str) -> Result<HashMap<String, String>, String> {
    let mut settings = HashMap::new();
    for (line_number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("saved settings line {} has no '='", line_number + 1))?;
        settings.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(settings)
}

fn saved_string<'a>(saved: &'a HashMap<String, String>, key: &str) -> Result<&'a str, String> {
    saved
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("saved setting '{key}' is missing"))
}

fn saved_u64(saved: &HashMap<String, String>, key: &str) -> Result<u64, String> {
    saved_string(saved, key)?
        .parse::<u64>()
        .map_err(|_| format!("saved setting '{key}' is invalid"))
}

fn saved_i32(saved: &HashMap<String, String>, key: &str) -> Result<i32, String> {
    saved_string(saved, key)?
        .parse::<i32>()
        .map_err(|_| format!("saved setting '{key}' is invalid"))
}

fn saved_bool(saved: &HashMap<String, String>, key: &str) -> Result<bool, String> {
    saved_string(saved, key)?
        .parse::<bool>()
        .map_err(|_| format!("saved setting '{key}' is invalid"))
}

#[derive(Clone, Copy)]
pub enum TrafficKind {
    Both,
    Udp,
    Tcp,
}

impl TrafficKind {
    pub fn menu_name(self) -> &'static str {
        match self {
            Self::Both => "tcp + udp",
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        }
    }

    pub fn next_protocol(self) -> Self {
        match self {
            Self::Both => Self::Udp,
            Self::Udp => Self::Tcp,
            Self::Tcp => Self::Both,
        }
    }

    fn saved_name(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        }
    }

    fn from_saved_name(saved: &str) -> Option<Self> {
        match saved {
            "both" => Some(Self::Both),
            "udp" => Some(Self::Udp),
            "tcp" => Some(Self::Tcp),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub struct TrafficRule {
    pub port: Option<u16>,
    pub protocol: TrafficKind,
}

impl Default for TrafficRule {
    fn default() -> Self {
        Self {
            port: None,
            protocol: TrafficKind::Both,
        }
    }
}

impl TrafficRule {
    pub fn summary(self) -> String {
        format!("{} port {}", self.protocol.menu_name(), self.port_name())
    }

    pub fn port_name(self) -> String {
        self.port
            .map(|port| port.to_string())
            .unwrap_or_else(|| "not set".to_string())
    }

    pub fn has_port(self) -> bool {
        self.port.is_some()
    }

    pub fn set_port_from_text(&mut self, entered: &str) -> Result<(), SettingsError> {
        let port = entered
            .parse::<u16>()
            .map_err(|_| SettingsError::PortNotANumber)?;
        if port == 0 {
            return Err(SettingsError::PortZero);
        }
        self.port = Some(port);
        Ok(())
    }

    pub fn compile_for_windivert(self) -> CString {
        // The selected port is the remote endpoint: destination on send, source on reply.
        let outgoing = self.packet_side("Dst");
        let incoming = self.packet_side("Src");
        CString::new(format!(
            "((outbound and {outgoing}) or (inbound and {incoming}))"
        ))
        .expect("generated WinDivert filter has no null bytes")
    }

    fn packet_side(self, direction: &str) -> String {
        let port = self
            .port
            .expect("port must be set before opening WinDivert");
        match self.protocol {
            TrafficKind::Both => format!(
                "((udp and udp.{direction}Port == {}) or (tcp and tcp.{direction}Port == {}))",
                port, port
            ),
            TrafficKind::Udp => {
                format!("(udp and udp.{direction}Port == {})", port)
            }
            TrafficKind::Tcp => {
                format!("(tcp and tcp.{direction}Port == {})", port)
            }
        }
    }
}

#[derive(Clone)]
pub enum HoldWindow {
    StockRandomized,
    Custom { min_ms: u64, max_ms: u64 },
}

impl HoldWindow {
    pub fn min_ms(&self) -> u64 {
        match self {
            Self::StockRandomized => STOCK_HOLD_MIN_MS,
            Self::Custom { min_ms, .. } => *min_ms,
        }
    }

    pub fn max_ms(&self) -> u64 {
        match self {
            Self::StockRandomized => STOCK_HOLD_MAX_MS,
            Self::Custom { max_ms, .. } => *max_ms,
        }
    }

    pub fn description(&self) -> String {
        match self {
            Self::StockRandomized => {
                format!("default ({}-{}ms)", STOCK_HOLD_MIN_MS, STOCK_HOLD_MAX_MS)
            }
            Self::Custom { min_ms, max_ms } => format!("custom ({}-{}ms)", min_ms, max_ms),
        }
    }

    pub fn option_name(&self) -> &'static str {
        match self {
            Self::StockRandomized => "default",
            Self::Custom { .. } => "custom",
        }
    }
}

#[derive(Clone)]
pub enum ReleasePacing {
    Default,
    Custom {
        packets_per_batch: usize,
        pause_ms: u64,
    },
}

impl ReleasePacing {
    pub fn packet_batch_size(&self) -> usize {
        match self {
            Self::Default => DEFAULT_RELEASE_BATCH_SIZE,
            Self::Custom {
                packets_per_batch, ..
            } => *packets_per_batch,
        }
    }

    pub fn batch_pause_ms(&self) -> u64 {
        match self {
            Self::Default => DEFAULT_RELEASE_DELAY_MS,
            Self::Custom { pause_ms, .. } => *pause_ms,
        }
    }

    pub fn description(&self) -> String {
        format!(
            "{} ({} packets / {}ms)",
            self.option_name(),
            self.packet_batch_size(),
            self.batch_pause_ms()
        )
    }

    pub fn option_name(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Custom { .. } => "custom",
        }
    }
}

fn checked_hold_value(entered: &str) -> Result<u64, SettingsError> {
    let milliseconds = entered
        .parse::<u64>()
        .map_err(|_| SettingsError::HoldNotANumber)?;
    if !(SHORTEST_HOLD_MS..=LONGEST_HOLD_MS).contains(&milliseconds) {
        return Err(SettingsError::HoldOutsideLimits);
    }
    Ok(milliseconds)
}

fn checked_batch_size(entered: &str) -> Result<usize, SettingsError> {
    let batch_size = entered
        .parse::<usize>()
        .map_err(|_| SettingsError::BatchNotANumber)?;
    if !(MIN_RELEASE_BATCH_SIZE..=MAX_RELEASE_BATCH_SIZE).contains(&batch_size) {
        return Err(SettingsError::BatchOutsideLimits);
    }
    Ok(batch_size)
}

fn checked_release_delay(entered: &str) -> Result<u64, SettingsError> {
    let delay = entered
        .parse::<u64>()
        .map_err(|_| SettingsError::DelayNotANumber)?;
    if delay > MAX_RELEASE_DELAY_MS {
        return Err(SettingsError::DelayOutsideLimits);
    }
    Ok(delay)
}

#[derive(Debug)]
pub enum SettingsError {
    PortNotANumber,
    PortZero,
    HoldNotANumber,
    HoldOutsideLimits,
    BatchNotANumber,
    BatchOutsideLimits,
    DelayNotANumber,
    DelayOutsideLimits,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::PortNotANumber => "enter a whole port number",
            Self::PortZero => "port 0 cannot carry intercepted traffic",
            Self::HoldNotANumber => "enter a hold time in milliseconds",
            Self::HoldOutsideLimits => "hold time must be between 100 and 10000 ms",
            Self::BatchNotANumber => "enter a packet count for each release batch",
            Self::BatchOutsideLimits => "batch size must be between 1 and 250 packets",
            Self::DelayNotANumber => "enter a delay in milliseconds",
            Self::DelayOutsideLimits => "release delay must be between 0 and 100 ms",
        };
        f.write_str(message)
    }
}

fn trigger_key_name(virtual_key: i32) -> &'static str {
    match virtual_key {
        VK_F9 => "f9",
        _ => "custom virtual key",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_rule_targets_the_remote_port_in_both_directions() {
        let rule = TrafficRule {
            port: Some(7777),
            protocol: TrafficKind::Udp,
        };

        assert_eq!(
            rule.compile_for_windivert().to_str().unwrap(),
            "((outbound and (udp and udp.DstPort == 7777)) or (inbound and (udp and udp.SrcPort == 7777)))"
        );
    }

    #[test]
    fn port_zero_is_not_a_usable_capture_rule() {
        let mut rule = TrafficRule::default();

        assert!(matches!(
            rule.set_port_from_text("0"),
            Err(SettingsError::PortZero)
        ));
    }

    #[test]
    fn factory_defaults_require_a_port_and_use_f9() {
        let settings = InterceptorSettings::with_update_status(UpdateStatus::UpToDate);

        assert!(!settings.traffic_rule.has_port());
        assert_eq!(settings.trigger_key, VK_F9);
        assert_eq!(settings.trigger_name, "f9");
    }

    #[test]
    fn custom_hold_window_does_not_silently_clamp_bad_input() {
        let mut settings = InterceptorSettings::default();

        assert!(matches!(
            settings.change_custom_minimum("99"),
            Err(SettingsError::HoldOutsideLimits)
        ));
    }

    #[test]
    fn choosing_default_does_not_forget_custom_timing() {
        let mut settings = InterceptorSettings::default();
        settings.change_custom_minimum("1200").unwrap();
        settings.change_custom_maximum("1600").unwrap();

        settings.switch_hold_window();
        assert!(matches!(settings.hold_window, HoldWindow::StockRandomized));

        settings.switch_hold_window();
        assert!(matches!(
            settings.hold_window,
            HoldWindow::Custom {
                min_ms: 1200,
                max_ms: 1600
            }
        ));
    }

    #[test]
    fn choosing_default_does_not_forget_custom_release_pacing() {
        let mut settings = InterceptorSettings::default();
        settings.change_custom_batch_size("20").unwrap();
        settings.change_custom_release_delay("8").unwrap();

        settings.switch_release_pacing();
        assert!(matches!(settings.release_pacing, ReleasePacing::Default));

        settings.switch_release_pacing();
        assert!(matches!(
            settings.release_pacing,
            ReleasePacing::Custom {
                packets_per_batch: 20,
                pause_ms: 8
            }
        ));
    }

    #[test]
    fn saved_preferences_round_trip_without_update_state() {
        let mut settings = InterceptorSettings::with_update_status(UpdateStatus::UpToDate);
        settings.traffic_rule.protocol = TrafficKind::Udp;
        settings.traffic_rule.port = Some(7788);
        settings.always_on_top = true;
        settings.change_custom_minimum("1300").unwrap();
        settings.change_custom_maximum("1700").unwrap();
        settings.change_custom_batch_size("18").unwrap();
        settings.change_custom_release_delay("6").unwrap();

        let saved = parse_saved_config(&settings.saved_values()).unwrap();
        let mut restored = InterceptorSettings::with_update_status(UpdateStatus::UpToDate);
        restored.apply_saved_values(&saved).unwrap();

        assert_eq!(restored.traffic_rule.port, Some(7788));
        assert!(matches!(restored.traffic_rule.protocol, TrafficKind::Udp));
        assert!(restored.always_on_top);
        assert!(matches!(
            restored.hold_window,
            HoldWindow::Custom {
                min_ms: 1300,
                max_ms: 1700
            }
        ));
        assert!(matches!(
            restored.release_pacing,
            ReleasePacing::Custom {
                packets_per_batch: 18,
                pause_ms: 6
            }
        ));
    }
}
