//! Application state and the per-frame update loop.

use eframe::egui;
use std::time::{Duration, Instant};

use egui::Key;
use masscap_engine::config::{Config, InputKind};
use masscap_engine::{Engine, EngineStatus, RecordState};

use crate::dests::DestPanel;
use crate::theme::*;
use crate::{grid, side, transport, Args};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Simple,
    Advanced,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GridView {
    MeterWall,
    StatusMatrix,
}

/// Either a live engine or the reason we could not start one.
pub enum Backend {
    Live(Engine),
    Failed(String),
}

pub struct App {
    pub args: Args,
    pub config: Config,
    pub backend: Backend,
    /// Last polled status; refreshed once per frame.
    pub status: EngineStatus,
    pub mode: Mode,
    pub view: GridView,
    pub faults_only: bool,
    pub selected: usize,
    /// Set when the operator pressed stop once; a second press within 2 s stops.
    pub stop_armed_at: Option<Instant>,
    pub dests: DestPanel,
    /// Write-rate estimate for the selected channel: (bytes, when).
    pub sel_rate_sample: Option<(u64, Instant, u64)>,
    pub sel_rate_bps: u64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args, config: Config) -> Self {
        apply_style(&cc.egui_ctx);
        let backend = open_backend(&config);
        let status = match &backend {
            Backend::Live(e) => e.status(),
            Backend::Failed(_) => empty_status(&config),
        };
        let dests = DestPanel::new(&config);
        Self {
            args,
            config,
            backend,
            status,
            mode: Mode::Simple,
            view: GridView::MeterWall,
            faults_only: false,
            selected: 0,
            stop_armed_at: None,
            dests,
            sel_rate_sample: None,
            sel_rate_bps: 0,
        }
    }

    pub fn engine(&self) -> Option<&Engine> {
        match &self.backend {
            Backend::Live(e) => Some(e),
            Backend::Failed(_) => None,
        }
    }

    pub fn recording(&self) -> bool {
        self.status.state == RecordState::Recording
    }

    /// Record/stop button or Ctrl+R.
    pub fn toggle_record(&mut self) {
        let Some(engine) = self.engine() else { return };
        if self.recording() {
            match self.stop_armed_at {
                Some(t) if t.elapsed() < Duration::from_secs(2) => {
                    engine.stop_take();
                    self.stop_armed_at = None;
                }
                _ => self.stop_armed_at = Some(Instant::now()),
            }
        } else {
            engine.start_take();
            self.stop_armed_at = None;
        }
    }

    pub fn mark(&self) {
        if let (Some(e), true) = (self.engine(), self.recording()) {
            e.mark("");
        }
    }

    pub fn save_config(&self) {
        if let Err(e) = self.config.save(&self.args.config_path) {
            log::error!("cannot save {}: {e}", self.args.config_path.display());
        }
    }

    fn poll(&mut self) {
        if let Backend::Live(e) = &self.backend {
            self.status = e.status();
        }
        if self.status.channels > 0 && self.selected >= self.status.channels {
            self.selected = self.status.channels - 1;
        }
        // stop confirmation times out
        if let Some(t) = self.stop_armed_at {
            if t.elapsed() > Duration::from_secs(2) {
                self.stop_armed_at = None;
            }
        }
        // selected-channel write rate from byte deltas (engine gives per-disk rates only)
        let bytes = self.status.channel_bytes.get(self.selected).copied().unwrap_or(0);
        let now = Instant::now();
        match self.sel_rate_sample {
            Some((b0, t0, ch)) if ch as usize == self.selected && self.recording() => {
                let dt = now.duration_since(t0).as_secs_f64();
                if dt >= 1.0 {
                    self.sel_rate_bps = ((bytes.saturating_sub(b0)) as f64 / dt) as u64;
                    self.sel_rate_sample = Some((bytes, now, ch));
                }
            }
            _ => {
                self.sel_rate_sample = Some((bytes, now, self.selected as u64));
                if !self.recording() {
                    self.sel_rate_bps = 0;
                }
            }
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let (rec, mark) = ctx.input(|i| {
            let rec = i.key_pressed(Key::R) && (i.modifiers.ctrl || i.modifiers.command);
            let mark = !typing && i.key_pressed(Key::Space);
            (rec, mark)
        });
        if rec {
            self.toggle_record();
        }
        if mark {
            self.mark();
        }
    }

    /// Retry with the simulated input after a failed device open.
    pub fn use_sim(&mut self) {
        self.config.input = InputKind::Sim;
        self.backend = open_backend(&self.config);
        if let Backend::Live(e) = &self.backend {
            self.status = e.status();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.keyboard(&ctx);
        if self.dests.poll_measurement(&mut self.config, &self.args) {
            if let Some(e) = self.engine() {
                e.set_destinations(self.config.destinations.clone());
            }
        }

        transport::title_bar(self, ui);
        transport::transport_bar(self, ui);
        transport::mode_bar(self, ui);
        side::side_panel(self, ui);
        grid::central(self, ui);

        // Meters animate; keep drawing.
        ctx.request_repaint_after(Duration::from_millis(16));
    }
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

fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
    let v = &mut style.visuals;
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = PANEL_DARK;
    v.faint_bg_color = CHIP;
    v.override_text_color = Some(TEXT_SOFT);
    v.widgets.noninteractive.bg_stroke.color = BORDER;
    v.widgets.inactive.bg_fill = CHIP;
    v.widgets.inactive.weak_bg_fill = CHIP;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, CHIP_BORDER);
    v.widgets.hovered.bg_fill = CHIP_HOVER;
    v.widgets.hovered.weak_bg_fill = CHIP_HOVER;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, rgb(0x39, 0x40, 0x4a));
    v.widgets.active.bg_fill = TAB_ACTIVE;
    v.widgets.active.weak_bg_fill = TAB_ACTIVE;
    v.selection.bg_fill = alpha(GREEN, 0.35);
    v.selection.stroke = egui::Stroke::new(1.0, GREEN);
    v.widgets.noninteractive.fg_stroke.color = TEXT_SOFT;
    v.widgets.inactive.fg_stroke.color = TEXT_SOFT;
    v.widgets.hovered.fg_stroke.color = TEXT;
    v.widgets.active.fg_stroke.color = TEXT;
    v.window_stroke = egui::Stroke::new(1.0, BORDER);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(11.0, 5.0);
    style.spacing.scroll.bar_width = 10.0;
    style.interaction.selectable_labels = false;
    });
}
