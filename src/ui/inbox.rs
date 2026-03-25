use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
    LabelsLoaded(Vec<(String, String)>),
    MessageUpdated(MessageSummary),
    Status(String),
    Error(String),
}

pub enum InboxCommand {
    LoadInitialPage,
    LoadMore,
    LoadDetail(String),
    ApplyFilter(FilterMode),
    CreateOrApplyLabel {
        message_id: String,
        label_name: String,
    },
    RemoveLabel {
        message_id: String,
        label_name: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum FilterMode {
    All,
    Primary,
    Promotions,
    Updates,
    Social,
    Forums,
    Important,
    Spam,
    Unread,
}

#[derive(Clone, Copy)]
enum GroupMode {
    None,
    Category,
    Date,
    Provider,
    Sender,
    SenderDomain,
    Account,
    ReadStatus,
    UserLabel,
}

enum InputMode {
    Search,
    ApplyLabel,
    RemoveLabel,
}

enum Row {
    Header(String),
    Message(usize),
    LoadMore,
}

pub struct InboxTui {
    messages: Vec<MessageSummary>,
    details: HashMap<String, MessageDetail>,
    label_names: HashMap<String, String>,
    selected_row: usize,
    loading_page: bool,
    loading_detail_for: Option<String>,
    page_index: usize,
    has_next_page: bool,
    pages_loaded: usize,
    error: Option<String>,
    status: Option<String>,
    detail_scroll: u16,
    filter: FilterMode,
    group_by: GroupMode,
    search_query: String,
    input_mode: Option<InputMode>,
    input_buffer: String,
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
            label_names: HashMap::new(),
            selected_row: 0,
            loading_page: true,
            loading_detail_for: None,
            page_index: 0,
            has_next_page: false,
            pages_loaded: 0,
            error: None,
            status: None,
            detail_scroll: 0,
            filter: FilterMode::All,
            group_by: GroupMode::None,
            search_query: String::new(),
            input_mode: None,
            input_buffer: String::new(),
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

                if self.input_mode.is_some() {
                    self.handle_input_key(key.code, key.modifiers);
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('/') => {
                        self.input_buffer = self.search_query.clone();
                        self.input_mode = Some(InputMode::Search);
                        self.status = Some("Search mode".to_string());
                    }
                    KeyCode::Char('l') => {
                        if self.selected_message_id().is_some() {
                            self.input_buffer.clear();
                            self.input_mode = Some(InputMode::ApplyLabel);
                            self.status =
                                Some("Label mode: type a label name and press Enter".to_string());
                        } else {
                            self.status =
                                Some("Select a message before applying a label".to_string());
                        }
                    }
                    KeyCode::Char('u') => {
                        if self.selected_message_id().is_some() {
                            self.input_buffer.clear();
                            self.input_mode = Some(InputMode::RemoveLabel);
                            self.status =
                                Some("Unlabel mode: type a label name and press Enter".to_string());
                        } else {
                            self.status =
                                Some("Select a message before removing a label".to_string());
                        }
                    }
                    KeyCode::Char('f') => {
                        self.filter = self.filter.next();
                        let _ = self.command_tx.send(InboxCommand::ApplyFilter(self.filter));
                        self.select_first_row();
                    }
                    KeyCode::Char('g') => {
                        self.group_by = self.group_by.next();
                        self.select_first_row();
                    }
                    KeyCode::Char('r') => {
                        self.filter = FilterMode::All;
                        self.group_by = GroupMode::None;
                        self.search_query.clear();
                        self.input_mode = None;
                        self.input_buffer.clear();
                        let _ = self.command_tx.send(InboxCommand::ApplyFilter(self.filter));
                        self.select_first_row();
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.next(),
                    KeyCode::Up | KeyCode::Char('k') => self.previous(),
                    KeyCode::Home => self.select_first_row(),
                    KeyCode::End => self.select_last_row(),
                    KeyCode::Right | KeyCode::Char('n') | KeyCode::Enter => {
                        if self.selected_is_load_more() && !self.loading_page && self.has_next_page
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

    fn handle_input_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
                self.input_mode = None;
                self.input_buffer.clear();
                self.status = Some("Canceled input".to_string());
            }
            KeyCode::Enter => {
                match self.input_mode {
                    Some(InputMode::Search) => {
                        self.search_query = self.input_buffer.clone();
                        self.select_first_row();
                    }
                    Some(InputMode::ApplyLabel) => {
                        if let Some(message_id) = self.selected_message_id() {
                            let label_name = self.input_buffer.trim().to_string();
                            if !label_name.is_empty() {
                                let _ = self.command_tx.send(InboxCommand::CreateOrApplyLabel {
                                    message_id,
                                    label_name,
                                });
                            }
                        }
                    }
                    Some(InputMode::RemoveLabel) => {
                        if let Some(message_id) = self.selected_message_id() {
                            let label_name = self.input_buffer.trim().to_string();
                            if !label_name.is_empty() {
                                let _ = self.command_tx.send(InboxCommand::RemoveLabel {
                                    message_id,
                                    label_name,
                                });
                            }
                        }
                    }
                    None => {}
                }
                self.input_mode = None;
                self.input_buffer.clear();
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.input_buffer.push(ch);
            }
            _ => {}
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
                        self.selected_row = 0;
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
                    if self.project_rows().len() == 1 {
                        self.select_first_row();
                    }
                }
                InboxEvent::DetailLoading { id } => {
                    self.loading_detail_for = Some(id);
                }
                InboxEvent::DetailLoaded(detail) => {
                    self.loading_detail_for = None;
                    self.detail_scroll = 0;
                    self.details.insert(detail.id.clone(), detail);
                }
                InboxEvent::LabelsLoaded(labels) => {
                    self.label_names = labels.into_iter().collect();
                }
                InboxEvent::MessageUpdated(summary) => {
                    if let Some(existing) =
                        self.messages.iter_mut().find(|msg| msg.id == summary.id)
                    {
                        *existing = summary;
                    }
                }
                InboxEvent::Status(message) => {
                    self.status = Some(message);
                }
                InboxEvent::Error(message) => {
                    self.loading_page = false;
                    self.loading_detail_for = None;
                    self.error = Some(message);
                }
            }
        }
    }

