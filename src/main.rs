//! `spacetrace-hub` — the fleet view.
//!
//! Agents scan their own machines and push snapshots here; this serves them
//! back as a dashboard, works out what is growing, and says so before a disk
//! fills up. It never touches the machines it watches.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use spacetrace_hub::config::{self, Config};
use spacetrace_hub::{db, web};

#[derive(Parser, Debug)]
#[command(
    name = "spacetrace-hub",
    version,
    about = "Collect snapshots from spacetrace agents and show what is growing",
    long_about = "spacetrace-hub receives snapshots pushed by agents, keeps a history per \
machine and folder, forecasts when a filesystem will fill up, and calls a webhook when a \
threshold is crossed. Self-hosted, one binary, one SQLite file."
)]
struct Cli {
    /// Configuration file
    #[arg(
        long,
        short,
        global = true,
        default_value = "/etc/spacetrace-hub/hub.toml",
        value_name = "FILE"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print a starter configuration file
    Init,
    /// Check the configuration and the database, then exit
    Check,
    /// Create an agent token and print it
    Token(TokenArgs),
    /// Serve the dashboard and accept pushes
    Serve,
}

#[derive(clap::Args, Debug)]
struct TokenArgs {
    /// Name for the agent, e.g. the hostname
    #[arg(value_name = "NAME")]
    name: String,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    // `init` must work before a config exists.
    if matches!(cli.command, Command::Init) {
        print!("{}", config::EXAMPLE_CONFIG);
        return Ok(());
    }

    let config = Config::load(&cli.config)?;
    prepare_database(&config)?;

    match &cli.command {
        Command::Init => unreachable!("handled above"),
        Command::Check => cmd_check(&config),
        Command::Token(args) => cmd_token(&config, &args.name),
        Command::Serve => cmd_serve(config),
    }
}

/// Create the parent directory, then run both migrations: the snapshot store's
/// and the hub's own tables, which share one file.
fn prepare_database(config: &Config) -> Result<()> {
    if let Some(parent) = config.db.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
    }
    // Opening the store runs its migration and refuses a newer schema.
    let _ = spacetrace_store::Store::open(&config.db)?;

    let conn = rusqlite::Connection::open(&config.db)
        .with_context(|| format!("opening {}", config.db.display()))?;
    db::migrate(&conn)?;
    Ok(())
}

fn cmd_check(config: &Config) -> Result<()> {
    println!("config       ok");
    println!("database     {}", config.db.display());
    println!("listen       {}", config.listen);
    println!(
        "admin token  {}",
        match config.resolve_admin_token()? {
            Some(_) => "configured",
            None => "MISSING (serve will refuse to start)",
        }
    );
    println!(
        "retention    {}",
        match config.keep_per_target {
            Some(keep) => format!("{keep} snapshots per target"),
            None => "unlimited (the database will grow without bound)".to_string(),
        }
    );

    let conn = rusqlite::Connection::open(&config.db)?;
    let tokens = db::list_tokens(&conn)?;
    let active = tokens.iter().filter(|t| !t.revoked).count();
    println!(
        "agents       {active} active tokens, {} total",
        tokens.len()
    );
    println!("alert rules  {}", db::list_rules(&conn)?.len());

    let store = spacetrace_store::Store::open(&config.db)?;
    let targets = spacetrace_hub::fleet::targets(&store)?;
    let summary = spacetrace_hub::fleet::summarise(&targets);
    println!(
        "fleet        {} machines, {} targets, {} snapshots",
        summary.hosts, summary.targets, summary.snapshots
    );
    if active == 0 {
        println!("\nNo agent tokens yet. Create one with:\n  spacetrace-hub token <name>");
    }
    Ok(())
}

fn cmd_token(config: &Config, name: &str) -> Result<()> {
    let conn = rusqlite::Connection::open(&config.db)?;
    let (token, plaintext) = db::create_token(&conn, name)?;
    println!("Created token #{} for {}", token.id, token.name);
    println!("\n{plaintext}\n");
    println!("Only its hash is stored, so this is the only time it will be shown.");
    println!("On the agent:");
    println!("  spacetrace-agent --config /etc/spacetrace/agent.toml \\");
    println!("    push https://your-hub.example.com --token {plaintext}");
    Ok(())
}

fn cmd_serve(config: Config) -> Result<()> {
    let admin_token = config.resolve_admin_token()?.context(
        "no admin token configured. Set admin_token_file, admin_token, or the \
         SPACETRACE_HUB_ADMIN_TOKEN environment variable — the dashboard lists every \
         path on every machine in the fleet, so leaving it open is not a sensible default",
    )?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(web::serve(&config, admin_token))
}
