//! Real audio input through `cpal`: the system host (Core Audio on macOS, so
//! Dante Virtual Soundcard works; WASAPI on Windows) or, with the `asio`
//! feature on Windows, an ASIO driver.
//!
//! Notes for bring-up on hardware:
//! * The sample rate is whatever the driver reports. Dante devices take their
//!   rate from the network (Dante Controller / Focusrite Notifier), so we never
//!   call set-sample-rate.
//! * The Dante ASIO drivers are documented as 24-bit but the ASIO sample type
//!   they report (Int24 packed vs Int32) is not; both are handled here.
//! * cpal allows one ASIO driver per process. A second card needs a second
//!   process or a raw COM binding; see docs/ARCHITECTURE.md.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Data, SampleFormat, StreamConfig};

use super::{Block, Callback, InputDevice, InputError, InputFormat};

/// Which cpal host to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    /// The platform default: Core Audio, WASAPI, ALSA.
    System,
    /// ASIO (Windows, `asio` feature).
    Asio,
}

fn host(kind: HostKind) -> Result<cpal::Host, InputError> {
    match kind {
        HostKind::System => Ok(cpal::default_host()),
        HostKind::Asio => {
            #[cfg(all(windows, feature = "asio"))]
            {
                cpal::host_from_id(cpal::HostId::Asio).map_err(|e| InputError::NoDevice(e.to_string()))
            }
            #[cfg(not(all(windows, feature = "asio")))]
            {
                Err(InputError::NoDevice("this build has no ASIO support (Windows + `asio` feature required)".into()))
            }
        }
    }
}

pub fn list_devices(kind: HostKind) -> Vec<String> {
    let Ok(host) = host(kind) else { return vec![] };
    host.input_devices().map(|it| it.filter_map(|d| device_name(&d)).collect()).unwrap_or_default()
}

fn device_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|desc| desc.name().to_string())
}

pub struct CpalInput {
    device: cpal::Device,
    format: InputFormat,
    sample_format: SampleFormat,
    stream: Option<cpal::Stream>,
    started: Arc<AtomicBool>,
}

impl CpalInput {
    /// Open `name` (or the default input device when empty) on the given host.
    pub fn open(kind: HostKind, name: &str) -> Result<Self, InputError> {
        let host = host(kind)?;
        let device = if name.trim().is_empty() {
            host.default_input_device().ok_or_else(|| InputError::NoDevice("no default input device".into()))?
        } else {
            host.input_devices()
                .map_err(|e| InputError::Device(e.to_string()))?
                .find(|d| device_name(d).as_deref() == Some(name))
                .ok_or_else(|| InputError::NoDevice(format!("input device '{name}' not found")))?
        };
        let supported = device.default_input_config().map_err(|e| InputError::Device(e.to_string()))?;
        // Ask for every channel the driver has.
        let max_channels = device
            .supported_input_configs()
            .map(|it| it.map(|c| c.channels()).max().unwrap_or(supported.channels()))
            .unwrap_or(supported.channels());
        let sample_format = supported.sample_format();
        match sample_format {
            SampleFormat::I32 | SampleFormat::I24 | SampleFormat::I16 | SampleFormat::F32 => {}
            other => return Err(InputError::Format(format!("{other:?}"))),
        }
        let format = InputFormat {
            device_name: device_name(&device).unwrap_or_else(|| "Audio device".into()),
            sample_rate: supported.sample_rate(),
            channels: max_channels as usize,
            channel_names: (0..max_channels).map(|i| format!("In {}", i + 1)).collect(),
        };
        Ok(Self { device, format, sample_format, stream: None, started: Arc::new(AtomicBool::new(false)) })
    }
}

impl InputDevice for CpalInput {
    fn format(&self) -> InputFormat {
        self.format.clone()
    }

    fn start(&mut self, mut cb: Callback) -> Result<(), InputError> {
        if self.stream.is_some() {
            return Err(InputError::AlreadyStarted);
        }
        let channels = self.format.channels;
        let config = StreamConfig { channels: channels as u16, sample_rate: self.format.sample_rate, buffer_size: cpal::BufferSize::Default };
        let sample_format = self.sample_format;
        // Planar scratch buffer; grown once if the driver's buffer is larger than expected.
        let mut planar: Vec<i32> = vec![0; channels * 4096];
        let data_cb = move |data: &Data, _info: &cpal::InputCallbackInfo| {
            let frames = data.len() / channels;
            if planar.len() < channels * frames {
                planar.resize(channels * frames, 0);
            }
            deinterleave(data, sample_format, channels, frames, &mut planar);
            cb(&Block::new(channels, frames, &planar[..channels * frames]));
        };
        let err_cb = |e: cpal::Error| log::error!("input stream error: {e}");
        let stream = self
            .device
            .build_input_stream_raw(config, sample_format, data_cb, err_cb, None)
            .map_err(|e| InputError::Device(e.to_string()))?;
        stream.play().map_err(|e| InputError::Device(e.to_string()))?;
        self.started.store(true, Ordering::SeqCst);
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(s) = self.stream.take() {
            let _ = s.pause();
        }
        self.started.store(false, Ordering::SeqCst);
    }
}

/// Interleaved driver samples → planar 24-bit-in-i32.
fn deinterleave(data: &Data, fmt: SampleFormat, channels: usize, frames: usize, out: &mut [i32]) {
    match fmt {
        SampleFormat::I32 => {
            let s = data.as_slice::<i32>().unwrap();
            for f in 0..frames {
                for c in 0..channels {
                    out[c * frames + f] = s[f * channels + c] >> 8;
                }
            }
        }
        SampleFormat::I24 => {
            let s = data.as_slice::<cpal::I24>().unwrap();
            for f in 0..frames {
                for c in 0..channels {
                    out[c * frames + f] = s[f * channels + c].inner();
                }
            }
        }
        SampleFormat::I16 => {
            let s = data.as_slice::<i16>().unwrap();
            for f in 0..frames {
                for c in 0..channels {
                    out[c * frames + f] = (s[f * channels + c] as i32) << 8;
                }
            }
        }
        SampleFormat::F32 => {
            let s = data.as_slice::<f32>().unwrap();
            for f in 0..frames {
                for c in 0..channels {
                    let v = (s[f * channels + c].clamp(-1.0, 1.0) * 8_388_607.0) as i32;
                    out[c * frames + f] = v;
                }
            }
        }
        _ => out.fill(0),
    }
}
