/// Ratatui TUI for dab-rtl.
///
/// Layout (Normal mode):
/// ┌──────────────────────────────────────────────────────────┐
/// │ Services (scroll)        │ Now Playing                   │
/// │  > BBC Radio 4           │  BBC Radio 4                  │
/// │    BBC Radio 2           │  Ensemble: BBC National DAB   │
/// │    BBC Radio 3           │  Text: "Song Title - Artist"  │
/// ├──────────────────────────────────────────────────────────┤
/// │ [↑↓] Navigate  [Enter] Play  [s] Stop  [c] Country  [q] │
/// └──────────────────────────────────────────────────────────┘
///
/// Layout (Scanning mode): a bottom log box is added between content and status bar.
/// ┌──────────────────────────────────────────────────────────┐
/// │ Services (scanning)      │ Now Playing                   │
/// ├──────────────────────────────────────────────────────────┤
/// │ Scan Log                                                 │
/// │   Tuning to 1/10: 5A…                                   │
/// │   Channel 5A: 0 stations → Tuning to 2/10: 5B…          │
/// ├──────────────────────────────────────────────────────────┤
/// │ Scanning… │ Scanning 2/10: 5B                           │
/// └──────────────────────────────────────────────────────────┘
///
/// Layout (CountrySelect mode): a popup overlaid on top.
use std::fs::OpenOptions;
use std::io;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

use nix::unistd;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use protocol::{Ensemble, NowPlaying, Service};

use crate::pipeline::{PipelineCmd, PipelineHandle, PipelineUpdate};

// ─────────────────────────────────────────────────────────────────────────── //
//  Types                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

/// A DAB service discovered while scanning (may come from any channel).
#[derive(Clone)]
pub struct DiscoveredService {
    pub label: String,
    pub sid: u32,
    pub freq_hz: u32,
    pub is_dab_plus: bool,
    pub dls_text: Option<String>,
    pub now_playing: Option<NowPlaying>,
}

/// Per-channel scan progress tracked by the TUI.
struct ScanState {
    /// `(channel_name, freq_hz)` for each channel to scan.
    channels: Vec<(String, u32)>,
    /// Index into `channels` for the channel currently being scanned.
    current_idx: usize,
    /// Ticks spent on the current channel (200 ms each).
    ticks: u32,
    /// Services collected across all channels so far.
    services: Vec<DiscoveredService>,
    /// SIds already collected across all channels (to avoid duplicates).
    seen_sids: std::collections::HashSet<u32>,
    /// Number of services found before tuning to the current channel (for per-channel reporting).
    channel_start_count: usize,
}

impl ScanState {
    fn new(channels: Vec<(String, u32)>) -> Self {
        ScanState {
            channels,
            current_idx: 0,
            ticks: 0,
            services: Vec::new(),
            seen_sids: std::collections::HashSet::new(),
            channel_start_count: 0,
        }
    }

    fn channel_name(&self) -> &str {
        self.channels
            .get(self.current_idx)
            .map(|(n, _)| n.as_str())
            .unwrap_or("")
    }

    fn total(&self) -> usize {
        self.channels.len()
    }
}

/// Which top-level view is active.
#[derive(Copy, Clone, PartialEq, Eq)]
enum UiMode {
    /// Normal station-list and now-playing view.
    Normal,
    /// Country selection popup.
    CountrySelect,
}

// ─────────────────────────────────────────────────────────────────────────── //
//  App state                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

/// How many 200 ms ticks to spend on each channel during a scan.
/// 25 ticks = 5 seconds.
const SCAN_TICKS_PER_CHANNEL: u32 = 25;

/// Maximum number of log lines kept in the scan log ring buffer.
const MAX_SCAN_LOG: usize = 200;

struct AppState {
    /// Most recent ensemble received from the pipeline.
    ensemble: Ensemble,
    /// Selection cursor for the service/discovered-service list.
    list_state: ListState,
    /// SId of the currently playing service (if any).
    playing_sid: Option<u32>,
    /// Label of the currently playing service (if any).
    playing_label: Option<String>,
    /// Status bar text.
    status: String,
    /// Active UI mode.
    mode: UiMode,

