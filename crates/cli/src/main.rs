//! masscap-cli: headless recorder and disk tools.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use masscap_engine::config::{Config, Destination, InputKind};
use masscap_engine::events::format_hms_short;
use masscap_engine::{Engine, RecordState};

#[derive(Parser)]
#[command(name = "masscap-cli", version, about = "MASSCAP multichannel recorder, headless")]
struct Cli {
    /// Config file (created with defaults if missing).
    #[arg(long, default_value = "masscap.toml")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Measure sustained sequential write throughput of a disk and store it in the config.
    DiskTest {
        /// Destination root folder, e.g. D:\MASSCAP
        path: PathBuf,
        /// Test size in gigabytes (default from config: 10).
        #[arg(long)]
        gb: Option<f64>,
        /// Only measure; do not add/update the destination in the config.
        #[arg(long)]
        no_save: bool,
    },
    /// Remove a destination from the config.
    DiskRemove { path: PathBuf },
    /// List configured destinations with free space and measured throughput.
    Disks,
    /// List audio input devices (system host; add --asio for ASIO drivers on Windows).
    ListDevices {
        #[arg(long)]
        asio: bool,
    },
    /// Record a take.
    Record {
        /// Use the simulated input regardless of config.
        #[arg(long)]
        sim: bool,
        /// Use the ASIO input regardless of config.
        #[arg(long)]
        asio: bool,
        /// Use the system audio device (Core Audio / WASAPI), e.g. Dante Virtual Soundcard.
        #[arg(long)]
        system: bool,
        /// Input device name (default device when omitted).
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        channels: Option<usize>,
        /// Simulated input sample rate.
        #[arg(long)]
        rate: Option<u32>,
        /// Stop after this many seconds (default: until Ctrl-C).
        #[arg(long)]
        duration: Option<f64>,
        /// Extra destinations for this run only (assumed 400 MB/s if unmeasured).
        #[arg(long = "dest")]
        dests: Vec<PathBuf>,
        #[arg(long)]
        session: Option<String>,
        /// Place a marker every N seconds (testing aid).
        #[arg(long)]
        mark_every: Option<f64>,
        /// Pre-roll seconds override.
        #[arg(long)]
        preroll: Option<f64>,
        /// Cache size override in megabytes.
        #[arg(long)]
        cache_mb: Option<u64>,
    },
    /// Write a default config file.
    Init,
}

fn load_or_default(path: &Path) -> Config {
    if path.exists() {
        match Config::load(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: cannot read {}: {e}; using defaults", path.display());
                Config::default()
            }
        }
    } else {
        Config::default()
    }
}

