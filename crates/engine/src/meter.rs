//! Per-channel metering shared between the input callback (writer of raw
//! accumulators) and the monitor thread (reader, producing dB values and states).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::config::Thresholds;
use crate::FULL_SCALE;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum ChannelState {
    Ok,
    Hot,
    Clip,
    Silent,
    Drop,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ChannelMeter {
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
    /// Peak-hold value with slow decay, for display.
    pub hold_dbfs: f32,
    pub clips_total: u32,
    pub dropped_frames_total: u64,
    pub state: ChannelState,
}

impl Default for ChannelMeter {
    fn default() -> Self {
        Self { peak_dbfs: -144.0, rms_dbfs: -144.0, hold_dbfs: -144.0, clips_total: 0, dropped_frames_total: 0, state: ChannelState::Silent }
    }
}

struct Accum {
    peak: AtomicU32,
    sumsq: AtomicU64,
    count: AtomicU64,
    clips: AtomicU32,
    last_signal_frame: AtomicU64,
}

/// Lock-free accumulators, one per channel.
pub struct Meters {
    acc: Vec<Accum>,
    silence_level: i32,
    clip_level: i32,
    clip_consecutive: u32,
}

/// Producer-side scratch (clip run lengths). Lives in the callback closure.
pub struct FeedState {
    clip_run: Vec<u32>,
}

impl FeedState {
    pub fn new(channels: usize) -> Self {
        Self { clip_run: vec![0; channels] }
    }
}

pub fn dbfs(v: f64) -> f32 {
    if v <= 0.0 {
        -144.0
    } else {
        (20.0 * (v / FULL_SCALE as f64).log10()) as f32
    }
}

impl Meters {
    pub fn new(channels: usize, th: &Thresholds) -> Self {
        let acc = (0..channels)
            .map(|_| Accum {
                peak: AtomicU32::new(0),
                sumsq: AtomicU64::new(0),
                count: AtomicU64::new(0),
                clips: AtomicU32::new(0),
                last_signal_frame: AtomicU64::new(0),
            })
            .collect();
        let silence_level = (FULL_SCALE as f64 * 10f64.powf(th.silence_dbfs as f64 / 20.0)) as i32;
        Self { acc, silence_level, clip_level: FULL_SCALE - 1, clip_consecutive: th.clip_consecutive.max(1) }
    }

    pub fn channels(&self) -> usize {
        self.acc.len()
    }

    /// Called from the input callback for one channel's block.
    #[inline]
    pub fn feed(&self, ch: usize, samples: &[i32], frame_pos: u64, st: &mut FeedState) {
        let a = &self.acc[ch];
        let mut peak: i32 = 0;
        let mut sumsq: u64 = 0;
        let mut run = st.clip_run[ch];
        let mut clips = 0u32;
        for &s in samples {
            let v = s.abs();
            if v > peak {
                peak = v;
            }
            sumsq += (s as i64 * s as i64) as u64;
            if v >= self.clip_level {
                run += 1;
                if run == self.clip_consecutive {
                    clips += 1;
                }
            } else {
                run = 0;
            }
        }
        st.clip_run[ch] = run;
        a.peak.fetch_max(peak as u32, Ordering::Relaxed);
        a.sumsq.fetch_add(sumsq, Ordering::Relaxed);
        a.count.fetch_add(samples.len() as u64, Ordering::Relaxed);
        if clips > 0 {
            a.clips.fetch_add(clips, Ordering::Relaxed);
        }
        if peak > self.silence_level {
            a.last_signal_frame.store(frame_pos + samples.len() as u64, Ordering::Relaxed);
        }
    }

    /// Monitor-side: drain the interval accumulators for channel `ch`.
    /// Returns (peak, rms) as dBFS and the running clip total.
    pub fn take_interval(&self, ch: usize) -> (f32, f32, u32, u64) {
        let a = &self.acc[ch];
        let peak = a.peak.swap(0, Ordering::Relaxed) as f64;
        let sumsq = a.sumsq.swap(0, Ordering::Relaxed) as f64;
        let count = a.count.swap(0, Ordering::Relaxed) as f64;
        let rms = if count > 0.0 { (sumsq / count).sqrt() } else { 0.0 };
        (dbfs(peak), dbfs(rms), a.clips.load(Ordering::Relaxed), a.last_signal_frame.load(Ordering::Relaxed))
    }

    pub fn reset_clips(&self) {
        for a in &self.acc {
            a.clips.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_needs_consecutive_full_scale() {
        let th = Thresholds { clip_consecutive: 3, ..Default::default() };
        let m = Meters::new(1, &th);
        let mut st = FeedState::new(1);
        let fs = FULL_SCALE - 1;
        m.feed(0, &[fs, 0, fs, fs, 0], 0, &mut st);
        assert_eq!(m.take_interval(0).2, 0);
        m.feed(0, &[fs, fs, fs, fs, 0, fs, fs, fs], 5, &mut st);
        assert_eq!(m.take_interval(0).2, 2);
    }

    #[test]
    fn dbfs_values() {
        assert!((dbfs(FULL_SCALE as f64) - 0.0).abs() < 1e-3);
        assert!((dbfs(FULL_SCALE as f64 / 2.0) + 6.02).abs() < 0.01);
        assert_eq!(dbfs(0.0), -144.0);
    }

    #[test]
    fn silence_tracks_last_signal_frame() {
        let m = Meters::new(1, &Thresholds::default());
        let mut st = FeedState::new(1);
        m.feed(0, &[0, 0, 0, 0], 0, &mut st);
        assert_eq!(m.take_interval(0).3, 0);
        m.feed(0, &[0, 100_000, 0, 0], 4, &mut st);
        assert_eq!(m.take_interval(0).3, 8);
    }
}