    /// Country list shown in the country-select popup.
    country_entries: &'static [(&'static str, &'static str, &'static [&'static str])],
    /// Selection cursor for the country popup.
    country_list_state: ListState,

    /// Active scan (Some while scanning is in progress).
    scan_state: Option<ScanState>,
    /// Accumulated services discovered across a completed (or in-progress) scan.
    discovered: Vec<DiscoveredService>,
    /// Log messages shown in the bottom scan-log panel during scanning.
    scan_log: std::collections::VecDeque<String>,
    /// Cached service labels used by the station list.
    service_items: Vec<String>,
    /// Cached now-playing lines.
    now_playing_lines: Vec<Line<'static>>,
    /// Cached scan-log title.
    scan_log_title: String,
}

impl AppState {
    fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let mut country_list_state = ListState::default();
        country_list_state.select(Some(0));

        AppState {
            ensemble: Ensemble::default(),
            list_state,
            playing_sid: None,
            playing_label: None,
            status: "Waiting for signal…".into(),
            mode: UiMode::Normal,
            country_entries: crate::countries::country_list(),
            country_list_state,
            scan_state: None,
            discovered: Vec::new(),
            scan_log: std::collections::VecDeque::new(),
            service_items: Vec::new(),
            now_playing_lines: Self::build_now_playing_lines(None, "", None, None),
            scan_log_title: " Scan Log ".into(),
        }
    }

    fn rebuild_service_items(&mut self) {
        self.service_items.clear();
        if !self.discovered.is_empty() {
            self.service_items.extend(self.discovered.iter().map(|s| {
                let tag = if s.is_dab_plus { " [DAB+]" } else { "" };
                format!("{}{tag}", s.label)
            }));
        } else {
            self.service_items.extend(self.ensemble.services.iter().map(|s| {
                let label = if s.label.is_empty() {
                    format!("{:08X}", s.id)
                } else {
                    s.label.clone()
                };
                let tag = if s.is_dab_plus { " [DAB+]" } else { "" };
                format!("{label}{tag}")
            }));
        }
    }

    fn rebuild_now_playing(&mut self) {
        let now_playing = self.playing_sid.and_then(|sid| {
            self.discovered
                .iter()
                .find(|s| s.sid == sid)
                .and_then(|s| s.now_playing.clone())
                .or_else(|| {
                    self.ensemble
                        .services
                        .iter()
                        .find(|s| s.id == sid)
                        .and_then(|s| s.now_playing.clone())
                })
        });
        let playing_service = self
            .playing_sid
            .and_then(|sid| self.ensemble.services.iter().find(|s| s.id == sid));
        self.now_playing_lines = Self::build_now_playing_lines(
            self.playing_label.as_deref(),
            &self.ensemble.label,
            playing_service,
            now_playing.as_ref(),
        );
    }

    fn rebuild_scan_log_title(&mut self) {
        let found = self.scan_state.as_ref().map_or(0, |s| s.services.len());
        self.scan_log_title = format!(" Scan Log — {found} stations found so far ");
    }

