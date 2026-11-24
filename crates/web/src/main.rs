//! MASSCAP recorder console: a web UI served by the recorder.
//!
//! `masscap [--config path] [--sim] [--channels N] [--rate HZ] [--cache-mb MB] [--bind ADDR]`
//!
//! `/` is the simple operator page, `/advanced` shows every channel, the disks and the event log.

mod api;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use masscap_engine::config::{Config, InputKind};

const DEFAULT_BIND: &str = "0.0.0.0:8080";

pub struct Args {
    pub config_path: PathBuf,
    pub sim: bool,
    pub channels: Option<usize>,
    pub rate: Option<u32>,
    pub cache_mb: Option<u64>,
    pub bind: SocketAddr,
}

fn usage() -> &'static str {
    "masscap [--config path] [--sim] [--channels N] [--rate HZ] [--cache-mb MB] [--bind ADDR (default 0.0.0.0:8080)]"
}

fn parse_args() -> Args {
    let mut a = Args {
        config_path: default_config_path(),
        sim: false,
        channels: None,
        rate: None,
        cache_mb: None,
        bind: DEFAULT_BIND.parse().expect("valid default bind address"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => a.config_path = it.next().map(PathBuf::from).unwrap_or(a.config_path),
            "--sim" => a.sim = true,
            "--channels" => a.channels = it.next().and_then(|v| v.parse().ok()),
            "--rate" => a.rate = it.next().and_then(|v| v.parse().ok()),
            "--cache-mb" => a.cache_mb = it.next().and_then(|v| v.parse().ok()),
            "--bind" => match it.next().map(|v| v.parse::<SocketAddr>()) {
                Some(Ok(addr)) => a.bind = addr,
                _ => {
                    eprintln!("--bind needs an address like 0.0.0.0:8080 or 127.0.0.1:9000");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    a
}

/// `masscap.toml` next to the executable, falling back to the working directory.
fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("masscap.toml")))
        .unwrap_or_else(|| PathBuf::from("masscap.toml"))
}

/// Load the config, applying command-line overrides. Creates the file if missing.
fn load_config(args: &Args) -> Config {
    let mut c = if args.config_path.exists() {
        Config::load(&args.config_path).unwrap_or_else(|e| {
            eprintln!("cannot read {}: {e}; using defaults", args.config_path.display());
            Config::default()
        })
    } else {
        let c = Config::default();
        if let Err(e) = c.save(&args.config_path) {
            eprintln!("cannot write {}: {e}", args.config_path.display());
        }
        c
    };
    if args.sim {
        c.input = InputKind::Sim;
    }
    if let Some(n) = args.channels {
        c.channels = n;
    }
    if let Some(r) = args.rate {
        c.sim_sample_rate = r;
    }
    if let Some(mb) = args.cache_mb {
        c.cache_bytes = Some(mb * 1_000_000);
    }
    c
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = parse_args();
    let config = load_config(&args);
    let state = Arc::new(state::AppState::new(args.config_path.clone(), config));
    if let Some(err) = state.backend_error() {
        log::error!("input not available: {err} (the web UI offers simulated input)");
    }
    state::spawn_measure_poller(state.clone());

    let listener = tokio::net::TcpListener::bind(args.bind).await.with_context(|| format!("cannot listen on {}", args.bind))?;
    let port = args.bind.port();
    if args.bind.ip().is_unspecified() {
        log::info!("console at http://localhost:{port}/ (advanced: /advanced), also reachable from the LAN on port {port}");
    } else {
        log::info!("console at http://{}/ (advanced: /advanced)", args.bind);
    }

    tokio::spawn(shutdown_on_ctrl_c(state.clone()));
    axum::serve(listener, api::router(state)).await.context("web server failed")?;
    Ok(())
}

/// Ctrl+C: stop any take cleanly (so WAV headers are finalized), then exit.
/// Status streams never end on their own, so we exit instead of waiting for them.
async fn shutdown_on_ctrl_c(state: Arc<state::AppState>) {
    if tokio::signal::ctrl_c().await.is_err() {
        return;
    }
    log::info!("shutting down");
    let _ = tokio::task::spawn_blocking(move || state.shutdown()).await;
    std::process::exit(0);
}