    fn selected_message_id(&self) -> Option<String> {
        let rows = self.project_rows();
        let Row::Message(message_index) = rows.get(self.selected_row)? else {
            return None;
        };
        let filtered = self.filtered_messages();
        Some(filtered.get(*message_index)?.id.clone())
    }

    fn maybe_request_selected_detail(&mut self) {
        let rows = self.project_rows();
        let Some(selected) = rows.get(self.selected_row) else {
            return;
        };
        let Row::Message(message_index) = selected else {
            return;
        };
        let filtered = self.filtered_messages();
        let Some(message) = filtered.get(*message_index) else {
            return;
        };

        if self.details.contains_key(&message.id) {
            return;
        }
        if self.loading_detail_for.as_deref() == Some(message.id.as_str()) {
            return;
        }

        let _ = self
            .command_tx
            .send(InboxCommand::LoadDetail(message.id.clone()));
    }

    fn filtered_messages(&self) -> Vec<&MessageSummary> {
        self.messages
            .iter()
            .filter(|message| self.matches_search(message))
            .collect()
    }

    fn matches_search(&self, message: &MessageSummary) -> bool {
        if self.search_query.trim().is_empty() {
            return true;
        }

        let query = self.search_query.to_ascii_lowercase();
        let haystack = format!(
            "{} {} {} {} {}",
            message.subject, message.from, message.snippet, message.category, message.account
        )
        .to_ascii_lowercase();

        haystack.contains(&query)
    }

    fn project_rows(&self) -> Vec<Row> {
        let filtered = self.filtered_messages();
        let mut rows = Vec::new();

        match self.group_by {
            GroupMode::None => {
                for index in 0..filtered.len() {
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::Category => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    if message.category != current {
                        current = message.category.clone();
                        rows.push(Row::Header(current.clone()));
                    }
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::Date => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    let group = date_group(&message.received_at);
                    if group != current {
                        current = group.clone();
                        rows.push(Row::Header(group));
                    }
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::Provider => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    if message.provider != current {
                        current = message.provider.clone();
                        rows.push(Row::Header(current.clone()));
                    }
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::Sender => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    let group = message.from.clone();
                    if group != current {
                        current = group.clone();
                        rows.push(Row::Header(group));
                    }
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::SenderDomain => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    let group = sender_domain(&message.from);
                    if group != current {
                        current = group.clone();
                        rows.push(Row::Header(group));
                    }
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::Account => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    let group = message.account.clone();
                    if group != current {
                        current = group.clone();
                        rows.push(Row::Header(group));
                    }
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::ReadStatus => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    let group = if message.labels.iter().any(|label| label == "UNREAD") {
                        "Unread".to_string()
                    } else {
                        "Read".to_string()
                    };
                    if group != current {
                        current = group.clone();
                        rows.push(Row::Header(group));
                    }
                    rows.push(Row::Message(index));
                }
            }
            GroupMode::UserLabel => {
                let mut current = String::new();
                for (index, message) in filtered.iter().enumerate() {
                    let group = first_user_label(&message.labels, &self.label_names);
                    if group != current {
                        current = group.clone();
                        rows.push(Row::Header(group));
                    }
                    rows.push(Row::Message(index));
                }
            }
        }

        if self.has_next_page {
            rows.push(Row::LoadMore);
        }

        rows
    }

