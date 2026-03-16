/// Ratatui TUI for dab-rtl.
///
/// Layout:
/// ┌──────────────────────────────────────────────────────────┐
/// │ Services (scroll)        │ Now Playing                   │
/// │  > BBC Radio 4           │  BBC Radio 4                  │
/// │    BBC Radio 2           │  Ensemble: BBC National DAB   │
/// │    BBC Radio 3           │                               │
/// ├──────────────────────────────────────────────────────────┤
/// │ [↑↓] Navigate  [Enter] Play  [s] Stop  [X] Clear cache  │
/// └──────────────────────────────────────────────────────────┘
use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use protocol::Ensemble;

use crate::cache;
use crate::pipeline::{PipelineCmd, PipelineHandle, PipelineUpdate};

// ─────────────────────────────────────────────────────────────────────────── //
//  App state                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

struct AppState {
    ensemble: Ensemble,
    list_state: ListState,
    playing_label: Option<String>,
    status: String,
    /// True while the service list is populated from cache, not live FIC.
    is_cached: bool,
}

impl AppState {
    fn new(channel: Option<String>) -> Self {
        let mut list_state = ListState::default();
        let mut ensemble = Ensemble::default();
        let mut is_cached = false;

        // Pre-populate from cache if we have a channel name.
        if let Some(ref ch) = channel {
            if let Some(cached) = cache::get_ensemble(ch) {
                ensemble.id = cached.id;
                ensemble.label = cached.label.clone();
                ensemble.services = cached
                    .services
                    .iter()
                    .map(|s| {
                        let mut svc = protocol::Service::default();
                        svc.id = s.id;
                        svc.label = s.label.clone();
                        svc.is_dab_plus = s.is_dab_plus;
                        svc
                    })
                    .collect();
                is_cached = true;
            }
        }

        if !ensemble.services.is_empty() {
            list_state.select(Some(0));
        }

        AppState {
            ensemble,
            list_state,
            playing_label: None,
            status: if is_cached {
                "Loaded from cache — waiting for live signal…".into()
            } else {
                "Waiting for signal…".into()
            },
            is_cached,
        }
    }

    fn selected_sid(&self) -> Option<u32> {
        let idx = self.list_state.selected()?;
        self.ensemble.services.get(idx).map(|s| s.id)
    }

    fn scroll_up(&mut self) {
        if self.ensemble.services.is_empty() {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(i.saturating_sub(1)));
    }

    fn scroll_down(&mut self) {
        if self.ensemble.services.is_empty() {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        let max = self.ensemble.services.len() - 1;
        self.list_state.select(Some((i + 1).min(max)));
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Entry point                                                                 //
// ─────────────────────────────────────────────────────────────────────────── //

/// Run the TUI until the user presses `q` or `Esc`.
///
/// `channel` is the DAB channel name (e.g. `"11C"`) used for cache look-ups.
/// Pass `None` for file-based sources.
pub fn run(handle: PipelineHandle, channel: Option<String>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, handle, channel);

    // Always restore terminal.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    handle: PipelineHandle,
    channel: Option<String>,
) -> io::Result<()> {
    let mut state = AppState::new(channel);
    let tick = Duration::from_millis(200);
    let mut last_tick = Instant::now();

    loop {
        // Drain pipeline updates.
        while let Ok(update) = handle.update_rx.try_recv() {
            match update {
                PipelineUpdate::Ensemble(ens) => {
                    // Preserve selection if possible.
                    let old_sid = state.selected_sid();
                    state.ensemble = ens;
                    // Live data has arrived — no longer showing cached results.
                    state.is_cached = false;
                    if state.ensemble.services.is_empty() {
                        state.list_state.select(None);
                    } else {
                        let new_idx = old_sid
                            .and_then(|sid| {
                                state.ensemble.services.iter().position(|s| s.id == sid)
                            })
                            .unwrap_or(0);
                        state.list_state.select(Some(new_idx));
                    }
                }
                PipelineUpdate::Playing { label } => {
                    state.playing_label = Some(label.clone());
                    state.status = format!("Playing: {label}");
                }
                PipelineUpdate::Status(s) => {
                    state.status = s;
                }
            }
        }

        terminal.draw(|f| render(f, &mut state))?;

        // Input handling.
        let timeout = tick.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Up | KeyCode::Char('k') => state.scroll_up(),
                    KeyCode::Down | KeyCode::Char('j') => state.scroll_down(),
                    KeyCode::Enter => {
                        if let Some(sid) = state.selected_sid() {
                            let _ = handle.cmd_tx.try_send(PipelineCmd::Play(sid));
                        }
                    }
                    KeyCode::Char('s') => {
                        let _ = handle.cmd_tx.try_send(PipelineCmd::Stop);
                        state.playing_label = None;
                        state.status = "Stopped".into();
                    }
                    KeyCode::Char('X') => {
                        cache::clear();
                        state.is_cached = false;
                        state.status = format!(
                            "Cache cleared: {}",
                            cache::cache_path().display()
                        );
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick {
            last_tick = Instant::now();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── //
//  Rendering                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

fn render(f: &mut Frame, state: &mut AppState) {
    let area = f.size();

    // Outer: vertical split — main content / status bar.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    // Main: horizontal split — service list / now playing.
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(outer[0]);

    render_service_list(f, state, main[0]);
    render_now_playing(f, state, main[1]);
    render_status_bar(f, state, outer[1]);
}

fn render_service_list(f: &mut Frame, state: &mut AppState, area: ratatui::layout::Rect) {
    let ens_title = if state.ensemble.label.is_empty() {
        " Services ".to_string()
    } else if state.is_cached {
        format!(" {} (cached) ", state.ensemble.label)
    } else {
        format!(" {} ", state.ensemble.label)
    };

    let items: Vec<ListItem> = state
        .ensemble
        .services
        .iter()
        .map(|s| {
            let label = if s.label.is_empty() {
                format!("{:08X}", s.id)
            } else {
                s.label.clone()
            };
            ListItem::new(label)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(ens_title))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut state.list_state);
}

fn render_now_playing(f: &mut Frame, state: &AppState, area: ratatui::layout::Rect) {
    let content = if let Some(ref label) = state.playing_label {
        vec![
            Line::from(vec![
                Span::styled(
                    "Now playing: ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(label.clone()),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Ensemble: ", Style::default().fg(Color::DarkGray)),
                Span::raw(state.ensemble.label.clone()),
            ]),
        ]
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
        ]
    };

    let para = Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Now Playing "),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, state: &AppState, area: ratatui::layout::Rect) {
    let help = Span::styled(
        " [↑↓/jk] Navigate  [Enter] Play  [s] Stop  [X] Clear cache  [q] Quit ",
        Style::default().fg(Color::DarkGray),
    );
    let status = Span::styled(
        format!(" {} ", state.status),
        Style::default().fg(Color::Green),
    );

    let line = Line::from(vec![help, Span::raw(" │"), status]);
    let para = Paragraph::new(line).block(Block::default().borders(Borders::ALL));

    f.render_widget(para, area);
}
