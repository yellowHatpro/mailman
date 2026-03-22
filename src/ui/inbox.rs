use std::collections::HashMap;
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
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::gmail::models::{MessageDetail, MessageSummary};

pub enum InboxEvent {
    PageLoading {
        page_index: usize,
        replace: bool,
    },
    PageMessageLoaded {
        page_index: usize,
        message: MessageSummary,
    },
    PageLoaded {
        page_index: usize,
        has_next_page: bool,
    },
    DetailLoading {
        id: String,
    },
    DetailLoaded(MessageDetail),
    Error(String),
}

pub enum InboxCommand {
    LoadInitialPage,
    LoadMore,
    LoadDetail(String),
}

pub struct InboxTui {
    messages: Vec<MessageSummary>,
    details: HashMap<String, MessageDetail>,
    selected: usize,
    loading_page: bool,
    loading_detail_for: Option<String>,
    page_index: usize,
    has_next_page: bool,
    pages_loaded: usize,
    error: Option<String>,
    detail_scroll: u16,
    event_rx: UnboundedReceiver<InboxEvent>,
    command_tx: UnboundedSender<InboxCommand>,
}

impl InboxTui {
    pub fn new(
        event_rx: UnboundedReceiver<InboxEvent>,
        command_tx: UnboundedSender<InboxCommand>,
    ) -> Self {
        Self {
            messages: Vec::new(),
            details: HashMap::new(),
            selected: 0,
            loading_page: true,
            loading_detail_for: None,
            page_index: 0,
            has_next_page: false,
            pages_loaded: 0,
            error: None,
            detail_scroll: 0,
            event_rx,
            command_tx,
        }
    }

