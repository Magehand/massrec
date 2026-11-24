//! Per-channel single-producer / single-consumer byte rings.
//!
//! The producer is the audio input callback; the consumer is the writer thread
//! owning the channel's disk. Samples are stored as packed 24-bit little-endian,
//! so the writer can copy bytes straight to disk with no conversion.
//!
//! Overflow policy: when the ring is full the producer discards the incoming
//! frames, counts them, and re-inserts the same number of zero frames as soon
//! as space frees up. Every channel file therefore keeps exact sample alignment
//! with every other channel; a stall shows up as silence plus a drop event.
//!
//! Idle policy: while not recording the producer keeps only the newest
//! `preroll` bytes by advancing the consumer cursor itself. This is only sound
//! while no consumer is reading, which the engine guarantees by protocol.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::BYTES_PER_SAMPLE;

struct Inner {
    buf: Box<[UnsafeCell<u8>]>,
    cap: usize,
    /// Total bytes ever appended (including inserted silence).
    produced: AtomicU64,
    /// Total bytes ever consumed or trimmed.
    consumed: AtomicU64,
    /// Total frames lost to overflow (zero frames were inserted for them).
    dropped_frames: AtomicU64,
}

unsafe impl Sync for Inner {}
unsafe impl Send for Inner {}

impl Inner {
    #[inline]
    fn len(&self) -> usize {
        (self.produced.load(Ordering::Acquire) - self.consumed.load(Ordering::Acquire)) as usize
    }
    #[inline]
    fn ptr(&self) -> *mut u8 {
        self.buf.as_ptr() as *mut u8
    }
    /// Copy `src` into the ring at logical position `pos`. Caller guarantees room.
    #[inline]
    unsafe fn write_at(&self, pos: u64, src: &[u8]) {
        let start = (pos % self.cap as u64) as usize;
        let first = src.len().min(self.cap - start);
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr().add(start), first);
            if first < src.len() {
                std::ptr::copy_nonoverlapping(src.as_ptr().add(first), self.ptr(), src.len() - first);
            }
        }
    }
    #[inline]
    unsafe fn zero_at(&self, pos: u64, n: usize) {
        let start = (pos % self.cap as u64) as usize;
        let first = n.min(self.cap - start);
        unsafe {
            std::ptr::write_bytes(self.ptr().add(start), 0, first);
            if first < n {
                std::ptr::write_bytes(self.ptr(), 0, n - first);
            }
        }
    }
    #[inline]
    unsafe fn read_at(&self, pos: u64, dst: &mut [u8]) {
        let start = (pos % self.cap as u64) as usize;
        let first = dst.len().min(self.cap - start);
        unsafe {
            std::ptr::copy_nonoverlapping(self.ptr().add(start), dst.as_mut_ptr(), first);
            if first < dst.len() {
                std::ptr::copy_nonoverlapping(self.ptr(), dst.as_mut_ptr().add(first), dst.len() - first);
            }
        }
    }
}

/// Producer half of a ring. Exactly one per channel, owned by the input thread.
pub struct Producer {
    inner: Arc<Inner>,
    pending_silence: usize,
}

/// Consumer half of a ring. Exactly one per channel, owned by a writer thread.
pub struct Consumer {
    inner: Arc<Inner>,
}

/// Read-only view of a ring's counters, safe to hold from any thread.
#[derive(Clone)]
pub struct RingStats {
    inner: Arc<Inner>,
}

impl RingStats {
    pub fn capacity(&self) -> usize {
        self.inner.cap
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }
    pub fn fill(&self) -> f32 {
        self.inner.len() as f32 / self.inner.cap as f32
    }
    pub fn dropped_frames(&self) -> u64 {
        self.inner.dropped_frames.load(Ordering::Relaxed)
    }
    pub fn produced_frames(&self) -> u64 {
        self.inner.produced.load(Ordering::Acquire) / BYTES_PER_SAMPLE as u64
    }
    pub fn consumed_frames(&self) -> u64 {
        self.inner.consumed.load(Ordering::Acquire) / BYTES_PER_SAMPLE as u64
    }
}

/// Create a ring holding `capacity_bytes` (rounded down to whole frames).
pub fn ring(capacity_bytes: usize) -> (Producer, Consumer) {
    let cap = (capacity_bytes / BYTES_PER_SAMPLE).max(16) * BYTES_PER_SAMPLE;
    // vec![0; n] is calloc-backed: pages are mapped lazily, so a 26 GB cache
    // starts instantly and only touches RAM as audio arrives.
    let v: Vec<UnsafeCell<u8>> = (0..cap).map(|_| UnsafeCell::new(0)).collect();
    let inner = Arc::new(Inner {
        buf: v.into_boxed_slice(),
        cap,
        produced: AtomicU64::new(0),
        consumed: AtomicU64::new(0),
        dropped_frames: AtomicU64::new(0),
    });
    (Producer { inner: inner.clone(), pending_silence: 0 }, Consumer { inner })
}

