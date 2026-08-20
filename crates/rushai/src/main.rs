mod paths;

use anyhow::Context;
use clap::{Parser, Subcommand};
use jiff::Timestamp;
use rushai_config::Config;
use rushai_core::store::Store;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "rush",
    version,
    about = "An agentic coding assistant for your terminal"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the resolved configuration
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// List sessions, most recently updated first
    Sessions,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the JSON schema for rushai.json
    Schema,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RUSHAI_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        Command::Config { command: None } => show_config(),
        Command::Config {
            command: Some(ConfigCommand::Schema),
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&rushai_config::json_schema())?
            );
            Ok(())
        }
        Command::Sessions => list_sessions().await,
    }
}

fn show_config() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let mut config = Config::load(cwd)?;
    for provider in config.providers.values_mut() {
        if provider.api_key.is_some() {
            provider.api_key = Some("[redacted]".into());
        }
    }
    println!("{}", serde_json::to_string_pretty(&config)?);
    Ok(())
}

async fn list_sessions() -> anyhow::Result<()> {
    let dir = paths::data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let store = Store::open(dir.join("rushai.db"))?;
    let sessions = store.sessions().await?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    for session in sessions {
        let updated = Timestamp::from_millisecond(session.updated_at)
            .map(|ts| ts.strftime("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|_| session.updated_at.to_string());
        println!("{}  {}  {}", session.id, updated, session.title);
    }
    Ok(())
}
