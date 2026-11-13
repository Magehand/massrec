//! Destination disks: list, add, remove, measure throughput.

use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use egui::{Align, Layout, RichText};
use masscap_engine::config::{Config, Destination};
use masscap_engine::disk::ThroughputResult;

use crate::app::App;
use crate::theme::*;
use crate::Args;

struct Measurement {
    path: PathBuf,
    total: u64,
    progress: Arc<AtomicU64>,
    handle: Option<JoinHandle<std::io::Result<ThroughputResult>>>,
}

pub struct DestPanel {
    pub open: bool,
    pub new_path: String,
    pub session: String,
    pub test_gb: String,
    measuring: Option<Measurement>,
    pub last_message: Option<String>,
}

impl DestPanel {
    pub fn new(config: &Config) -> Self {
        Self {
            open: false,
            new_path: String::new(),
            session: config.session.clone(),
            test_gb: format!("{:.0}", config.disk_test_bytes as f64 / 1e9),
            measuring: None,
            last_message: None,
        }
    }

    pub fn measuring_path(&self) -> Option<&PathBuf> {
        self.measuring.as_ref().map(|m| &m.path)
    }

    fn start_measure(&mut self, path: PathBuf, config: &Config) {
        let total = self.test_gb.trim().parse::<f64>().ok().filter(|g| *g > 0.0).map(|g| (g * 1e9) as u64).unwrap_or(config.disk_test_bytes);
        let progress = Arc::new(AtomicU64::new(0));
        let p2 = progress.clone();
        let block = config.effective_block_bytes();
        let dir = path.clone();
        let handle = std::thread::Builder::new()
            .name("disk-test".into())
            .spawn(move || masscap_engine::disk::throughput_test(&dir, total, block, |b| p2.store(b, Ordering::Relaxed)))
            .ok();
        self.measuring = Some(Measurement { path, total, progress, handle });
        self.last_message = None;
    }

    /// Called every frame: pick up a finished measurement and persist it.
    /// Returns true when the destinations changed and the engine should be told.
    pub fn poll_measurement(&mut self, config: &mut Config, args: &Args) -> bool {
        let finished = self.measuring.as_ref().map(|m| m.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true)).unwrap_or(false);
        if !finished {
            return false;
        }
        let m = self.measuring.take().unwrap();
        let result = m.handle.and_then(|h| h.join().ok());
        match result {
            Some(Ok(r)) => {
                let serial = masscap_engine::disk::volume_serial(&m.path).unwrap_or_default();
                let now = chrono::Local::now().to_rfc3339();
                match config.destinations.iter_mut().find(|d| d.path == m.path) {
                    Some(d) => {
                        d.throughput_bps = Some(r.bytes_per_second);
                        d.volume_serial = serial;
                        d.measured_at = Some(now);
                    }
                    None => config.destinations.push(Destination { path: m.path.clone(), volume_serial: serial, throughput_bps: Some(r.bytes_per_second), measured_at: Some(now) }),
                }
                if let Err(e) = config.save(&args.config_path) {
                    self.last_message = Some(format!("measured but could not save config: {e}"));
                } else {
                    self.last_message = Some(format!("{}: sustained {} (slowest block {})", m.path.display(), rate_human(r.bytes_per_second), rate_human(r.worst_block_bps)));
                }
                true
            }
            Some(Err(e)) => {
                self.last_message = Some(format!("{}: test failed: {e}", m.path.display()));
                false
            }
            None => {
                self.last_message = Some("disk test thread panicked".into());
                false
            }
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let recording = app.recording();
    ui.horizontal(|ui| {
        ui.label(label("DESTINATIONS"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new("session").font(mono(10.0)).color(LABEL));
            let resp = ui.add_enabled(!recording, egui::TextEdit::singleline(&mut app.dests.session).desired_width(220.0).font(mono(11.0)));
            if resp.lost_focus() && app.dests.session != app.config.session && !app.dests.session.trim().is_empty() {
                app.config.session = app.dests.session.trim().to_string();
                if let Some(e) = app.engine() {
                    e.set_session(app.config.session.clone());
                }
                app.save_config();
            }
        });
    });
    ui.add_space(4.0);

