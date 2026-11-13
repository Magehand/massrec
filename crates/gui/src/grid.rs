//! Central area: fault bar, the channel grid (meter wall / status matrix), legend.
//! Everything is drawn with painter calls so 300 channels stay cheap.

use eframe::egui;
use egui::{Align, CornerRadius, Layout, Rect, RichText, Sense, Stroke, StrokeKind};
use masscap_engine::meter::ChannelState;

use crate::app::{App, Backend, GridView, Mode};
use crate::dests;
use crate::theme::*;

fn is_fault(s: ChannelState) -> bool {
    !matches!(s, ChannelState::Ok | ChannelState::Hot)
}

pub fn central(app: &mut App, ui: &mut egui::Ui) {
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(GRID_BG)).show(ui, |ui| {
        if let Backend::Failed(err) = &app.backend {
            let err = err.clone();
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.label(RichText::new("INPUT NOT AVAILABLE").font(sans(11.0)).strong().color(FAULT_TEXT));
                ui.add_space(8.0);
                ui.label(RichText::new(err).font(sans(13.0)).color(FAULT_SOFT));
                ui.add_space(16.0);
                if chip_button(ui, RichText::new("Use simulated input").font(sans(12.0)).color(GREEN_TEXT), REC_BG, REC_BORDER, egui::vec2(180.0, 34.0)).clicked() {
                    app.use_sim();
                }
            });
            return;
        }

        // Bottom-up: legend footer first so the scroll area gets the rest.
        egui::Panel::bottom("legend")
            .frame(egui::Frame::NONE.fill(PANEL_DARK).inner_margin(egui::Margin::symmetric(14, 0)))
            .exact_size(28.0)
            .show_separator_line(false)
            .show(ui, |ui| {
                let r = ui.max_rect();
                ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, BORDER));
                ui.horizontal_centered(|ui| {
                    for (col, name) in [(GREEN_METER, "SIGNAL OK"), (HOT, "HOT"), (CLIP, "CLIP"), (SILENT, "NO SIGNAL"), (DROP, "DISK DROP")] {
                        let (sw, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(sw, 0, col);
                        ui.label(RichText::new(name).font(mono(10.0)).color(TEXT_FAINT));
                        ui.add_space(8.0);
                    }
                    let shown = app.status.meters.iter().filter(|m| !app.faults_only || is_fault(m.state)).count();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("{shown} of {} channels shown", app.status.channels)).font(mono(10.0)).color(LABEL_FAINT));
                    });
                });
            });

        if app.mode == Mode::Advanced && app.dests.open {
            egui::Panel::bottom("dests")
                .frame(egui::Frame::NONE.fill(PANEL).inner_margin(egui::Margin::symmetric(14, 10)))
                .show_separator_line(false)
                .show(ui, |ui| {
                    let r = ui.max_rect();
                    ui.painter().hline(r.x_range(), r.top() - 10.0, Stroke::new(1.0, BORDER));
                    dests::show(app, ui);
                });
        }

        fault_bar(app, ui);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin::same(12)).show(ui, |ui| match app.view_effective() {
                GridView::MeterWall => meter_wall(app, ui),
                GridView::StatusMatrix => status_matrix(app, ui),
            });
        });
    });
}

impl App {
    /// Simple mode always shows the meter wall.
    pub fn view_effective(&self) -> GridView {
        if self.mode == Mode::Simple {
            GridView::MeterWall
        } else {
            self.view
        }
    }
}

fn fault_bar(app: &mut App, ui: &mut egui::Ui) {
    let s = &app.status;
    let mut parts = Vec::new();
    let drops: Vec<usize> = s.meters.iter().enumerate().filter(|(_, m)| m.state == ChannelState::Drop).map(|(i, _)| i + 1).collect();
    let clips: Vec<usize> = s.meters.iter().enumerate().filter(|(_, m)| m.state == ChannelState::Clip).map(|(i, _)| i + 1).collect();
    if !drops.is_empty() {
        parts.push(format!("Ch {} disk write dropped", list(&drops)));
    }
    if !clips.is_empty() {
        parts.push(format!("Ch {} clipping", list(&clips)));
    }
    if s.silent_count > 0 {
        parts.push(format!("{} channels with no signal", s.silent_count));
    }
    for d in s.disks.iter().filter(|d| d.failed) {
        parts.push(format!("Disk {} failed", d.path));
    }
    if parts.is_empty() || app.faults_only {
        return;
    }
    egui::Frame::NONE.fill(FAULT_BG).inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            chip_frame(FAULT_BORDER, FAULT_BORDER).show(ui, |ui| {
                ui.label(RichText::new(spaced("ATTENTION")).font(mono(10.0)).strong().color(FAULT_TEXT));
            });
            ui.label(RichText::new(parts.join(" · ")).font(sans(12.0)).color(FAULT_SOFT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if chip_button(ui, RichText::new("Show only these").font(sans(11.0)).color(FAULT_BTN_TEXT), FAULT_BORDER, FAULT_BTN_BORDER, egui::vec2(0.0, 24.0)).clicked() {
                    app.mode = Mode::Advanced;
                    app.faults_only = true;
                }
            });
        });
        let r = ui.max_rect();
        ui.painter().hline(r.x_range(), r.bottom() + 8.0, Stroke::new(1.0, FAULT_BORDER));
    });
}

fn list(v: &[usize]) -> String {
    let shown: Vec<String> = v.iter().take(6).map(|n| n.to_string()).collect();
    if v.len() > 6 {
        format!("{} +{}", shown.join(", "), v.len() - 6)
    } else {
        shown.join(", ")
    }
}

