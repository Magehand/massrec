//! Title bar, transport bar (record / mark / clocks / health) and the mode bar.

use eframe::egui;
use egui::{Align, Color32, CornerRadius, Layout, RichText, Stroke, StrokeKind};

use crate::app::{App, GridView, Mode};
use crate::theme::*;

pub fn title_bar(app: &App, ui: &mut egui::Ui) {
    egui::Panel::top("title")
        .frame(egui::Frame::NONE.fill(PANEL_DARK).inner_margin(egui::Margin::symmetric(10, 0)))
        .exact_size(30.0)
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(spaced("MASSCAP 300")).font(mono(10.0)).strong().color(LABEL));
                ui.label(RichText::new("|").font(sans(11.0)).color(rgb(0x3f, 0x45, 0x4d)));
                ui.label(RichText::new(format!("SESSION / {}", app.status.session)).font(mono(11.0)).color(TEXT_FAINT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(&app.status.device_name).font(mono(10.0)).color(LABEL_FAINT));
                });
            });
            let r = ui.max_rect();
            ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0, BORDER));
        });
}

pub fn transport_bar(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::top("transport")
        .frame(egui::Frame::NONE.fill(PANEL))
        .show_separator_line(false)
        .show(ui, |ui| {
            let bottom = ui.max_rect().bottom();
            ui.horizontal(|ui| {
                // --- record / mark -------------------------------------------------
                egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        record_button(app, ui);
                        let mark = chip_button(
                            ui,
                            RichText::new("◆\nMARK").font(sans(10.0)).color(TEXT_SOFT),
                            CHIP,
                            CHIP_BORDER,
                            egui::vec2(62.0, 46.0),
                        );
                        if mark.clicked() {
                            app.mark();
                        }
                    });
                });
                vsep(ui, 70.0);

                // --- clocks ------------------------------------------------------
                egui::Frame::NONE.inner_margin(egui::Margin::symmetric(22, 12)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(label("ELAPSED"));
                            let c = if app.recording() { TEXT_BRIGHT } else { TEXT_DIM };
                            ui.label(RichText::new(hms(app.status.elapsed_seconds)).font(mono(34.0)).strong().color(c));
                        });
                        ui.add_space(10.0);
                        vsep(ui, 46.0);
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.label(label("TIME REMAINING ON DISK"));
                            ui.horizontal(|ui| {
                                let rem = app.status.remaining_seconds.filter(|r| r.is_finite()).map(hms).unwrap_or_else(|| "--:--:--".into());
                                ui.label(RichText::new(rem).font(mono(34.0)).strong().color(TEXT_MID));
                                ui.label(RichText::new(format!("{} GB FREE", gb_plain(app.status.free_bytes_total))).font(mono(11.0)).color(LABEL));
                            });
                            // thin fill bar: fraction of the smallest disk's capacity still free is not
                            // exposed, so show cache headroom instead of inventing a number: remaining
                            // hours capped at 12 h → bar length.
                            let frac = app.status.remaining_seconds.map(|r| (r / (12.0 * 3600.0)).clamp(0.0, 1.0) as f32).unwrap_or(0.0);
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(210.0, 3.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, CornerRadius::same(2), BORDER);
                            let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * frac, rect.height()));
                            let col = if frac < 0.08 { CLIP } else if frac < 0.25 { HOT } else { GREEN };
                            ui.painter().rect_filled(fill, CornerRadius::same(2), col);
                        });
                        ui.add_space(10.0);
                        vsep(ui, 46.0);
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            let (lbl, name) = match &app.status.take_name {
                                Some(t) => ("CURRENT TAKE", t.clone()),
                                None => ("NEXT TAKE", chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string()),
                            };
                            ui.label(label(lbl));
                            ui.label(RichText::new(name).font(mono(20.0)).strong().color(TEXT_MID));
                            let n = app.status.channels;
                            ui.label(RichText::new(format!("{n} of {n} channels · {} disks", app.status.disks.iter().filter(|d| !d.failed).count())).font(mono(10.0)).color(LABEL));
                        });
                    });
                });

                // --- health ------------------------------------------------------
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| {
                        ui.vertical(|ui| {
                            health_pill(app, ui);
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let s = &app.status;
                                counter(ui, "CLIPS", &s.clips_total.to_string(), if s.clips_total > 0 { FAULT_TEXT } else { TEXT_DIM });
                                counter(ui, "DROPS", &s.drops_total.to_string(), if s.drops_total > 0 { DROP_TEXT } else { TEXT_DIM });
                                counter(ui, "SILENT", &s.silent_count.to_string(), if s.silent_count > 0 { HOT_TEXT } else { TEXT_DIM });
                                counter(ui, "FORMAT", &format!("{}k · 24b", s.sample_rate / 1000), TEXT_DIM);
                                let cf = s.cache_fill * 100.0;
                                counter(ui, "RAM CACHE", &format!("{cf:.0}%"), if cf > 80.0 { FAULT_TEXT } else if cf > 50.0 { HOT_TEXT } else { TEXT_DIM });
                            });
                        });
                    });
                    vsep(ui, 70.0);
                });
            });
            ui.painter().hline(ui.max_rect().x_range(), bottom - 1.0, Stroke::new(1.0, BORDER));
        });
}

