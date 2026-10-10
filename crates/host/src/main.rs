use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use falos_host::register::{self, Client};
use falos_host::server::{self, HostFile};
use falos_host::{Host, HostOptions};

#[derive(Parser)]
#[command(
    name = "falos-host",
    about = "fal-os canvas host: MCP server + canvas sync"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve a project folder (created if missing).
    Serve {
        #[arg(long)]
        project: PathBuf,
        /// Preferred port; the next free one is used if busy.
        #[arg(long, default_value_t = server::DEFAULT_PORT)]
        port: u16,
    },
    /// Add/update the fal-os MCP server in an agent client's config.
    Register {
        #[arg(long, value_enum)]
        client: Client,
        /// Print the diff without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,loro=warn,loro_internal=warn".into()),
        )
        .init();
    match Cli::parse().cmd {
        Cmd::Serve { project, port } => {
            let token = server::load_or_create_token()?;
            let host = Host::open(&project, HostOptions::default())?;
            let running = server::serve(host.clone(), port, &token).await?;
            let port = running.addr.port();
            HostFile {
                port,
                url: server::mcp_url(port),
                token,
                pid: std::process::id(),
                project: host.dir().display().to_string(),
            }
            .save()?;
            tracing::info!(
                "serving {} — MCP {} · sync ws://127.0.0.1:{port}/sync/<canvas> · token in {}",
                host.dir().display(),
                server::mcp_url(port),
                server::home_dir().join("host.json").display()
            );
            if host.opts.fal_key.is_none() {
                tracing::warn!("FAL_KEY is not set: `generate` will return an error");
            }
            tokio::signal::ctrl_c().await?;
            running.cancel.cancel();
            running.task.await?;
        }
        Cmd::Register { client, dry_run } => {
            let token = server::load_or_create_token()?;
            let port = HostFile::load().map_or(server::DEFAULT_PORT, |h| h.port);
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .context("HOME is not set")?;
            let o = register::register(client, &home, &server::mcp_url(port), &token, dry_run)?;
            if !o.changed() {
                println!("{}: already up to date", o.path.display());
            } else {
                let redact = |s: &str| s.replace(&token, "<token from ~/.fal-os/token>");
                println!(
                    "--- {}\n{}",
                    o.path.display(),
                    register::diff(&redact(&o.before), &redact(&o.after))
                );
                println!(
                    "{}",
                    if dry_run {
                        "(dry run: nothing written)"
                    } else {
                        "written"
                    }
                );
            }
        }
    }
    Ok(())
}