/// Lay out `n` cells of `cell` size in a wrapping grid and call `draw` per cell.
fn cell_grid(ui: &mut egui::Ui, n: usize, cell: egui::Vec2, gap: f32, mut draw: impl FnMut(&egui::Painter, usize, Rect, bool)) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let avail = ui.available_width();
    let cols = ((avail + gap) / (cell.x + gap)).floor().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    let cell_w = (avail - gap * (cols as f32 - 1.0)) / cols as f32; // stretch to fill the row
    let total = egui::vec2(avail, rows as f32 * (cell.y + gap) - gap);
    let (area, _) = ui.allocate_exact_size(total, Sense::hover());
    let clip = ui.clip_rect();
    let mut clicked = None;
    let painter = ui.painter().clone();
    for i in 0..n {
        let (r, c) = (i / cols, i % cols);
        let min = area.min + egui::vec2(c as f32 * (cell_w + gap), r as f32 * (cell.y + gap));
        let rect = Rect::from_min_size(min, egui::vec2(cell_w, cell.y));
        if !clip.intersects(rect) {
            continue; // off-screen rows cost nothing
        }
        let resp = ui.interact(rect, ui.id().with(("cell", i)), Sense::click());
        if resp.clicked() {
            clicked = Some(i);
        }
        draw(&painter, i, rect, resp.hovered());
        if resp.hovered() {
            resp.on_hover_ui_at_pointer(|_| {});
        }
    }
    clicked
}

fn meter_wall(app: &mut App, ui: &mut egui::Ui) {
    let idx: Vec<usize> = (0..app.status.channels).filter(|&i| !app.faults_only || is_fault(app.status.meters[i].state)).collect();
    let selected = app.selected;
    let meters = &app.status.meters;
    let clicked = cell_grid(ui, idx.len(), egui::vec2(34.0, 66.0), 3.0, |p, k, rect, hovered| {
        let ch = idx[k];
        let m = &meters[ch];
        let is_sel = ch == selected;
        let bg = if is_sel { CELL_SEL } else if hovered { rgb(0x15, 0x18, 0x1d) } else { CELL };
        p.rect(rect, CornerRadius::same(2), bg, Stroke::new(1.0, if is_sel { GREEN } else { CELL_BORDER }), StrokeKind::Inside);
        // 14 segments, bottom-up
        let segs = 14;
        let inner = Rect::from_min_max(rect.min + egui::vec2(3.0, 4.0), egui::pos2(rect.max.x - 3.0, rect.min.y + 48.0));
        let lit = (meter_pos(m.peak_dbfs) * segs as f32).round() as usize;
        let hold = (meter_pos(m.hold_dbfs) * segs as f32).round() as usize;
        let col = state_color(m.state, m.hold_dbfs);
        let seg_h = (inner.height() - (segs as f32 - 1.0)) / segs as f32;
        for i in 0..segs {
            let y1 = inner.max.y - i as f32 * (seg_h + 1.0);
            let sr = Rect::from_min_max(egui::pos2(inner.min.x, y1 - seg_h), egui::pos2(inner.max.x, y1));
            let on = i < lit || (i + 1 == hold && hold > 0);
            let c = if !on {
                SEG_OFF
            } else if i >= segs - 1 {
                CLIP
            } else if i >= segs - 3 {
                HOT
            } else {
                col
            };
            p.rect_filled(sr, CornerRadius::same(1), c);
        }
        if matches!(m.state, ChannelState::Silent | ChannelState::Drop) {
            // colour the bottom segment so the state reads even with no level
            let sr = Rect::from_min_max(egui::pos2(inner.min.x, inner.max.y - seg_h), inner.max);
            p.rect_filled(sr, CornerRadius::same(1), col);
        }
        let num_col = if is_sel { TEXT } else { LABEL };
        p.text(egui::pos2(rect.center().x, rect.max.y - 8.0), egui::Align2::CENTER_CENTER, format!("{}", ch + 1), mono(9.0), num_col);
    });
    if let Some(k) = clicked {
        app.selected = idx[k];
    }
}

fn status_matrix(app: &mut App, ui: &mut egui::Ui) {
    let idx: Vec<usize> = (0..app.status.channels).filter(|&i| !app.faults_only || is_fault(app.status.meters[i].state)).collect();
    let selected = app.selected;
    let meters = &app.status.meters;
    let clicked = cell_grid(ui, idx.len(), egui::vec2(78.0, 26.0), 3.0, |p, k, rect, hovered| {
        let ch = idx[k];
        let m = &meters[ch];
        let is_sel = ch == selected;
        let bg = if is_sel { CELL_SEL } else if hovered { rgb(0x15, 0x18, 0x1d) } else { CELL };
        p.rect(rect, CornerRadius::same(2), bg, Stroke::new(1.0, if is_sel { GREEN } else { BORDER }), StrokeKind::Inside);
        let col = state_color(m.state, m.hold_dbfs);
        let w = rect.width() * meter_pos(m.peak_dbfs);
        let fill = Rect::from_min_size(rect.min + egui::vec2(1.0, 1.0), egui::vec2((w - 2.0).max(0.0), rect.height() - 2.0));
        p.rect_filled(fill, CornerRadius::same(1), alpha(col, 0.4));
        let tc = if m.state == ChannelState::Silent { LABEL } else { TEXT };
        p.text(egui::pos2(rect.left() + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("{}", ch + 1), mono(10.0), tc);
        p.circle_filled(egui::pos2(rect.right() - 8.0, rect.center().y), 2.5, if m.state == ChannelState::Silent { SILENT.gamma_multiply(1.6) } else { col });
    });
    if let Some(k) = clicked {
        app.selected = idx[k];
    }
}
