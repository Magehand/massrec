//! Right panel: selected channel, tall meter, file info, event log.

use eframe::egui;
use egui::{Align, CornerRadius, Layout, Rect, RichText, Sense, Stroke};
use masscap_engine::events::Severity;
use masscap_engine::meter::ChannelMeter;

use crate::app::App;
use crate::theme::*;

pub fn side_panel(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::right("side")
        .exact_size(300.0)
        .resizable(false)
        .frame(egui::Frame::NONE.fill(PANEL))
        .show_separator_line(false)
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().vline(r.left(), r.y_range(), Stroke::new(1.0, BORDER));
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                selected_header(app, ui);
                hline(ui);
                meter_block(app, ui);
                hline(ui);
                writing_to(app, ui);
                hline(ui);
                event_log(app, ui);
            });
        });
}

fn hline(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0, BORDER);
}

fn section(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin::same(14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn meter_of(app: &App) -> ChannelMeter {
    app.status.meters.get(app.selected).copied().unwrap_or_default()
}

fn selected_header(app: &App, ui: &mut egui::Ui) {
    let m = meter_of(app);
    let name = app.status.channel_names.get(app.selected).cloned().unwrap_or_else(|| "—".into());
    section(ui, |ui| {
        ui.label(label("SELECTED CHANNEL"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{:03}", app.selected + 1)).font(mono(30.0)).strong().color(TEXT));
            ui.label(RichText::new(name).font(sans(13.0)).color(rgb(0x9a, 0xa1, 0xa9)));
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (bg, bd, fg) = state_pill(m.state);
            chip_frame(bg, bd).show(ui, |ui| {
                ui.label(RichText::new(spaced(state_label(m.state, app.recording()))).font(mono(10.0)).strong().color(fg));
            });
            chip_frame(CHIP, CHIP_BORDER).show(ui, |ui| {
                ui.label(RichText::new(&app.status.device_name).font(mono(10.0)).color(TEXT_DIM));
            });
        });
    });
}

fn meter_block(app: &App, ui: &mut egui::Ui) {
    let m = meter_of(app);
    section(ui, |ui| {
        ui.horizontal(|ui| {
            // 26-segment tall meter with scale
            let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 150.0), Sense::hover());
            let p = ui.painter();
            let segs = 26;
            let seg_h = (rect.height() - (segs as f32 - 1.0) * 2.0) / segs as f32;
            let lit = (meter_pos(m.peak_dbfs) * segs as f32).round() as usize;
            let hold = (meter_pos(m.hold_dbfs) * segs as f32).round() as usize;
            for i in 0..segs {
                let y1 = rect.max.y - i as f32 * (seg_h + 2.0);
                let sr = Rect::from_min_max(egui::pos2(rect.min.x, y1 - seg_h), egui::pos2(rect.max.x, y1));
                let on = i < lit || (i + 1 == hold && hold > 0);
                let c = if !on {
                    SEG_OFF
                } else if i >= segs - 2 {
                    CLIP
                } else if i >= segs - 6 {
                    HOT
                } else {
                    GREEN_METER
                };
                p.rect_filled(sr, CornerRadius::same(1), c);
            }
            let (scale, _) = ui.allocate_exact_size(egui::vec2(22.0, 150.0), Sense::hover());
            for (i, l) in ["0", "-6", "-18", "-36", "-∞"].iter().enumerate() {
                let y = scale.min.y + 4.0 + (scale.height() - 8.0) * i as f32 / 4.0;
                ui.painter().text(egui::pos2(scale.min.x, y), egui::Align2::LEFT_CENTER, l, mono(8.0), LABEL_FAINT);
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(label("PEAK"));
                let pc = if m.hold_dbfs > -0.5 { FAULT_TEXT } else if m.hold_dbfs > -3.0 { HOT_TEXT } else { TEXT };
                ui.label(RichText::new(format!("{} dB", db(m.hold_dbfs))).font(mono(20.0)).strong().color(pc));
                ui.add_space(6.0);
                ui.label(label("RMS"));
                ui.label(RichText::new(format!("{} dB", db(m.rms_dbfs))).font(mono(15.0)).color(TEXT_SOFT));
                ui.add_space(6.0);
                ui.label(label("CLIPS THIS TAKE"));
                ui.label(RichText::new(m.clips_total.to_string()).font(mono(15.0)).color(if m.clips_total > 0 { FAULT_TEXT } else { TEXT_DIM }));
                if m.dropped_frames_total > 0 {
                    ui.add_space(6.0);
                    ui.label(label("DROPPED SAMPLES"));
                    ui.label(RichText::new(thousands(m.dropped_frames_total)).font(mono(15.0)).color(DROP_TEXT));
                }
            });
        });
    });
}

fn writing_to(app: &App, ui: &mut egui::Ui) {
    let path = app.status.channel_paths.get(app.selected).cloned().unwrap_or_default();
    let bytes = app.status.channel_bytes.get(app.selected).copied().unwrap_or(0);
    section(ui, |ui| {
        ui.label(label("WRITING TO"));
        ui.add_space(4.0);
        if path.is_empty() {
            let disk = app.status.disks.iter().find(|d| d.first_channel.map(|f| app.selected >= f && app.selected < f + d.count).unwrap_or(false));
            let txt = match disk {
                Some(d) => format!("{} (planned)", d.path),
                None => "no destination planned".to_string(),
            };
            ui.label(RichText::new(txt).font(mono(11.0)).color(LABEL));
        } else {
            ui.add(egui::Label::new(RichText::new(path).font(mono(11.0)).color(TEXT_DIM)).wrap());
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(bytes_human(bytes)).font(mono(10.0)).color(LABEL));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(rate_human(app.sel_rate_bps)).font(mono(10.0)).color(LABEL));
            });
        });
    });
}

fn event_log(app: &App, ui: &mut egui::Ui) {
    section(ui, |ui| {
        ui.label(label("EVENT LOG"));
        ui.add_space(2.0);
        for ev in app.status.recent_events.iter().rev().take(60) {
            let col = match ev.severity() {
                Severity::Fault => {
                    if matches!(ev.kind, masscap_engine::events::EventKind::Drop { .. }) {
                        DROP_TEXT
                    } else {
                        FAULT_TEXT
                    }
                }
                Severity::Warning => HOT_TEXT,
                Severity::Info => {
                    if matches!(ev.kind, masscap_engine::events::EventKind::TakeStarted { .. }) {
                        GREEN_TEXT
                    } else {
                        TEXT_DIM
                    }
                }
            };
            let t = match ev.elapsed {
                Some(e) => hms(e),
                None => ev.time.format("%H:%M:%S").to_string(),
            };
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(t).font(mono(10.0)).color(LABEL_FAINT));
                ui.add(egui::Label::new(RichText::new(ev.message()).font(mono(10.0)).color(col)).wrap());
            });
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(r, 0, TAB_BG);
        }
        if app.status.recent_events.is_empty() {
            ui.label(RichText::new("no events yet").font(mono(10.0)).color(LABEL_FAINT));
        }
    });
}
