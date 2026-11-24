//! Per-disk writer threads.
//!
//! Each destination disk gets one thread that owns the [`Consumer`] half of every
//! ring assigned to it plus one [`ChannelFile`] per channel. It drains rings in
//! whole aligned blocks, rewrites headers periodically, and on `Stop` drains every
//! channel to exactly the same absolute frame so all files end sample-aligned.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::cache::Consumer;
use crate::io::{align_up, AlignedBuf, DirectFile};
use crate::wav::{build_header, file_len, WavSpec, HEADER_LEN};
use crate::BYTES_PER_SAMPLE;

/// One channel's output file with a block-sized staging buffer.
pub struct ChannelFile {
    pub path: PathBuf,
    file: DirectFile,
    spec: WavSpec,
    buf: AlignedBuf,
    fill: usize,
    /// Bytes at which the current block is flushed. Smaller for the very first
    /// block so that channels flush at different moments instead of all at once.
    target: usize,
    /// Sample bytes flushed to disk.
    data_bytes: u64,
}

impl ChannelFile {
    /// `first_block_bytes` staggers the first flush; it is rounded to the block
    /// quantum so every block boundary stays sector-aligned.
    pub fn create(path: PathBuf, spec: WavSpec, block_bytes: usize, first_block_bytes: usize, hdr: &mut AlignedBuf) -> io::Result<Self> {
        debug_assert_eq!(block_bytes % crate::config::BLOCK_QUANTUM, 0);
        let q = crate::config::BLOCK_QUANTUM;
        let target = (first_block_bytes.clamp(q, block_bytes) / q) * q;
        let mut file = DirectFile::create(&path)?;
        hdr.as_mut_slice()[..HEADER_LEN].copy_from_slice(&build_header(&spec, 0));
        file.write_aligned(0, hdr, HEADER_LEN)?;
        Ok(Self { path, file, spec, buf: AlignedBuf::new(block_bytes), fill: 0, target, data_bytes: 0 })
    }

    /// Bytes of audio committed to disk plus staged in memory.
    pub fn total_bytes(&self) -> u64 {
        self.data_bytes + self.fill as u64
    }

    /// Room left before the staging block is flushed.
    pub fn space(&self) -> usize {
        self.target - self.fill
    }

    /// Pull up to `max_bytes` (whole frames) from the ring; flush when the block fills.
    pub fn fill_from(&mut self, c: &mut Consumer, max_bytes: usize) -> io::Result<usize> {
        let want = self.space().min(max_bytes) / BYTES_PER_SAMPLE * BYTES_PER_SAMPLE;
        if want == 0 {
            return Ok(0);
        }
        let n = c.read(&mut self.buf.as_mut_slice()[self.fill..self.fill + want]);
        self.fill += n;
        if self.fill == self.target {
            self.flush_block()?;
        }
        Ok(n)
    }

    fn flush_block(&mut self) -> io::Result<()> {
        let off = HEADER_LEN as u64 + self.data_bytes;
        self.file.write_aligned(off, &self.buf, self.fill)?;
        self.data_bytes += self.fill as u64;
        self.fill = 0;
        self.target = self.buf.len();
        Ok(())
    }

    /// Drop the newest `bytes` of audio (whole frames): first from the staging
    /// block, then from what is already on disk. `finalize` rewrites the header
    /// from `data_bytes` and truncates the file, which removes the flushed tail.
    pub fn trim(&mut self, bytes: usize) {
        debug_assert_eq!(bytes % BYTES_PER_SAMPLE, 0);
        let from_fill = bytes.min(self.fill);
        self.fill -= from_fill;
        let rest = (bytes - from_fill) as u64;
        // Only reached with an empty staging block, so finalize writes nothing
        // more at the (now unaligned) data_bytes offset.
        debug_assert!(rest <= self.data_bytes);
        self.data_bytes -= rest.min(self.data_bytes);
    }

    pub fn rewrite_header(&mut self, hdr: &mut AlignedBuf) -> io::Result<()> {
        hdr.as_mut_slice()[..HEADER_LEN].copy_from_slice(&build_header(&self.spec, self.data_bytes));
        self.file.write_aligned(0, hdr, HEADER_LEN)
    }

    /// Write the partial last block, the final header, and trim to the exact length.
    pub fn finalize(&mut self, hdr: &mut AlignedBuf) -> io::Result<()> {
        if self.fill > 0 {
            let padded = align_up(self.fill);
            self.buf.as_mut_slice()[self.fill..padded].fill(0);
            let off = HEADER_LEN as u64 + self.data_bytes;
            self.file.write_aligned(off, &self.buf, padded)?;
            self.data_bytes += self.fill as u64;
            self.fill = 0;
        }
        self.rewrite_header(hdr)?;
        self.file.set_len(file_len(self.data_bytes))?;
        self.file.sync()
    }
}

