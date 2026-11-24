//! Shared server state: config, the engine (or why it could not start), and disk measurement.
//!
//! Lock order: never acquire `backend` while holding `config`.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use masscap_engine::config::{Config, Destination, InputKind};
use masscap_engine::disk::ThroughputResult;
use masscap_engine::{Engine, EngineStatus, RecordState};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};

/// Either a live engine or the reason we could not start one.
pub enum Backend {
    Live(Engine),
    Failed(String),
}

/// Why an operator action was refused.
pub enum Refusal {
    /// Not possible in the current state (recording, measuring, no input, …). HTTP 409.
    Conflict(String),
    /// The request itself is wrong. HTTP 400.
    BadRequest(String),
}

pub type Outcome = Result<(), Refusal>;

struct Measurement {
    path: PathBuf,
    total: u64,
    progress: Arc<AtomicU64>,
    handle: Option<JoinHandle<io::Result<ThroughputResult>>>,
}

pub struct AppState {
    config_path: PathBuf,
    config: Mutex<Config>,
    backend: RwLock<Backend>,
    measuring: Mutex<Option<Measurement>>,
    last_message: Mutex<Option<String>>,
}

impl AppState {
    pub fn new(config_path: PathBuf, config: Config) -> Self {
        let backend = open_backend(&config);
        Self { config_path, config: Mutex::new(config), backend: RwLock::new(backend), measuring: Mutex::new(None), last_message: Mutex::new(None) }
    }

    pub fn backend_error(&self) -> Option<String> {
        match &*self.backend.read() {
            Backend::Live(_) => None,
            Backend::Failed(e) => Some(e.clone()),
        }
    }

    pub fn status(&self) -> EngineStatus {
        let backend = self.backend.read();
        match &*backend {
            Backend::Live(e) => e.status(),
            Backend::Failed(_) => {
                drop(backend);
                empty_status(&self.config.lock())
            }
        }
    }

    fn recording(&self) -> bool {
        self.status().state == RecordState::Recording
    }

    fn with_engine(&self, f: impl FnOnce(&Engine) -> Outcome) -> Outcome {
        match &*self.backend.read() {
            Backend::Live(e) => f(e),
            Backend::Failed(err) => Err(Refusal::Conflict(format!("input not available: {err}"))),
        }
    }

    fn set_message(&self, m: impl Into<String>) {
        *self.last_message.lock() = Some(m.into());
    }

    fn save(&self, config: &Config) -> Result<(), String> {
        config.save(&self.config_path).map_err(|e| format!("cannot save {}: {e}", self.config_path.display()))
    }

    // ---- transport ---------------------------------------------------------

    pub fn start(&self) -> Outcome {
        self.with_engine(|e| {
            let s = e.status();
            if s.state == RecordState::Recording {
                return Ok(());
            }
            if let Some(nr) = s.not_ready {
                return Err(Refusal::Conflict(format!("cannot record: {nr}")));
            }
            e.start_take();
            Ok(())
        })
    }

    pub fn stop(&self) -> Outcome {
        self.with_engine(|e| {
            e.stop_take();
            Ok(())
        })
    }

    pub fn mark(&self, label: String) -> Outcome {
        self.with_engine(|e| {
            if e.status().state != RecordState::Recording {
                return Err(Refusal::Conflict("markers can only be placed while recording".into()));
            }
            e.mark(label);
            Ok(())
        })
    }