fn human_bytes(b: u64) -> String {
    let f = b as f64;
    if f >= 1e12 {
        format!("{:.2} TB", f / 1e12)
    } else if f >= 1e9 {
        format!("{:.1} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else {
        format!("{b} B")
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp_millis().init();
    let cli = Cli::parse();
    let mut config = load_or_default(&cli.config);

    match cli.cmd {
        Command::Init => {
            config.save(&cli.config)?;
            println!("wrote {}", cli.config.display());
        }
        Command::ListDevices { asio } => {
            use masscap_engine::input::cpal_input::HostKind;
            let kind = if asio { HostKind::Asio } else { HostKind::System };
            let devs = masscap_engine::input::list_devices(kind);
            if devs.is_empty() {
                println!("no input devices found{}", if asio { " (or this build lacks the `asio` feature)" } else { "" });
            }
            for d in devs {
                println!("{d}");
            }
        }
        Command::Disks => {
            if config.destinations.is_empty() {
                println!("no destinations configured; run `disk-test <path>`");
            }
            for d in &config.destinations {
                let free = masscap_engine::disk::free_space(&d.path).map(human_bytes).unwrap_or_else(|e| format!("error: {e}"));
                let tp = d.throughput_bps.map(|t| format!("{} /s", human_bytes(t))).unwrap_or_else(|| "unmeasured".into());
                println!("{:<40} free {:<12} throughput {:<14} serial {}", d.path.display(), free, tp, d.volume_serial);
            }
        }
        Command::DiskRemove { path } => {
            let before = config.destinations.len();
            config.destinations.retain(|d| d.path != path);
            if config.destinations.len() == before {
                bail!("{} is not a configured destination", path.display());
            }
            config.save(&cli.config)?;
            println!("removed {}", path.display());
        }
        Command::DiskTest { path, gb, no_save } => {
            let bytes = gb.map(|g| (g * 1e9) as u64).unwrap_or(config.disk_test_bytes);
            std::fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
            let free = masscap_engine::disk::free_space(&path)?;
            if free < bytes + 1_000_000_000 {
                bail!("only {} free on {}; need {} for the test", human_bytes(free), path.display(), human_bytes(bytes));
            }
            println!("writing {} to {} with direct I/O ...", human_bytes(bytes), path.display());
            let t0 = Instant::now();
            let mut last_print = Instant::now();
            let r = masscap_engine::disk::throughput_test(&path, bytes, config.effective_block_bytes(), |done| {
                if last_print.elapsed() > Duration::from_millis(500) {
                    last_print = Instant::now();
                    let s = t0.elapsed().as_secs_f64();
                    eprint!("\r  {:>6.1}%  {}/s   ", done as f64 / bytes as f64 * 100.0, human_bytes((done as f64 / s) as u64));
                }
            })?;
            eprintln!();
            println!("sustained {}/s over {:.1} s (slowest block {}/s)", human_bytes(r.bytes_per_second), r.seconds, human_bytes(r.worst_block_bps));
            let per_ch = 96_000 * 3;
            println!("at 50% safety that carries {} channels of 96k/24-bit", (r.bytes_per_second as f64 * config.throughput_safety / per_ch as f64).floor());
            if !no_save {
                let serial = masscap_engine::disk::volume_serial(&path).unwrap_or_default();
                let now = chrono_now();
                if let Some(d) = config.destinations.iter_mut().find(|d| d.path == path || (!serial.is_empty() && d.volume_serial == serial)) {
                    d.path = path.clone();
                    d.volume_serial = serial;
                    d.throughput_bps = Some(r.bytes_per_second);
                    d.measured_at = Some(now);
                } else {
                    config.destinations.push(Destination { path: path.clone(), volume_serial: serial, throughput_bps: Some(r.bytes_per_second), measured_at: Some(now) });
                }
                config.save(&cli.config)?;
                println!("saved to {}", cli.config.display());
            }
        }
        Command::Record { sim, asio, system, device, channels, rate, duration, dests, session, mark_every, preroll, cache_mb } => {
            if sim {
                config.input = InputKind::Sim;
            }
            if asio {
                config.input = InputKind::Asio;
            }
            if system {
                config.input = InputKind::System;
            }
            if let Some(d) = device {
                config.device = d;
            }
            if let Some(c) = channels {
                config.channels = c;
            }
            if let Some(r) = rate {
                config.sim_sample_rate = r;
            }
            if let Some(s) = session {
                config.session = s;
            }
            if let Some(p) = preroll {
                config.preroll_seconds = p;
            }
            if let Some(mb) = cache_mb {
                config.cache_bytes = Some(mb * 1_000_000);
            }
            for d in dests {
                std::fs::create_dir_all(&d)?;
                if !config.destinations.iter().any(|x| x.path == d) {
                    config.destinations.push(Destination { path: d, volume_serial: String::new(), throughput_bps: Some(400_000_000), measured_at: None });
                }
            }
            if config.destinations.is_empty() {
                bail!("no destinations: run `disk-test <path>` or pass --dest");
            }
            let input = masscap_engine::input::open(&config)?;
            let fmt = input.format();
            println!("input: {} · {} ch available · {} Hz", fmt.device_name, fmt.channels, fmt.sample_rate);
            println!("cache: {} · recording {} channels", human_bytes(config.effective_cache_bytes()), config.channels.min(fmt.channels));
            let engine = Engine::new(config, input)?;
            let events = engine.subscribe();
            std::thread::sleep(Duration::from_millis(300));
            let st = engine.status();
            if let Some(nr) = st.not_ready {
                bail!("cannot record: {nr}");
            }
            for d in &st.disks {
                println!("  {:<36} free {:<10} ch {:>3}-{:<3}", d.path, human_bytes(d.free_bytes), d.first_channel.map(|f| f + 1).unwrap_or(0), d.first_channel.map(|f| f + d.count).unwrap_or(0));
            }

            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let stop = stop.clone();
                ctrlc_handler(move || stop.store(true, std::sync::atomic::Ordering::SeqCst));
            }
            engine.start_take();
            let t0 = Instant::now();
            let mut last_line = Instant::now();
            let mut next_mark = mark_every.map(|m| t0 + Duration::from_secs_f64(m));
            loop {
                for ev in events.try_iter() {
                    println!("[{}] {}", ev.elapsed.map(format_hms_short).unwrap_or_else(|| "--:--:--".into()), ev.message());
                }
                let st = engine.status();
                if st.state == RecordState::Idle && t0.elapsed() > Duration::from_secs(2) {
                    if let Some(e) = &st.last_error {
                        bail!("{e}");
                    }
                }
                if let Some(nm) = next_mark {
                    if Instant::now() >= nm {
                        engine.mark("");
                        next_mark = Some(nm + Duration::from_secs_f64(mark_every.unwrap()));
                    }
                }
                if last_line.elapsed() > Duration::from_secs(1) {
                    last_line = Instant::now();
                    let rate: u64 = st.disks.iter().map(|d| d.write_rate_bps).sum();
                    eprintln!(
                        "  {}  remaining {}  cache {:>3.0}%  drops {}  clips {}  silent {}  write {}/s",
                        format_hms_short(st.elapsed_seconds),
                        st.remaining_seconds.map(format_hms_short).unwrap_or_else(|| "--:--:--".into()),
                        st.cache_fill * 100.0,
                        st.drops_total,
                        st.clips_total,
                        st.silent_count,
                        human_bytes(rate)
                    );
                }
                let done = duration.map(|d| t0.elapsed().as_secs_f64() >= d).unwrap_or(false) || stop.load(std::sync::atomic::Ordering::SeqCst);
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            engine.stop_take();
            let t1 = Instant::now();
            while engine.status().state != RecordState::Idle && t1.elapsed() < Duration::from_secs(120) {
                std::thread::sleep(Duration::from_millis(50));
            }
            for ev in events.try_iter() {
                println!("[{}] {}", ev.elapsed.map(format_hms_short).unwrap_or_else(|| "--:--:--".into()), ev.message());
            }
            let st = engine.status();
            println!("done · drops {} · clips {}", st.drops_total, st.clips_total);
            engine.shutdown();
        }
    }
    Ok(())
}

fn chrono_now() -> String {
    // Avoid a direct chrono dependency in the CLI: the engine re-exports what we need via events.
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("unix:{}", d.as_secs())
}

/// Minimal Ctrl-C hook without an extra crate: on Unix via libc-free signal
/// handling is awkward, so use the portable approach of a watcher thread on stdin
/// closure plus std's default SIGINT (which terminates). We install a handler
/// only where the platform makes it trivial.
fn ctrlc_handler(f: impl Fn() + Send + Sync + 'static) {
    static HANDLER: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();
    let _ = HANDLER.set(Box::new(f));
    #[cfg(windows)]
    unsafe {
        extern "system" fn handler(_ctrl: u32) -> i32 {
            if let Some(h) = HANDLER.get() {
                h();
            }
            1
        }
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1);
    }
    #[cfg(unix)]
    unsafe {
        extern "C" fn handler(_sig: i32) {
            if let Some(h) = HANDLER.get() {
                h();
            }
        }
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
    }
}
