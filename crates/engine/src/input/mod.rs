//! Input devices. One trait, three sources: a signal generator that runs
//! anywhere, the system audio device via cpal (Core Audio on macOS, which is how
//! Dante Virtual Soundcard appears; WASAPI on Windows), and ASIO behind the
//! `asio` feature on Windows.
//!
//! Sample convention everywhere in the engine: `i32` holding a 24-bit value in
//! the low bits, i.e. full scale is ±2^23. Blocks are planar (one contiguous
//! slice per channel) so the callback can push each channel to its ring with a
//! single copy.

pub mod cpal_input;
pub mod sim;

use crate::config::{Config, InputKind};

#[derive(Clone, Debug)]
pub struct InputFormat {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: usize,
    /// Driver-reported channel names, one per channel (may be empty strings).
    pub channel_names: Vec<String>,
}

/// One callback's worth of audio, planar.
pub struct Block<'a> {
    pub frames: usize,
    pub channels: usize,
    planar: &'a [i32],
}

impl<'a> Block<'a> {
    /// `planar.len()` must equal `channels * frames`.
    pub fn new(channels: usize, frames: usize, planar: &'a [i32]) -> Self {
        debug_assert_eq!(planar.len(), channels * frames);
        Self { frames, channels, planar }
    }
    #[inline]
    pub fn channel(&self, c: usize) -> &[i32] {
        &self.planar[c * self.frames..(c + 1) * self.frames]
    }
}

pub type Callback = Box<dyn FnMut(&Block) + Send + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("no input device available: {0}")]
    NoDevice(String),
    #[error("input device error: {0}")]
    Device(String),
    #[error("unsupported sample format: {0}")]
    Format(String),
    #[error("input already started")]
    AlreadyStarted,
}

pub trait InputDevice: Send {
    fn format(&self) -> InputFormat;
    /// Start delivering blocks to `cb` from the device's own thread.
    fn start(&mut self, cb: Callback) -> Result<(), InputError>;
    fn stop(&mut self);
}

/// Open the input described by the config.
pub fn open(config: &Config) -> Result<Box<dyn InputDevice>, InputError> {
    match config.input {
        InputKind::Sim => Ok(Box::new(sim::SimInput::new(sim::SimConfig {
            sample_rate: config.sim_sample_rate,
            channels: config.channels,
            ..sim::SimConfig::default()
        }))),
        InputKind::System => cpal_input::CpalInput::open(cpal_input::HostKind::System, &config.device).map(|d| Box::new(d) as Box<dyn InputDevice>),
        InputKind::Asio => cpal_input::CpalInput::open(cpal_input::HostKind::Asio, &config.device).map(|d| Box::new(d) as Box<dyn InputDevice>),
    }
}

/// Names of input devices on the given host (empty when unsupported).
pub fn list_devices(kind: cpal_input::HostKind) -> Vec<String> {
    cpal_input::list_devices(kind)
}
