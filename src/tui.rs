use std::collections::VecDeque;
use std::io;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Sparkline, Wrap,
};
use ratatui::Terminal;

use crate::config::{HoldWindow, InterceptorSettings, ReleasePacing};
use crate::console_window;
use crate::interceptor::{LastEvent, State};
use crate::updater::{check_for_update, open_update_page, update_available, UpdateStatus};

const CLI_BANNER: &str = include_str!("../assets/cli.txt");
const SETTINGS_NOTICE_DURATION: Duration = Duration::from_secs(3);
const PANEL_WIDTH_PERCENT: u16 = 92;
const PANEL_HEIGHT_PERCENT: u16 = 90;
const PANEL_MIN_HEIGHT: u16 = 24;
const ASCII_TRAFFIC_BARS: symbols::bar::Set<'static> = symbols::bar::Set {
    // this is high activity
    full: "#",
    seven_eighths: "#",
    three_quarters: "#",

    // this is medium activity
    five_eighths: "+",
    half: "+",
    three_eighths: "+",

    // this is low activity
    one_quarter: ".",
    one_eighth: ".",

    // this is no activity
    empty: " ",
};

pub enum MenuAction {
    Start(InterceptorSettings),
    Exit,
}

pub enum DashboardAction {
    BackToMenu,
    ForceExit,
}

#[derive(Clone, Copy)]
enum MenuView {
    Main,
    Settings,
    CaptureKeybind,
    CapturePort,
    CaptureCustomMin,
    CaptureCustomMax,
    CaptureReleaseBatch,
    CaptureReleaseDelay,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsMode {
    Easy,
    Advanced,
}

impl SettingsMode {
    fn label(self) -> &'static str {
        match self {
            Self::Easy => "ez mode",
            Self::Advanced => "advanced mode",
        }
    }
}

#[derive(Clone, Copy)]
enum SettingAction {
    Protocol,
    Port,
    Trigger,
    AlwaysOnTop,
    HoldWindow,
    CustomHoldMin,
    CustomHoldMax,
    ReleasePacing,
    CustomBatch,
    CustomDelay,
    SwitchMode,
    Divider,
    Save,
    RestoreDefaults,
    Back,
}

pub fn cli_menu(initial_settings: Option<InterceptorSettings>) -> MenuAction {
    if let Err(error) = enable_raw_mode() {
        eprintln!("failed to initialize terminal menu: {error}");
        return MenuAction::Exit;
    }

    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
        disable_raw_mode().ok();
        execute!(stdout, DisableMouseCapture, LeaveAlternateScreen).ok();
        eprintln!("failed to open terminal menu: {error}");
        return MenuAction::Exit;
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            disable_raw_mode().ok();
            let mut cleanup_stdout = io::stdout();
            execute!(cleanup_stdout, DisableMouseCapture, LeaveAlternateScreen).ok();
            eprintln!("failed to create terminal menu: {error}");
            return MenuAction::Exit;
        }
    };

    let (mut settings, load_problem) = match initial_settings {
        Some(settings) => (settings, None),
        None => InterceptorSettings::load_saved(),
    };
    let (update_sender, update_receiver) = mpsc::channel();
    if matches!(settings.update_status, UpdateStatus::Checking) {
        std::thread::spawn(move || {
            update_sender.send(check_for_update()).ok();
        });
    }
    console_window::set_always_on_top(settings.always_on_top);
    let mut main_selected = 0usize;
    let mut settings_selected = 0usize;
    let mut settings_mode = SettingsMode::Easy;
    let mut input = String::new();
    let mut input_problem = None;
    let mut settings_notice = load_problem.map(|problem| (problem, Color::Red, Instant::now()));
    let mut view = MenuView::Main;

    let action = loop {
        if let Ok(update_status) = update_receiver.try_recv() {
            settings.update_status = update_status;
            main_selected = 0;
        }
        if settings_notice
            .as_ref()
            .is_some_and(|(_, _, started)| started.elapsed() >= SETTINGS_NOTICE_DURATION)
        {
            settings_notice = None;
        }
        if terminal
            .draw(|frame| {
                draw_menu(
                    frame,
                    main_selected,
                    settings_selected,
                    settings_mode,
                    view,
                    &settings,
                    &input,
                    input_problem.as_deref(),
                    settings_notice
                        .as_ref()
                        .map(|(message, color, _)| (message.as_str(), *color)),
                )
            })
            .is_err()
        {
            break MenuAction::Exit;
        }

        let has_event = match event::poll(Duration::from_millis(100)) {
            Ok(has_event) => has_event,
            Err(_) => break MenuAction::Exit,
        };
        if !has_event {
            continue;
        }
        let next_event = match event::read() {
            Ok(next_event) => next_event,
            Err(_) => break MenuAction::Exit,
        };

        if matches!(view, MenuView::CaptureKeybind) {
            match next_event {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc => view = MenuView::Settings,
                    code => {
                        if let Some((vk, label)) = key_code_to_virtual_key(code) {
                            settings.trigger_key = vk;
                            settings.trigger_name = label;
                            input_problem = None;
                            view = MenuView::Settings;
                        }
                    }
                },
                Event::Mouse(mouse) => {
                    if let MouseEventKind::Down(button) = mouse.kind {
                        let (vk, label) = mouse_button_to_virtual_key(button);
                        settings.trigger_key = vk;
                        settings.trigger_name = label;
                        input_problem = None;
                        view = MenuView::Settings;
                    }
                }
                _ => {}
            }
            continue;
        }

        let Event::Key(key) = next_event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match view {
            MenuView::Main => {
                if let Some(action) = handle_main_key(
                    key.code,
                    &mut main_selected,
                    &mut view,
                    &mut settings,
                    &mut settings_notice,
                ) {
                    break action;
                }
            }
            MenuView::Settings => {
                handle_settings_key(
                    key.code,
                    &mut settings_selected,
                    &mut settings_mode,
                    &mut view,
                    &mut input,
                    &mut settings,
                    &mut input_problem,
                    &mut settings_notice,
                );
            }
            MenuView::CaptureKeybind => {}
            MenuView::CapturePort => handle_port_input(
                key.code,
                &mut input,
                &mut settings,
                &mut view,
                &mut input_problem,
            ),
            MenuView::CaptureCustomMin => {
                handle_hold_input(
                    key.code,
                    &mut input,
                    &mut settings,
                    true,
                    &mut view,
                    &mut input_problem,
                );
            }
            MenuView::CaptureCustomMax => {
                handle_hold_input(
                    key.code,
                    &mut input,
                    &mut settings,
                    false,
                    &mut view,
                    &mut input_problem,
                );
            }
            MenuView::CaptureReleaseBatch => {
                handle_release_input(
                    key.code,
                    &mut input,
                    &mut settings,
                    true,
                    &mut view,
                    &mut input_problem,
                );
            }
            MenuView::CaptureReleaseDelay => {
                handle_release_input(
                    key.code,
                    &mut input,
                    &mut settings,
                    false,
                    &mut view,
                    &mut input_problem,
                );
            }
        }
    };

    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )
    .ok();
    terminal.show_cursor().ok();
    action
}

