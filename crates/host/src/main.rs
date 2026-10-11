use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use falos_host::register::{self, Client};
use falos_host::server::{self, HostFile};
use falos_host::workspace::Workspace;
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
    /// Serve a workspace of projects (default ~/fal-os-projects), or one project folder.
    Serve {
        /// Workspace folder: one subfolder per project (created if missing).
        #[arg(long, conflicts_with = "project")]
        workspace: Option<PathBuf>,
        /// Single-project mode: serve only this project folder (created if missing).
        #[arg(long)]
        project: Option<PathBuf>,
        /// Preferred port; the next free one is used if busy.
        #[arg(long, default_value_t = server::DEFAULT_PORT)]
        port: u16,
        /// Agent runtime (apps/agent-runtime) to spawn and supervise; auto-detected when omitted.
        #[arg(long)]
        runtime: Option<PathBuf>,
        /// Don't start the agent runtime (chat messages queue until one connects).
        #[arg(long)]
        no_runtime: bool,
    },
    /// Sign in with your fal account (browser, OAuth via fal AuthKit).
    Login,
    /// Forget the stored fal session.
    Logout,
    /// Show the signed-in fal account and check the token against fal's API.
    Whoami,
    /// Stdio MCP server: relays stdin/stdout to the running host's /mcp (for clients that
    /// prefer stdio). Reads ~/.fal-os/host.json.
    Mcp,
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
        // stdout belongs to the MCP relay (`falos-host mcp`).
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().cmd {
        Cmd::Serve { workspace, project, port, runtime, no_runtime } => {
            let token = server::load_or_create_token()?;
            let space = match project {
                Some(p) => Workspace::single(Host::open(&p, HostOptions::default())?),
                None => {
                    let root = match workspace {
                        Some(w) => w,
                        None => std::env::var_os("HOME")
                            .map(PathBuf::from)
                            .context("HOME is not set")?
                            .join("fal-os-projects"),
                    };
                    Workspace::open(&root, HostOptions::default)?
                }
            };
            let default = space.default_host()?;
            let running = server::serve_workspace(space.clone(), port, &token).await?;
            let port = running.addr.port();
            HostFile {
                port,
                url: server::mcp_url(port),
                token: token.clone(),
                pid: std::process::id(),
                project: default.dir().display().to_string(),
                workspace: space.root().map(|r| r.display().to_string()),
            }
            .save()?;
            tracing::info!(
                "serving {} — MCP {} · workspace ws://127.0.0.1:{port}/workspace · sync ws://127.0.0.1:{port}/sync/<project> · token in {}",
                space.root().unwrap_or(default.dir()).display(),
                server::mcp_url(port),
                server::home_dir().join("host.json").display()
            );
            if default.opts.fal_key.is_none() && !falos_host::fal_relay::Relay::available() {
                tracing::warn!("not signed in to fal: run `falos-host login` so agents can `generate`");
            }
            match (no_runtime, runtime.or_else(falos_host::runtime::detect)) {
                (false, Some(dir)) => {
                    falos_host::runtime::supervise(space.clone(), dir, port, token, running.cancel.clone())
                }
                (false, None) => tracing::warn!("no agent runtime found (pass --runtime <apps/agent-runtime>): chat is disabled"),
                _ => {}
            }
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                r = tokio::signal::ctrl_c() => r?,
                _ = term.recv() => {}
            }
            running.cancel.cancel();
            running.task.await?;
        }
        Cmd::Mcp => falos_host::relay::run().await?,
        Cmd::Login => {
            let t = falos_host::auth::login(|url| {
                println!("Opening your browser to sign in to fal…\nIf it doesn't open, visit:\n{url}\n");
                let _ = std::process::Command::new("open").arg(url).status();
            })
            .await?;
            let who = falos_host::auth::claims(&t.access_token).unwrap_or_default();
            println!("Signed in to fal{}.", who["email"].as_str().map(|e| format!(" as {e}")).unwrap_or_default());
        }
        Cmd::Logout => {
            falos_host::auth::logout()?;
            println!("Signed out of fal.");
        }
        Cmd::Whoami => {
            let http = reqwest::Client::new();
            let token = falos_host::auth::access_token(&http).await?;
            let c = falos_host::auth::claims(&token).unwrap_or_default();
            println!("iss: {}\nsub: {}\nemail: {}\naud: {}\nexp: {}", c["iss"], c["sub"], c["email"], c["aud"], c["exp"]);
            let res = http
                .get("https://queue.fal.run/fal-ai/flux/requests/00000000-0000-0000-0000-000000000000/status")
                .bearer_auth(&token)
                .send()
                .await?;
            let status = res.status();
            println!("fal queue API with bearer token: {status} {}", res.text().await.unwrap_or_default());
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
