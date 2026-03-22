use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "mailman",
    version,
    about = "A terminal scaffold for Gmail workflows"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Create the default config file for mailman
    Init,
    /// Start Gmail authentication flow
    Auth,
    /// Show recent inbox messages
    Inbox {
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Read a message by its Gmail message id
    Read {
        #[arg(help = "The Gmail message id")]
        id: String,
    },
    /// Send a message
    Send {
        #[arg(short, long, required = true)]
        to: Vec<String>,
        #[arg(short, long)]
        subject: String,
        #[arg(short, long)]
        body: String,
    },
}