fn handle_main_key(
    code: KeyCode,
    selected: &mut usize,
    view: &mut MenuView,
    settings: &mut InterceptorSettings,
    settings_notice: &mut Option<(String, Color, Instant)>,
) -> Option<MenuAction> {
    let has_update = update_available(&settings.update_status);
    let last_item = if has_update { 1 } else { 2 };
    match code {
        KeyCode::Esc => return Some(MenuAction::Exit),
        KeyCode::Up => *selected = selected.saturating_sub(1),
        KeyCode::Down => *selected = (*selected + 1).min(last_item),
        KeyCode::Char(number @ '1'..='3') => {
            *selected = (number as usize - '1' as usize).min(last_item);
            return select_main_item(selected, view, settings, settings_notice);
        }
        KeyCode::Enter => return select_main_item(selected, view, settings, settings_notice),
        _ => {}
    }
    None
}

fn select_main_item(
    selected: &mut usize,
    view: &mut MenuView,
    settings: &mut InterceptorSettings,
    settings_notice: &mut Option<(String, Color, Instant)>,
) -> Option<MenuAction> {
    if update_available(&settings.update_status) {
        match *selected {
            0 => open_update_page(&settings.update_status),
            1 => {
                if let UpdateStatus::Available { version, url } = &settings.update_status {
                    settings.update_status = UpdateStatus::Skipped {
                        version: version.clone(),
                        url: url.clone(),
                    };
                    *selected = 0;
                }
            }
            _ => {}
        }
    } else {
        match *selected {
            0 if settings.traffic_rule.has_port() => {
                return Some(MenuAction::Start(settings.clone()))
            }
            0 => {
                *view = MenuView::Settings;
                *settings_notice = Some((
                    "set a port before starting interceptor".to_string(),
                    Color::Yellow,
                    Instant::now(),
                ));
            }
            1 => *view = MenuView::Settings,
            2 => return Some(MenuAction::Exit),
            _ => {}
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn handle_settings_key(
    code: KeyCode,
    selected: &mut usize,
    mode: &mut SettingsMode,
    view: &mut MenuView,
    input: &mut String,
    settings: &mut InterceptorSettings,
    input_problem: &mut Option<String>,
    settings_notice: &mut Option<(String, Color, Instant)>,
) {
    let visible_actions = settings_actions(*mode, settings);
    match code {
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') | KeyCode::Char('1') => {
            *view = MenuView::Main
        }
        KeyCode::Up => move_settings_selection(selected, &visible_actions, false),
        KeyCode::Down => move_settings_selection(selected, &visible_actions, true),
        code => {
            if let Some(index) = settings_shortcut_index(code, &visible_actions) {
                if index < visible_actions.len() {
                    *selected = index;
                    select_setting(
                        selected,
                        mode,
                        view,
                        input,
                        settings,
                        input_problem,
                        settings_notice,
                    );
                }
            } else if code == KeyCode::Enter {
                select_setting(
                    selected,
                    mode,
                    view,
                    input,
                    settings,
                    input_problem,
                    settings_notice,
                );
            }
        }
    }
}

fn select_setting(
    selected: &mut usize,
    mode: &mut SettingsMode,
    view: &mut MenuView,
    input: &mut String,
    settings: &mut InterceptorSettings,
    input_problem: &mut Option<String>,
    settings_notice: &mut Option<(String, Color, Instant)>,
) {
    *input_problem = None;
    *settings_notice = None;
    let Some(action) = settings_actions(*mode, settings)
        .get(*selected)
        .map(|(_, action)| *action)
    else {
        *selected = 0;
        return;
    };
    match action {
        SettingAction::Protocol => {
            settings.traffic_rule.protocol = settings.traffic_rule.protocol.next_protocol()
        }
        SettingAction::Port => {
            input.clear();
            *view = MenuView::CapturePort;
        }
        SettingAction::Trigger => *view = MenuView::CaptureKeybind,
        SettingAction::AlwaysOnTop => {
            let desired = !settings.always_on_top;
            if console_window::set_always_on_top(desired) {
                settings.always_on_top = desired;
            }
        }
        SettingAction::HoldWindow => settings.switch_hold_window(),
        SettingAction::CustomHoldMin => {
            input.clear();
            *view = MenuView::CaptureCustomMin;
        }
        SettingAction::CustomHoldMax => {
            input.clear();
            *view = MenuView::CaptureCustomMax;
        }
        SettingAction::ReleasePacing => settings.switch_release_pacing(),
        SettingAction::CustomBatch => {
            input.clear();
            *view = MenuView::CaptureReleaseBatch;
        }
        SettingAction::CustomDelay => {
            input.clear();
            *view = MenuView::CaptureReleaseDelay;
        }
        SettingAction::SwitchMode => {
            *mode = match *mode {
                SettingsMode::Easy => SettingsMode::Advanced,
                SettingsMode::Advanced => SettingsMode::Easy,
            };
            *selected = 0;
        }
        SettingAction::Divider => {}
        SettingAction::Save => match settings.save() {
            Ok(()) => {
                *settings_notice = Some((
                    "settings saved for next launch".to_string(),
                    Color::Green,
                    Instant::now(),
                ))
            }
            Err(problem) => *settings_notice = Some((problem, Color::Red, Instant::now())),
        },
        SettingAction::RestoreDefaults => {
            console_window::set_always_on_top(false);
            match settings.restore_defaults() {
                Ok(()) => {
                    *settings_notice = Some((
                        "defaults restored and saved".to_string(),
                        Color::Green,
                        Instant::now(),
                    ))
                }
                Err(problem) => {
                    *settings_notice = Some((
                        format!("defaults restored, but {problem}"),
                        Color::Red,
                        Instant::now(),
                    ))
                }
            }
        }
        SettingAction::Back => *view = MenuView::Main,
    }
    if matches!(*view, MenuView::Settings) {
        *selected = (*selected).min(settings_actions(*mode, settings).len() - 1);
    }
}

fn settings_shortcut_index(code: KeyCode, actions: &[(String, SettingAction)]) -> Option<usize> {
    let rank = match code {
        KeyCode::Char(number @ '2'..='9') => number as usize - '2' as usize,
        KeyCode::Char('0') => 8,
        KeyCode::Char(letter) if letter.is_ascii_alphabetic() => {
            9 + letter.to_ascii_lowercase() as usize - 'a' as usize
        }
        _ => return None,
    };
    actions
        .iter()
        .enumerate()
        .filter(|(_, (_, action))| !matches!(action, SettingAction::Divider | SettingAction::Back))
        .nth(rank)
        .map(|(index, _)| index)
}

fn move_settings_selection(
    selected: &mut usize,
    actions: &[(String, SettingAction)],
    forward: bool,
) {
    let next = if forward {
        ((*selected + 1)..actions.len())
            .find(|index| !matches!(actions[*index].1, SettingAction::Divider))
    } else {
        (0..*selected)
            .rev()
            .find(|index| !matches!(actions[*index].1, SettingAction::Divider))
    };
    if let Some(index) = next {
        *selected = index;
    }
}

fn settings_actions(
    mode: SettingsMode,
    settings: &InterceptorSettings,
) -> Vec<(String, SettingAction)> {
    let mut actions = vec![
        (
            format!("protocol: {}", settings.traffic_rule.protocol.menu_name()),
            SettingAction::Protocol,
        ),
        (
            format!("port: {}", settings.traffic_rule.port_name()),
            SettingAction::Port,
        ),
        (
            format!("change trigger: {}", settings.trigger_name),
            SettingAction::Trigger,
        ),
        (
            format!("always on top: {}", settings.always_on_top_name()),
            SettingAction::AlwaysOnTop,
        ),
    ];

    if mode == SettingsMode::Advanced {
        actions.push((
            format!("hold window: {}", settings.hold_window.option_name()),
            SettingAction::HoldWindow,
        ));
        if matches!(settings.hold_window, HoldWindow::Custom { .. }) {
            actions.push((
                format!("custom hold minimum: {}ms", settings.custom_min_ms()),
                SettingAction::CustomHoldMin,
            ));
            actions.push((
                format!("custom hold maximum: {}ms", settings.custom_max_ms()),
                SettingAction::CustomHoldMax,
            ));
        }
        actions.push((
            format!("release pacing: {}", settings.release_pacing.option_name()),
            SettingAction::ReleasePacing,
        ));
        if matches!(settings.release_pacing, ReleasePacing::Custom { .. }) {
            actions.push((
                format!(
                    "custom batch size: {} packets",
                    settings.custom_batch_size()
                ),
                SettingAction::CustomBatch,
            ));
            actions.push((
                format!(
                    "custom batch delay: {}ms",
                    settings.custom_release_delay_ms()
                ),
                SettingAction::CustomDelay,
            ));
        }
        actions.push(("ez mode".to_string(), SettingAction::SwitchMode));
    } else {
        actions.push(("advanced mode".to_string(), SettingAction::SwitchMode));
    }
    actions.push((String::new(), SettingAction::Divider));
    actions.push(("save settings".to_string(), SettingAction::Save));
    actions.push((
        "restore defaults".to_string(),
        SettingAction::RestoreDefaults,
    ));
    actions.push(("back".to_string(), SettingAction::Back));
    actions
}

#[allow(clippy::too_many_arguments)]
fn draw_menu(
    frame: &mut ratatui::Frame,
    main_selected: usize,
    settings_selected: usize,
    settings_mode: SettingsMode,
    view: MenuView,
    settings: &InterceptorSettings,
    input: &str,
    input_problem: Option<&str>,
    settings_notice: Option<(&str, Color)>,
) {
    let (area, footer_area) = app_panel_layout(frame.area());
    frame.render_widget(Clear, area);

    let mut outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    if !matches!(view, MenuView::Main) {
        outer = outer
            .title(" interceptor ")
            .title_alignment(Alignment::Center);
    }
    frame.render_widget(outer, area);

    let inner = area.inner(Margin {
        horizontal: 3,
        vertical: if matches!(view, MenuView::Main) { 1 } else { 2 },
    });
    let chunks = if matches!(view, MenuView::Main) {
        Layout::vertical([Constraint::Length(7), Constraint::Min(11)]).split(inner)
    } else {
        Layout::vertical([Constraint::Min(7)]).split(inner)
    };

    match view {
        MenuView::Main => {
            draw_menu_banner(frame, chunks[0]);
            draw_main_menu(frame, chunks[1], main_selected, settings);
        }
        MenuView::Settings => {
            draw_settings(frame, chunks[0], settings_selected, settings, settings_mode)
        }
        MenuView::CaptureKeybind => draw_keybind_capture(frame, chunks[0], settings),
        MenuView::CapturePort => {
            draw_port_capture(frame, chunks[0], input, settings, input_problem)
        }
        MenuView::CaptureCustomMin => draw_number_capture(
            frame,
            chunks[0],
            "custom minimum hold",
            input,
            settings,
            input_problem,
        ),
        MenuView::CaptureCustomMax => draw_number_capture(
            frame,
            chunks[0],
            "custom maximum hold",
            input,
            settings,
            input_problem,
        ),
        MenuView::CaptureReleaseBatch => draw_release_capture(
            frame,
            chunks[0],
            "packets per release batch",
            input,
            settings,
            input_problem,
            true,
        ),
        MenuView::CaptureReleaseDelay => draw_release_capture(
            frame,
            chunks[0],
            "pause between batches",
            input,
            settings,
            input_problem,
            false,
        ),
    }

    let help_text = match view {
        MenuView::Main => "up/down: move  enter: select  esc: exit",
        MenuView::Settings => "up/down: move  enter: select  1/esc: back",
        MenuView::CaptureKeybind => "press a keyboard key or mouse button  esc: cancel",
        MenuView::CapturePort => "type port number  enter: save  esc: cancel",
        MenuView::CaptureReleaseBatch => "type packets per batch  enter: save  esc: cancel",
        _ => "type milliseconds  enter: save  esc: cancel",
    };
    let help_line = if matches!(view, MenuView::Settings) {
        settings_notice.map_or_else(
            || Line::styled(help_text, Style::default().fg(Color::DarkGray)),
            |(message, color)| {
                Line::styled(
                    message.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )
            },
        )
    } else {
        Line::styled(help_text, Style::default().fg(Color::DarkGray))
    };
    frame.render_widget(
        Paragraph::new(help_line).alignment(Alignment::Center),
        footer_area,
    );
}

fn draw_menu_banner(frame: &mut ratatui::Frame, area: Rect) {
    let banner_text = if area.height < 7 {
        "interceptor".to_string()
    } else {
        format!("\n{CLI_BANNER}")
    };
    frame.render_widget(
        Paragraph::new(banner_text)
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_main_menu(
    frame: &mut ratatui::Frame,
    area: Rect,
    selected: usize,
    settings: &InterceptorSettings,
) {
    let labels: Vec<&str> = if update_available(&settings.update_status) {
        vec!["update now", "skip update"]
    } else {
        vec!["start interceptor", "settings", "exit"]
    };
    let items = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("[{}] ", index + 1),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(*label),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(selected));
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    let content_height = labels.len() as u16 + 2;
    let content_area = Rect {
        x: area.x,
        y: area.y + (area.height.saturating_sub(content_height) / 2).saturating_sub(2),
        width: area.width,
        height: content_height.min(area.height),
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(labels.len() as u16),
    ])
    .split(content_area);
    frame.render_widget(
        Paragraph::new(update_status_line(&settings.update_status)).alignment(Alignment::Center),
        chunks[0],
    );
    let menu_area = Layout::horizontal([
        Constraint::Percentage(29),
        Constraint::Percentage(42),
        Constraint::Percentage(29),
    ])
    .split(chunks[2])[1];
    frame.render_stateful_widget(list, menu_area, &mut state);
}

fn draw_settings(
    frame: &mut ratatui::Frame,
    area: Rect,
    selected: usize,
    settings: &InterceptorSettings,
    mode: SettingsMode,
) {
    let panel_area = centered_rect_with_min(94, 98, 20, area);
    let mut summary = vec![
        setting_line("mode", mode.label().to_string(), Color::Green),
        setting_line("traffic", settings.traffic_rule.summary(), Color::Cyan),
        setting_line("trigger", settings.trigger_name.clone(), Color::Cyan),
        setting_line(
            "always on top",
            settings.always_on_top_name().to_string(),
            Color::Green,
        ),
    ];
    if mode == SettingsMode::Advanced {
        summary.insert(
            2,
            setting_line("hold", settings.hold_window.description(), Color::Yellow),
        );
        summary.insert(
            3,
            setting_line(
                "release",
                settings.release_pacing.description(),
                Color::Magenta,
            ),
        );
    }
    let summary_height = summary.len() as u16 + 2;
    let rows = Layout::vertical([Constraint::Length(summary_height), Constraint::Min(10)])
        .split(panel_area);
    frame.render_widget(
        Paragraph::new(summary).alignment(Alignment::Center).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" current settings "),
        ),
        rows[0],
    );

    let actions = settings_actions(mode, settings);
    let items = actions
        .iter()
        .enumerate()
        .map(|(index, (label, action))| {
            if matches!(action, SettingAction::Divider) {
                ListItem::new(Line::styled(
                    "  ---------------- profile ----------------",
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                let hotkey = setting_hotkey(index, *action, &actions);
                action_line(&hotkey, label, *action)
            }
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(selected));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" actions "))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    if rows[1].width >= 86 {
        let body = Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(rows[1]);
        let mut details = vec![
            setting_line(
                "capture layer",
                "WinDivert network".to_string(),
                Color::Cyan,
            ),
            setting_line("traffic", settings.traffic_rule.summary(), Color::Cyan),
            setting_line(
                "always on top",
                settings.always_on_top_name().to_string(),
                Color::Green,
            ),
        ];
        if mode == SettingsMode::Advanced {
            details.push(setting_line(
                "hold",
                settings.hold_window.description(),
                Color::Yellow,
            ));
            details.push(setting_line(
                "release",
                settings.release_pacing.description(),
                Color::Magenta,
            ));
        }
        frame.render_widget(
            Paragraph::new(details)
                .block(Block::default().borders(Borders::ALL).title(" details ")),
            body[0],
        );
        frame.render_stateful_widget(list, body[1], &mut state);
    } else {
        frame.render_stateful_widget(list, rows[1], &mut state);
    }
}

fn setting_hotkey(
    index: usize,
    action: SettingAction,
    actions: &[(String, SettingAction)],
) -> String {
    if matches!(action, SettingAction::Back) {
        return "1".to_string();
    }
    let rank = actions[..index]
        .iter()
        .filter(|(_, earlier)| !matches!(earlier, SettingAction::Divider | SettingAction::Back))
        .count();
    match rank {
        0..=7 => (rank + 2).to_string(),
        8 => "0".to_string(),
        _ => ((b'A' + (rank - 9) as u8) as char).to_string(),
    }
}

fn action_line(hotkey: &str, label: &str, action: SettingAction) -> ListItem<'static> {
    let value_color = match action {
        SettingAction::Protocol | SettingAction::Port | SettingAction::Trigger => Color::Cyan,
        SettingAction::AlwaysOnTop => Color::Green,
        SettingAction::HoldWindow | SettingAction::CustomHoldMin | SettingAction::CustomHoldMax => {
            Color::Yellow
        }
        SettingAction::ReleasePacing | SettingAction::CustomBatch | SettingAction::CustomDelay => {
            Color::Magenta
        }
        SettingAction::SwitchMode => Color::Green,
        SettingAction::Save => Color::Green,
        SettingAction::RestoreDefaults => Color::Yellow,
        SettingAction::Divider => Color::DarkGray,
        SettingAction::Back => Color::DarkGray,
    };
    let mut spans = vec![Span::styled(
        format!("[{hotkey}] "),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some((name, value)) = label.split_once(": ") {
        spans.push(Span::styled(
            format!("{name}: "),
            Style::default().fg(Color::Gray),
        ));
        spans.push(Span::styled(
            value.to_string(),
            Style::default()
                .fg(value_color)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            label.to_string(),
            Style::default()
                .fg(value_color)
                .add_modifier(Modifier::BOLD),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn setting_line(label: &str, value: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            value,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn draw_keybind_capture(frame: &mut ratatui::Frame, area: Rect, settings: &InterceptorSettings) {
    let panels = draw_editor_header(
        frame,
        area,
        "trigger keybind",
        "select the input used to begin a hold window",
    );
    draw_current_value_panel(
        frame,
        panels[0],
        "current binding",
        &settings.trigger_name,
        Color::Cyan,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::styled(
                "waiting for input...",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                "press any keyboard key or mouse button",
                Style::default().fg(Color::Gray),
            ),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" new binding ")
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        panels[1],
    );
}

fn draw_port_capture(
    frame: &mut ratatui::Frame,
    area: Rect,
    input: &str,
    settings: &InterceptorSettings,
    input_problem: Option<&str>,
) {
    let panels = draw_editor_header(
        frame,
        area,
        "traffic port",
        "choose the remote port to capture during a hold window",
    );
    draw_current_value_panel(
        frame,
        panels[0],
        "current filter",
        &settings.traffic_rule.summary(),
        Color::Cyan,
    );
    draw_value_entry_panel(
        frame,
        panels[1],
        "new port",
        "port",
        input,
        "",
        "valid range: 1 - 65535",
        input_problem,
        Color::Cyan,
    );
}

fn draw_number_capture(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    input: &str,
    settings: &InterceptorSettings,
    input_problem: Option<&str>,
) {
    let panels = draw_editor_header(
        frame,
        area,
        title,
        "customize how long matching traffic is held",
    );
    draw_current_value_panel(
        frame,
        panels[0],
        "current hold",
        &settings.hold_window.description(),
        Color::Yellow,
    );
    draw_value_entry_panel(
        frame,
        panels[1],
        "new duration",
        "duration",
        input,
        " ms",
        "valid range: 100 - 10000 ms",
        input_problem,
        Color::Yellow,
    );
}

fn draw_release_capture(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    input: &str,
    settings: &InterceptorSettings,
    input_problem: Option<&str>,
    editing_batch: bool,
) {
    let (suffix, range) = if editing_batch {
        (" packets", "valid range: 1 - 250 packets")
    } else {
        (" ms", "valid range: 0 - 100 ms")
    };
    let panels = draw_editor_header(
        frame,
        area,
        title,
        "customize how queued packets are released",
    );
    draw_current_value_panel(
        frame,
        panels[0],
        "current release",
        &settings.release_pacing.description(),
        Color::Magenta,
    );
    draw_value_entry_panel(
        frame,
        panels[1],
        "new value",
        "value",
        input,
        suffix,
        range,
        input_problem,
        Color::Magenta,
    );
}

fn draw_editor_header(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    subtitle: &str,
) -> Vec<Rect> {
    let editor_area = centered_rect_with_min(92, 96, 16, area);
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(10)]).split(editor_area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                title.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(subtitle.to_string(), Style::default().fg(Color::DarkGray)),
        ])
        .alignment(Alignment::Center),
        rows[0],
    );

    if rows[1].width >= 72 {
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(rows[1])
            .to_vec()
    } else {
        Layout::vertical([Constraint::Length(5), Constraint::Min(6)])
            .split(rows[1])
            .to_vec()
    }
}

fn draw_current_value_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    value: &str,
    color: Color,
) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::styled(
                value.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} "))
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        area,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_value_entry_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    label: &str,
    input: &str,
    suffix: &str,
    range: &str,
    input_problem: Option<&str>,
    color: Color,
) {
    let entered = if input.is_empty() { "_" } else { input };
    let status = match input_problem {
        Some(problem) => Line::styled(
            problem.to_string(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        None => Line::styled(range.to_string(), Style::default().fg(Color::DarkGray)),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled(format!("{label}: "), Style::default().fg(Color::Gray)),
                Span::styled(
                    entered.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(suffix.to_string(), Style::default().fg(Color::Gray)),
            ]),
            Line::raw(""),
            status,
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} "))
                .border_style(Style::default().fg(color)),
        ),
        area,
    );
}