pub struct WriterChannel {
    /// Engine channel index (0-based).
    pub index: usize,
    pub consumer: Consumer,
    pub file: ChannelFile,
}

pub enum WriterCmd {
    /// Drain every channel to `end_frame` (absolute stream frame), finalize, exit.
    Stop { end_frame: u64 },
}

pub enum WriterMsg {
    /// A write failed. The thread keeps running (skipping data) until told to stop.
    Failed { disk: usize, error: String },
    /// Thread finished; consumers are handed back for the next take.
    Done { disk: usize, consumers: Vec<(usize, Consumer)>, ok: bool },
}

pub struct WriterParams {
    pub disk: usize,
    pub channels: Vec<WriterChannel>,
    pub cmd: Receiver<WriterCmd>,
    pub msg: Sender<WriterMsg>,
    /// Per-engine-channel byte counters, updated as blocks land.
    pub channel_bytes: Arc<Vec<AtomicU64>>,
    /// Per-disk byte counter.
    pub disk_bytes: Arc<AtomicU64>,
    pub header_interval: Duration,
    /// Test hook: fail the first write after this many bytes on this disk.
    pub fail_after_bytes: Option<u64>,
}

/// Smallest ring backlog worth copying into the staging block (whole frames).
const PULL_MIN: usize = 64 * 1024 / BYTES_PER_SAMPLE * BYTES_PER_SAMPLE;

pub fn spawn(p: WriterParams) -> JoinHandle<()> {
    std::thread::Builder::new().name(format!("writer-{}", p.disk)).spawn(move || run(p)).expect("spawn writer thread")
}