impl Producer {
    pub fn stats(&self) -> RingStats {
        RingStats { inner: self.inner.clone() }
    }
    pub fn capacity(&self) -> usize {
        self.inner.cap
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }
    pub fn free(&self) -> usize {
        self.inner.cap - self.inner.len()
    }
    /// Total frames appended so far, including inserted silence.
    pub fn produced_frames(&self) -> u64 {
        self.inner.produced.load(Ordering::Acquire) / BYTES_PER_SAMPLE as u64
    }
    pub fn dropped_frames(&self) -> u64 {
        self.inner.dropped_frames.load(Ordering::Relaxed)
    }
    /// Frames currently owed as silence (dropped but not yet re-inserted).
    pub fn pending_silence_frames(&self) -> usize {
        self.pending_silence / BYTES_PER_SAMPLE
    }

    /// Flush owed silence into the ring as far as space allows.
    fn flush_silence(&mut self) {
        if self.pending_silence == 0 {
            return;
        }
        let n = self.pending_silence.min(self.free());
        if n > 0 {
            let pos = self.inner.produced.load(Ordering::Relaxed);
            unsafe { self.inner.zero_at(pos, n) };
            self.inner.produced.store(pos + n as u64, Ordering::Release);
            self.pending_silence -= n;
        }
    }

    /// Append raw packed bytes. Returns the number of frames dropped by this call.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> usize {
        debug_assert_eq!(bytes.len() % BYTES_PER_SAMPLE, 0);
        self.flush_silence();
        if self.pending_silence > 0 || self.free() < bytes.len() {
            // Still owing silence, or no room: this block is lost.
            self.pending_silence += bytes.len();
            let frames = bytes.len() / BYTES_PER_SAMPLE;
            self.inner.dropped_frames.fetch_add(frames as u64, Ordering::Relaxed);
            return frames;
        }
        let pos = self.inner.produced.load(Ordering::Relaxed);
        unsafe { self.inner.write_at(pos, bytes) };
        self.inner.produced.store(pos + bytes.len() as u64, Ordering::Release);
        0
    }

    /// Append samples given as 24-bit values in the low bits of `i32`.
    /// Uses a small stack buffer; never allocates.
    pub fn push_i24(&mut self, samples: &[i32]) -> usize {
        let mut dropped = 0;
        let mut tmp = [0u8; 3 * 512];
        for chunk in samples.chunks(512) {
            let n = chunk.len() * 3;
            for (i, s) in chunk.iter().enumerate() {
                let b = s.to_le_bytes();
                tmp[i * 3] = b[0];
                tmp[i * 3 + 1] = b[1];
                tmp[i * 3 + 2] = b[2];
            }
            dropped += self.push_bytes(&tmp[..n]);
        }
        dropped
    }

    /// Idle-mode trim: keep at most `keep_bytes` of the newest data.
    /// Only legal while no consumer is reading this ring.
    pub fn trim_to(&mut self, keep_bytes: usize) {
        let keep = (keep_bytes / BYTES_PER_SAMPLE) * BYTES_PER_SAMPLE;
        let len = self.inner.len();
        if len > keep {
            let consumed = self.inner.consumed.load(Ordering::Relaxed);
            self.inner.consumed.store(consumed + (len - keep) as u64, Ordering::Release);
        }
    }
}