fn handle_port_input(
    code: KeyCode,
    input: &mut String,
    settings: &mut InterceptorSettings,
    view: &mut MenuView,
    input_problem: &mut Option<String>,
) {
    match code {
        KeyCode::Esc => {
            input.clear();
            *input_problem = None;
            *view = MenuView::Settings;
        }
        KeyCode::Backspace => {
            input.pop();
            *input_problem = None;
        }
        KeyCode::Char(ch) if ch.is_ascii_digit() && input.len() < 5 => {
            input.push(ch);
            *input_problem = None;
        }
        KeyCode::Enter => match settings.traffic_rule.set_port_from_text(input) {
            Ok(()) => {
                input.clear();
                *input_problem = None;
                *view = MenuView::Settings;
            }
            Err(problem) => *input_problem = Some(problem.to_string()),
        },
        _ => {}
    }
}

fn handle_hold_input(
    code: KeyCode,
    input: &mut String,
    settings: &mut InterceptorSettings,
    editing_min: bool,
    view: &mut MenuView,
    input_problem: &mut Option<String>,
) {
    match code {
        KeyCode::Esc => {
            input.clear();
            *input_problem = None;
            *view = MenuView::Settings;
        }
        KeyCode::Backspace => {
            input.pop();
            *input_problem = None;
        }
        KeyCode::Char(ch) if ch.is_ascii_digit() && input.len() < 5 => {
            input.push(ch);
            *input_problem = None;
        }
        KeyCode::Enter => {
            let result = if editing_min {
                settings.change_custom_minimum(input)
            } else {
                settings.change_custom_maximum(input)
            };
            match result {
                Ok(()) => {
                    input.clear();
                    *input_problem = None;
                    *view = MenuView::Settings;
                }
                Err(problem) => *input_problem = Some(problem.to_string()),
            }
        }
        _ => {}
    }
}