    pub fn set_session(&self, name: &str) -> Outcome {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::BadRequest("session name is empty".into()));
        }
        if self.recording() {
            return Err(Refusal::Conflict("cannot rename the session while recording".into()));
        }
        let saved = {
            let mut c = self.config.lock();
            c.session = name.to_string();
            self.save(&c)
        };
        if let Backend::Live(e) = &*self.backend.read() {
            e.set_session(name.to_string());
        }
        saved.map_err(Refusal::Conflict)
    }

    /// Retry with the simulated input after a failed device open.
    pub fn use_sim(&self) -> Outcome {
        if self.backend_error().is_none() {
            return Err(Refusal::Conflict("input is already running".into()));
        }
        let config = {
            let mut c = self.config.lock();
            c.input = InputKind::Sim;
            c.clone()
        };
        let new = open_backend(&config);
        let failed = match &new {
            Backend::Failed(e) => Some(e.clone()),
            Backend::Live(_) => None,
        };
        *self.backend.write() = new;
        match failed {
            Some(e) => Err(Refusal::Conflict(format!("simulated input failed too: {e}"))),
            None => Ok(()),
        }
    }

    /// Stop the engine (finalizing any take). Afterwards the state reports "shut down".
    pub fn shutdown(&self) {
        let old = std::mem::replace(&mut *self.backend.write(), Backend::Failed("shut down".into()));
        if let Backend::Live(e) = old {
            e.shutdown();
        }
    }

    // ---- destinations ------------------------------------------------------

    fn ensure_disks_editable(&self) -> Outcome {
        if self.recording() {
            return Err(Refusal::Conflict("cannot change destinations while recording".into()));
        }
        if self.measuring.lock().is_some() {
            return Err(Refusal::Conflict("a disk measurement is running".into()));
        }
        Ok(())
    }

    /// Persist the destination list and hand it to the engine.
    fn commit_destinations(&self, config: &Config) -> Result<(), String> {
        let dests = config.destinations.clone();
        let saved = self.save(config);
        if let Backend::Live(e) = &*self.backend.read() {
            e.set_destinations(dests);
        }
        saved
    }

    pub fn add_destination(&self, path: &str) -> Outcome {
        let path = path.trim();
        if path.is_empty() {
            return Err(Refusal::BadRequest("path is empty".into()));
        }
        self.ensure_disks_editable()?;
        let p = PathBuf::from(path);
        std::fs::create_dir_all(&p).map_err(|e| Refusal::Conflict(format!("cannot create {}: {e}", p.display())))?;
        let config = {
            let mut c = self.config.lock();
            if c.destinations.iter().any(|d| d.path == p) {
                return Err(Refusal::Conflict(format!("{} is already a destination", p.display())));
            }
            c.destinations.push(Destination { path: p.clone(), volume_serial: masscap_engine::disk::volume_serial(&p).unwrap_or_default(), throughput_bps: None, measured_at: None });
            c.clone()
        };
        self.commit_destinations(&config).map_err(Refusal::Conflict)?;
        self.set_message(format!("added {}; measure it before recording", p.display()));
        Ok(())
    }

    pub fn remove_destination(&self, path: &str) -> Outcome {
        self.ensure_disks_editable()?;
        let config = {
            let mut c = self.config.lock();
            let before = c.destinations.len();
            c.destinations.retain(|d| d.path.display().to_string() != path);
            if c.destinations.len() == before {
                return Err(Refusal::BadRequest(format!("{path} is not a destination")));
            }
            c.clone()
        };
        self.commit_destinations(&config).map_err(Refusal::Conflict)?;
        self.set_message(format!("removed {path}"));
        Ok(())
    }

    pub fn start_measure(&self, path: &str, test_gb: Option<f64>) -> Outcome {
        self.ensure_disks_editable()?;
        let (dir, total, block) = {
            let c = self.config.lock();
            let Some(d) = c.destinations.iter().find(|d| d.path.display().to_string() == path) else {
                return Err(Refusal::BadRequest(format!("{path} is not a destination")));
            };
            let total = test_gb.filter(|g| g.is_finite() && *g > 0.0).map(|g| (g * 1e9) as u64).unwrap_or(c.disk_test_bytes);
            (d.path.clone(), total, c.effective_block_bytes())
        };
        let progress = Arc::new(AtomicU64::new(0));
        let p2 = progress.clone();
        let dir2 = dir.clone();
        let handle = std::thread::Builder::new()
            .name("disk-test".into())
            .spawn(move || masscap_engine::disk::throughput_test(&dir2, total, block, |b| p2.store(b, Ordering::Relaxed)))
            .map_err(|e| Refusal::Conflict(format!("cannot start disk test: {e}")))?;
        *self.measuring.lock() = Some(Measurement { path: dir, total, progress, handle: Some(handle) });
        *self.last_message.lock() = None;
        Ok(())
    }

    /// Pick up a finished measurement, store the result in the config and tell the engine.
    fn poll_measurement(&self) {
        let m = {
            let mut slot = self.measuring.lock();
            let finished = slot.as_ref().map(|m| m.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true)).unwrap_or(false);
            if !finished {
                return;
            }
            slot.take().expect("checked above")
        };
        match m.handle.and_then(|h| h.join().ok()) {
            Some(Ok(r)) => {
                let serial = masscap_engine::disk::volume_serial(&m.path).unwrap_or_default();
                let now = chrono::Local::now().to_rfc3339();
                let config = {
                    let mut c = self.config.lock();
                    match c.destinations.iter_mut().find(|d| d.path == m.path) {
                        Some(d) => {
                            d.throughput_bps = Some(r.bytes_per_second);
                            d.volume_serial = serial;
                            d.measured_at = Some(now);
                        }
                        None => c.destinations.push(Destination { path: m.path.clone(), volume_serial: serial, throughput_bps: Some(r.bytes_per_second), measured_at: Some(now) }),
                    }
                    c.clone()
                };
                match self.commit_destinations(&config) {
                    Ok(()) => self.set_message(format!("{}: sustained {} (slowest block {})", m.path.display(), rate_human(r.bytes_per_second), rate_human(r.worst_block_bps))),
                    Err(e) => self.set_message(format!("measured but {e}")),
                }
            }
            Some(Err(e)) => self.set_message(format!("{}: test failed: {e}", m.path.display())),
            None => self.set_message("disk test thread panicked"),
        }
    }

    /// UI-only state that the engine status does not carry.
    pub fn ui_info(&self) -> Value {
        let measuring = self.measuring.lock().as_ref().map(|m| json!({ "path": m.path.display().to_string(), "done": m.progress.load(Ordering::Relaxed), "total": m.total }));
        let c = self.config.lock();
        let destinations: Vec<Value> = c
            .destinations
            .iter()
            .map(|d| json!({ "path": d.path.display().to_string(), "throughput_bps": d.throughput_bps, "measured_at": d.measured_at }))
            .collect();
        json!({
            "backend_error": self.backend_error(),
            "measuring": measuring,
            "last_message": *self.last_message.lock(),
            "destinations": destinations,
            "disk_test_gb": c.disk_test_bytes as f64 / 1e9,
            "config_path": self.config_path.display().to_string(),
        })
    }
}