impl Consumer {
    pub fn capacity(&self) -> usize {
        self.inner.cap
    }
    /// Bytes available to read.
    pub fn available(&self) -> usize {
        self.inner.len()
    }
    /// Total frames consumed so far (absolute stream position of the read cursor).
    pub fn consumed_frames(&self) -> u64 {
        self.inner.consumed.load(Ordering::Acquire) / BYTES_PER_SAMPLE as u64
    }
    /// Total frames produced so far (absolute stream position of the write cursor).
    pub fn produced_frames(&self) -> u64 {
        self.inner.produced.load(Ordering::Acquire) / BYTES_PER_SAMPLE as u64
    }
    pub fn dropped_frames(&self) -> u64 {
        self.inner.dropped_frames.load(Ordering::Relaxed)
    }
    /// Copy up to `dst.len()` bytes out (whole frames only). Returns bytes read.
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let avail = self.available();
        let n = (dst.len().min(avail) / BYTES_PER_SAMPLE) * BYTES_PER_SAMPLE;
        if n == 0 {
            return 0;
        }
        let pos = self.inner.consumed.load(Ordering::Relaxed);
        unsafe { self.inner.read_at(pos, &mut dst[..n]) };
        self.inner.consumed.store(pos + n as u64, Ordering::Release);
        n
    }
    /// Move the read cursor back to absolute stream frame `frame`, so data that
    /// was consumed past it is delivered again. Refused (returns false) when
    /// `frame` is ahead of the cursor, or when the producer may already be reusing
    /// that region: only rewinds that keep the ring at most half full are allowed,
    /// which leaves a producer that checked `free()` before the rewind plenty of
    /// room for the chunk it is writing.
    pub fn rewind_to_frame(&mut self, frame: u64) -> bool {
        let target = frame * BYTES_PER_SAMPLE as u64;
        let pos = self.inner.consumed.load(Ordering::Relaxed);
        if target > pos {
            return false;
        }
        let produced = self.inner.produced.load(Ordering::Acquire);
        if produced - target > (self.inner.cap / 2) as u64 {
            return false;
        }
        self.inner.consumed.store(target, Ordering::Release);
        true
    }
    /// Discard up to `bytes` (whole frames). Returns bytes skipped.
    pub fn skip(&mut self, bytes: usize) -> usize {
        let n = (bytes.min(self.available()) / BYTES_PER_SAMPLE) * BYTES_PER_SAMPLE;
        let pos = self.inner.consumed.load(Ordering::Relaxed);
        self.inner.consumed.store(pos + n as u64, Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize, v: u8) -> Vec<u8> {
        vec![v; n * 3]
    }

    #[test]
    fn roundtrip_and_wrap() {
        let (mut p, mut c) = ring(30); // 10 frames
        assert_eq!(p.push_bytes(&frames(7, 1)), 0);
        let mut out = vec![0u8; 15];
        assert_eq!(c.read(&mut out), 15);
        assert!(out.iter().all(|&b| b == 1));
        // now 2 frames left; push 8 more, wrapping the buffer end
        assert_eq!(p.push_bytes(&frames(8, 2)), 0);
        assert_eq!(c.available(), 30);
        let mut out = vec![0u8; 30];
        assert_eq!(c.read(&mut out), 30);
        assert!(out[..6].iter().all(|&b| b == 1));
        assert!(out[6..].iter().all(|&b| b == 2));
    }

    #[test]
    fn overflow_inserts_equal_silence_and_counts_drop() {
        let (mut p, mut c) = ring(48); // 16 frames
        assert_eq!(p.push_bytes(&frames(16, 9)), 0);
        // full: these 4 frames are dropped
        assert_eq!(p.push_bytes(&frames(4, 7)), 4);
        assert_eq!(p.dropped_frames(), 4);
        assert_eq!(p.pending_silence_frames(), 4);
        // consumer frees 6 frames
        let mut out = vec![0u8; 18];
        assert_eq!(c.read(&mut out), 18);
        // next push: silence (4) flushed first, then 2 new frames fit
        assert_eq!(p.push_bytes(&frames(2, 5)), 0);
        assert_eq!(p.pending_silence_frames(), 0);
        let mut out = vec![0u8; 48];
        assert_eq!(c.read(&mut out), 48);
        assert!(out[..30].iter().all(|&b| b == 9)); // 10 frames remaining old
        assert!(out[30..42].iter().all(|&b| b == 0)); // 4 frames silence
        assert!(out[42..].iter().all(|&b| b == 5)); // 2 new frames
        // stream position accounts for everything, including the silence
        assert_eq!(c.consumed_frames(), 22);
        assert_eq!(p.produced_frames(), 22);
    }

    #[test]
    fn trim_keeps_newest() {
        let (mut p, mut c) = ring(300);
        for i in 0..20u8 {
            p.push_bytes(&frames(1, i));
        }
        p.trim_to(5 * 3);
        assert_eq!(c.available(), 15);
        let mut out = vec![0u8; 15];
        c.read(&mut out);
        assert_eq!(out[0], 15);
        assert_eq!(out[14], 19);
    }

    #[test]
    fn rewind_redelivers_consumed_data() {
        let (mut p, mut c) = ring(300); // 100 frames
        for i in 0..20u8 {
            p.push_bytes(&frames(1, i));
        }
        let mut out = vec![0u8; 15 * 3];
        assert_eq!(c.read(&mut out), 45);
        assert!(!c.rewind_to_frame(16), "cannot rewind forward");
        assert!(c.rewind_to_frame(10));
        assert_eq!(c.consumed_frames(), 10);
        assert_eq!(c.available(), 30);
        let mut out = vec![0u8; 3];
        c.read(&mut out);
        assert_eq!(out[0], 10);
    }

    #[test]
    fn rewind_refuses_past_safe_window() {
        let (mut p, mut c) = ring(300); // 100 frames
        p.push_bytes(&frames(60, 1));
        let mut out = vec![0u8; 60 * 3];
        c.read(&mut out);
        assert!(!c.rewind_to_frame(0), "60 frames back exceeds half the ring");
        assert!(c.rewind_to_frame(10));
    }

    #[test]
    fn push_i24_packs_little_endian() {
        let (mut p, mut c) = ring(300);
        p.push_i24(&[0x123456, -1, 0x7FFFFF]);
        let mut out = [0u8; 9];
        c.read(&mut out);
        assert_eq!(&out, &[0x56, 0x34, 0x12, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]);
    }
}