fn handle_release_input(
    code: KeyCode,
    input: &mut String,
    settings: &mut InterceptorSettings,
    editing_batch: bool,
    view: &mut MenuView,
    input_problem: &mut Option<String>,
) {
    match code {
        KeyCode::Esc => {
            input.clear();
            *input_problem = None;
            *view = MenuView::Settings;
        }
        KeyCode::Backspace => {
            input.pop();
            *input_problem = None;
        }
        KeyCode::Char(ch) if ch.is_ascii_digit() && input.len() < 3 => {
            input.push(ch);
            *input_problem = None;
        }
        KeyCode::Enter => {
            let result = if editing_batch {
                settings.change_custom_batch_size(input)
            } else {
                settings.change_custom_release_delay(input)
            };
            match result {
                Ok(()) => {
                    input.clear();
                    *input_problem = None;
                    *view = MenuView::Settings;
                }
                Err(problem) => *input_problem = Some(problem.to_string()),
            }
        }
        _ => {}
    }
}

fn key_code_to_virtual_key(code: KeyCode) -> Option<(i32, String)> {
    match code {
        KeyCode::Char(ch) if ch.is_ascii_alphabetic() => {
            let upper = ch.to_ascii_uppercase();
            Some((upper as i32, upper.to_string()))
        }
        KeyCode::Char(ch) if ch.is_ascii_digit() => Some((ch as i32, ch.to_string())),
        KeyCode::Char(' ') => Some((0x20, "space".to_string())),
        KeyCode::Enter => Some((0x0D, "enter".to_string())),
        KeyCode::Tab => Some((0x09, "tab".to_string())),
        KeyCode::Backspace => Some((0x08, "backspace".to_string())),
        KeyCode::Left => Some((0x25, "left arrow".to_string())),
        KeyCode::Up => Some((0x26, "up arrow".to_string())),
        KeyCode::Right => Some((0x27, "right arrow".to_string())),
        KeyCode::Down => Some((0x28, "down arrow".to_string())),
        KeyCode::Home => Some((0x24, "home".to_string())),
        KeyCode::End => Some((0x23, "end".to_string())),
        KeyCode::PageUp => Some((0x21, "page up".to_string())),
        KeyCode::PageDown => Some((0x22, "page down".to_string())),
        KeyCode::Delete => Some((0x2E, "delete".to_string())),
        KeyCode::Insert => Some((0x2D, "insert".to_string())),
        KeyCode::F(n) if (1..=12).contains(&n) => Some((0x70 + n as i32 - 1, format!("f{n}"))),
        _ => None,
    }
}