    fn next(&mut self) {
        let rows = self.project_rows();
        if rows.is_empty() {
            return;
        }

        let mut index = self.selected_row.saturating_add(1);
        while index < rows.len() {
            if matches!(rows[index], Row::Message(_) | Row::LoadMore) {
                self.selected_row = index;
                self.detail_scroll = 0;
                return;
            }
            index += 1;
        }
    }

    fn previous(&mut self) {
        let rows = self.project_rows();
        if rows.is_empty() || self.selected_row == 0 {
            return;
        }

        let mut index = self.selected_row.saturating_sub(1);
        loop {
            if matches!(rows[index], Row::Message(_) | Row::LoadMore) {
                self.selected_row = index;
                self.detail_scroll = 0;
                return;
            }
            if index == 0 {
                return;
            }
            index -= 1;
        }
    }

    fn select_first_row(&mut self) {
        let rows = self.project_rows();
        self.selected_row = rows
            .iter()
            .position(|row| matches!(row, Row::Message(_) | Row::LoadMore))
            .unwrap_or(0);
        self.detail_scroll = 0;
    }

    fn select_last_row(&mut self) {
        let rows = self.project_rows();
        self.selected_row = rows
            .iter()
            .rposition(|row| matches!(row, Row::Message(_) | Row::LoadMore))
            .unwrap_or(0);
        self.detail_scroll = 0;
    }

