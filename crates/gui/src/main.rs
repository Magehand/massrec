//! MASSCAP recorder console.
//!
//! `masscap [--config path] [--sim] [--channels N] [--rate HZ] [--cache-mb MB]`

mod app;
mod dests;
mod grid;
mod side;
mod theme;
mod transport;

use std::path::PathBuf;


use masscap_engine::config::{Config, InputKind};

pub struct Args {
    pub config_path: PathBuf,
    pub sim: bool,
    pub channels: Option<usize>,
    pub rate: Option<u32>,
    pub cache_mb: Option<u64>,
}

fn parse_args() -> Args {
    let mut a = Args { config_path: default_config_path(), sim: false, channels: None, rate: None, cache_mb: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => a.config_path = it.next().map(PathBuf::from).unwrap_or(a.config_path),
            "--sim" => a.sim = true,
            "--channels" => a.channels = it.next().and_then(|v| v.parse().ok()),
            "--rate" => a.rate = it.next().and_then(|v| v.parse().ok()),
            "--cache-mb" => a.cache_mb = it.next().and_then(|v| v.parse().ok()),
            "-h" | "--help" => {
                println!("masscap [--config path] [--sim] [--channels N] [--rate HZ] [--cache-mb MB]");
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
pub fn load_config(args: &Args) -> Config {
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

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = parse_args();
    let config = load_config(&args);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("MASSCAP 300")
            .with_inner_size([1680.0, 1000.0])
            .with_min_inner_size([1100.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native("MASSCAP 300", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, args, config)))))
}