fn mouse_button_to_virtual_key(button: MouseButton) -> (i32, String) {
    match button {
        MouseButton::Left => (0x01, "left mouse button".to_string()),
        MouseButton::Right => (0x02, "right mouse button".to_string()),
        MouseButton::Middle => (0x04, "middle mouse button".to_string()),
    }
}

fn update_status_line(status: &UpdateStatus) -> Line<'static> {
    match status {
        UpdateStatus::Checking => Line::styled(
            "checking for updates...",
            Style::default().fg(Color::DarkGray),
        ),
        UpdateStatus::UpToDate => Line::styled(
            "up to date",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        UpdateStatus::Available { version, .. } | UpdateStatus::Skipped { version, .. } => {
            Line::from(vec![
                Span::styled(
                    "update available: ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("v{version}")),
            ])
        }
        UpdateStatus::Failed(error) => Line::from(vec![
            Span::styled("update check failed", Style::default().fg(Color::Red)),
            Span::raw(format!(": {error}")),
        ]),
    }
}

pub fn run_interceptor_dashboard(
    state: Arc<Mutex<State>>,
    settings: InterceptorSettings,
) -> DashboardAction {
    if enable_raw_mode().is_err() {
        return DashboardAction::BackToMenu;
    }
    let mut stdout = io::stdout();
    if execute!(stdout, EnterAlternateScreen).is_err() {
        disable_raw_mode().ok();
        execute!(stdout, LeaveAlternateScreen).ok();
        return DashboardAction::BackToMenu;
    }
    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(terminal) => terminal,
        Err(_) => {
            disable_raw_mode().ok();
            let mut cleanup_stdout = io::stdout();
            execute!(cleanup_stdout, LeaveAlternateScreen).ok();
            return DashboardAction::BackToMenu;
        }
    };
    let mut traffic_activity = TrafficActivity::new();

    let action = loop {
        traffic_activity.sample(&state);
        if terminal
            .draw(|frame| draw_runtime_dashboard(frame, &state, &settings, &traffic_activity))
            .is_err()
        {
            break DashboardAction::BackToMenu;
        }
        let has_event = match event::poll(Duration::from_millis(100)) {
            Ok(has_event) => has_event,
            Err(_) => break DashboardAction::BackToMenu,
        };
        if has_event {
            let next_event = match event::read() {
                Ok(next_event) => next_event,
                Err(_) => break DashboardAction::BackToMenu,
            };
            if let Event::Key(key) = next_event {
                if let Some(action) = dashboard_action_for_key(key) {
                    break action;
                }
            }
        }
    };

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    action
}

