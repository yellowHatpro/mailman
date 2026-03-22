use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Margin};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::gmail::models::MessageSummary;

pub enum InboxEvent {
    Started { total: usize },
    MessageLoaded(MessageSummary),
    Finished,
    Failed(String),
}

pub struct InboxTui {
    messages: Vec<MessageSummary>,
    selected: usize,
    loading: bool,
    expected_total: usize,
    loaded_total: usize,
    error: Option<String>,
    receiver: UnboundedReceiver<InboxEvent>,
}

impl InboxTui {
    pub fn new(receiver: UnboundedReceiver<InboxEvent>) -> Self {
        Self {
            messages: Vec::new(),
            selected: 0,
            loading: true,
            expected_total: 0,
            loaded_total: 0,
            error: None,
            receiver,
        }
    }

    pub fn run(mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), Show, LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        result
    }

    fn run_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            self.drain_events();
            terminal.draw(|frame| self.render(frame))?;

            if !event::poll(Duration::from_millis(120))? {
                continue;
            }

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => self.next(),
                    KeyCode::Up | KeyCode::Char('k') => self.previous(),
                    KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
                    KeyCode::End | KeyCode::Char('G') => {
                        self.selected = self.messages.len().saturating_sub(1);
                    }
                    _ => {}
                }
            }
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                InboxEvent::Started { total } => {
                    self.loading = true;
                    self.expected_total = total;
                    self.loaded_total = 0;
                    self.error = None;
                }
                InboxEvent::MessageLoaded(message) => {
                    self.messages.push(message);
                    self.loaded_total += 1;
                }
                InboxEvent::Finished => {
                    self.loading = false;
                }
                InboxEvent::Failed(message) => {
                    self.loading = false;
                    self.error = Some(message);
                }
            }
        }
    }

    fn next(&mut self) {
        if self.messages.is_empty() {
            return;
        }

        self.selected = (self.selected + 1).min(self.messages.len() - 1);
    }

    fn previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(area);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);

        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                "mailman",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Inbox viewer"),
        ]))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, chunks[0]);

        let items: Vec<ListItem> = if self.messages.is_empty() {
            let text = if let Some(error) = &self.error {
                format!("Failed to load inbox: {error}")
            } else if self.loading {
                "Loading inbox...".to_string()
            } else {
                "No inbox messages found.".to_string()
            };
            vec![ListItem::new(Line::from(text))]
        } else {
            self.messages
                .iter()
                .map(|message| {
                    ListItem::new(vec![
                        Line::from(Span::styled(
                            truncate(&message.subject, 52),
                            Style::default().add_modifier(Modifier::BOLD),
                        )),
                        Line::from(format!(
                            "{} | {}",
                            truncate(&message.from, 36),
                            truncate(&message.received_at, 28)
                        )),
                    ])
                })
                .collect()
        };

        let mut list_state = ListState::default();
        if !self.messages.is_empty() {
            list_state.select(Some(self.selected.min(self.messages.len() - 1)));
        }

        let list_title = if self.expected_total > 0 {
            format!("Messages ({}/{})", self.loaded_total, self.expected_total)
        } else if self.loading {
            "Messages (loading)".to_string()
        } else {
            format!("Messages ({})", self.messages.len())
        };

        let list = List::new(items)
            .block(Block::default().title(list_title).borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">");
        frame.render_stateful_widget(list, body[0], &mut list_state);

        let detail = if let Some(message) = self.messages.get(self.selected) {
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("From: ", Style::default().fg(Color::Yellow)),
                    Span::raw(&message.from),
                ]),
                Line::from(vec![
                    Span::styled("Date: ", Style::default().fg(Color::Yellow)),
                    Span::raw(&message.received_at),
                ]),
                Line::from(vec![
                    Span::styled("Id:   ", Style::default().fg(Color::Yellow)),
                    Span::raw(&message.id),
                ]),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "Subject",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]),
                Line::from(message.subject.as_str()),
                Line::from(""),
                Line::from("Full message viewing is not wired yet."),
            ])
        } else if let Some(error) = &self.error {
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Inbox load failed",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(error.as_str()),
            ])
        } else if self.loading {
            Paragraph::new(vec![
                Line::from("Loading inbox metadata..."),
                Line::from(""),
                Line::from(format!(
                    "Loaded {} of {} messages",
                    self.loaded_total, self.expected_total
                )),
            ])
        } else {
            Paragraph::new("Authenticate first and load inbox messages to use the TUI.")
        }
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
        frame.render_widget(detail, body[1]);

        let footer_text = if self.loading {
            "Loading messages...  q quit  j/k or arrows move"
        } else {
            "q quit  j/k or arrows move  g top  G bottom"
        };
        let footer = Paragraph::new(footer_text)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        frame.render_widget(
            footer,
            chunks[2].inner(Margin {
                vertical: 0,
                horizontal: 0,
            }),
        );
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}