fn record_button(app: &mut App, ui: &mut egui::Ui) {
    let rec = app.recording();
    let armed = app.stop_armed_at.is_some();
    let (bg, border, fg) = if rec { (STOP_BG, STOP_BORDER, STOP_TEXT) } else { (REC_BG, REC_BORDER, GREEN_TEXT) };
    let text = if rec {
        if armed {
            "PRESS AGAIN"
        } else {
            "STOP"
        }
    } else {
        "RECORD"
    };
    let enabled = app.engine().is_some() && (rec || app.status.not_ready.is_none());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(130.0, 46.0), egui::Sense::click());
    let p = ui.painter();
    let bg = if !enabled { alpha(bg, 0.5) } else if resp.hovered() { bg.gamma_multiply(1.3) } else { bg };
    p.rect(rect, CornerRadius::same(3), bg, Stroke::new(1.0, border), StrokeKind::Inside);
    let dot = egui::pos2(rect.left() + 22.0, rect.center().y);
    let dot_col = if rec {
        let t = ui.input(|i| i.time);
        let a = 0.2 + 0.8 * (0.5 + 0.5 * ((t * std::f64::consts::TAU / 1.4).sin() as f32));
        alpha(RED_DOT, a)
    } else {
        GREEN
    };
    p.circle_filled(dot, 6.0, dot_col);
    p.text(egui::pos2(rect.left() + 38.0, rect.center().y), egui::Align2::LEFT_CENTER, spaced(text), sans(12.0), if enabled { fg } else { alpha(fg, 0.5) });
    if resp.clicked() && enabled {
        app.toggle_record();
    }
    if let Some(nr) = &app.status.not_ready {
        if !rec {
            resp.on_hover_text(format!("Cannot record: {nr}"));
        }
    } else if rec && !armed {
        resp.on_hover_text("Click twice (or Ctrl+R twice) within 2 s to stop");
    }
}

fn health_pill(app: &App, ui: &mut egui::Ui) {
    let s = &app.status;
    let faults = s.drops_total > 0 || s.clips_total > 0 || s.silent_count > 0 || s.disks.iter().any(|d| d.failed);
    let (bg, border, fg, dot, text) = if let Some(nr) = &s.not_ready {
        (HOT_BG, HOT_BORDER, HOT_TEXT, HOT, format!("NOT READY · {}", nr.to_uppercase()))
    } else if faults {
        (FAULT_BG, FAULT_BORDER, FAULT_TEXT, RED_DOT, "FAULTS PRESENT".to_string())
    } else {
        (GREEN_BG, GREEN_BORDER, GREEN_TEXT, GREEN, "ALL CHANNELS NOMINAL".to_string())
    };
    let text = if text.len() > 48 { format!("{}…", &text[..47]) } else { text };
    chip_frame(bg, border).show(ui, |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 3.5, dot);
            ui.label(RichText::new(spaced(&text)).font(sans(10.0)).strong().color(fg));
        });
    });
}

fn counter(ui: &mut egui::Ui, name: &str, value: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.label(label(name));
        ui.label(RichText::new(value).font(mono(14.0)).strong().color(color));
    });
    ui.add_space(6.0);
}

pub fn mode_bar(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::top("modes")
        .frame(egui::Frame::NONE.fill(PANEL_DARK).inner_margin(egui::Margin::symmetric(14, 5)))
        .show_separator_line(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                tab_group(ui, |ui| {
                    if tab_button(ui, app.mode == Mode::Simple, "Simple").clicked() {
                        app.mode = Mode::Simple;
                        app.faults_only = false;
                        app.dests.open = false;
                    }
                    if tab_button(ui, app.mode == Mode::Advanced, "Advanced").clicked() {
                        app.mode = Mode::Advanced;
                    }
                });
                if app.mode == Mode::Advanced {
                    ui.add_space(6.0);
                    tab_group(ui, |ui| {
                        if tab_button(ui, app.view == GridView::MeterWall, "Meter wall").clicked() {
                            app.view = GridView::MeterWall;
                        }
                        if tab_button(ui, app.view == GridView::StatusMatrix, "Status matrix").clicked() {
                            app.view = GridView::StatusMatrix;
                        }
                    });
                    let n = app.status.channels;
                    let hint = match app.view {
                        GridView::MeterWall => format!("{n} strips · 14-segment meters"),
                        GridView::StatusMatrix => format!("{n} rows · number, level bar, status dot"),
                    };
                    ui.label(RichText::new(hint).font(mono(10.0)).color(LABEL_FAINT));
                    let (bg, border, fg, text) = if app.faults_only {
                        (FAULT_BORDER, FAULT_BTN_BORDER, FAULT_BTN_TEXT, "Showing faults only ✕")
                    } else {
                        (TAB_BG, TAB_BORDER, TEXT_DIM, "Filter: faults only")
                    };
                    if chip_button(ui, RichText::new(text).font(sans(11.0)).color(fg), bg, border, egui::vec2(0.0, 24.0)).clicked() {
                        app.faults_only = !app.faults_only;
                    }
                    let dests_text = if app.dests.open { "Destinations ▴" } else { "Destinations ▾" };
                    if chip_button(ui, RichText::new(dests_text).font(sans(11.0)).color(TEXT_DIM), TAB_BG, TAB_BORDER, egui::vec2(0.0, 24.0)).clicked() {
                        app.dests.open = !app.dests.open;
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("Ctrl+R record · Space marker").font(mono(10.0)).color(LABEL_FAINT));
                    if let Some(err) = &app.status.last_error {
                        ui.label(RichText::new(err.as_str()).font(sans(11.0)).color(FAULT_TEXT));
                    }
                });
            });
            let r = ui.max_rect();
            ui.painter().hline(r.x_range(), r.bottom() + 5.0, Stroke::new(1.0, BORDER));
        });
}

fn tab_group(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(TAB_BG)
        .stroke(Stroke::new(1.0, TAB_BORDER))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| add(ui));
        });
}