fn dashboard_action_for_key(key: event::KeyEvent) -> Option<DashboardAction> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return Some(DashboardAction::ForceExit);
    }
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        return Some(DashboardAction::BackToMenu);
    }
    None
}

struct TrafficActivity {
    matched: VecDeque<u64>,
    queued: VecDeque<u64>,
    released: VecDeque<u64>,
    previous_totals: (u64, u64, u64),
    last_sample: Instant,
}

impl TrafficActivity {
    const WIDTH: usize = 40;

    fn new() -> Self {
        Self {
            matched: VecDeque::from(vec![0; Self::WIDTH]),
            queued: VecDeque::from(vec![0; Self::WIDTH]),
            released: VecDeque::from(vec![0; Self::WIDTH]),
            previous_totals: (0, 0, 0),
            last_sample: Instant::now() - Duration::from_millis(100),
        }
    }

    fn sample(&mut self, state: &Arc<Mutex<State>>) {
        if self.last_sample.elapsed() < Duration::from_millis(100) {
            return;
        }
        let totals = state
            .lock()
            .map(|state| (state.matched_total, state.queued_total, state.burst_total))
            .unwrap_or(self.previous_totals);
        Self::push_sample(
            &mut self.matched,
            totals.0.saturating_sub(self.previous_totals.0),
        );
        Self::push_sample(
            &mut self.queued,
            totals.1.saturating_sub(self.previous_totals.1),
        );
        Self::push_sample(
            &mut self.released,
            totals.2.saturating_sub(self.previous_totals.2),
        );
        self.previous_totals = totals;
        self.last_sample = Instant::now();
    }

