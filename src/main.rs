mod app;
mod cli;
mod config;
mod gmail;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    app::run(cli).await
}
