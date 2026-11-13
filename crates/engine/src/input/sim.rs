//! Simulated multichannel input: tones with slow level movement, plus scripted
//! faults (silent, clipping, hot channels) so the engine and UI can be exercised
//! without hardware. Runs in real time by default; tests can run it flat out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{Block, Callback, InputDevice, InputError, InputFormat};
use crate::FULL_SCALE;

#[derive(Clone, Debug)]
pub struct SimScenario {
    pub silent: Vec<usize>,
    pub clipping: Vec<usize>,
    pub hot: Vec<usize>,
}

impl SimScenario {
    /// The fault pattern from the design mockup, scaled to `channels`.
    pub fn default_for(channels: usize) -> Self {
        let mut s = SimScenario { silent: vec![], clipping: vec![], hot: vec![] };
        for i in 0..channels {
            let n = i + 1;
            let seed = (n as u64 * 2654435761 % 1000) as f64 / 1000.0;
            if n == 47 || n == 112 || n > 264 {
                s.silent.push(i);
            } else if n == 18 || n == 164 {
                s.clipping.push(i);
            } else if seed > 0.93 {
                s.hot.push(i);
            }
        }
        s
    }
    pub fn clean() -> Self {
        SimScenario { silent: vec![], clipping: vec![], hot: vec![] }
    }
}

#[derive(Clone, Debug)]
pub struct SimConfig {
    pub sample_rate: u32,
    pub channels: usize,
    pub block_frames: usize,
    /// Pace delivery to wall-clock time. `false` runs as fast as possible.
    pub realtime: bool,
    /// Stop after this many frames (non-realtime tests).
    pub stop_after_frames: Option<u64>,
    pub scenario: SimScenario,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self { sample_rate: 96_000, channels: 300, block_frames: 512, realtime: true, stop_after_frames: None, scenario: SimScenario::default_for(300) }
    }
}

pub struct SimInput {
    cfg: SimConfig,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SimInput {
    pub fn new(cfg: SimConfig) -> Self {
        Self { cfg, running: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

struct Gen {
    phase: Vec<f64>,
    freq: Vec<f64>,
    base: Vec<f64>,
    kind: Vec<u8>, // 0 ok, 1 silent, 2 clip, 3 hot
    t: f64,
}

impl Gen {
    fn new(cfg: &SimConfig) -> Self {
        let n = cfg.channels;
        let mut kind = vec![0u8; n];
        for &c in &cfg.scenario.silent {
            if c < n {
                kind[c] = 1;
            }
        }
        for &c in &cfg.scenario.clipping {
            if c < n {
                kind[c] = 2;
            }
        }
        for &c in &cfg.scenario.hot {
            if c < n {
                kind[c] = 3;
            }
        }
        let freq = (0..n).map(|i| 80.0 * 1.02f64.powi(i as i32 % 120) + 0.37 * i as f64).collect();
        let base = (0..n).map(|i| 0.10 + ((i * 7919) % 1000) as f64 / 1000.0 * 0.25).collect();
        Self { phase: vec![0.0; n], freq, base, kind, t: 0.0 }
    }

    /// Fill `out` (planar) with `frames` frames.
    fn fill(&mut self, out: &mut [i32], frames: usize, rate: f64) {
        let dt = 1.0 / rate;
        let n = self.phase.len();
        for c in 0..n {
            let dst = &mut out[c * frames..(c + 1) * frames];
            match self.kind[c] {
                1 => dst.fill(0),
                k => {
                    // slow amplitude movement so meters look alive
                    let mut ph = self.phase[c];
                    let w = 2.0 * std::f64::consts::PI * self.freq[c] * dt;
                    let mod_ = 0.55 + 0.45 * (self.t * (0.31 + c as f64 * 0.013)).sin();
                    let amp = match k {
                        2 => 1.35,
                        3 => 0.78,
                        _ => self.base[c] * mod_,
                    };
                    for s in dst.iter_mut() {
                        ph += w;
                        if ph > std::f64::consts::TAU {
                            ph -= std::f64::consts::TAU;
                        }
                        let v = (ph.sin() * amp).clamp(-1.0, 1.0);
                        *s = (v * (FULL_SCALE - 1) as f64) as i32;
                    }
                    self.phase[c] = ph;
                }
            }
        }
        self.t += frames as f64 * dt;
    }
}

impl InputDevice for SimInput {
    fn format(&self) -> InputFormat {
        InputFormat {
            device_name: "Simulated input".into(),
            sample_rate: self.cfg.sample_rate,
            channels: self.cfg.channels,
            channel_names: (0..self.cfg.channels).map(|i| format!("Sim {:03}", i + 1)).collect(),
        }
    }

    fn start(&mut self, mut cb: Callback) -> Result<(), InputError> {
        if self.thread.is_some() {
            return Err(InputError::AlreadyStarted);
        }
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let cfg = self.cfg.clone();
        self.thread = Some(std::thread::Builder::new().name("sim-input".into()).spawn(move || {
            let mut gen = Gen::new(&cfg);
            let frames = cfg.block_frames;
            let mut buf = vec![0i32; cfg.channels * frames];
            let block_dur = Duration::from_secs_f64(frames as f64 / cfg.sample_rate as f64);
            let start = Instant::now();
            let mut produced: u64 = 0;
            let mut next = start;
            while running.load(Ordering::Relaxed) {
                if let Some(limit) = cfg.stop_after_frames {
                    if produced >= limit {
                        break;
                    }
                }
                gen.fill(&mut buf, frames, cfg.sample_rate as f64);
                cb(&Block::new(cfg.channels, frames, &buf));
                produced += frames as u64;
                if cfg.realtime {
                    next += block_dur;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else if now - next > Duration::from_secs(2) {
                        // fell far behind (debugger, suspended laptop): resync rather than burst
                        next = now;
                    }
                }
            }
            running.store(false, Ordering::SeqCst);
        }).map_err(|e| InputError::Device(e.to_string()))?);
        Ok(())
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for SimInput {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn delivers_planar_blocks_with_faults() {
        let cfg = SimConfig {
            sample_rate: 48_000,
            channels: 8,
            block_frames: 256,
            realtime: false,
            stop_after_frames: Some(4096),
            scenario: SimScenario { silent: vec![1], clipping: vec![2], hot: vec![] },
        };
        let mut dev = SimInput::new(cfg);
        let seen = Arc::new(Mutex::new((0u64, 0i32, 0i32, false)));
        let s2 = seen.clone();
        dev.start(Box::new(move |b: &Block| {
            let mut g = s2.lock().unwrap();
            g.0 += b.frames as u64;
            g.1 = g.1.max(b.channel(0).iter().map(|s| s.abs()).max().unwrap());
            g.2 = g.2.max(b.channel(2).iter().map(|s| s.abs()).max().unwrap());
            g.3 |= b.channel(1).iter().any(|&s| s != 0);
        }))
        .unwrap();
        while dev.running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let g = seen.lock().unwrap();
        assert_eq!(g.0, 4096);
        assert!(g.1 > 0 && g.1 < FULL_SCALE / 2, "normal channel peak {}", g.1);
        assert_eq!(g.2, FULL_SCALE - 1, "clipping channel hits full scale");
        assert!(!g.3, "silent channel stays zero");
    }
}
