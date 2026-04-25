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
    /// Ticks since the last new piece of FIC info (new SId, new/changed
    /// service label, or first ensemble label) on the current channel.
    /// Resets to 0 every time `note_new_info()` is called.
    quiet_ticks: u32,
    /// `true` once any FIC info has been observed on the current channel —
    /// distinguishes "still hunting" from "settled after lock".
    saw_info: bool,
    /// Services collected across all channels so far.
    services: Vec<DiscoveredService>,
    /// SIds already collected across all channels (to avoid duplicates).
    seen_sids: std::collections::HashSet<u32>,
    /// Latest known label per SId on the current channel — used to detect
    /// when a label first arrives or changes (resets the quiet timer).
    /// Cleared when moving to the next channel.
    known_labels: std::collections::HashMap<u32, String>,
    /// Whether the ensemble label has been observed on the current channel.
    /// Reset when moving to the next channel.
    ensemble_label_seen: bool,
    /// Number of services found before tuning to the current channel (for per-channel reporting).
    channel_start_count: usize,
}

impl ScanState {
    fn new(channels: Vec<(String, u32)>) -> Self {
        ScanState {
            channels,
            current_idx: 0,
            ticks: 0,
            quiet_ticks: 0,
            saw_info: false,
            services: Vec::new(),
            seen_sids: std::collections::HashSet::new(),
            known_labels: std::collections::HashMap::new(),
            ensemble_label_seen: false,
            channel_start_count: 0,
        }
    }

    fn channel_name(&self) -> &str {
        self.channels
            .get(self.current_idx)
            .map(|(n, _)| n.as_str())
            .unwrap_or("")
    }

    fn current_freq(&self) -> Option<u32> {
        self.channels.get(self.current_idx).map(|(_, f)| *f)
    }

    fn total(&self) -> usize {
        self.channels.len()
    }

    /// Record that something new was decoded on the current channel.
    /// Resets the per-channel quiet timer and marks the channel as locked.
    fn note_new_info(&mut self) {
        self.saw_info = true;
        self.quiet_ticks = 0;
    }