    fn selected_is_load_more(&self) -> bool {
        matches!(
            self.project_rows().get(self.selected_row),
            Some(Row::LoadMore)
        )
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
            Span::raw(format!(
                "  filter:{}(mailbox)  group:{}",
                self.filter.label(),
                self.group_by.label()
            )),
        ]))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, chunks[0]);

        let filtered = self.filtered_messages();
        let rows = self.project_rows();
        let items: Vec<ListItem> = if filtered.is_empty() && !self.has_next_page {
            let text = if let Some(error) = &self.error {
                format!("Failed to load inbox: {error}")
            } else if self.loading_page {
                format!("Loading {} mails from mailbox...", self.filter.label())
            } else {
                format!(
                    "No messages found for mailbox filter '{}' and search '{}'.",
                    self.filter.label(),
                    self.search_query
                )
            };
            vec![ListItem::new(Line::from(text))]
        } else {
            rows.iter()
                .map(|row| match row {
                    Row::Header(text) => ListItem::new(vec![
                        Line::from(""),
                        Line::from(Span::styled(
                            text.as_str(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        )),
                    ]),
                    Row::Message(index) => {
                        let message = filtered[*index];
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
                            Line::from(vec![
                                Span::styled("User: ", Style::default().fg(Color::Gray)),
                                Span::styled(
                                    first_user_label(&message.labels, &self.label_names),
                                    Style::default().fg(Color::Magenta),
                                ),
                            ]),
                            Line::from(""),
                        ])
                    }
                    Row::LoadMore => ListItem::new(vec![
                        Line::from(""),
                        Line::from(Span::styled(
                            if self.loading_page {
                                "Loading more messages..."
                            } else {
                                "Load more messages..."
                            },
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(format!("Loaded pages: {}", self.pages_loaded)),
                        Line::from("Press Enter or n"),
                        Line::from(""),
                    ]),
                })
                .collect()
        };

        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(self.selected_row.min(items.len() - 1)));
        }

        let page_label = format!(
            "Messages ({} shown, {} loaded, {} pages)",
            filtered.len(),
            self.messages.len(),
            self.pages_loaded.max(1)
        );

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

        let detail = if self.selected_is_load_more() {
            Paragraph::new(vec![
                Line::from("Load another page of inbox messages."),
                Line::from(""),
                Line::from(format!("Pages loaded: {}", self.pages_loaded)),
                Line::from(format!("Messages loaded: {}", self.messages.len())),
            ])
        } else if let Some(Row::Message(index)) = rows.get(self.selected_row) {
            let message = filtered[*index];
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
                    Line::from(vec![
                        Span::styled("Labels: ", Style::default().fg(Color::Yellow)),
                        Span::raw(render_user_labels(&message.labels, &self.label_names)),
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

        let footer_text = if let Some(mode) = &self.input_mode {
            let label = match mode {
                InputMode::Search => "Search",
                InputMode::ApplyLabel => "Apply/Create label",
                InputMode::RemoveLabel => "Remove label",
            };
            format!(
                "{}: {}_  Enter apply  Esc cancel  Backspace delete",
                label, self.input_buffer
            )
        } else if let Some(status) = &self.status {
            format!(
                "{}  |  / search  f filter:{}  g group:{}  l label  u unlabel",
                status,
                self.filter.label(),
                self.group_by.label()
            )
        } else {
            format!(
                "q quit  / search  f filter:{}  g group:{}  l label  u unlabel  r reset  Enter load more  PgUp/PgDn scroll",
                self.filter.label(),
                self.group_by.label()
            )
        };
        let footer = Paragraph::new(footer_text)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        frame.render_widget(footer, chunks[2]);

        if let Some(mode) = &self.input_mode {
            let popup = centered_rect(72, 8, area);
            let title = match mode {
                InputMode::Search => "Search",
                InputMode::ApplyLabel => "Apply Or Create Label",
                InputMode::RemoveLabel => "Remove Label",
            };
            let body = Paragraph::new(vec![
                Line::from(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    match mode {
                        InputMode::Search => "Filter the loaded messages by text.",
                        InputMode::ApplyLabel => {
                            "Create a Gmail label if needed, then apply it to the selected mail."
                        }
                        InputMode::RemoveLabel => "Remove one Gmail label from the selected mail.",
                    },
                    Style::default().fg(Color::Gray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Input",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!("{}_", self.input_buffer)),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter apply  Esc cancel",
                    Style::default().fg(Color::Green),
                )),
            ])
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .style(Style::default().fg(Color::White).bg(Color::Rgb(12, 16, 28))),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(Clear, popup);
            frame.render_widget(body, popup);
        }
    }
}

impl FilterMode {
    pub fn next(self) -> Self {
        match self {
            FilterMode::All => FilterMode::Primary,
            FilterMode::Primary => FilterMode::Promotions,
            FilterMode::Promotions => FilterMode::Updates,
            FilterMode::Updates => FilterMode::Social,
            FilterMode::Social => FilterMode::Forums,
            FilterMode::Forums => FilterMode::Important,
            FilterMode::Important => FilterMode::Spam,
            FilterMode::Spam => FilterMode::Unread,
            FilterMode::Unread => FilterMode::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FilterMode::All => "all",
            FilterMode::Primary => "primary",
            FilterMode::Promotions => "promotions",
            FilterMode::Updates => "updates",
            FilterMode::Social => "social",
            FilterMode::Forums => "forums",
            FilterMode::Important => "important",
            FilterMode::Spam => "spam",
            FilterMode::Unread => "unread",
        }
    }
}

impl GroupMode {
    fn next(self) -> Self {
        match self {
            GroupMode::None => GroupMode::Category,
            GroupMode::Category => GroupMode::Date,
            GroupMode::Date => GroupMode::Provider,
            GroupMode::Provider => GroupMode::Sender,
            GroupMode::Sender => GroupMode::SenderDomain,
            GroupMode::SenderDomain => GroupMode::Account,
            GroupMode::Account => GroupMode::ReadStatus,
            GroupMode::ReadStatus => GroupMode::UserLabel,
            GroupMode::UserLabel => GroupMode::None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            GroupMode::None => "none",
            GroupMode::Category => "category",
            GroupMode::Date => "date",
            GroupMode::Provider => "provider",
            GroupMode::Sender => "sender",
            GroupMode::SenderDomain => "sender-domain",
            GroupMode::Account => "account",
            GroupMode::ReadStatus => "read-status",
            GroupMode::UserLabel => "user-label",
        }
    }
}

fn date_group(received_at: &str) -> String {
    let compact = received_at.replace("  ", " ");
    compact
        .split(':')
        .next()
        .unwrap_or(received_at)
        .trim()
        .to_string()
}

fn sender_domain(from: &str) -> String {
    let start = from.rfind('<').map(|idx| idx + 1).unwrap_or(0);
    let end = from.rfind('>').unwrap_or(from.len());
    let candidate = &from[start..end];
    candidate
        .split('@')
        .nth(1)
        .unwrap_or(candidate)
        .trim()
        .to_string()
}

fn first_user_label(labels: &[String], label_names: &HashMap<String, String>) -> String {
    for id in labels {
        if let Some(name) = label_names.get(id) {
            return name.clone();
        }
    }
    "No user label".to_string()
}

fn render_user_labels(labels: &[String], label_names: &HashMap<String, String>) -> String {
    let values: Vec<String> = labels
        .iter()
        .filter_map(|id| label_names.get(id).cloned())
        .collect();
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
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

fn centered_rect(
    percent_x: u16,
    height: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Length(height),
            Constraint::Percentage(35),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}
