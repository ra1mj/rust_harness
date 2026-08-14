//! A full-screen terminal UI for `rh` (closest in spirit to Grok Build's TUI).
//!
//! The TUI subscribes to the session event bus: every durable
//! [`SessionEvent`] the agent loop appends is broadcast on the context and
//! rendered live. Typing a task and pressing Enter drives one turn on a
//! shared session, so the transcript accumulates like a chat.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

use rh_agent::{Agent, AgentBuilder, AgentDefinition};
use rh_session::{ContentBlock, SessionEvent, SessionStore};

use crate::Assembled;

/// Messages flowing into the UI loop.
enum Message {
    /// A durable session fact was appended and broadcast.
    Session(SessionEvent),
    /// A background agent turn finished.
    Done(anyhow::Result<()>),
}

/// The UI state: transcript lines + the input buffer.
struct App {
    lines: Vec<Line<'static>>,
    input: String,
    running: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            lines: vec![Line::from(Span::styled(
                "type a task and press Enter (q to quit)",
                Style::default().dark_gray(),
            ))],
            input: String::new(),
            running: false,
        }
    }
}

impl App {
    fn push_event(&mut self, event: SessionEvent) {
        for line in render_event(&event) {
            self.lines.push(line);
        }
    }

    fn render(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(frame.area());

        let status = if self.running { "working…" } else { "idle" };
        let title = format!(" rh — Rust Harness Agent · {status} · Enter: send · q: quit ");

        let transcript_area = chunks[0];
        let visible_height = transcript_area.height.saturating_sub(2) as usize;
        let start = self.lines.len().saturating_sub(visible_height);
        let visible: Vec<Line> = self.lines[start..].to_vec();
        frame.render_widget(
            Paragraph::new(Text::from(visible)).block(Block::bordered().title(title)),
            transcript_area,
        );

        let input_area = chunks[1];
        let prompt = format!("> {}", self.input);
        frame.render_widget(
            Paragraph::new(prompt.clone()).block(Block::bordered().title("input")),
            input_area,
        );
        frame.set_cursor_position(Position::new(
            (2 + self.input.len() as u16).min(input_area.width.saturating_sub(1)),
            input_area.y + 1,
        ));
    }
}

/// Render one session event into transcript lines.
fn render_event(event: &SessionEvent) -> Vec<Line<'static>> {
    match event {
        SessionEvent::UserMessage { content, .. } => {
            let text = content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            vec![Line::from(vec![
                Span::styled("you ", Style::default().green().bold()),
                Span::raw(text),
            ])]
        }
        // Render only text here; the assistant's tool-call *intent* is
        // shown by the separate `ToolCall` event that follows it.
        SessionEvent::AssistantMessage { content, .. } => content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(Line::from(vec![
                    Span::styled("rh  ", Style::default().cyan().bold()),
                    Span::raw(text.clone()),
                ])),
                _ => None,
            })
            .collect(),
        SessionEvent::AssistantChunk { text, .. } => {
            vec![Line::from(vec![
                Span::styled("rh  ", Style::default().cyan().bold()),
                Span::raw(text.clone()),
            ])]
        }
        SessionEvent::ToolCall {
            tool_name,
            arguments,
            ..
        } => vec![Line::from(vec![
            Span::styled("tool", Style::default().yellow().bold()),
            Span::raw(format!(" {tool_name} {arguments}")),
        ])],
        SessionEvent::ToolResult {
            output, is_error, ..
        } => {
            let (glyph, color) = if *is_error {
                ("✗", Color::Red)
            } else {
                ("✓", Color::Green)
            };
            vec![Line::from(vec![
                Span::styled(format!("  {glyph} "), Style::default().fg(color)),
                Span::raw(truncate(&output.to_string(), 500)),
            ])]
        }
        SessionEvent::TurnStart { .. } => vec![Line::from(
            Span::styled("── turn ──", Style::default().dark_gray()),
        )],
        SessionEvent::StepStart { .. }
        | SessionEvent::StepEnd { .. }
        | SessionEvent::TurnEnd { .. } => vec![],
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}

/// Run the TUI against an assembled runtime.
pub async fn run(assembled: Assembled) -> anyhow::Result<()> {
    // Keep `assembled` alive for the whole loop: its disposers hold the
    // service registrations in place.
    let ctx = assembled.ctx.clone();

    let store = ctx
        .service::<SessionStore>()
        .ok_or_else(|| anyhow::anyhow!("no session store registered"))?;
    let session = store.create_fresh();
    let definition = AgentDefinition {
        name: "rh-tui".to_string(),
        model: "mock".to_string(),
        system_prompt: "You are a Rust harness agent. Use tools when useful.".to_string(),
        tool_ids: Vec::new(),
        max_steps: 8,
    };
    let agent = Arc::new(AgentBuilder::new(ctx.clone(), definition).build(session)?);

    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Subscribe to the session event bus: everything the loop appends is
    // rendered live. Hold the disposer for the process lifetime.
    let tx_events = tx.clone();
    let _subscription = ctx.on::<SessionEvent>(Arc::new(move |event: &SessionEvent| {
        let _ = tx_events.send(Message::Session(event.clone()));
    }));

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::default();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));

    let result = loop {
        terminal.draw(|frame| app.render(frame))?;

        tokio::select! {
            message = rx.recv() => {
                match message {
                    Some(Message::Session(event)) => app.push_event(event),
                    Some(Message::Done(result)) => {
                        app.running = false;
                        if let Err(err) = result {
                            app.lines.push(Line::from(vec![
                                Span::styled("error ", Style::default().red().bold()),
                                Span::raw(err.to_string()),
                            ]));
                        }
                    }
                    None => break Ok(()),
                }
            }
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
                            KeyCode::Esc | KeyCode::Char('q') => break Ok(()),
                            KeyCode::Enter => submit(&mut app, &agent, &tx),
                            KeyCode::Backspace => { app.input.pop(); }
                            KeyCode::Char(c) => app.input.push(c),
                            _ => {}
                        }
                    }
                    Some(Ok(_)) | Some(Err(_)) => {}
                    None => break Ok(()),
                }
            }
            _ = tick.tick() => {}
        }
    };

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

/// Submit the input buffer as a new turn on the shared session.
fn submit(app: &mut App, agent: &Arc<Agent>, tx: &mpsc::UnboundedSender<Message>) {
    let input = app.input.trim().to_string();
    if input.is_empty() || app.running {
        return;
    }
    app.input.clear();
    app.running = true;

    let agent = Arc::clone(agent);
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = agent.run(&input).await.map(|_| ());
        let _ = tx.send(Message::Done(result));
    });
}