pub fn spawn_measure_poller(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tick.tick().await;
            state.poll_measurement();
        }
    });
}

fn open_backend(config: &Config) -> Backend {
    match masscap_engine::input::open(config) {
        Ok(input) => match Engine::new(config.clone(), input) {
            Ok(e) => Backend::Live(e),
            Err(e) => Backend::Failed(e.to_string()),
        },
        Err(e) => Backend::Failed(e.to_string()),
    }
}

fn empty_status(config: &Config) -> EngineStatus {
    EngineStatus {
        state: RecordState::Idle,
        session: config.session.clone(),
        device_name: "no input".into(),
        sample_rate: config.sim_sample_rate,
        channels: 0,
        channel_names: vec![],
        take_name: None,
        elapsed_seconds: 0.0,
        remaining_seconds: None,
        free_bytes_total: 0,
        cache_bytes: 0,
        cache_fill: 0.0,
        drops_total: 0,
        clips_total: 0,
        silent_count: 0,
        markers: 0,
        meters: vec![],
        channel_paths: vec![],
        channel_bytes: vec![],
        disks: vec![],
        last_error: None,
        recent_events: Default::default(),
        not_ready: Some("no input device".into()),
    }
}

fn rate_human(bps: u64) -> String {
    let f = bps as f64;
    if f >= 1e9 {
        format!("{:.2} GB/s", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB/s", f / 1e6)
    } else {
        format!("{:.0} kB/s", f / 1e3)
    }
}

