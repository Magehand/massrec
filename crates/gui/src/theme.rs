//! Palette and formatting helpers shared by all panels.

use eframe::egui;
use egui::{Color32, CornerRadius, FontId, Margin, RichText, Stroke};

pub const BG: Color32 = rgb(0x00, 0x00, 0x00);
pub const PANEL: Color32 = rgb(0x0e, 0x10, 0x13);
pub const PANEL_DARK: Color32 = rgb(0x0a, 0x0b, 0x0d);
pub const GRID_BG: Color32 = rgb(0x0c, 0x0d, 0x0f);
pub const BORDER: Color32 = rgb(0x1a, 0x1e, 0x24);
pub const CELL: Color32 = rgb(0x10, 0x12, 0x16);
pub const CELL_BORDER: Color32 = rgb(0x1c, 0x20, 0x27);
pub const CELL_SEL: Color32 = rgb(0x1e, 0x24, 0x2c);
pub const SEG_OFF: Color32 = rgb(0x19, 0x1d, 0x23);
pub const CHIP: Color32 = rgb(0x17, 0x1a, 0x1f);
pub const CHIP_BORDER: Color32 = rgb(0x27, 0x2c, 0x34);
pub const CHIP_HOVER: Color32 = rgb(0x1f, 0x23, 0x2a);
pub const TAB_BG: Color32 = rgb(0x14, 0x17, 0x1b);
pub const TAB_BORDER: Color32 = rgb(0x23, 0x28, 0x2f);
pub const TAB_ACTIVE: Color32 = rgb(0x2a, 0x30, 0x3a);

pub const TEXT: Color32 = rgb(0xe8, 0xea, 0xec);
pub const TEXT_BRIGHT: Color32 = rgb(0xf2, 0xf4, 0xf6);
pub const TEXT_MID: Color32 = rgb(0xcf, 0xd4, 0xd9);
pub const TEXT_SOFT: Color32 = rgb(0xb9, 0xbf, 0xc6);
pub const TEXT_DIM: Color32 = rgb(0x8d, 0x94, 0x9c);
pub const TEXT_FAINT: Color32 = rgb(0x6e, 0x75, 0x7e);
pub const LABEL: Color32 = rgb(0x5d, 0x64, 0x6d);
pub const LABEL_FAINT: Color32 = rgb(0x4b, 0x51, 0x58);

pub const GREEN: Color32 = rgb(0x4a, 0x9d, 0x5f);
pub const GREEN_METER: Color32 = rgb(0x3f, 0x7d, 0x52);
pub const GREEN_TEXT: Color32 = rgb(0x7f, 0xd4, 0x9a);
pub const GREEN_BG: Color32 = rgb(0x14, 0x20, 0x1a);
pub const GREEN_BORDER: Color32 = rgb(0x25, 0x40, 0x30);
pub const REC_BG: Color32 = rgb(0x1c, 0x2f, 0x22);
pub const REC_BORDER: Color32 = rgb(0x2f, 0x5a, 0x3c);

pub const HOT: Color32 = rgb(0xb8, 0xa1, 0x3a);
pub const HOT_TEXT: Color32 = rgb(0xd8, 0xc4, 0x6a);
pub const HOT_BG: Color32 = rgb(0x22, 0x1c, 0x10);
pub const HOT_BORDER: Color32 = rgb(0x40, 0x34, 0x18);

pub const CLIP: Color32 = rgb(0xc0, 0x39, 0x2b);
pub const RED_DOT: Color32 = rgb(0xe0, 0x4a, 0x30);
pub const FAULT_TEXT: Color32 = rgb(0xe8, 0x83, 0x6a);
pub const FAULT_BG: Color32 = rgb(0x2a, 0x15, 0x12);
pub const FAULT_BORDER: Color32 = rgb(0x4a, 0x22, 0x1c);
pub const FAULT_BTN_BORDER: Color32 = rgb(0x6b, 0x32, 0x27);
pub const FAULT_SOFT: Color32 = rgb(0xed, 0xb9, 0xa8);
pub const FAULT_BTN_TEXT: Color32 = rgb(0xf0, 0xc4, 0xb4);
pub const STOP_BG: Color32 = rgb(0x3a, 0x15, 0x12);
pub const STOP_BORDER: Color32 = rgb(0x7a, 0x2b, 0x22);
pub const STOP_TEXT: Color32 = rgb(0xf0, 0xa5, 0x95);

pub const DROP: Color32 = rgb(0x7a, 0x4b, 0xd0);
pub const DROP_TEXT: Color32 = rgb(0xb9, 0x8a, 0xe8);
pub const DROP_BG: Color32 = rgb(0x1e, 0x14, 0x30);
pub const DROP_BORDER: Color32 = rgb(0x38, 0x26, 0x57);