    fn build_now_playing_lines(
        playing_label: Option<&str>,
        ensemble_label: &str,
        service: Option<&Service>,
        now_playing: Option<&NowPlaying>,
    ) -> Vec<Line<'static>> {
        if let Some(label) = playing_label {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(
                        "Now playing: ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(label.to_string()),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Ensemble: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(ensemble_label.to_string()),
                ]),
            ];
            if let Some(service) = service {
                let app_labels = service.advertised_app_labels();
                if !app_labels.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("Apps: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(app_labels.join(", ")),
                    ]));
                }
            }
            if let Some(meta) = now_playing {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("Text: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(meta.raw_text.clone(), Style::default().fg(Color::Yellow)),
                ]));
                if let Some(title) = &meta.title {
                    lines.push(Line::from(vec![
                        Span::styled("Title: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(title.clone()),
                    ]));
                }
                if let Some(artist) = &meta.artist {
                    lines.push(Line::from(vec![
                        Span::styled("Artist: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(artist.clone()),
                    ]));
                }
            }
            lines
        } else {
            vec![
                Line::from(Span::styled(
                    "No station selected",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press [Enter] to play the selected station",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press [c] to select country and scan channels",
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        }
    }

    /// Append a message to the scan log ring buffer, capped at `MAX_SCAN_LOG` lines.
    fn push_scan_log(&mut self, msg: String) {
        self.scan_log.push_back(msg);
        while self.scan_log.len() > MAX_SCAN_LOG {
            self.scan_log.pop_front();
        }
        self.rebuild_scan_log_title();
    }

    /// Return the SId and freq of the currently highlighted service.
    fn selected_service(&self) -> Option<(u32, u32)> {
        let idx = self.list_state.selected()?;
        if self.discovered.is_empty() {
            let svc = self.ensemble.services.get(idx)?;
            Some((svc.id, self.ensemble.freq_hz))
        } else {
            let svc = self.discovered.get(idx)?;
            Some((svc.sid, svc.freq_hz))
        }
    }

    /// Number of items in the current service list.
    fn service_count(&self) -> usize {
        if self.discovered.is_empty() {
            self.ensemble.services.len()
        } else {
            self.discovered.len()
        }
    }

    fn scroll_up(&mut self) {
        if self.service_count() == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(i.saturating_sub(1)));
    }

    fn scroll_down(&mut self) {
        let n = self.service_count();
        if n == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some((i + 1).min(n - 1)));
    }

    fn country_scroll_up(&mut self) {
        let i = self.country_list_state.selected().unwrap_or(0);
        self.country_list_state.select(Some(i.saturating_sub(1)));
    }

    fn country_scroll_down(&mut self) {
        let n = self.country_entries.len();
        if n == 0 {
            return;
        }
        let i = self.country_list_state.selected().unwrap_or(0);
        self.country_list_state.select(Some((i + 1).min(n - 1)));
    }

    /// Collect newly-discovered services from the current ensemble into the scan state.
    ///
    /// Uses scoped borrows to avoid overlapping mutable/immutable access.
    fn collect_from_ensemble(&mut self) {
        if self.scan_state.is_none() {
            return;
        }
        let freq = self.ensemble.freq_hz;

        // Build the list of candidates from the ensemble (shared borrow only).
        let candidates: Vec<(u32, DiscoveredService)> = self
            .ensemble
            .services
            .iter()
            .filter(|svc| !svc.label.is_empty())
            .map(|svc| {
                (
                    svc.id,
                    DiscoveredService {
                        label: svc.label.clone(),
                        sid: svc.id,
                        freq_hz: freq,
                        is_dab_plus: svc.is_dab_plus,
                        dls_text: svc.dls_text.clone(),
                        now_playing: svc.now_playing.clone(),
                    },
                )
            })
            .collect();

        // Now update the scan state (separate borrow).
        if let Some(ref mut scan) = self.scan_state {
            for (sid, entry) in candidates {
                if scan.seen_sids.insert(sid) {
                    scan.services.push(entry);
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Entry point                                                                 //
// ─────────────────────────────────────────────────────────────────────────── //

/// Run the TUI until the user presses `q` or `Esc` in Normal mode.
///
/// `initial_channels` is the ordered list of `(channel_name, freq_hz)` pairs
/// to scan automatically on startup (e.g. for a country-mode launch).
/// Pass an empty slice for single-channel mode.
pub fn run(handle: PipelineHandle, initial_channels: Vec<(String, u32)>) -> io::Result<()> {
    // The RTL-SDR C library writes "Found … tuner" and "Allocating … buffers"
    // directly to fd 2 (stderr).  Redirect stderr to /dev/null while the TUI is
    // active so those messages don't corrupt the alternate-screen rendering.
    // Only do this when stderr is a TTY — if the user has redirected stderr to a
    // file (e.g. `2> debug.log`) we must leave it alone so logs are preserved.
    let stderr_fd = io::stderr().as_raw_fd();
    let saved_stderr = if unistd::isatty(stderr_fd).unwrap_or(false) {
        let devnull = OpenOptions::new().write(true).open("/dev/null")?;
        let saved = unistd::dup(stderr_fd).map_err(io::Error::from)?;
        unistd::dup2(devnull.as_raw_fd(), stderr_fd).map_err(io::Error::from)?;
        Some(saved)
    } else {
        None
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, handle, initial_channels);

    // Always restore terminal.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Restore stderr if we redirected it.
    if let Some(saved) = saved_stderr {
        let _ = unistd::dup2(saved, stderr_fd);
        let _ = unistd::close(saved);
    }

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    handle: PipelineHandle,
    initial_channels: Vec<(String, u32)>,
) -> io::Result<()> {
    let mut state = AppState::new();

    // If launched in country-scanning mode, start scanning immediately.
    if !initial_channels.is_empty() {
        start_scan(&mut state, &handle, initial_channels);
    }
    state.rebuild_service_items();
    state.rebuild_now_playing();
    state.rebuild_scan_log_title();

    let tick = Duration::from_millis(200);
    let mut last_tick = Instant::now();
    let mut dirty = true;

    loop {
        // Drain pipeline updates.
        while let Ok(update) = handle.update_rx.try_recv() {
            match update {
                PipelineUpdate::Ensemble(mut ens) => {
                    let old_idx = state.list_state.selected().unwrap_or(0);
                    // Preserve DLS text across ensemble refreshes: FIC snapshots
                    // never carry dls_text (DLS arrives via packet-mode MSC), so
                    // carry it forward from the previous ensemble by SId.
                    for svc in &mut ens.services {
                        if let Some(old) = state.ensemble.services.iter().find(|s| s.id == svc.id) {
                            svc.dls_text = old.dls_text.clone();
                            svc.now_playing = old.now_playing.clone();
                        }
                    }
                    state.ensemble = ens;
                    state.rebuild_now_playing();

                    if state.scan_state.is_some() {
                        state.collect_from_ensemble();
                        state.rebuild_service_items();
                        state.rebuild_scan_log_title();
                    } else if state.discovered.is_empty() {
                        state.rebuild_service_items();
                        let n = state.ensemble.services.len();
                        if n == 0 {
                            state.list_state.select(None);
                        } else {
                            state.list_state.select(Some(old_idx.min(n - 1)));
                        }
                    }
                    dirty = true;
                }
                PipelineUpdate::Playing { sid, label } => {
                    state.playing_sid = Some(sid);
                    state.playing_label = Some(label.clone());
                    state.status = format!("Playing: {label}");
                    state.rebuild_now_playing();
                    dirty = true;
                }
                PipelineUpdate::Status(s) => {
                    if state.scan_state.is_none() {
                        state.status = s;
                        dirty = true;
                    } else {
                        log::debug!("pipeline status (suppressed during scan): {s}");
                    }
                }
                PipelineUpdate::NowPlaying { sid, metadata } => {
                    let text = metadata.raw_text.clone();
                    // Update metadata in the live ensemble snapshot.
                    if let Some(svc) = state.ensemble.services.iter_mut().find(|s| s.id == sid) {
                        svc.dls_text = Some(text.clone());
                        svc.now_playing = Some(metadata.clone());
                    }
                    // Also update any discovered service entry for cross-channel scans.
                    if let Some(entry) = state.discovered.iter_mut().find(|s| s.sid == sid) {
                        entry.dls_text = Some(text.clone());
                        entry.now_playing = Some(metadata.clone());
                    }
                    state.rebuild_now_playing();
                    dirty = true;
                }
            }
        }

        // Tick-driven scan advancement.
        if last_tick.elapsed() >= tick {
            last_tick = Instant::now();
            advance_scan(&mut state, &handle);
            dirty = true;
        }

        if dirty {
            terminal.draw(|f| render(f, &mut state))?;
            dirty = false;
        }

        // Input handling.
        let timeout = tick.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // Ctrl-C always quits immediately regardless of mode or scan state.
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(());
                }
                // Capture mode before handling the key (it may change inside).
                let was_normal = matches!(state.mode, UiMode::Normal);
                handle_key(key.code, &mut state, &handle);
                state.rebuild_now_playing();
                state.rebuild_service_items();
                dirty = true;

                // Only quit if we were already in Normal mode with no active scan.
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    && was_normal
                    && state.scan_state.is_none()
                {
                    return Ok(());
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Scan orchestration                                                          //
// ─────────────────────────────────────────────────────────────────────────── //

fn start_scan(state: &mut AppState, handle: &PipelineHandle, channels: Vec<(String, u32)>) {
    if channels.is_empty() {
        return;
    }
    let first_freq = channels[0].1;
    let first_name = channels[0].0.clone();
    let total = channels.len();

    state.discovered.clear();
    state.list_state.select(None);
    state.status = format!("Scanning 1/{total}: {first_name}…");
    state.scan_log.clear();
    state.push_scan_log(format!("Starting scan: {total} channels"));
    state.push_scan_log(format!("Tuning to channel 1/{total}: {first_name}…"));
    state.scan_state = Some(ScanState::new(channels));
    state.rebuild_service_items();
    state.rebuild_scan_log_title();

    let _ = handle.cmd_tx.try_send(PipelineCmd::Stop);
    let _ = handle.cmd_tx.try_send(PipelineCmd::Retune(first_freq));
}

/// Called once per 200 ms tick to advance the channel-by-channel scan.
fn advance_scan(state: &mut AppState, handle: &PipelineHandle) {
    // Tick and decide what to do — keep this borrow scoped.
    let action = {
        let Some(ref mut scan) = state.scan_state else {
            return;
        };
        scan.ticks += 1;
        if scan.ticks < SCAN_TICKS_PER_CHANNEL {
            return;
        }
        scan.ticks = 0;

        let prev_name = scan.channel_name().to_string();
        let found_on_channel = scan.services.len() - scan.channel_start_count;

        scan.current_idx += 1;
        scan.seen_sids.clear();

        let idx = scan.current_idx;
        let total = scan.total();
        let next = scan.channels.get(idx).cloned();

        scan.channel_start_count = scan.services.len();

        (idx, total, prev_name, found_on_channel, next)
    }; // mutable borrow of state.scan_state ends here

    let (idx, total, prev_name, found_on_channel, next_channel) = action;

    // Log the result for the channel we just finished.
    let station_word = if found_on_channel == 1 {
        "station"
    } else {
        "stations"
    };
    state.push_scan_log(format!(
        "  {prev_name}: {found_on_channel} {station_word} found"
    ));

    if let Some((next_name, next_freq)) = next_channel {
        state.status = format!("Scanning {}/{total}: {next_name}…", idx + 1);
        state.push_scan_log(format!(
            "Tuning to channel {}/{total}: {next_name}…",
            idx + 1
        ));
        let _ = handle.cmd_tx.try_send(PipelineCmd::Retune(next_freq));
    } else {
        // Scan complete — safe to take because the borrow above has ended.
        let services = state.scan_state.take().unwrap().services;
        let count = services.len();
        state.discovered = services;
        state.rebuild_service_items();
        let msg = if count == 0 {
            "Scan complete — no stations found".to_string()
        } else {
            format!("Scan complete — {count} stations found")
        };
        state.status = msg.clone();
        state.push_scan_log(msg);
        state
            .list_state
            .select(if count > 0 { Some(0) } else { None });
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Input handling                                                              //
// ─────────────────────────────────────────────────────────────────────────── //

fn handle_key(code: KeyCode, state: &mut AppState, handle: &PipelineHandle) {
    match state.mode {
        UiMode::CountrySelect => match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                state.mode = UiMode::Normal;
            }
            KeyCode::Up | KeyCode::Char('k') => state.country_scroll_up(),
            KeyCode::Down | KeyCode::Char('j') => state.country_scroll_down(),
            KeyCode::Enter => {
                if let Some(idx) = state.country_list_state.selected() {
                    if let Some(&(_, _, channels)) = state.country_entries.get(idx) {
                        let ch_list: Vec<(String, u32)> = channels
                            .iter()
                            .filter_map(|&ch| {
                                crate::channel_to_freq(ch).map(|f| (ch.to_string(), f))
                            })
                            .collect();
                        state.mode = UiMode::Normal;
                        start_scan(state, handle, ch_list);
                    }
                }
            }
            _ => {}
        },
        UiMode::Normal => match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                // Quit logic handled by the caller in run_loop.
            }
            KeyCode::Up | KeyCode::Char('k') => state.scroll_up(),
            KeyCode::Down | KeyCode::Char('j') => state.scroll_down(),
            KeyCode::Char('c') => {
                state.mode = UiMode::CountrySelect;
            }
            KeyCode::Enter => {
                if let Some((sid, freq_hz)) = state.selected_service() {
                    if freq_hz != 0 && freq_hz != state.ensemble.freq_hz {
                        let _ = handle.cmd_tx.try_send(PipelineCmd::Retune(freq_hz));
                    }
                    let _ = handle.cmd_tx.try_send(PipelineCmd::Play(sid));
                }
            }
            KeyCode::Char('s') => {
                let _ = handle.cmd_tx.try_send(PipelineCmd::Stop);
                state.playing_sid = None;
                state.playing_label = None;
                state.status = "Stopped".into();
            }
            _ => {}
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Rendering                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

fn render(f: &mut Frame, state: &mut AppState) {
    let area = f.size();
    let scanning = state.scan_state.is_some();

    // During scanning, insert a bottom log panel between content and status bar.
    let (content_area, log_area, status_area) = if scanning {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(6),
                Constraint::Length(3),
            ])
            .split(area);
        (outer[0], Some(outer[1]), outer[2])
    } else {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);
        (outer[0], None, outer[1])
    };

    // Main: horizontal split — service list / now playing.
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(content_area);

    render_service_list(f, state, main[0]);
    render_now_playing(f, state, main[1]);
    if let Some(log) = log_area {
        render_scan_log(f, state, log);
    }
    render_status_bar(f, state, status_area);

    // Overlays drawn last so they appear on top.
    if let UiMode::CountrySelect = state.mode {
        render_country_popup(f, state, area);
    }
}

fn render_service_list(f: &mut Frame, state: &mut AppState, area: Rect) {
    let title = if state.scan_state.is_some() {
        " Scanning… ".to_string()
    } else if !state.discovered.is_empty() {
        format!(" {} stations found ", state.discovered.len())
    } else if !state.ensemble.label.is_empty() {
        format!(" {} ", state.ensemble.label)
    } else {
        " Services ".to_string()
    };

    let items: Vec<ListItem> = state
        .service_items
        .iter()
        .map(|label| ListItem::new(label.as_str()))
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut state.list_state);
}

fn render_now_playing(f: &mut Frame, state: &AppState, area: Rect) {
    let para = Paragraph::new(state.now_playing_lines.clone())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Now Playing "),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, state: &AppState, area: Rect) {
    let help_text = match state.mode {
        UiMode::CountrySelect => " [↑↓/jk] Navigate  [Enter] Select  [Esc/q] Cancel ",
        UiMode::Normal if state.scan_state.is_some() => " Scanning… ",
        UiMode::Normal => " [↑↓/jk] Navigate  [Enter] Play  [s] Stop  [c] Country  [q] Quit ",
    };
    let help = Span::styled(help_text, Style::default().fg(Color::DarkGray));
    let status = Span::styled(
        format!(" {} ", state.status),
        Style::default().fg(Color::Green),
    );

    let line = Line::from(vec![help, Span::raw(" │"), status]);
    let para = Paragraph::new(line).block(Block::default().borders(Borders::ALL));

    f.render_widget(para, area);
}

/// Centered popup for country selection.
fn render_country_popup(f: &mut Frame, state: &mut AppState, area: Rect) {
    let popup_width = 52u16.min(area.width.saturating_sub(4));
    let popup_height = (state.country_entries.len() as u16 + 4).min(area.height.saturating_sub(4));

    let popup_area = Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = state
        .country_entries
        .iter()
        .map(|&(code, name, channels)| {
            ListItem::new(format!("{code}  {name:<20} ({} ch)", channels.len()))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Select Country  [Enter] to scan  [Esc] to cancel ")
                .title_alignment(Alignment::Center),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, popup_area, &mut state.country_list_state);
}

/// Bottom panel shown during scanning, displaying a scrolling log of channel scan progress.
fn render_scan_log(f: &mut Frame, state: &AppState, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let log_len = state.scan_log.len();
    let skip = log_len.saturating_sub(inner_height);

    let items: Vec<ListItem> = state
        .scan_log
        .iter()
        .skip(skip)
        .map(|msg| ListItem::new(msg.as_str()))
        .collect();

    // Show how many stations have been found so far in the title.
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(state.scan_log_title.as_str())
            .title_alignment(Alignment::Left),
    );

    f.render_widget(list, area);
}