    // header row
    egui::Grid::new("dests-grid").num_columns(7).spacing([16.0, 4.0]).striped(false).show(ui, |ui| {
        for h in ["PATH", "FREE", "THROUGHPUT", "CHANNELS", "WRITTEN", "RATE", ""] {
            ui.label(label(h));
        }
        ui.end_row();
        let mut remove: Option<usize> = None;
        let mut measure: Option<PathBuf> = None;
        let disks = app.status.disks.clone();
        for (i, d) in app.config.destinations.iter().enumerate() {
            let st = disks.iter().find(|s| s.path == d.path.display().to_string());
            let failed = st.map(|s| s.failed).unwrap_or(false);
            let pc = if failed { FAULT_TEXT } else { TEXT_SOFT };
            ui.label(RichText::new(format!("{}{}", d.path.display(), if failed { "  FAILED" } else { "" })).font(mono(11.0)).color(pc));
            ui.label(RichText::new(st.map(|s| bytes_human(s.free_bytes)).unwrap_or_else(|| "—".into())).font(mono(11.0)).color(TEXT_DIM));
            let tp = match d.throughput_bps {
                Some(t) => RichText::new(rate_human(t)).font(mono(11.0)).color(TEXT_DIM),
                None => RichText::new("unmeasured").font(mono(11.0)).color(HOT_TEXT),
            };
            ui.label(tp);
            let range = st.and_then(|s| s.first_channel.map(|f| format!("{:03}-{:03}", f + 1, f + s.count))).unwrap_or_else(|| "—".into());
            ui.label(RichText::new(range).font(mono(11.0)).color(TEXT_DIM));
            ui.label(RichText::new(st.map(|s| bytes_human(s.bytes_written)).unwrap_or_else(|| "—".into())).font(mono(11.0)).color(TEXT_DIM));
            ui.label(RichText::new(st.map(|s| rate_human(s.write_rate_bps)).unwrap_or_else(|| "—".into())).font(mono(11.0)).color(TEXT_DIM));
            ui.horizontal(|ui| {
                let busy = app.dests.measuring_path().is_some();
                if ui.add_enabled(!recording && !busy, egui::Button::new(RichText::new("Measure").font(sans(11.0)))).clicked() {
                    measure = Some(d.path.clone());
                }
                if ui.add_enabled(!recording && !busy, egui::Button::new(RichText::new("Remove").font(sans(11.0)))).clicked() {
                    remove = Some(i);
                }
            });
            ui.end_row();
        }
        if let Some(i) = remove {
            app.config.destinations.remove(i);
            app.save_config();
            if let Some(e) = app.engine() {
                e.set_destinations(app.config.destinations.clone());
            }
        }
        if let Some(p) = measure {
            let cfg = app.config.clone();
            app.dests.start_measure(p, &cfg);
        }
    });

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Add folder").font(sans(11.0)).color(TEXT_DIM));
        ui.add_enabled(!recording, egui::TextEdit::singleline(&mut app.dests.new_path).desired_width(320.0).font(mono(11.0)).hint_text("D:\\MASSCAP"));
        if ui.add_enabled(!recording && !app.dests.new_path.trim().is_empty(), egui::Button::new(RichText::new("Add").font(sans(11.0)))).clicked() {
            let p = PathBuf::from(app.dests.new_path.trim());
            if let Err(e) = std::fs::create_dir_all(&p) {
                app.dests.last_message = Some(format!("cannot create {}: {e}", p.display()));
            } else if !app.config.destinations.iter().any(|d| d.path == p) {
                app.config.destinations.push(Destination { path: p.clone(), volume_serial: masscap_engine::disk::volume_serial(&p).unwrap_or_default(), throughput_bps: None, measured_at: None });
                app.save_config();
                if let Some(e) = app.engine() {
                    e.set_destinations(app.config.destinations.clone());
                }
                app.dests.new_path.clear();
                app.dests.last_message = Some(format!("added {}; measure it before recording", p.display()));
            }
        }
        ui.add_space(12.0);
        ui.label(RichText::new("test size GB").font(sans(11.0)).color(TEXT_DIM));
        ui.add(egui::TextEdit::singleline(&mut app.dests.test_gb).desired_width(40.0).font(mono(11.0)));
    });

    if let Some(m) = &app.dests.measuring {
        let done = m.progress.load(Ordering::Relaxed);
        let frac = (done as f64 / m.total.max(1) as f64) as f32;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("measuring {} …", m.path.display())).font(mono(10.0)).color(TEXT_DIM));
            ui.add(egui::ProgressBar::new(frac).desired_width(300.0).text(RichText::new(format!("{} / {}", bytes_human(done), bytes_human(m.total))).font(mono(10.0))));
        });
    }
    if let Some(msg) = &app.dests.last_message {
        ui.label(RichText::new(msg).font(mono(10.0)).color(TEXT_DIM));
    }
}
