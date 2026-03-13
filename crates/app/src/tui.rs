/// Ratatui TUI for dab-rtl.
///
/// Layout:
/// ┌──────────────────────────────────────────────────────────┐
/// │ Services (scroll)        │ Now Playing                   │
/// │  > BBC Radio 4           │  BBC Radio 4                  │
/// │    BBC Radio 2           │  Ensemble: BBC National DAB   │
/// │    BBC Radio 3           │                               │
/// ├──────────────────────────────────────────────────────────┤
/// │ [↑↓] Navigate  [Enter] Play  [s] Stop  [q] Quit  Status │
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

use crate::pipeline::{PipelineCmd, PipelineHandle, PipelineUpdate};

// ─────────────────────────────────────────────────────────────────────────── //
//  App state                                                                   //
// ─────────────────────────────────────────────────────────────────────────── //

struct AppState {
    ensemble: Ensemble,
    list_state: ListState,
    playing_label: Option<String>,
    status: String,
}

impl AppState {
    fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        AppState {
            ensemble: Ensemble::default(),
            list_state,
            playing_label: None,
            status: "Waiting for signal…".into(),
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
pub fn run(handle: PipelineHandle) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, handle);

    // Always restore terminal.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    handle: PipelineHandle,
) -> io::Result<()> {
    let mut state = AppState::new();
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
                Span::styled("Now playing: ", Style::default().add_modifier(Modifier::BOLD)),
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
        .block(Block::default().borders(Borders::ALL).title(" Now Playing "))
        .wrap(Wrap { trim: true });

    f.render_widget(para, area);
}

fn render_status_bar(f: &mut Frame, state: &AppState, area: ratatui::layout::Rect) {
    let help = Span::styled(
        " [↑↓/jk] Navigate  [Enter] Play  [s] Stop  [q] Quit ",
        Style::default().fg(Color::DarkGray),
    );
    let status = Span::styled(
        format!(" {} ", state.status),
        Style::default().fg(Color::Green),
    );

    let line = Line::from(vec![help, Span::raw(" │"), status]);
    let para = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(para, area);
}