pub const SILENT: Color32 = rgb(0x2b, 0x30, 0x38);

pub const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub fn alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
pub fn sans(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Small uppercase section label in the mockup's letter-spaced style.
pub fn label(text: &str) -> RichText {
    RichText::new(spaced(text)).font(sans(9.0)).color(LABEL)
}

/// Approximate letter-spacing by inserting thin spaces between characters.
pub fn spaced(text: &str) -> String {
    let mut s = String::with_capacity(text.len() * 2);
    for (i, ch) in text.chars().enumerate() {
        if i > 0 {
            s.push('\u{2009}');
        }
        s.push(ch);
    }
    s
}

pub fn chip_frame(fill: Color32, border: Color32) -> egui::Frame {
    egui::Frame::NONE.fill(fill).stroke(Stroke::new(1.0, border)).corner_radius(CornerRadius::same(3)).inner_margin(Margin::symmetric(9, 5))
}

/// A mockup-style flat button; returns the response.
pub fn chip_button(ui: &mut egui::Ui, text: RichText, fill: Color32, border: Color32, min_size: egui::Vec2) -> egui::Response {
    let b = egui::Button::new(text).fill(fill).stroke(Stroke::new(1.0, border)).corner_radius(CornerRadius::same(3)).min_size(min_size);
    ui.add(b)
}

pub fn tab_button(ui: &mut egui::Ui, active: bool, text: &str) -> egui::Response {
    let t = RichText::new(text).font(sans(11.0)).color(if active { TEXT } else { TEXT_FAINT });
    let b = egui::Button::new(t)
        .fill(if active { TAB_ACTIVE } else { Color32::TRANSPARENT })
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(2))
        .min_size(egui::vec2(0.0, 22.0));
    ui.add(b)
}

/// Vertical hairline for separating header groups.
pub fn vsep(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, height), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0, BORDER);
}

// ---- formatting ------------------------------------------------------------

/// `1,842 GB` style with thousands separators, unit chosen by magnitude.
pub fn bytes_human(b: u64) -> String {
    let f = b as f64;
    if f >= 1e12 {
        format!("{:.2} TB", f / 1e12)
    } else if f >= 1e9 {
        format!("{} GB", thousands((f / 1e9).round() as u64))
    } else if f >= 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.1} kB", f / 1e3)
    } else {
        format!("{b} B")
    }
}

/// Gigabytes with separators and no unit, for the "GB FREE" readout.
pub fn gb_plain(b: u64) -> String {
    thousands((b as f64 / 1e9).round() as u64)
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn rate_human(bps: u64) -> String {
    let f = bps as f64;
    if f >= 1e9 {
        format!("{:.2} GB/s", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB/s", f / 1e6)
    } else {
        format!("{:.0} kB/s", f / 1e3)
    }
}

pub fn db(v: f32) -> String {
    if v <= -100.0 {
        "−∞".to_string()
    } else {
        format!("{v:.1}")
    }
}

pub fn hms(seconds: f64) -> String {
    masscap_engine::events::format_hms_short(seconds)
}

/// Linear meter position 0..1 for a dBFS value over a 60 dB window.
pub fn meter_pos(dbfs: f32) -> f32 {
    ((dbfs + 60.0) / 60.0).clamp(0.0, 1.0)
}

pub fn state_color(state: masscap_engine::meter::ChannelState, hold_dbfs: f32) -> Color32 {
    use masscap_engine::meter::ChannelState::*;
    match state {
        Drop => DROP,
        Silent => SILENT,
        Clip => CLIP,
        Hot => HOT,
        Ok => {
            if hold_dbfs > -0.5 {
                CLIP
            } else if hold_dbfs > -3.0 {
                HOT
            } else {
                GREEN_METER
            }
        }
    }
}

pub fn state_label(state: masscap_engine::meter::ChannelState, recording: bool) -> &'static str {
    use masscap_engine::meter::ChannelState::*;
    match state {
        Drop => "DISK DROP",
        Silent => "NO SIGNAL",
        Clip => "CLIPPING",
        Hot | Ok => {
            if recording {
                "RECORDING"
            } else {
                "STANDBY"
            }
        }
    }
}

/// (bg, border, fg) for the state pill.
pub fn state_pill(state: masscap_engine::meter::ChannelState) -> (Color32, Color32, Color32) {
    use masscap_engine::meter::ChannelState::*;
    match state {
        Ok | Hot => (GREEN_BG, GREEN_BORDER, GREEN_TEXT),
        Clip => (FAULT_BG, FAULT_BORDER, FAULT_TEXT),
        Silent => (HOT_BG, HOT_BORDER, HOT_TEXT),
        Drop => (DROP_BG, DROP_BORDER, DROP_TEXT),
    }
}