    /// Reset per-channel state when advancing to the next channel.
    fn reset_for_next_channel(&mut self) {
        self.ticks = 0;
        self.quiet_ticks = 0;
        self.saw_info = false;
        self.seen_sids.clear();
        self.known_labels.clear();
        self.ensemble_label_seen = false;
        self.channel_start_count = self.services.len();
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

/// Maximum ticks to wait for *any* FIC info on a channel before declaring it
/// empty.  30 ticks × 200 ms = 6 s.  Generous enough to absorb retune /
/// stream-reopen latency and OFDM lock-on, while still skipping truly empty
/// channels in well under double-digit seconds.
const SCAN_NO_LOCK_TICKS: u32 = 30;

/// Once info has been observed, advance to the next channel after this many
/// ticks without any new info.  25 ticks × 200 ms = 5 s.  This window is
/// reset every time a new SId, a new/changed service label, or the ensemble
/// label arrives — so on lively channels we keep dwelling as long as the FIC
/// keeps producing fresh data.
const SCAN_QUIET_TICKS: u32 = 25;

/// Hard ceiling per channel.  75 ticks × 200 ms = 15 s.  Caps total scan
/// time on noisy channels where labels keep flickering in and out.
const SCAN_MAX_TICKS: u32 = 75;

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
    /// `true` when the user pressed `i`, requesting the run loop to suspend
    /// the alt screen and render the selected slideshow image inline.
    pending_image_view: bool,
    /// Cached ASCII cover art for the selected JPEG and terminal size.
    ascii_art_cache: AsciiArtCache,
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
            pending_image_view: false,
            ascii_art_cache: AsciiArtCache::default(),
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
        self.discovered.sort_by_key(|a| a.label.to_lowercase());
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
                    let mut selected_detail = format!(
                        "{}/{}  {}  ({})",
                        idx + 1,
                        service.content_items.len(),
                        selected.content_type,
                        selected.filename
                    );
                    if let Some(dim) = crate::image_dim::sniff(&selected.bytes) {
                        selected_detail.push_str(&format!("  {}×{}", dim.width, dim.height));
                    }
                    lines.push(Line::from(vec![
                        Span::styled("Selected: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(selected_detail),
                    ]));
                    if let Some(category) = selected.category_title.as_deref() {
                        if !category.is_empty() {
                            lines.push(Line::from(vec![
                                Span::styled("Category: ", Style::default().fg(Color::DarkGray)),
                                Span::raw(category.to_string()),
                            ]));
                        }
                    }
                }
            }
            if let Some(meta) = now_playing {
                lines.push(Line::from(""));
                let status = match meta.item_running {
                    Some(true) => Some(("Playing", Color::Green)),
                    Some(false) => Some(("Idle", Color::DarkGray)),
                    None => None,
                };
                if let Some((label, colour)) = status {
                    lines.push(Line::from(vec![
                        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(label.to_string(), Style::default().fg(colour)),
                    ]));
                }
                lines.push(Line::from(vec![
                    Span::styled("Text: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(meta.raw_text.clone(), Style::default().fg(Color::Yellow)),
                ]));
                // When the broadcaster signals the item has stopped, suppress
                // structured DL+ song fields (TS 102 980 §7.3.2 IR=0). The raw
                // DLS line stays so the user still sees any station message
                // ("Up next…", advertisement copy, etc.) being broadcast.
                let suppress_song_fields = meta.item_running == Some(false);
                if !suppress_song_fields {
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
                    if let Some(album) = &meta.album {
                        lines.push(Line::from(vec![
                            Span::styled("Album: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(album.clone()),
                        ]));
                    }
                    if let Some(track) = &meta.track {
                        lines.push(Line::from(vec![
                            Span::styled("Track: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(track.clone()),
                        ]));
                    }
                    if let Some(composer) = &meta.composer {
                        lines.push(Line::from(vec![
                            Span::styled("Composer: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(composer.clone()),
                        ]));
                    }
                    if let Some(band) = &meta.band {
                        lines.push(Line::from(vec![
                            Span::styled("Band: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(band.clone()),
                        ]));
                    }
                    if let Some(genre) = &meta.genre {
                        lines.push(Line::from(vec![
                            Span::styled("Genre: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(genre.clone()),
                        ]));
                    }
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

    fn selected_or_latest_jpeg_content_item(&self) -> Option<SelectedJpegContent<'_>> {
        let sid = self.playing_sid?;
        let service = self.ensemble.services.iter().find(|s| s.id == sid)?;
        if service.content_items.is_empty() {
            return None;
        }
        let jpeg_indices = jpeg_content_indices(service);
        if jpeg_indices.is_empty() {
            return None;
        }

        let idx = self.content_selection.min(service.content_items.len() - 1);
        if let Some(pos) = jpeg_indices.iter().position(|&jpeg_idx| jpeg_idx == idx) {
            let item = &service.content_items[idx];
            return Some(SelectedJpegContent {
                item,
                position: pos,
                count: jpeg_indices.len(),
            });
        }

        let fallback_pos = jpeg_indices.len() - 1;
        let fallback_idx = jpeg_indices[fallback_pos];
        Some(SelectedJpegContent {
            item: &service.content_items[fallback_idx],
            position: fallback_pos,
            count: jpeg_indices.len(),
        })
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

    fn cycle_cover_next(&mut self) {
        if !self.cycle_jpeg_content(true) {
            self.cycle_content_next();
        }
    }

    fn cycle_cover_prev(&mut self) {
        if !self.cycle_jpeg_content(false) {
            self.cycle_content_prev();
        }
    }

    fn cycle_jpeg_content(&mut self, forward: bool) -> bool {
        let Some(service) = self
            .playing_sid
            .and_then(|sid| self.ensemble.services.iter().find(|s| s.id == sid))
        else {
            return false;
        };
        let jpeg_indices = jpeg_content_indices(service);
        if jpeg_indices.is_empty() {
            return false;
        }

        let current_pos = jpeg_indices
            .iter()
            .position(|&idx| idx == self.content_selection)
            .unwrap_or(jpeg_indices.len() - 1);
        let next_pos = if forward {
            (current_pos + 1) % jpeg_indices.len()
        } else {
            (current_pos + jpeg_indices.len() - 1) % jpeg_indices.len()
        };
        self.content_selection = jpeg_indices[next_pos];
        true
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
    /// Drops ensembles whose `freq_hz` does not match the channel currently
    /// being scanned (stale snapshots from a previous channel that arrived
    /// after we already advanced).  Resets the scan's quiet timer whenever
    /// a new SId, a new/changed service label, or the ensemble label is
    /// observed — that keeps the scan dwelling on a channel as long as the
    /// FIC keeps producing fresh information.
    ///
    /// Uses scoped borrows to avoid overlapping mutable/immutable access.
    fn collect_from_ensemble(&mut self) {
        let Some(scan) = self.scan_state.as_ref() else {
            return;
        };
        let Some(current_freq) = scan.current_freq() else {
            return;
        };
        let freq = self.ensemble.freq_hz;
        // Drop stale ensemble snapshots from a previous channel.
        if freq != current_freq {
            return;
        }

        // Snapshot ensemble label and per-service (sid, label, ...) tuples
        // up-front so the rest of the function can hold a mutable borrow on
        // `self.scan_state`.
        let ensemble_label_present = !self.ensemble.label.is_empty();
        let services_snapshot: Vec<(u32, String, bool, DiscoveredService)> = self
            .ensemble
            .services
            .iter()
            .map(|svc| {
                (
                    svc.id,
                    svc.label.clone(),
                    !svc.label.is_empty(),
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

        let Some(scan) = self.scan_state.as_mut() else {
            return;
        };

        let mut got_info = false;

        if ensemble_label_present && !scan.ensemble_label_seen {
            scan.ensemble_label_seen = true;
            got_info = true;
        }

        for (sid, label, has_label, entry) in services_snapshot {
            // New SId observed → reset quiet timer.  Track unconditionally so
            // bare FIG 0/2 entries (no FIG 1/1 yet) still count as info.
            let sid_is_new = scan.seen_sids.insert(sid);
            if sid_is_new {
                got_info = true;
            }
            // New or updated label → reset quiet timer too, and add the
            // service to the discovered list (only labelled services are
            // user-meaningful).
            if has_label {
                let label_changed = match scan.known_labels.get(&sid) {
                    Some(prev) => prev != &label,
                    None => true,
                };
                if label_changed {
                    scan.known_labels.insert(sid, label);
                    got_info = true;
                    // Insert into the discovered list if this SId hasn't
                    // appeared with a label before; otherwise update the
                    // existing entry (broadcasters can revise labels mid-scan).
                    if let Some(existing) = scan.services.iter_mut().find(|s| s.sid == sid) {
                        existing.label = entry.label.clone();
                        existing.is_dab_plus = entry.is_dab_plus;
                    } else {
                        scan.services.push(entry);
                    }
                }
            }
        }

        if got_info {
            scan.note_new_info();
        }
    }
}

#[derive(Default)]
struct AsciiArtCache {
    key: Option<AsciiArtKey>,
    lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AsciiArtKey {
    sid: u32,
    filename: String,
    updated_at_unix_ms: u64,
    bytes_len: usize,
    width: u16,
    height: u16,
}

struct SelectedJpegContent<'a> {
    item: &'a ContentItem,
    position: usize,
    count: usize,
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
                    // While scanning, drop snapshots whose freq_hz doesn't
                    // match the channel we're currently dwelling on — the
                    // pipeline's update channel can hold a few queued
                    // updates from before a retune, and treating those as
                    // current-channel data would corrupt the discovered
                    // service list.
                    if let Some(scan) = state.scan_state.as_ref() {
                        if scan.current_freq().is_some_and(|f| f != ens.freq_hz) {
                            log::debug!(
                                "scan: dropping stale Ensemble update freq={} \
                                 (current channel freq={:?})",
                                ens.freq_hz,
                                scan.current_freq()
                            );
                            continue;
                        }
                    }

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
                    ens.services.sort_by_key(|a| a.label.to_lowercase());
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

                // Honour any pending image-view request before continuing.
                // The handler temporarily leaves the alt screen, so we have
                // to clear ratatui's cached buffer when we come back so the
                // next draw repaints from scratch.
                if std::mem::take(&mut state.pending_image_view) {
                    if let Some(content) = state.selected_content_item() {
                        let bytes = content.bytes.clone();
                        let term_size = terminal.size().unwrap_or(Rect {
                            x: 0,
                            y: 0,
                            width: 80,
                            height: 24,
                        });
                        let width_cells = term_size.width.saturating_sub(2).max(20) as u32;
                        match crate::image_view::view(&bytes, width_cells) {
                            Ok(()) => state.status = "Image preview closed".into(),
                            Err(err) => {
                                state.status = format!("Image preview failed: {err}");
                            }
                        }
                    }
                    terminal.clear()?;
                }
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
///
/// Per-channel timing is adaptive (rather than a fixed 5 s budget):
/// * If no FIC info has arrived at all, we wait up to `SCAN_NO_LOCK_TICKS`
///   before declaring the channel empty and skipping.
/// * Once info has arrived, we keep dwelling until `SCAN_QUIET_TICKS` have
///   elapsed without any further new info — every new SId or service label
///   resets that timer in `collect_from_ensemble`.
/// * `SCAN_MAX_TICKS` is a hard ceiling for noisy channels where labels
///   keep flickering.
fn advance_scan(state: &mut AppState, handle: &PipelineHandle) {
    // Tick and decide what to do — keep this borrow scoped.
    let action = {
        let Some(ref mut scan) = state.scan_state else {
            return;
        };
        scan.ticks += 1;
        scan.quiet_ticks += 1;

        let should_advance = if scan.saw_info {
            scan.quiet_ticks >= SCAN_QUIET_TICKS || scan.ticks >= SCAN_MAX_TICKS
        } else {
            scan.ticks >= SCAN_NO_LOCK_TICKS
        };
        if !should_advance {
            return;
        }

        let prev_name = scan.channel_name().to_string();
        let found_on_channel = scan.services.len() - scan.channel_start_count;

        scan.current_idx += 1;
        scan.reset_for_next_channel();

        let idx = scan.current_idx;
        let total = scan.total();
        let next = scan.channels.get(idx).cloned();

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
            KeyCode::Left | KeyCode::Char('h') => state.cycle_cover_prev(),
            KeyCode::Right | KeyCode::Char('l') => state.cycle_cover_next(),
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
            KeyCode::Char('i') => request_image_view(state),
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
            KeyCode::Left | KeyCode::Char('h') => state.cycle_cover_prev(),
            KeyCode::Right | KeyCode::Char('l') => state.cycle_cover_next(),
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
            KeyCode::Char('i') => request_image_view(state),
            _ => {}
        },
    }
}

/// Mark the currently-selected slideshow image for inline rendering. The
/// run loop performs the suspend/restore dance after `handle_key` returns
/// (we can't do it here because `crate::image_view::view` needs to take over
/// the terminal).
fn request_image_view(state: &mut AppState) {
    let Some(item) = state.selected_content_item() else {
        state.status = "No image content selected".into();
        return;
    };
    if !item.content_type.starts_with("image/") {
        state.status = format!("Selected content is {} (not an image)", item.content_type);
        return;
    }
    if !crate::image_view::SUPPORTED {
        state.status =
            "Image preview not available — rebuild with --features slideshow-image".into();
        return;
    }
    state.pending_image_view = true;
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

fn render_now_playing(f: &mut Frame, state: &mut AppState, area: Rect) {
    let title = if matches!(state.mode, UiMode::Playback) && state.scan_state.is_none() {
        " Selected Service "
    } else {
        " Now Playing "
    };

    let show_art = state.playing_sid.is_some()
        && state.selected_or_latest_jpeg_content_item().is_some()
        && area.width >= 36
        && area.height >= 10;

    if show_art {
        let horizontal = area.width >= 72;
        let panes = Layout::default()
            .direction(if horizontal {
                Direction::Horizontal
            } else {
                Direction::Vertical
            })
            .constraints(if horizontal {
                [Constraint::Percentage(48), Constraint::Percentage(52)]
            } else {
                [Constraint::Percentage(45), Constraint::Percentage(55)]
            })
            .split(area);

        render_now_playing_text(f, state, panes[0], title);
        render_ascii_cover_art(f, state, panes[1]);
        return;
    }

    render_now_playing_text(f, state, area, title);
}

fn render_now_playing_text(f: &mut Frame, state: &AppState, area: Rect, title: &str) {
    let para = Paragraph::new(state.now_playing_lines.clone())
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: true });

    f.render_widget(para, area);
}

fn render_ascii_cover_art(f: &mut Frame, state: &mut AppState, area: Rect) {
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2);
    let Some((key, bytes, position, count)) = state
        .selected_or_latest_jpeg_content_item()
        .and_then(|selected| {
            Some((
                AsciiArtKey {
                    sid: state.playing_sid?,
                    filename: selected.item.filename.clone(),
                    updated_at_unix_ms: selected.item.updated_at_unix_ms,
                    bytes_len: selected.item.bytes.len(),
                    width: inner_width,
                    height: inner_height,
                },
                selected.item.bytes.clone(),
                selected.position,
                selected.count,
            ))
        })
    else {
        return;
    };

    if state.ascii_art_cache.key.as_ref() != Some(&key) {
        state.ascii_art_cache.lines =
            crate::ascii_art::jpeg_to_ascii(&bytes, inner_width, inner_height).unwrap_or_default();
        state.ascii_art_cache.key = Some(key);
    }

    let lines = if state.ascii_art_cache.lines.is_empty() {
        vec![Line::from(Span::styled(
            "JPEG cover art could not be decoded",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let mut art_lines: Vec<Line> = state
            .ascii_art_cache
            .lines
            .iter()
            .map(|line| Line::from(Span::raw(line.clone())))
            .collect();
        
        // Add OCR text below the art if available
        if let Some(selected) = state.selected_or_latest_jpeg_content_item() {
            if let Some(ocr_text) = &selected.item.ocr_text {
                if !ocr_text.is_empty() {
                    art_lines.push(Line::from("")); // Empty line separator
                    for line in ocr_text.lines() {
                        art_lines.push(Line::from(Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::White),
                        )));
                    }
                }
            }
        }
        
        art_lines
    };

    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " Cover Art {}/{} ",
            position + 1,
            count
        )))
        .alignment(Alignment::Center);
    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, state: &AppState, area: Rect) {
    let view_key = if crate::image_view::SUPPORTED {
        "  [i] View image"
    } else {
        ""
    };
    let browse_help = format!(
        " [↑↓/jk] Navigate  [Enter] Play  [←→/hl] Cover  [d] Download{view_key}  [s] Stop  [c] Country  [q] Quit "
    );
    let playback_help = format!(
        " [b] Browse  [←→/hl] Cover  [d] Download{view_key}  [s] Stop  [c] Country  [q] Quit "
    );
    let help_text: &str = match state.mode {
        UiMode::CountrySelect => " [↑↓/jk] Navigate  [Enter] Select  [Esc/q] Cancel ",
        UiMode::Browse if state.scan_state.is_some() => " Scanning… ",
        UiMode::Browse => &browse_help,
        UiMode::Playback => &playback_help,
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

fn jpeg_content_indices(service: &Service) -> Vec<usize> {
    service
        .content_items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| is_jpeg_content(item).then_some(idx))
        .collect()
}

fn is_jpeg_content(item: &ContentItem) -> bool {
    matches!(item.content_type.as_str(), "image/jpeg" | "image/jpg")
        || (item.bytes.len() >= 2 && item.bytes[0] == 0xFF && item.bytes[1] == 0xD8)
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
                    category_title: None,
                    ocr_text: None,
                    updated_at_unix_ms: 0,
                },
                ContentItem {
                    content_type: "image/png".into(),
                    filename: "slide.png".into(),
                    bytes: vec![4, 5, 6],
                    category_title: Some("Now Playing".into()),
                    ocr_text: None,
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
        // The category title from the second item should surface.
        assert!(rendered.contains("Category: Now Playing"));
    }

    #[test]
    fn build_now_playing_lines_shows_image_dimensions_when_decodable() {
        // Minimal valid PNG signature + IHDR with 320x240.
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1A\n");
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&240u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);

        let service = Service {
            label: "Radio".into(),
            content_items: vec![ContentItem {
                content_type: "image/png".into(),
                filename: "cover.png".into(),
                bytes: png,
                category_title: None,
                ocr_text: None,
                updated_at_unix_ms: 0,
            }],
            ..Default::default()
        };
        let rendered = AppState::build_now_playing_lines(
            Some("Radio"),
            "Ensemble",
            Some(&service),
            None,
            0,
            None,
            None,
        )
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("320×240"));
    }

    #[test]
    fn cover_cycle_skips_non_jpeg_items() {
        let mut state = AppState::new();
        state.playing_sid = Some(0x1234);
        state.ensemble.services.push(Service {
            id: 0x1234,
            label: "Radio".into(),
            content_items: vec![
                ContentItem {
                    content_type: "image/png".into(),
                    filename: "slide.png".into(),
                    bytes: vec![0x89, b'P', b'N', b'G'],
                    category_title: None,
                    ocr_text: None,
                    updated_at_unix_ms: 0,
                },
                ContentItem {
                    content_type: "image/jpeg".into(),
                    filename: "cover-a.jpg".into(),
                    bytes: vec![0xFF, 0xD8],
                    category_title: None,
                    ocr_text: None,
                    updated_at_unix_ms: 1,
                },
                ContentItem {
                    content_type: "image/jpeg".into(),
                    filename: "cover-b.jpg".into(),
                    bytes: vec![0xFF, 0xD8],
                    category_title: None,
                    ocr_text: None,
                    updated_at_unix_ms: 2,
                },
            ],
            ..Default::default()
        });

        state.content_selection = 0;
        assert_eq!(
            state
                .selected_or_latest_jpeg_content_item()
                .map(|selected| selected.item.filename.as_str()),
            Some("cover-b.jpg")
        );

        state.cycle_cover_next();
        assert_eq!(state.content_selection, 1);
        assert_eq!(
            state
                .selected_content_item()
                .map(|item| item.filename.as_str()),
            Some("cover-a.jpg")
        );

        state.cycle_cover_next();
        assert_eq!(state.content_selection, 2);
        assert_eq!(
            state
                .selected_content_item()
                .map(|item| item.filename.as_str()),
            Some("cover-b.jpg")
        );

        state.cycle_cover_next();
        assert_eq!(state.content_selection, 1);
    }

    #[test]
    fn build_now_playing_lines_clears_song_fields_when_idle() {
        let now_playing = NowPlaying {
            raw_text: "Coming up: morning show".into(),
            title: Some("Should be hidden".into()),
            artist: Some("Should be hidden".into()),
            album: Some("Should be hidden".into()),
            item_running: Some(false),
            ..Default::default()
        };

        let rendered = AppState::build_now_playing_lines(
            Some("Radio"),
            "Ensemble",
            None,
            Some(&now_playing),
            0,
            None,
            None,
        )
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("Status: Idle"));
        assert!(rendered.contains("Coming up: morning show"));
        assert!(!rendered.contains("Title:"));
        assert!(!rendered.contains("Artist:"));
        assert!(!rendered.contains("Album:"));
    }

    #[test]
    fn build_now_playing_lines_keeps_song_fields_when_running() {
        let now_playing = NowPlaying {
            raw_text: "Artist - Title".into(),
            title: Some("Title".into()),
            artist: Some("Artist".into()),
            item_running: Some(true),
            ..Default::default()
        };
        let rendered = AppState::build_now_playing_lines(
            Some("Radio"),
            "Ensemble",
            None,
            Some(&now_playing),
            0,
            None,
            None,
        )
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("Status: Playing"));
        assert!(rendered.contains("Title: Title"));
        assert!(rendered.contains("Artist: Artist"));
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

    fn scanning_state(channels: Vec<(&str, u32)>) -> AppState {
        let mut state = AppState::new();
        let owned: Vec<(String, u32)> = channels
            .into_iter()
            .map(|(n, f)| (n.to_string(), f))
            .collect();
        state.scan_state = Some(ScanState::new(owned));
        state
    }

    /// Bare FIG 0/2 (SId without a label) should reset the quiet timer and
    /// flip `saw_info`, but should NOT push a service into the discovered
    /// list — only labelled services are user-meaningful.
    #[test]
    fn scan_collect_unlabelled_sid_resets_quiet_but_no_discovery() {
        let mut state = scanning_state(vec![("11C", 220_352_000)]);
        state.ensemble.freq_hz = 220_352_000;
        state.ensemble.services.push(Service {
            id: 0xABCD,
            label: String::new(),
            ..Default::default()
        });

        let scan = state.scan_state.as_mut().unwrap();
        scan.quiet_ticks = 17;

        state.collect_from_ensemble();
        let scan = state.scan_state.as_ref().unwrap();
        assert!(scan.saw_info, "SId discovery should mark info seen");
        assert_eq!(scan.quiet_ticks, 0, "new SId should reset quiet timer");
        assert_eq!(scan.services.len(), 0, "unlabelled SId is not discovered");
        assert!(scan.seen_sids.contains(&0xABCD));
    }

    /// A label arriving on a previously-unlabelled SId should reset the
    /// quiet timer and add the service to the discovered list.
    #[test]
    fn scan_collect_label_arrival_resets_and_adds() {
        let mut state = scanning_state(vec![("11C", 220_352_000)]);
        state.ensemble.freq_hz = 220_352_000;

        // First snapshot: SId without a label.
        state.ensemble.services.push(Service {
            id: 0xABCD,
            label: String::new(),
            ..Default::default()
        });
        state.collect_from_ensemble();
        state.scan_state.as_mut().unwrap().quiet_ticks = 22;

        // Second snapshot: same SId, now with a label.
        state.ensemble.services[0].label = "Radio Test".into();
        state.collect_from_ensemble();

        let scan = state.scan_state.as_ref().unwrap();
        assert_eq!(
            scan.quiet_ticks, 0,
            "label arrival should reset the quiet timer"
        );
        assert_eq!(scan.services.len(), 1);
        assert_eq!(scan.services[0].label, "Radio Test");
    }

    /// Unchanged labels on subsequent ensemble updates should NOT keep
    /// resetting the quiet timer — otherwise we'd never advance off a
    /// channel that simply repeats its FIBs.
    #[test]
    fn scan_collect_unchanged_label_does_not_reset_quiet() {
        let mut state = scanning_state(vec![("11C", 220_352_000)]);
        state.ensemble.freq_hz = 220_352_000;
        state.ensemble.services.push(Service {
            id: 0xABCD,
            label: "Radio Test".into(),
            ..Default::default()
        });
        state.collect_from_ensemble();

        let scan = state.scan_state.as_mut().unwrap();
        scan.quiet_ticks = 18;

        // Same ensemble snapshot again — nothing new.
        state.collect_from_ensemble();
        let scan = state.scan_state.as_ref().unwrap();
        assert_eq!(scan.quiet_ticks, 18);
    }

    /// Ensemble snapshots whose freq_hz doesn't match the current scan
    /// channel are stale leftovers from a previous channel's queue.  They
    /// must not contribute to the timer or the discovered list.
    #[test]
    fn scan_collect_drops_mismatched_freq() {
        let mut state = scanning_state(vec![("11C", 220_352_000), ("11D", 222_064_000)]);

        // First channel: ingest a labelled service to advance saw_info.
        state.ensemble.freq_hz = 220_352_000;
        state.ensemble.services.push(Service {
            id: 0xABCD,
            label: "On 11C".into(),
            ..Default::default()
        });
        state.collect_from_ensemble();

        // Advance to the second channel.
        let scan = state.scan_state.as_mut().unwrap();
        scan.current_idx = 1;
        scan.reset_for_next_channel();
        scan.quiet_ticks = 12;

        // A stale ensemble for 11C arrives after we've moved on — must be
        // dropped, leaving the new channel's state untouched.
        state.collect_from_ensemble();
        let scan = state.scan_state.as_ref().unwrap();
        assert!(!scan.saw_info, "stale snapshot must not flip saw_info");
        assert_eq!(scan.quiet_ticks, 12, "stale snapshot must not reset timer");
        assert!(
            scan.known_labels.is_empty(),
            "stale snapshot must not populate per-channel label cache"
        );
    }

    /// Ensemble label arrival (FIG 1/0) on its own counts as info — even
    /// without any services yet, we don't want to skip a channel just
    /// because the ensemble label is the first thing we decode.
    #[test]
    fn scan_collect_ensemble_label_alone_resets_quiet() {
        let mut state = scanning_state(vec![("11C", 220_352_000)]);
        state.ensemble.freq_hz = 220_352_000;
        state.ensemble.label = "Some Ensemble".into();

        state.scan_state.as_mut().unwrap().quiet_ticks = 14;
        state.collect_from_ensemble();

        let scan = state.scan_state.as_ref().unwrap();
        assert!(scan.saw_info);
        assert!(scan.ensemble_label_seen);
        assert_eq!(scan.quiet_ticks, 0);
    }
}
