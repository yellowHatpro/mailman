use anyhow::Result;
use tokio::sync::mpsc;

use crate::cli::{Cli, Commands};
use crate::config::AppConfig;
use crate::gmail::client::{GmailClient, StubGmailClient};
use crate::ui::inbox::{InboxCommand, InboxEvent, InboxTui};

pub async fn run(cli: Cli) -> Result<()> {
    if cli.command.is_none() {
        let config = AppConfig::load_or_init()?;
        let client = StubGmailClient::from_config(&config);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let limit = 25;
            let mut page_start_tokens = vec![None::<String>];
            let mut current_page = 0usize;
            let mut next_page_token = None::<String>;

            while let Some(command) = command_rx.recv().await {
                match command {
                    InboxCommand::LoadInitialPage => {
                        current_page = 0;
                        page_start_tokens = vec![None];
                        next_page_token = None;
                        let page_token = page_start_tokens[current_page].clone();
                        if load_page(
                            &client,
                            &event_tx,
                            limit,
                            current_page,
                            page_token.as_deref(),
                            true,
                            &mut next_page_token,
                            &mut page_start_tokens,
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                    }
                    InboxCommand::LoadMore => {
                        if next_page_token.is_none() {
                            continue;
                        }
                        current_page += 1;
                        if page_start_tokens.len() <= current_page {
                            page_start_tokens.push(next_page_token.clone());
                        }
                        let page_token = page_start_tokens[current_page].clone();
                        if load_page(
                            &client,
                            &event_tx,
                            limit,
                            current_page,
                            page_token.as_deref(),
                            false,
                            &mut next_page_token,
                            &mut page_start_tokens,
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                    }
                    InboxCommand::LoadDetail(id) => {
                        let _ = event_tx.send(InboxEvent::DetailLoading { id: id.clone() });
                        match client.read_message(&id).await {
                            Ok(detail) => {
                                let _ = event_tx.send(InboxEvent::DetailLoaded(detail));
                            }
                            Err(error) => {
                                let _ = event_tx.send(InboxEvent::Error(error.to_string()));
                            }
                        }
                    }
                }
            }
        });

        InboxTui::new(event_rx, command_tx).run()?;
        return Ok(());
    }

    match cli.command.expect("checked above") {
        Commands::Init => {
            let path = AppConfig::init_default_config()?;
            println!("Created config at {}", path.display());
        }
        Commands::Auth => {
            let config = AppConfig::load_or_init()?;
            let client = StubGmailClient::from_config(&config);
            client.authenticate().await?;
        }
        Commands::Inbox { limit } => {
            let config = AppConfig::load_or_init()?;
            let client = StubGmailClient::from_config(&config);
            let messages = client.list_inbox(limit).await?;

            if messages.is_empty() {
                println!("Inbox is empty.");
            } else {
                for message in messages {
                    println!(
                        "{}  [{}]  {} <{}>",
                        message.id, message.received_at, message.subject, message.from
                    );
                }
            }
        }
        Commands::Read { id } => {
            let config = AppConfig::load_or_init()?;
            let client = StubGmailClient::from_config(&config);
            let message = client.read_message(&id).await?;

            println!("Id: {}", message.id);
            println!("From: {}", message.from);
            println!("To: {}", message.to.join(", "));
            println!("Subject: {}", message.subject);
            println!("Date: {}", message.received_at);
            println!();
            println!("{}", message.body);
        }
        Commands::Send { to, subject, body } => {
            let config = AppConfig::load_or_init()?;
            let client = StubGmailClient::from_config(&config);
            client.send_message(&to, &subject, &body).await?;

            println!("Message queued for delivery to {}", to.join(", "));
        }
    }

    Ok(())
}

async fn load_page(
    client: &StubGmailClient,
    event_tx: &mpsc::UnboundedSender<InboxEvent>,
    limit: usize,
    page_index: usize,
    page_token: Option<&str>,
    replace: bool,
    next_page_token: &mut Option<String>,
    page_start_tokens: &mut Vec<Option<String>>,
) -> Result<()> {
    let _ = event_tx.send(InboxEvent::PageLoading {
        page_index,
        replace,
    });
    let page = client.fetch_inbox_page(limit, page_token).await?;
    for id in &page.ids {
        let message = client.fetch_message_summary(id).await?;
        let _ = event_tx.send(InboxEvent::PageMessageLoaded {
            page_index,
            message,
        });
    }

    *next_page_token = page.next_page_token.clone();
    if let Some(token) = &page.next_page_token {
        if page_start_tokens.len() == page_index + 1 {
            page_start_tokens.push(Some(token.clone()));
        } else if page_start_tokens.len() > page_index + 1 {
            page_start_tokens[page_index + 1] = Some(token.clone());
        }
    }

    let _ = event_tx.send(InboxEvent::PageLoaded {
        page_index,
        has_next_page: page.next_page_token.is_some(),
    });
    Ok(())
}