    pub fn run(mut self) -> Result<()> {
        let _ = self.command_tx.send(InboxCommand::LoadInitialPage);

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
            self.maybe_request_selected_detail();
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
                    KeyCode::Right | KeyCode::Char('n') | KeyCode::Enter => {
                        if self.is_load_more_selected() && !self.loading_page && self.has_next_page
                        {
                            let _ = self.command_tx.send(InboxCommand::LoadMore);
                        }
                    }
                    KeyCode::PageDown => {
                        self.detail_scroll = self.detail_scroll.saturating_add(12);
                    }
                    KeyCode::PageUp => {
                        self.detail_scroll = self.detail_scroll.saturating_sub(12);
                    }
                    _ => {}
                }
            }
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                InboxEvent::PageLoading {
                    page_index,
                    replace,
                } => {
                    self.loading_page = true;
                    self.page_index = page_index;
                    if replace {
                        self.messages.clear();
                        self.details.clear();
                        self.selected = 0;
                        self.detail_scroll = 0;
                        self.pages_loaded = 0;
                    }
                    self.loading_detail_for = None;
                    self.error = None;
                }
                InboxEvent::PageMessageLoaded {
                    page_index,
                    message,
                } => {
                    self.page_index = page_index;
                    self.messages.push(message);
                }
                InboxEvent::PageLoaded {
                    page_index,
                    has_next_page,
                } => {
                    self.loading_page = false;
                    self.page_index = page_index;
                    self.has_next_page = has_next_page;
                    self.pages_loaded = self.page_index + 1;
                }
                InboxEvent::DetailLoading { id } => {
                    self.loading_detail_for = Some(id);
                }
                InboxEvent::DetailLoaded(detail) => {
                    self.loading_detail_for = None;
                    self.detail_scroll = 0;
                    self.details.insert(detail.id.clone(), detail);
                }
                InboxEvent::Error(message) => {
                    self.loading_page = false;
                    self.loading_detail_for = None;
                    self.error = Some(message);
                }
            }
        }
    }

    fn maybe_request_selected_detail(&mut self) {
        if self.is_load_more_selected() {
            return;
        }

        let Some(selected) = self.messages.get(self.selected) else {
            return;
        };

        if self.details.contains_key(&selected.id) {
            return;
        }

        if self.loading_detail_for.as_deref() == Some(selected.id.as_str()) {
            return;
        }

        let _ = self
            .command_tx
            .send(InboxCommand::LoadDetail(selected.id.clone()));
    }

    fn next(&mut self) {
        let max_index = self.messages.len() + usize::from(self.has_next_page);
        if max_index == 0 {
            return;
        }

        self.selected = (self.selected + 1).min(max_index - 1);
        self.detail_scroll = 0;
    }

    fn previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.detail_scroll = 0;
    }

    fn is_load_more_selected(&self) -> bool {
        self.has_next_page && self.selected == self.messages.len()
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
            .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(chunks[1]);

        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                "mailman",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Gmail inbox"),
        ]))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, chunks[0]);

        let items: Vec<ListItem> = if self.messages.is_empty() {
            let text = if let Some(error) = &self.error {
                format!("Failed to load inbox: {error}")
            } else if self.loading_page {
                "Loading inbox page...".to_string()
            } else {
                "No inbox messages found.".to_string()
            };
            vec![ListItem::new(Line::from(text))]
        } else {
            let mut items: Vec<ListItem> = self
                .messages
                .iter()
                .map(|message| {
                    let tag_color = category_color(&message.category);
                    ListItem::new(vec![
                        Line::from(Span::styled(
                            truncate(&message.subject, 50),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(vec![
                            Span::styled("From: ", Style::default().fg(Color::Gray)),
                            Span::styled(
                                truncate(&message.from, 42),
                                Style::default().fg(Color::White),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("Date: ", Style::default().fg(Color::Gray)),
                            Span::styled(
                                truncate(&message.received_at, 34),
                                Style::default().fg(Color::White),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("Tag:  ", Style::default().fg(Color::Gray)),
                            Span::styled(
                                message.category.as_str(),
                                Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::from(""),
                    ])
                })
                .collect();

            if self.has_next_page {
                let text = if self.loading_page {
                    "Loading more messages..."
                } else {
                    "Load more messages..."
                };
                items.push(ListItem::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        text,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(format!("Loaded pages: {}", self.pages_loaded)),
                    Line::from("Press Enter or n"),
                    Line::from(""),
                ]));
            }

            items
        };

        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(self.selected.min(items.len() - 1)));
        }

        let page_label = if self.loading_page && self.pages_loaded == 0 {
            "Messages (loading page 1)".to_string()
        } else if self.loading_page {
            format!(
                "Messages ({} pages loaded, loading more)",
                self.pages_loaded
            )
        } else {
            format!(
                "Messages ({} mails, {} pages)",
                self.messages.len(),
                self.pages_loaded.max(1)
            )
        };

        let list = List::new(items)
            .block(Block::default().title(page_label).borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(28, 52, 84))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">");
        frame.render_stateful_widget(list, body[0], &mut list_state);

        let detail = if self.is_load_more_selected() {
            Paragraph::new(vec![
                Line::from("Load another page of inbox messages."),
                Line::from(""),
                Line::from(format!("Pages loaded: {}", self.pages_loaded)),
                Line::from(format!("Messages loaded: {}", self.messages.len())),
            ])
        } else if let Some(message) = self.messages.get(self.selected) {
            if let Some(detail) = self.details.get(&message.id) {
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled("From: ", Style::default().fg(Color::Yellow)),
                        Span::raw(&detail.from),
                    ]),
                    Line::from(vec![
                        Span::styled("To:   ", Style::default().fg(Color::Yellow)),
                        Span::raw(detail.to.join(", ")),
                    ]),
                    Line::from(vec![
                        Span::styled("Date: ", Style::default().fg(Color::Yellow)),
                        Span::raw(&detail.received_at),
                    ]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "Subject",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(detail.subject.as_str()),
                    Line::from(""),
                    Line::from(detail.body.as_str()),
                ])
            } else if self.loading_detail_for.as_deref() == Some(message.id.as_str()) {
                Paragraph::new(vec![
                    Line::from("Loading full message..."),
                    Line::from(""),
                    Line::from(message.subject.as_str()),
                ])
            } else {
                Paragraph::new("Waiting to load selected message...")
            }
        } else if let Some(error) = &self.error {
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Inbox error",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(error.as_str()),
            ])
        } else {
            Paragraph::new("No message selected.")
        }
        .block(Block::default().title("Message").borders(Borders::ALL))
        .scroll((self.detail_scroll, 0))
        .wrap(Wrap { trim: false });
        frame.render_widget(detail, body[1]);

        let footer = Paragraph::new(
            "q quit  j/k move  Enter/n load more  PgUp/PgDn scroll mail  g/G top/bottom",
        )
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
        frame.render_widget(footer, chunks[2]);
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

fn category_color(category: &str) -> Color {
    match category {
        "Promotions" | "Promotion" => Color::Yellow,
        "Updates" => Color::Green,
        "Primary" => Color::Blue,
        "Social" => Color::Cyan,
        "Forums" => Color::Magenta,
        _ => Color::Gray,
    }
}