    fn push_sample(history: &mut VecDeque<u64>, amount: u64) {
        if history.len() == Self::WIDTH {
            history.pop_front();
        }
        history.push_back(amount);
    }

    fn last(history: &VecDeque<u64>) -> u64 {
        history.back().copied().unwrap_or(0)
    }
}

fn draw_runtime_dashboard(
    frame: &mut ratatui::Frame,
    state: &Arc<Mutex<State>>,
    settings: &InterceptorSettings,
    traffic_activity: &TrafficActivity,
) {
    let (area, footer_area) = app_panel_layout(frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(" interceptor ")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 3,
        vertical: 1,
    });
    let chunks = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(13),
        Constraint::Length(5),
    ])
    .split(inner);

    let snapshot = state.lock().ok();
    let (status, color, queue, matched, queued, released, failed, remaining, last_event) =
        if let Some(state) = snapshot.as_ref() {
            let remaining = if state.throttling {
                match (state.throttle_start, state.hold_duration) {
                    (Some(start), Some(hold)) => {
                        hold.saturating_sub(start.elapsed()).as_millis().to_string()
                    }
                    _ => "unknown".to_string(),
                }
            } else {
                "-".to_string()
            };
            let (status, color) = if state.throttling {
                ("throttling", Color::Yellow)
            } else if state.session_active {
                ("releasing", Color::Magenta)
            } else {
                ("armed", Color::Green)
            };
            (
                status,
                color,
                state.queue_len(),
                state.matched_total,
                state.queued_total,
                state.burst_total,
                state.release_failures,
                remaining,
                state.last_event.clone(),
            )
        } else {
            (
                "state unavailable",
                Color::Red,
                0,
                0,
                0,
                0,
                0,
                "-".to_string(),
                LastEvent::Error("state lock failed".into()),
            )
        };

    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(vec![
                Span::raw("status: "),
                Span::styled(
                    status,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("   trigger: "),
                Span::styled(&settings.trigger_name, Style::default().fg(Color::Yellow)),
                Span::raw("   hold: "),
                Span::styled(
                    settings.hold_window.description(),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" status ")),
        chunks[0],
    );

    let body = if chunks[1].width >= 86 {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1])
    } else {
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(chunks[1])
    };
    draw_live_traffic(
        frame,
        body[0],
        traffic_activity,
        queue,
        matched,
        queued,
        released,
        failed,
    );
    draw_active_setup(frame, body[1], settings, remaining);
    frame.render_widget(
        Paragraph::new(vec![Line::raw(""), event_line(&last_event)])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" last event ")),
        chunks[2],
    );
    frame.render_widget(
        Paragraph::new("q/esc: main menu  |  ctrl+c: force exit")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center),
        footer_area,
    );
}

fn draw_active_setup(
    frame: &mut ratatui::Frame,
    area: Rect,
    settings: &InterceptorSettings,
    remaining: String,
) {
    let lines = if area.height >= 12 {
        vec![
            Line::raw(""),
            metric_line("trigger", settings.trigger_name.clone(), Color::Cyan),
            metric_line("traffic", settings.traffic_rule.summary(), Color::Green),
            Line::raw(""),
            metric_line(
                "hold window",
                settings.hold_window.description(),
                Color::Yellow,
            ),
            metric_line(
                "release pacing",
                settings.release_pacing.description(),
                Color::Magenta,
            ),
            metric_line("remaining", format!("{remaining} ms"), Color::Yellow),
        ]
    } else {
        vec![
            metric_line("trigger", settings.trigger_name.clone(), Color::Cyan),
            metric_line("traffic", settings.traffic_rule.summary(), Color::Green),
            metric_line("hold", settings.hold_window.description(), Color::Yellow),
            metric_line(
                "release",
                settings.release_pacing.description(),
                Color::Magenta,
            ),
        ]
    };
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" active setup "),
        ),
        area,
    );
}

