use anyhow::Result;

use crate::cli::{Cli, Commands};
use crate::config::AppConfig;
use crate::gmail::client::{GmailClient, StubGmailClient};
use crate::ui::inbox::{InboxEvent, InboxTui};
use tokio::sync::mpsc;

pub async fn run(cli: Cli) -> Result<()> {
    if cli.command.is_none() {
        let config = AppConfig::load_or_init()?;
        let client = StubGmailClient::from_config(&config);
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let limit = 25;
            match client.list_inbox_ids(limit).await {
                Ok(ids) => {
                    let total = ids.len();
                    let _ = tx.send(InboxEvent::Started { total });

                    for id in ids {
                        match client.fetch_message_summary(&id).await {
                            Ok(message) => {
                                let _ = tx.send(InboxEvent::MessageLoaded(message));
                            }
                            Err(error) => {
                                let _ = tx.send(InboxEvent::Failed(error.to_string()));
                                return;
                            }
                        }
                    }

                    let _ = tx.send(InboxEvent::Finished);
                }
                Err(error) => {
                    let _ = tx.send(InboxEvent::Failed(error.to_string()));
                }
            }
        });

        InboxTui::new(rx).run()?;
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
