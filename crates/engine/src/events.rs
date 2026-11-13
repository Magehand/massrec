//! Engine events: everything the operator should know about, in one stream.
//! The engine broadcasts these to subscribers and appends them to `events.txt`
//! in every take folder.

use chrono::{DateTime, Local};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Severity {
    Info,
    Warning,
    Fault,
}

#[derive(Clone, Debug, Serialize)]
pub enum StopReason {
    Operator,
    DiskFailed { disk: String },
    Shutdown,
}

#[derive(Clone, Debug, Serialize)]
pub enum EventKind {
    TakeStarted { take: String, channels: usize, preroll_seconds: f64 },
    TakeStopped { take: String, reason: StopReason },
    Marker { index: usize, label: String },
    /// Frames lost for a channel because its ring overflowed; silence was written instead.
    Drop { channel: usize, frames: u64 },
    Clip { channel: usize },
    SignalLost { channel: usize },
    SignalReturned { channel: usize },
    DiskFailed { disk: String, error: String },
    DiskAssigned { disk: String, first_channel: usize, count: usize },
    CacheFill { fraction: f32 },
    Info(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub time: DateTime<Local>,
    /// Seconds into the current take, if one is running.
    pub elapsed: Option<f64>,
    pub kind: EventKind,
}

impl Event {
    pub fn now(elapsed: Option<f64>, kind: EventKind) -> Self {
        Self { time: Local::now(), elapsed, kind }
    }

    pub fn severity(&self) -> Severity {
        use EventKind::*;
        match &self.kind {
            Drop { .. } | DiskFailed { .. } => Severity::Fault,
            Clip { .. } | SignalLost { .. } | CacheFill { .. } => Severity::Warning,
            TakeStopped { reason: StopReason::DiskFailed { .. }, .. } => Severity::Fault,
            _ => Severity::Info,
        }
    }

    /// Human line, e.g. for the event log panel.
    pub fn message(&self) -> String {
        use EventKind::*;
        match &self.kind {
            TakeStarted { take, channels, preroll_seconds } => {
                format!("{take} record start · {channels} ch · pre-roll {preroll_seconds:.1} s")
            }
            TakeStopped { take, reason } => match reason {
                StopReason::Operator => format!("{take} stopped"),
                StopReason::DiskFailed { disk } => format!("{take} stopped: disk {disk} failed"),
                StopReason::Shutdown => format!("{take} stopped: shutdown"),
            },
            Marker { index, label } if label.is_empty() => format!("Marker {index} placed"),
            Marker { index, label } => format!("Marker {index}: {label}"),
            Drop { channel, frames } => format!("Ch {:03} write underrun ({frames} smp)", channel + 1),
            Clip { channel } => format!("Ch {:03} clipping", channel + 1),
            SignalLost { channel } => format!("Ch {:03} signal lost", channel + 1),
            SignalReturned { channel } => format!("Ch {:03} signal returned", channel + 1),
            DiskFailed { disk, error } => format!("Disk {disk} failed: {error}"),
            DiskAssigned { disk, first_channel, count } => {
                format!("{disk} ← ch {:03}-{:03}", first_channel + 1, first_channel + count)
            }
            CacheFill { fraction } => format!("RAM cache {:.0}% full", fraction * 100.0),
            Info(s) => s.clone(),
        }
    }

    /// Tab-separated line for `events.txt`.
    pub fn log_line(&self) -> String {
        let el = self.elapsed.map(format_hms).unwrap_or_else(|| "-".into());
        format!("{}\t{}\t{:?}\t{}", self.time.format("%Y-%m-%d %H:%M:%S%.3f"), el, self.severity(), self.message())
    }
}

/// `HH:MM:SS.mmm`
pub fn format_hms(seconds: f64) -> String {
    let ms = (seconds * 1000.0).round() as u64;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// `HH:MM:SS`
pub fn format_hms_short(seconds: f64) -> String {
    let s = seconds.max(0.0).floor() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}