fn event_line(event: &LastEvent) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let bold = |color: Color| Style::default().fg(color).add_modifier(Modifier::BOLD);
    match event {
        LastEvent::Waiting => Line::styled("waiting for keybind", dim),
        LastEvent::OpeningCapture => Line::styled("opening capture window", dim),
        LastEvent::ThrottleOn {
            hold_secs,
            matched,
            queue,
        } => Line::from(vec![
            Span::raw(format!("throttle on: auto-hold {hold_secs:.1}s, matched ")),
            Span::styled(matched.to_string(), bold(Color::Cyan)),
            Span::raw(", queue "),
            Span::styled(queue.to_string(), bold(Color::Yellow)),
        ]),
        LastEvent::PacketQueued {
            bytes,
            queue,
            total,
        } => Line::from(vec![
            Span::styled("queued packet ", dim),
            Span::styled(format!("{bytes} bytes"), bold(Color::Cyan)),
            Span::styled(", queue ", dim),
            Span::styled(queue.to_string(), bold(Color::Yellow)),
            Span::styled(", total queued ", dim),
            Span::styled(total.to_string(), bold(Color::Yellow)),
        ]),
        LastEvent::QueueLimitReached {
            queued,
            bytes,
            passed_through,
        } => Line::from(vec![
            Span::styled("queue limit reached: ", bold(Color::Yellow)),
            Span::styled(queued.to_string(), bold(Color::Yellow)),
            Span::raw(" held / "),
            Span::styled(
                format!("{} MiB", bytes / (1024 * 1024)),
                bold(Color::Yellow),
            ),
            Span::raw(", passed through "),
            Span::styled(passed_through.to_string(), bold(Color::Cyan)),
        ]),
        LastEvent::NoTraffic { summary } => {
            Line::raw(format!("no matching {summary} traffic during hold window"))
        }
        LastEvent::AutoBurst {
            hold_secs,
            matched,
            queue,
        } => Line::from(vec![
            Span::raw(format!("auto-burst after {hold_secs:.1}s: matched ")),
            Span::styled(matched.to_string(), bold(Color::Cyan)),
            Span::raw(", queued "),
            Span::styled(queue.to_string(), bold(Color::Yellow)),
        ]),
        LastEvent::Released { count, remaining } => Line::from(vec![
            Span::raw("released "),
            Span::styled(count.to_string(), bold(Color::Green)),
            Span::raw(" packets, remaining "),
            Span::styled(remaining.to_string(), bold(Color::Cyan)),
        ]),
        LastEvent::ReleaseFailed {
            windows_error,
            total_failures,
        } => Line::from(vec![
            Span::styled("release failed ", bold(Color::Red)),
            Span::raw(format!("(Windows error {windows_error}, ")),
            Span::styled(total_failures.to_string(), bold(Color::Red)),
            Span::raw(" total failures)"),
        ]),
        LastEvent::ReleaseComplete { sent } => Line::from(vec![
            Span::styled("release completed: ", dim),
            Span::styled(sent.to_string(), bold(Color::Green)),
            Span::styled(" packets sent", Style::default().fg(Color::Green)),
        ]),
        LastEvent::ReleaseWithFailures { sent, failed } => Line::from(vec![
            Span::styled("release completed: ", dim),
            Span::styled(format!("{sent} sent"), bold(Color::Green)),
            Span::raw(", "),
            Span::styled(format!("{failed} failed"), bold(Color::Red)),
        ]),
        LastEvent::StillQueued { remaining } => Line::from(vec![
            Span::raw("release ended with "),
            Span::styled(remaining.to_string(), bold(Color::Yellow)),
            Span::raw(" packets still queued"),
        ]),
        LastEvent::Error(msg) => Line::styled(msg.clone(), bold(Color::Red)),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_live_traffic(
    frame: &mut ratatui::Frame,
    area: Rect,
    activity: &TrafficActivity,
    queue: usize,
    matched: u64,
    queued: u64,
    released: u64,
    failed: u64,
) {
    if area.height < 12 {
        frame.render_widget(
            Paragraph::new(vec![
                metric_line("queue", queue.to_string(), Color::Cyan),
                metric_line("matched", matched.to_string(), Color::Green),
                metric_line("queued", queued.to_string(), Color::Yellow),
                metric_line("released", released.to_string(), Color::Magenta),
                metric_line("release errors", failed.to_string(), Color::Red),
            ])
            .block(Block::default().borders(Borders::ALL).title(" traffic ")),
            area,
        );
        return;
    }

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" live traffic / 100ms ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);
    let matched_data = activity.matched.iter().copied().collect::<Vec<_>>();
    let queued_data = activity.queued.iter().copied().collect::<Vec<_>>();
    let released_data = activity.released.iter().copied().collect::<Vec<_>>();

    draw_traffic_sparkline(
        frame,
        rows[0],
        "matched",
        TrafficActivity::last(&activity.matched),
        matched,
        &matched_data,
        Color::Green,
    );
    draw_traffic_sparkline(
        frame,
        rows[1],
        "queued",
        TrafficActivity::last(&activity.queued),
        queued,
        &queued_data,
        Color::Yellow,
    );
    draw_traffic_sparkline(
        frame,
        rows[2],
        "released",
        TrafficActivity::last(&activity.released),
        released,
        &released_data,
        Color::Magenta,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("queue: ", Style::default().fg(Color::DarkGray)),
            Span::styled(queue.to_string(), Style::default().fg(Color::Cyan)),
            Span::raw("   "),
            Span::styled("errors: ", Style::default().fg(Color::DarkGray)),
            Span::styled(failed.to_string(), Style::default().fg(Color::Red)),
        ]))
        .alignment(Alignment::Center),
        rows[4],
    );
}

fn draw_traffic_sparkline(
    frame: &mut ratatui::Frame,
    area: Rect,
    label: &str,
    current: u64,
    total: u64,
    samples: &[u64],
    color: Color,
) {
    frame.render_widget(
        Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(vec![
                        Span::raw(format!(" {label}: ")),
                        Span::styled(
                            format!("+{current}"),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" / total "),
                        Span::styled(
                            total.to_string(),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                    ])),
            )
            .style(Style::default().fg(color))
            .bar_set(ASCII_TRAFFIC_BARS)
            .data(samples),
        area,
    );
}

fn metric_line(label: &str, value: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            value,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn centered_rect_with_min(percent_x: u16, percent_y: u16, min_height: u16, area: Rect) -> Rect {
    let target_height = (area.height.saturating_mul(percent_y) / 100).max(min_height);
    let height = target_height.min(area.height);
    let target_width = area.width.saturating_mul(percent_x) / 100;
    let width = target_width.max(40).min(area.width);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn app_panel_area(area: Rect) -> Rect {
    centered_rect_with_min(
        PANEL_WIDTH_PERCENT,
        PANEL_HEIGHT_PERCENT,
        PANEL_MIN_HEIGHT,
        area,
    )
}

fn app_panel_layout(area: Rect) -> (Rect, Rect) {
    let mut panel = app_panel_area(area);
    if panel.bottom() >= area.bottom() {
        panel.y = panel.y.saturating_sub(1);
    }
    let footer = Rect {
        x: panel.x,
        y: panel.bottom().min(area.bottom().saturating_sub(1)),
        width: panel.width,
        height: 1,
    };
    (panel, footer)
}

#[cfg(test)]
mod dashboard_key_tests {
    use super::*;

    #[test]
    fn ctrl_c_force_exits_the_runtime_dashboard() {
        assert!(matches!(
            dashboard_action_for_key(event::KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )),
            Some(DashboardAction::ForceExit)
        ));
    }

    #[test]
    fn q_and_escape_return_to_the_menu() {
        assert!(matches!(
            dashboard_action_for_key(event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(DashboardAction::BackToMenu)
        ));
        assert!(matches!(
            dashboard_action_for_key(event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(DashboardAction::BackToMenu)
        ));
    }
}
