//! `masscap.toml`: everything the operator configures, human-editable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    /// Built-in signal generator; runs anywhere.
    Sim,
    /// System audio device: Core Audio on macOS (e.g. Dante Virtual Soundcard), WASAPI on Windows.
    System,
    /// ASIO driver (Windows, `asio` feature).
    Asio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Destination {
    /// Root folder on the disk, e.g. `D:\MASSCAP`.
    pub path: PathBuf,
    /// Volume identity so a re-lettered drive keeps its measurement.
    #[serde(default)]
    pub volume_serial: String,
    /// Sustained sequential write rate measured by the disk test, bytes/s.
    #[serde(default)]
    pub throughput_bps: Option<u64>,
    #[serde(default)]
    pub measured_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thresholds {
    /// Consecutive full-scale samples that count as a clip.
    pub clip_consecutive: u32,
    pub hot_dbfs: f32,
    pub silence_dbfs: f32,
    pub silence_seconds: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { clip_consecutive: 3, hot_dbfs: -3.0, silence_dbfs: -80.0, silence_seconds: 10.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// Session name; becomes the folder under every destination root.
    pub session: String,
    pub input: InputKind,
    /// Input device name for `system`/`asio`; empty = default device.
    #[serde(default, alias = "asio_device")]
    pub device: String,
    /// Number of channels to record (1..=channels of the device).
    pub channels: usize,
    /// Simulated input sample rate (real devices dictate their own).
    pub sim_sample_rate: u32,
    pub preroll_seconds: f64,
    /// Fraction of installed RAM to use for the cache when `cache_bytes` is unset.
    pub cache_fraction: f64,
    #[serde(default)]
    pub cache_bytes: Option<u64>,
    /// Disk write block per channel. Rounded to a multiple of 12288 (lcm of 3 and 4096).
    pub block_bytes: usize,
    /// Fraction of measured throughput a disk may be planned to carry.
    pub throughput_safety: f64,
    /// Bytes written by the disk throughput test.
    pub disk_test_bytes: u64,
    #[serde(default)]
    pub destinations: Vec<Destination>,
    #[serde(default)]
    pub thresholds: Thresholds,
    /// 1-based channel number → name override.
    #[serde(default)]
    pub channel_names: BTreeMap<u32, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            session: "Session".into(),
            input: InputKind::Sim,
            device: String::new(),
            channels: 300,
            sim_sample_rate: 96_000,
            preroll_seconds: 10.0,
            cache_fraction: 0.6,
            cache_bytes: None,
            block_bytes: 4 * 1024 * 1024,
            throughput_safety: 0.5,
            disk_test_bytes: 10_000_000_000,
            destinations: vec![],
            thresholds: Thresholds::default(),
            channel_names: BTreeMap::new(),
        }
    }
}

pub const BLOCK_QUANTUM: usize = 12_288; // lcm(3, 4096)

impl Config {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        toml::from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let s = toml::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, s)
    }

    /// Block size rounded down to a whole number of frames and sectors.
    pub fn effective_block_bytes(&self) -> usize {
        (self.block_bytes / BLOCK_QUANTUM).max(1) * BLOCK_QUANTUM
    }

    /// Total cache bytes: explicit override or a fraction of installed RAM.
    pub fn effective_cache_bytes(&self) -> u64 {
        if let Some(b) = self.cache_bytes {
            return b;
        }
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let total = sys.total_memory();
        ((total as f64) * self.cache_fraction.clamp(0.05, 0.9)) as u64
    }

    pub fn channel_name(&self, index0: usize, device_name: Option<&str>) -> String {
        if let Some(n) = self.channel_names.get(&(index0 as u32 + 1)) {
            return n.clone();
        }
        match device_name {
            Some(n) if !n.trim().is_empty() => n.to_string(),
            _ => format!("Ch{:03}", index0 + 1),
        }
    }
}

/// Make a channel name safe for a file name.
pub fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_toml() {
        let mut c = Config::default();
        c.destinations.push(Destination { path: "D:\\MASSCAP".into(), volume_serial: "ABCD".into(), throughput_bps: Some(500_000_000), measured_at: None });
        c.channel_names.insert(1, "Kick In".into());
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.destinations[0].throughput_bps, Some(500_000_000));
        assert_eq!(back.channel_name(0, Some("Dante 1")), "Kick In");
        assert_eq!(back.channel_name(1, Some("Dante 2")), "Dante 2");
        assert_eq!(back.channel_name(2, None), "Ch003");
    }

    #[test]
    fn block_is_quantized() {
        let c = Config::default();
        assert_eq!(c.effective_block_bytes() % 4096, 0);
        assert_eq!(c.effective_block_bytes() % 3, 0);
        assert!(c.effective_block_bytes() <= 4 * 1024 * 1024);
    }

    #[test]
    fn sanitize_names() {
        assert_eq!(sanitize("Kick In"), "Kick_In");
        assert_eq!(sanitize("OH L/R  (x)"), "OH_L_R_x");
    }
}