fn run(mut p: WriterParams) {
    let mut hdr = AlignedBuf::new(HEADER_LEN);
    let mut failed = false;
    let mut last_hdr = Instant::now();
    let disk = p.disk;

    let fail = |failed: &mut bool, msg: &Sender<WriterMsg>, e: &io::Error| {
        if !*failed {
            *failed = true;
            let _ = msg.send(WriterMsg::Failed { disk, error: e.to_string() });
        }
    };

    loop {
        match p.cmd.try_recv() {
            Ok(WriterCmd::Stop { end_frame }) => {
                drain(&mut p, end_frame, &mut failed, &mut hdr);
                break;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }

        let mut did_work = false;
        for ch in p.channels.iter_mut() {
            if failed {
                // Data for this disk is lost anyway; keep the ring moving so the
                // producer does not overflow before the engine stops the take.
                ch.consumer.skip(ch.consumer.available());
                continue;
            }
            // Pull eagerly (anything above PULL_MIN) so the ring only holds real
            // backlog; the staging block absorbs the rest until it flushes.
            while ch.consumer.available() >= ch.file.space().min(PULL_MIN) {
                if let Some(limit) = p.fail_after_bytes {
                    if p.disk_bytes.load(Ordering::Relaxed) >= limit {
                        fail(&mut failed, &p.msg, &io::Error::other("injected disk failure"));
                        break;
                    }
                }
                let space = ch.file.space();
                match ch.file.fill_from(&mut ch.consumer, space) {
                    Ok(n) => {
                        did_work = true;
                        p.disk_bytes.fetch_add(n as u64, Ordering::Relaxed);
                        p.channel_bytes[ch.index].store(ch.file.total_bytes(), Ordering::Relaxed);
                    }
                    Err(e) => {
                        fail(&mut failed, &p.msg, &e);
                        break;
                    }
                }
            }
        }

        if !failed && last_hdr.elapsed() >= p.header_interval {
            last_hdr = Instant::now();
            for ch in p.channels.iter_mut() {
                if let Err(e) = ch.file.rewrite_header(&mut hdr) {
                    fail(&mut failed, &p.msg, &e);
                    break;
                }
            }
        }

        if !did_work {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let consumers = p.channels.into_iter().map(|c| (c.index, c.consumer)).collect();
    let _ = p.msg.send(WriterMsg::Done { disk, consumers, ok: !failed });
}

/// Consume every channel exactly up to `end_frame`, then finalize files.
fn drain(p: &mut WriterParams, end_frame: u64, failed: &mut bool, hdr: &mut AlignedBuf) {
    // Before it saw `Stop`, the run loop may have pulled channels past
    // `end_frame` (by up to a block or more on slow disks).
    rewind_overshoot(p, end_frame, *failed);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mut pending = false;
        for ch in p.channels.iter_mut() {
            let consumed = ch.consumer.consumed_frames();
            if consumed >= end_frame {
                continue;
            }
            let remaining = ((end_frame - consumed) as usize) * BYTES_PER_SAMPLE;
            let avail = ch.consumer.available();
            if avail == 0 {
                pending = true; // producer has not delivered up to end_frame yet
                continue;
            }
            let n = remaining.min(avail);
            if *failed {
                ch.consumer.skip(n);
            } else {
                match ch.file.fill_from(&mut ch.consumer, n) {
                    Ok(n) => {
                        p.disk_bytes.fetch_add(n as u64, Ordering::Relaxed);
                        p.channel_bytes[ch.index].store(ch.file.total_bytes(), Ordering::Relaxed);
                    }
                    Err(e) => {
                        if !*failed {
                            *failed = true;
                            let _ = p.msg.send(WriterMsg::Failed { disk: p.disk, error: e.to_string() });
                        }
                    }
                }
            }
            if ch.consumer.consumed_frames() < end_frame {
                pending = true;
            }
        }
        if !pending {
            break;
        }
        if Instant::now() > deadline {
            log::error!("writer {}: drain timed out before reaching frame {end_frame}", p.disk);
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    rewind_overshoot(p, end_frame, *failed);
    for ch in p.channels.iter_mut() {
        match ch.file.finalize(hdr) {
            Ok(()) => p.channel_bytes[ch.index].store(ch.file.total_bytes(), Ordering::Relaxed),
            Err(e) => {
                if !*failed {
                    *failed = true;
                    let _ = p.msg.send(WriterMsg::Failed { disk: p.disk, error: e.to_string() });
                }
            }
        }
    }
}

/// Put every channel that was consumed past `end_frame` back on it: trim the
/// excess from the file and rewind the ring, so all files of the take end on the
/// same frame and a continuation take starts every channel at `end_frame`.
fn rewind_overshoot(p: &mut WriterParams, end_frame: u64, failed: bool) {
    for ch in p.channels.iter_mut() {
        let consumed = ch.consumer.consumed_frames();
        if consumed <= end_frame {
            continue;
        }
        let excess = ((consumed - end_frame) as usize) * BYTES_PER_SAMPLE;
        if !failed {
            ch.file.trim(excess);
            p.disk_bytes.fetch_sub((excess as u64).min(p.disk_bytes.load(Ordering::Relaxed)), Ordering::Relaxed);
            p.channel_bytes[ch.index].store(ch.file.total_bytes(), Ordering::Relaxed);
        }
        if !ch.consumer.rewind_to_frame(end_frame) {
            log::error!("writer {}: channel {} consumed {consumed} frames, cannot rewind to {end_frame}", p.disk, ch.index + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::ring;
    use crate::wav::parse_header;
    use chrono::Local;

    fn spec() -> WavSpec {
        WavSpec { sample_rate: 48_000, channels: 1, bits_per_sample: 24, description: "t".into(), originator: "MASSCAP".into(), originator_reference: String::new(), start_time: Local::now() }
    }

    #[test]
    fn channel_file_blocks_and_finalize() {
        let dir = tempfile::tempdir().unwrap();
        let mut hdr = AlignedBuf::new(HEADER_LEN);
        let block = crate::config::BLOCK_QUANTUM; // 12288 bytes = 4096 frames
        let mut cf = ChannelFile::create(dir.path().join("a.wav"), spec(), block, block, &mut hdr).unwrap();
        let (mut p, mut c) = ring(block * 4);
        let samples: Vec<i32> = (0..5000).map(|i| i - 2500).collect(); // 15000 bytes: one block + 2712
        p.push_i24(&samples);
        while c.available() > 0 {
            let n = c.available();
            cf.fill_from(&mut c, n).unwrap();
        }
        assert_eq!(cf.total_bytes(), 15_000);
        cf.finalize(&mut hdr).unwrap();
        let data = std::fs::read(dir.path().join("a.wav")).unwrap();
        assert_eq!(data.len() as u64, file_len(15_000));
        let h = parse_header(&data).unwrap();
        assert_eq!(h.data_bytes, 15_000);
        assert_eq!(h.sample_rate, 48_000);
        // sample 4999 = 2499 → LE bytes
        let tail = &data[HEADER_LEN + 4999 * 3..HEADER_LEN + 5000 * 3];
        assert_eq!(tail, &2499i32.to_le_bytes()[..3]);
    }

    #[test]
    fn writer_thread_drains_to_exact_end_frame() {
        let dir = tempfile::tempdir().unwrap();
        let mut hdr = AlignedBuf::new(HEADER_LEN);
        let block = crate::config::BLOCK_QUANTUM;
        let mut chans = Vec::new();
        let mut producers = Vec::new();
        for i in 0..3 {
            let (p, c) = ring(block * 8);
            producers.push(p);
            let file = ChannelFile::create(dir.path().join(format!("ch{i}.wav")), spec(), block, block / 2 * (i + 1) / 3 * 3, &mut hdr).unwrap();
            chans.push(WriterChannel { index: i, consumer: c, file });
        }
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (msg_tx, msg_rx) = crossbeam_channel::unbounded();
        let channel_bytes = Arc::new((0..3).map(|_| AtomicU64::new(0)).collect::<Vec<_>>());
        let disk_bytes = Arc::new(AtomicU64::new(0));
        let h = spawn(WriterParams { disk: 0, channels: chans, cmd: cmd_rx, msg: msg_tx, channel_bytes: channel_bytes.clone(), disk_bytes: disk_bytes.clone(), header_interval: Duration::from_millis(50), fail_after_bytes: None });
        // feed 10000 frames per channel, then ask to stop at frame 9000
        let blk: Vec<i32> = (0..1000).collect();
        for _ in 0..10 {
            for p in producers.iter_mut() {
                p.push_i24(&blk);
            }
        }
        cmd_tx.send(WriterCmd::Stop { end_frame: 9000 }).unwrap();
        let done = msg_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match done {
            WriterMsg::Done { consumers, ok, .. } => {
                assert!(ok);
                assert_eq!(consumers.len(), 3);
                for (_, c) in &consumers {
                    assert_eq!(c.consumed_frames(), 9000);
                    assert_eq!(c.available(), 3000);
                }
            }
            _ => panic!("expected Done"),
        }
        h.join().unwrap();
        for i in 0..3 {
            let data = std::fs::read(dir.path().join(format!("ch{i}.wav"))).unwrap();
            let hd = parse_header(&data).unwrap();
            assert_eq!(hd.data_bytes, 9000 * 3);
            assert_eq!(data.len() as u64, file_len(9000 * 3));
            assert_eq!(channel_bytes[i].load(Ordering::Relaxed), 27_000);
        }
        assert_eq!(disk_bytes.load(Ordering::Relaxed), 81_000);
    }

    /// The writer has already pulled (and flushed) past the stop frame before it
    /// sees `Stop`: files must still end exactly there and the rings must be
    /// rewound so a continuation take starts on that frame.
    #[test]
    fn writer_trims_and_rewinds_overshoot() {
        let dir = tempfile::tempdir().unwrap();
        let mut hdr = AlignedBuf::new(HEADER_LEN);
        let block = crate::config::BLOCK_QUANTUM; // 4096 frames
        let mut chans = Vec::new();
        let mut producers = Vec::new();
        for i in 0..3 {
            let (p, c) = ring(block * 8);
            producers.push(p);
            let file = ChannelFile::create(dir.path().join(format!("ch{i}.wav")), spec(), block, block * (i + 1) / 3, &mut hdr).unwrap();
            chans.push(WriterChannel { index: i, consumer: c, file });
        }
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (msg_tx, msg_rx) = crossbeam_channel::unbounded();
        let channel_bytes = Arc::new((0..3).map(|_| AtomicU64::new(0)).collect::<Vec<_>>());
        let h = spawn(WriterParams { disk: 0, channels: chans, cmd: cmd_rx, msg: msg_tx, channel_bytes: channel_bytes.clone(), disk_bytes: Arc::new(AtomicU64::new(0)), header_interval: Duration::from_secs(60), fail_after_bytes: None });
        let blk: Vec<i32> = (0..1000).collect();
        for _ in 0..10 {
            for p in producers.iter_mut() {
                p.push_i24(&blk);
            }
        }
        // Let the writer flush two whole blocks (8192 frames) before it is told to stop at 5000.
        let t0 = Instant::now();
        while channel_bytes.iter().any(|b| b.load(Ordering::Relaxed) < 2 * block as u64) {
            assert!(t0.elapsed() < Duration::from_secs(5), "writer did not pull the backlog");
            std::thread::sleep(Duration::from_millis(5));
        }
        cmd_tx.send(WriterCmd::Stop { end_frame: 5000 }).unwrap();
        match msg_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            WriterMsg::Done { consumers, ok, .. } => {
                assert!(ok);
                for (_, c) in &consumers {
                    assert_eq!(c.consumed_frames(), 5000);
                    assert_eq!(c.available(), 5000 * 3);
                }
            }
            _ => panic!("expected Done"),
        }
        h.join().unwrap();
        for i in 0..3 {
            let data = std::fs::read(dir.path().join(format!("ch{i}.wav"))).unwrap();
            let hd = parse_header(&data).unwrap();
            assert_eq!(hd.data_bytes, 5000 * 3);
            assert_eq!(data.len() as u64, file_len(5000 * 3));
            let tail = &data[HEADER_LEN + 4999 * 3..HEADER_LEN + 5000 * 3];
            assert_eq!(tail, &999i32.to_le_bytes()[..3]);
            assert_eq!(channel_bytes[i].load(Ordering::Relaxed), 15_000);
        }
    }
}
