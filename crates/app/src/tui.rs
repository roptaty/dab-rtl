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
use std::path::PathBuf;
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

use protocol::{ContentItem, Ensemble, NowPlaying, Service};

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
    pub content_items: Vec<ContentItem>,
    pub mot_content_types: Vec<String>,
    pub codec: Option<String>,
    pub signal_quality_percent: Option<u8>,
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
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum UiMode {
    /// Station-list and browse-focused view.
    Browse,
    /// Playback-focused view for the selected service.
    Playback,
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
    /// Selected downloadable content index for the active service.
    content_selection: usize,
    /// Last codec reported for the active service.
    codec: Option<String>,
    /// Last signal quality reported for the active service.
    signal_quality_percent: Option<u8>,
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
            mode: UiMode::Browse,
            country_entries: crate::countries::country_list(),
            country_list_state,
            scan_state: None,
            discovered: Vec::new(),
            scan_log: std::collections::VecDeque::new(),
            service_items: Vec::new(),
            now_playing_lines: Self::build_now_playing_lines(None, "", None, None, 0, None, None),
            content_selection: 0,
            codec: None,
            signal_quality_percent: None,
            scan_log_title: " Scan Log ".into(),
        }
    }

    fn rebuild_service_items(&mut self) {
        self.service_items.clear();
        if !self.discovered.is_empty() {
            self.service_items.extend(self.discovered.iter().map(|s| {
                let tag = if s.is_dab_plus { "" } else { " [DAB Legacy]" };
                format!("{}{tag}", s.label)
            }));
        } else {
            self.service_items
                .extend(self.ensemble.services.iter().map(|s| {
                    let label = if s.label.is_empty() {
                        format!("{:08X}", s.id)
                    } else {
                        s.label.clone()
                    };
                    let tag = if s.is_dab_plus { "" } else { " [DAB Legacy]" };
                    format!("{label}{tag}")
                }));
        }
    }

    fn sort_discovered(&mut self) {
        self.discovered
            .sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
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
            self.content_selection,
            self.codec.as_deref(),
            self.signal_quality_percent,
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
        content_selection: usize,
        codec: Option<&str>,
        signal_quality_percent: Option<u8>,
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
            if let Some(codec) = codec {
                lines.push(Line::from(vec![
                    Span::styled("Codec: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(codec.to_string()),
                ]));
            }
            if let Some(signal_quality_percent) = signal_quality_percent {
                lines.push(Line::from(vec![
                    Span::styled("Reception: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!("{signal_quality_percent}%")),
                ]));
            }
            if let Some(service) = service {
                let app_labels = service.advertised_app_labels();
                if !app_labels.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("Apps: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(app_labels.join(", ")),
                    ]));
                }
                if service.advertised_app_labels().contains(&"SlideShow") {
                    lines.push(Line::from(vec![
                        Span::styled("Slideshow: ", Style::default().fg(Color::DarkGray)),
                        Span::raw("Signalled"),
                    ]));
                }
                if service.advertised_app_labels().contains(&"SlideShow")
                    || !service.mot_content_types.is_empty()
                {
                    lines.push(Line::from(vec![
                        Span::styled("MOT Types: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(if service.mot_content_types.is_empty() {
                            "none received".to_string()
                        } else {
                            service.mot_content_types.join(", ")
                        }),
                    ]));
                }
                if !service.content_items.is_empty() {
                    let idx = content_selection.min(service.content_items.len() - 1);
                    let selected = &service.content_items[idx];
                    let types = service
                        .content_items
                        .iter()
                        .map(|item| item.content_type.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(Line::from(vec![
                        Span::styled("Content: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(types),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Selected: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(format!(
                            "{}/{}  {}  ({})",
                            idx + 1,
                            service.content_items.len(),
                            selected.content_type,
                            selected.filename
                        )),
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

    fn selected_content_item(&self) -> Option<&ContentItem> {
        let sid = self.playing_sid?;
        let service = self.ensemble.services.iter().find(|s| s.id == sid)?;
        if service.content_items.is_empty() {
            return None;
        }
        let idx = self.content_selection.min(service.content_items.len() - 1);
        service.content_items.get(idx)
    }

    fn cycle_content_next(&mut self) {
        let Some(count) = self
            .playing_sid
            .and_then(|sid| self.ensemble.services.iter().find(|s| s.id == sid))
            .map(|svc| svc.content_items.len())
        else {
            return;
        };
        if count > 0 {
            self.content_selection = (self.content_selection + 1) % count;
        }
    }

    fn cycle_content_prev(&mut self) {
        let Some(count) = self
            .playing_sid
            .and_then(|sid| self.ensemble.services.iter().find(|s| s.id == sid))
            .map(|svc| svc.content_items.len())
        else {
            return;
        };
        if count > 0 {
            self.content_selection = (self.content_selection + count - 1) % count;
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
                        content_items: svc.content_items.clone(),
                        mot_content_types: svc.mot_content_types.clone(),
                        codec: None,
                        signal_quality_percent: None,
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
                            svc.content_items = old.content_items.clone();
                            svc.mot_content_types = old.mot_content_types.clone();
                        }
                    }
                    ens.services
                        .sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
                    state.ensemble = ens;
                    state.rebuild_now_playing();

                    if state.scan_state.is_some() {
                        state.collect_from_ensemble();
                        state.sort_discovered();
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
                    state.content_selection = 0;
                    state.codec = state
                        .ensemble
                        .services
                        .iter()
                        .find(|s| s.id == sid)
                        .map(|s| {
                            if s.is_dab_plus {
                                "HE-AAC v2"
                            } else {
                                "MPEG Layer II"
                            }
                            .to_string()
                        });
                    state.signal_quality_percent = None;
                    if state.scan_state.is_none() {
                        state.mode = UiMode::Playback;
                    }
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
                        if is_new_now_playing_item(svc.now_playing.as_ref(), &metadata) {
                            svc.content_items.clear();
                            if state.playing_sid == Some(sid) {
                                state.content_selection = 0;
                            }
                        }
                        svc.dls_text = Some(text.clone());
                        svc.now_playing = Some(metadata.clone());
                    }
                    // Also update any discovered service entry for cross-channel scans.
                    if let Some(entry) = state.discovered.iter_mut().find(|s| s.sid == sid) {
                        if is_new_now_playing_item(entry.now_playing.as_ref(), &metadata) {
                            entry.content_items.clear();
                        }
                        entry.dls_text = Some(text.clone());
                        entry.now_playing = Some(metadata.clone());
                    }
                    state.rebuild_now_playing();
                    dirty = true;
                }
                PipelineUpdate::PlaybackMeta {
                    sid,
                    codec,
                    signal_quality_percent,
                } => {
                    if state.playing_sid == Some(sid) {
                        state.codec = Some(codec.clone());
                        state.signal_quality_percent = Some(signal_quality_percent);
                    }
                    if let Some(entry) = state.discovered.iter_mut().find(|s| s.sid == sid) {
                        entry.codec = Some(codec);
                        entry.signal_quality_percent = Some(signal_quality_percent);
                    }
                    state.rebuild_now_playing();
                    dirty = true;
                }
                PipelineUpdate::Content { sid, content } => {
                    if let Some(svc) = state.ensemble.services.iter_mut().find(|s| s.id == sid) {
                        record_mot_content_type(&mut svc.mot_content_types, &content.content_type);
                        upsert_content_item(&mut svc.content_items, content.clone());
                    }
                    if let Some(entry) = state.discovered.iter_mut().find(|s| s.sid == sid) {
                        record_mot_content_type(
                            &mut entry.mot_content_types,
                            &content.content_type,
                        );
                        upsert_content_item(&mut entry.content_items, content);
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
                let was_browse = matches!(state.mode, UiMode::Browse);
                handle_key(key.code, &mut state, &handle);
                state.rebuild_now_playing();
                state.rebuild_service_items();
                dirty = true;

                // Only quit if we were already in Browse mode with no active scan.
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    && was_browse
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
        state.sort_discovered();
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
                state.mode = UiMode::Browse;
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
                        state.mode = UiMode::Browse;
                        start_scan(state, handle, ch_list);
                    }
                }
            }
            _ => {}
        },
        UiMode::Browse => match code {
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
                    if state.scan_state.is_none() {
                        state.mode = UiMode::Playback;
                    }
                }
            }
            KeyCode::Char('s') => {
                let _ = handle.cmd_tx.try_send(PipelineCmd::Stop);
                state.playing_sid = None;
                state.playing_label = None;
                state.status = "Stopped".into();
                state.mode = UiMode::Browse;
            }
            KeyCode::Left | KeyCode::Char('h') => state.cycle_content_prev(),
            KeyCode::Right | KeyCode::Char('l') => state.cycle_content_next(),
            KeyCode::Char('d') => match save_selected_content(state) {
                Ok(Some(path)) => {
                    state.status = format!("Saved {}", path.display());
                }
                Ok(None) => {
                    state.status = "No downloadable content selected".into();
                }
                Err(err) => {
                    state.status = format!("Save failed: {err}");
                }
            },
            _ => {}
        },
        UiMode::Playback => match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                // Quit logic handled by the caller in run_loop.
            }
            KeyCode::Char('b') => {
                state.mode = UiMode::Browse;
            }
            KeyCode::Char('c') => {
                state.mode = UiMode::CountrySelect;
            }
            KeyCode::Char('s') => {
                let _ = handle.cmd_tx.try_send(PipelineCmd::Stop);
                state.playing_sid = None;
                state.playing_label = None;
                state.status = "Stopped".into();
                state.mode = UiMode::Browse;
            }
            KeyCode::Left | KeyCode::Char('h') => state.cycle_content_prev(),
            KeyCode::Right | KeyCode::Char('l') => state.cycle_content_next(),
            KeyCode::Char('d') => match save_selected_content(state) {
                Ok(Some(path)) => {
                    state.status = format!("Saved {}", path.display());
                }
                Ok(None) => {
                    state.status = "No downloadable content selected".into();
                }
                Err(err) => {
                    state.status = format!("Save failed: {err}");
                }
            },
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

    if matches!(state.mode, UiMode::Playback) && state.scan_state.is_none() {
        render_now_playing(f, state, content_area);
    } else {
        // Main: horizontal split — service list / now playing.
        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(content_area);

        render_service_list(f, state, main[0]);
        render_now_playing(f, state, main[1]);
    }
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
    } else if matches!(state.mode, UiMode::Playback) {
        " Playback ".to_string()
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
    let title = if matches!(state.mode, UiMode::Playback) && state.scan_state.is_none() {
        " Selected Service "
    } else {
        " Now Playing "
    };
    let para = Paragraph::new(state.now_playing_lines.clone())
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: true });

    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, state: &AppState, area: Rect) {
    let help_text = match state.mode {
        UiMode::CountrySelect => " [↑↓/jk] Navigate  [Enter] Select  [Esc/q] Cancel ",
        UiMode::Browse if state.scan_state.is_some() => " Scanning… ",
        UiMode::Browse => " [↑↓/jk] Navigate  [Enter] Play  [←→/hl] Content  [d] Download  [s] Stop  [c] Country  [q] Quit ",
        UiMode::Playback => " [b] Browse  [←→/hl] Content  [d] Download  [s] Stop  [c] Country  [q] Quit ",
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

fn upsert_content_item(items: &mut Vec<ContentItem>, content: ContentItem) {
    if let Some(existing) = items
        .iter_mut()
        .find(|item| item.filename == content.filename && item.content_type == content.content_type)
    {
        *existing = content;
    } else {
        items.push(content);
    }
}

fn record_mot_content_type(types: &mut Vec<String>, content_type: &str) {
    if !types.iter().any(|existing| existing == content_type) {
        types.push(content_type.to_string());
        types.sort();
    }
}

fn is_new_now_playing_item(previous: Option<&NowPlaying>, next: &NowPlaying) -> bool {
    let Some(previous) = previous else {
        return false;
    };

    if previous.toggle.is_some() && next.toggle.is_some() && previous.toggle != next.toggle {
        return true;
    }
    if previous.title != next.title && (previous.title.is_some() || next.title.is_some()) {
        return true;
    }
    if previous.artist != next.artist && (previous.artist.is_some() || next.artist.is_some()) {
        return true;
    }
    previous.raw_text != next.raw_text
}

fn save_selected_content(state: &AppState) -> io::Result<Option<PathBuf>> {
    let Some(content) = state.selected_content_item() else {
        return Ok(None);
    };
    let filename = sanitize_download_name(&content.filename, &content.content_type);
    let path = PathBuf::from(filename);
    std::fs::write(&path, &content.bytes)?;
    Ok(Some(path))
}

fn sanitize_download_name(name: &str, content_type: &str) -> String {
    let mut clean = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if clean.is_empty() {
        let ext = match content_type {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            other => other.rsplit('/').next().unwrap_or("bin"),
        };
        clean = format!("content.{ext}");
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use protocol::{
        ensemble::{Component, ProtectionLevel, ServiceType, UserApplication},
        Service,
    };

    fn dummy_handle() -> PipelineHandle {
        let (_update_tx, update_rx) = mpsc::sync_channel(1);
        let (cmd_tx, _cmd_rx) = mpsc::sync_channel(4);
        PipelineHandle { update_rx, cmd_tx }
    }

    #[test]
    fn build_now_playing_lines_shows_slideshow_capability() {
        let mut service = Service {
            label: "Radio".into(),
            ..Default::default()
        };
        service.components.push(Component {
            subchannel_id: 1,
            scids: Some(0),
            service_type: ServiceType::Audio,
            start_address: 0,
            size: 0,
            protection: ProtectionLevel::EepA(2),
            packet_address: None,
            user_applications: vec![UserApplication {
                uatype: UserApplication::UATYPE_SLIDESHOW,
                data: vec![],
                xpad_app_type: Some(12),
                dscty: Some(0x3C),
                uses_msc_data_groups: Some(true),
                ca_applies: Some(false),
            }],
        });

        let lines = AppState::build_now_playing_lines(
            Some("Radio"),
            "Ensemble",
            Some(&service),
            None,
            0,
            Some("HE-AAC v2"),
            Some(87),
        );
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Codec: HE-AAC v2"));
        assert!(rendered.contains("Reception: 87%"));
        assert!(rendered.contains("Slideshow: Signalled"));
        assert!(rendered.contains("MOT Types: none received"));
    }

    #[test]
    fn enter_switches_to_playback_mode() {
        let handle = dummy_handle();
        let mut state = AppState::new();
        state.ensemble.services.push(Service {
            id: 0x1234,
            label: "Radio".into(),
            ..Default::default()
        });
        state.rebuild_service_items();

        handle_key(KeyCode::Enter, &mut state, &handle);

        assert_eq!(state.mode, UiMode::Playback);
    }

    #[test]
    fn playback_mode_can_return_to_browse() {
        let handle = dummy_handle();
        let mut state = AppState::new();
        state.mode = UiMode::Playback;

        handle_key(KeyCode::Char('b'), &mut state, &handle);

        assert_eq!(state.mode, UiMode::Browse);
    }

    #[test]
    fn build_now_playing_lines_show_selected_content() {
        let service = Service {
            label: "Radio".into(),
            mot_content_types: vec!["image/jpeg".into(), "image/png".into()],
            content_items: vec![
                ContentItem {
                    content_type: "image/jpeg".into(),
                    filename: "cover.jpg".into(),
                    bytes: vec![1, 2, 3],
                    updated_at_unix_ms: 0,
                },
                ContentItem {
                    content_type: "image/png".into(),
                    filename: "slide.png".into(),
                    bytes: vec![4, 5, 6],
                    updated_at_unix_ms: 0,
                },
            ],
            ..Default::default()
        };

        let lines = AppState::build_now_playing_lines(
            Some("Radio"),
            "Ensemble",
            Some(&service),
            None,
            1,
            None,
            None,
        );
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Content: image/jpeg, image/png"));
        assert!(rendered.contains("MOT Types: image/jpeg, image/png"));
        assert!(rendered.contains("Selected: 2/2  image/png  (slide.png)"));
    }

    #[test]
    fn sanitize_download_name_keeps_transmitted_filename() {
        assert_eq!(
            sanitize_download_name("cover.art", "image/jpeg"),
            "cover.art"
        );
        assert_eq!(sanitize_download_name("", "image/png"), "content.png");
    }

    #[test]
    fn new_now_playing_item_clears_stale_mot() {
        let old = NowPlaying {
            raw_text: "Artist A - Song A".into(),
            title: Some("Song A".into()),
            artist: Some("Artist A".into()),
            toggle: Some(false),
            ..Default::default()
        };
        let new = NowPlaying {
            raw_text: "Artist B - Song B".into(),
            title: Some("Song B".into()),
            artist: Some("Artist B".into()),
            toggle: Some(true),
            ..Default::default()
        };

        assert!(is_new_now_playing_item(Some(&old), &new));
        assert!(!is_new_now_playing_item(None, &new));
    }

    #[test]
    fn record_mot_content_type_keeps_unique_sorted_list() {
        let mut types = vec!["image/png".to_string()];
        record_mot_content_type(&mut types, "image/jpeg");
        record_mot_content_type(&mut types, "image/png");
        assert_eq!(
            types,
            vec!["image/jpeg".to_string(), "image/png".to_string()]
        );
    }
}
