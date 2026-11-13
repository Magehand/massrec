//! The engine: owns the input, the rings, the disks and the take lifecycle.
//!
//! A supervisor thread does all control work so callers (CLI, GUI) never block
//! on disk drains. Callers send commands and poll a status snapshot that is
//! refreshed about 30 times a second.
//!
//! Recording protocol (see `cache` for why):
//! * Idle: the input callback pushes into every ring and trims each to the
//!   pre-roll length, so the rings always hold the last N seconds.
//! * Arming: the callback trims one final time, notes the absolute frame the
//!   take starts at, and flips to Recording. Writers are only started after
//!   that acknowledgement, so producer and consumer never touch the read cursor
//!   at the same time.
//! * Recording: the callback only pushes. Writers drain.
//! * Stop: the supervisor fixes an end frame, tells every writer to drain to it,
//!   collects the consumers back, and only then returns to Idle (so trimming
//!   cannot start while a writer is still reading).
//! * Disk failure: stop as above, then restart immediately in place with the
//!   failed disk removed. The new take begins exactly at the old end frame, so
//!   nothing is lost on the surviving disks and the files stay aligned.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use crossbeam_channel::{select, tick, Receiver, Sender};
use parking_lot::RwLock;
use serde::Serialize;

use crate::assign::{assign, Assignment, DiskInput};
use crate::cache::{ring, Consumer, Producer, RingStats};
use crate::config::{sanitize, Config, Destination};
use crate::events::{format_hms, Event, EventKind, StopReason};
use crate::input::{Block, InputDevice, InputFormat};
use crate::io::AlignedBuf;
use crate::meter::{ChannelMeter, ChannelState, FeedState, Meters};
use crate::wav::{WavSpec, HEADER_LEN};
use crate::writer::{self, ChannelFile, WriterChannel, WriterCmd, WriterMsg, WriterParams};
use crate::BYTES_PER_SAMPLE;

const STATE_IDLE: u8 = 0;
const STATE_ARMING: u8 = 1;
const STATE_RECORDING: u8 = 2;

/// Shared between the input callback and the supervisor.
struct Control {
    state: AtomicU8,
    /// Absolute frames delivered by the input so far.
    frames: AtomicU64,
    /// Absolute frame at which the current take starts (set on Arming ack).
    start_frame: AtomicU64,
    /// Pre-roll frames actually captured at take start.
    preroll_frames: AtomicU64,
    preroll_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RecordState {
    Idle,
    Recording,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiskStatus {
    pub path: String,
    pub free_bytes: u64,
    pub throughput_bps: Option<u64>,
    pub failed: bool,
    /// Assigned channel range for the current (or planned) take, 0-based first index.
    pub first_channel: Option<usize>,
    pub count: usize,
    pub bytes_written: u64,
    pub write_rate_bps: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct EngineStatus {
    pub state: RecordState,
    pub session: String,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub channel_names: Vec<String>,
    pub take_name: Option<String>,
    pub elapsed_seconds: f64,
    /// Time until the first disk fills at the current/planned assignment.
    pub remaining_seconds: Option<f64>,
    pub free_bytes_total: u64,
    pub cache_bytes: u64,
    pub cache_fill: f32,
    pub drops_total: u64,
    pub clips_total: u32,
    pub silent_count: usize,
    pub markers: usize,
    pub meters: Vec<ChannelMeter>,
    pub channel_paths: Vec<String>,
    pub channel_bytes: Vec<u64>,
    pub disks: Vec<DiskStatus>,
    pub last_error: Option<String>,
    pub recent_events: VecDeque<Event>,
    /// Why recording cannot start right now, if it cannot.
    pub not_ready: Option<String>,
}

enum Cmd {
    Start,
    Stop,
    Mark(String),
    Subscribe(Sender<Event>),
    SetDestinations(Vec<Destination>),
    SetSession(String),
    InjectDiskFailure { disk: usize, after_bytes: u64 },
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Input(#[from] crate::input::InputError),
    #[error("engine is shut down")]
    Gone,
}

pub struct Engine {
    cmd: Sender<Cmd>,
    status: Arc<RwLock<EngineStatus>>,
    supervisor: Option<JoinHandle<()>>,
}

impl Engine {
    /// Allocate the cache, start the input and the supervisor thread.
    pub fn new(config: Config, mut input: Box<dyn InputDevice>) -> Result<Self, EngineError> {
        let format = input.format();
        let channels = config.channels.clamp(1, format.channels.max(1));
        let rate = format.sample_rate;
        let cache_total = config.effective_cache_bytes();
        let per_ring = ((cache_total / channels as u64) as usize).max(crate::config::BLOCK_QUANTUM * 4);
        let preroll_bytes = ((config.preroll_seconds.max(0.0) * rate as f64) as usize * BYTES_PER_SAMPLE).min(per_ring / 2);

        let mut producers: Vec<Producer> = Vec::with_capacity(channels);
        let mut consumers: Vec<Option<Consumer>> = Vec::with_capacity(channels);
        let mut stats: Vec<RingStats> = Vec::with_capacity(channels);
        for _ in 0..channels {
            let (p, c) = ring(per_ring);
            stats.push(p.stats());
            producers.push(p);
            consumers.push(Some(c));
        }

        let control = Arc::new(Control {
            state: AtomicU8::new(STATE_IDLE),
            frames: AtomicU64::new(0),
            start_frame: AtomicU64::new(0),
            preroll_frames: AtomicU64::new(0),
            preroll_bytes,
        });
        let meters = Arc::new(Meters::new(channels, &config.thresholds));

        // ---- input callback -------------------------------------------------
        {
            let control = control.clone();
            let meters = meters.clone();
            let mut feed = FeedState::new(channels);
            let mut producers = producers;
            input.start(Box::new(move |block: &Block| {
                let frames0 = control.frames.load(Ordering::Relaxed);
                let st = control.state.load(Ordering::Acquire);
                let n = channels.min(block.channels);
                for c in 0..n {
                    let s = block.channel(c);
                    meters.feed(c, s, frames0, &mut feed);
                    producers[c].push_i24(s);
                    if st == STATE_IDLE {
                        producers[c].trim_to(control.preroll_bytes);
                    }
                }
                control.frames.store(frames0 + block.frames as u64, Ordering::Release);
                if st == STATE_ARMING {
                    for p in producers.iter_mut() {
                        p.trim_to(control.preroll_bytes);
                    }
                    let pre = (producers[0].len() / BYTES_PER_SAMPLE) as u64;
                    control.preroll_frames.store(pre, Ordering::Relaxed);
                    control.start_frame.store(producers[0].produced_frames() - pre, Ordering::Relaxed);
                    control.state.store(STATE_RECORDING, Ordering::Release);
                }
            }))?;
        }

        let channel_names: Vec<String> = (0..channels).map(|i| config.channel_name(i, format.channel_names.get(i).map(|s| s.as_str()))).collect();
        let status = Arc::new(RwLock::new(EngineStatus {
            state: RecordState::Idle,
            session: config.session.clone(),
            device_name: format.device_name.clone(),
            sample_rate: rate,
            channels,
            channel_names: channel_names.clone(),
            take_name: None,
            elapsed_seconds: 0.0,
            remaining_seconds: None,
            free_bytes_total: 0,
            cache_bytes: (per_ring * channels) as u64,
            cache_fill: 0.0,
            drops_total: 0,
            clips_total: 0,
            silent_count: 0,
            markers: 0,
            meters: vec![ChannelMeter::default(); channels],
            channel_paths: vec![String::new(); channels],
            channel_bytes: vec![0; channels],
            disks: vec![],
            last_error: None,
            recent_events: VecDeque::new(),
            not_ready: None,
        }));

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let sup = Supervisor {
            config,
            format,
            channels,
            channel_names,
            input,
            control,
            meters,
            stats,
            consumers,
            disks: Vec::new(),
            take: None,
            subscribers: Vec::new(),
            status: status.clone(),
            cmd_rx,
            channel_bytes: Arc::new((0..channels).map(|_| AtomicU64::new(0)).collect()),
            meter_state: vec![MeterTrack::default(); channels],
            hdr: AlignedBuf::new(HEADER_LEN),
            last_error: None,
            recent: VecDeque::new(),
            fail_injection: None,
        };
        let supervisor = std::thread::Builder::new().name("masscap-supervisor".into()).spawn(move || sup.run()).expect("spawn supervisor");
        Ok(Self { cmd: cmd_tx, status, supervisor: Some(supervisor) })
    }

    pub fn status(&self) -> EngineStatus {
        self.status.read().clone()
    }
    pub fn start_take(&self) {
        let _ = self.cmd.send(Cmd::Start);
    }
    pub fn stop_take(&self) {
        let _ = self.cmd.send(Cmd::Stop);
    }
    pub fn mark(&self, label: impl Into<String>) {
        let _ = self.cmd.send(Cmd::Mark(label.into()));
    }
    pub fn set_destinations(&self, d: Vec<Destination>) {
        let _ = self.cmd.send(Cmd::SetDestinations(d));
    }
    pub fn set_session(&self, s: impl Into<String>) {
        let _ = self.cmd.send(Cmd::SetSession(s.into()));
    }
    /// Test hook: make writer `disk` fail once it has written `after_bytes`.
    pub fn inject_disk_failure(&self, disk: usize, after_bytes: u64) {
        let _ = self.cmd.send(Cmd::InjectDiskFailure { disk, after_bytes });
    }
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let _ = self.cmd.send(Cmd::Subscribe(tx));
        rx
    }
    /// Stop any take, stop the input, join the supervisor.
    pub fn shutdown(mut self) {
        let _ = self.cmd.send(Cmd::Shutdown);
        if let Some(h) = self.supervisor.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Shutdown);
        if let Some(h) = self.supervisor.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct MeterTrack {
    hold_dbfs: f32,
    hold_at: Option<Instant>,
    clips_seen: u32,
    clip_until: Option<Instant>,
    drops_seen: u64,
    drop_until: Option<Instant>,
    silent: bool,
    last_clip_event: Option<Instant>,
}

struct DiskState {
    dest: Destination,
    free_bytes: u64,
    failed: bool,
    failed_reason: Option<String>,
    bytes: Arc<AtomicU64>,
    last_bytes: u64,
    last_rate_at: Instant,
    rate_bps: u64,
}

struct TakeDisk {
    disk: usize,
    dir: PathBuf,
    first: usize,
    count: usize,
    cmd: Sender<WriterCmd>,
    handle: Option<JoinHandle<()>>,
    events_file: Option<File>,
    markers_file: Option<File>,
}

struct Take {
    name: String,
    start_time: DateTime<Local>,
    start_frame: u64,
    preroll_frames: u64,
    disks: Vec<TakeDisk>,
    msg_rx: Receiver<WriterMsg>,
    markers: usize,
}

#[derive(Serialize)]
struct Manifest<'a> {
    take: &'a str,
    session: &'a str,
    start_time: String,
    end_time: Option<String>,
    sample_rate: u32,
    bits_per_sample: u16,
    channels: usize,
    preroll_frames: u64,
    frames: Option<u64>,
    stop_reason: Option<String>,
    disks: Vec<ManifestDisk>,
    files: Vec<ManifestFile>,
}
#[derive(Serialize)]
struct ManifestDisk {
    path: String,
    take_dir: String,
    first_channel: usize,
    count: usize,
}
#[derive(Serialize)]
struct ManifestFile {
    channel: usize,
    name: String,
    disk: String,
    file: String,
}

struct Supervisor {
    config: Config,
    format: InputFormat,
    channels: usize,
    channel_names: Vec<String>,
    input: Box<dyn InputDevice>,
    control: Arc<Control>,
    meters: Arc<Meters>,
    stats: Vec<RingStats>,
    consumers: Vec<Option<Consumer>>,
    disks: Vec<DiskState>,
    take: Option<Take>,
    subscribers: Vec<Sender<Event>>,
    status: Arc<RwLock<EngineStatus>>,
    cmd_rx: Receiver<Cmd>,
    channel_bytes: Arc<Vec<AtomicU64>>,
    meter_state: Vec<MeterTrack>,
    hdr: AlignedBuf,
    last_error: Option<String>,
    recent: VecDeque<Event>,
    fail_injection: Option<(usize, u64)>,
}

impl Supervisor {
    fn run(mut self) {
        self.set_destinations(self.config.destinations.clone());
        let ticker = tick(Duration::from_millis(33));
        let slow = tick(Duration::from_secs(2));
        let (never_tx, never_rx) = crossbeam_channel::unbounded::<WriterMsg>();
        let _keep = never_tx;
        loop {
            let writer_rx = self.take.as_ref().map(|t| t.msg_rx.clone()).unwrap_or_else(|| never_rx.clone());
            select! {
                recv(self.cmd_rx) -> cmd => match cmd {
                    Ok(Cmd::Start) => self.start_take(false),
                    Ok(Cmd::Stop) => { self.stop_take(StopReason::Operator); }
                    Ok(Cmd::Mark(label)) => self.mark(label),
                    Ok(Cmd::Subscribe(tx)) => self.subscribers.push(tx),
                    Ok(Cmd::SetDestinations(d)) => self.set_destinations(d),
                    Ok(Cmd::SetSession(s)) => { self.config.session = s; }
                    Ok(Cmd::InjectDiskFailure { disk, after_bytes }) => self.fail_injection = Some((disk, after_bytes)),
                    Ok(Cmd::Shutdown) | Err(_) => break,
                },
                recv(writer_rx) -> msg => if let Ok(msg) = msg { self.on_writer_msg(msg) },
                recv(ticker) -> _ => self.tick(),
                recv(slow) -> _ => self.refresh_disks(),
            }
        }
        if self.take.is_some() {
            self.stop_take(StopReason::Shutdown);
        }
        self.input.stop();
    }

    // ---- helpers ----------------------------------------------------------

    fn rate(&self) -> u32 {
        self.format.sample_rate
    }
    fn bytes_per_channel_second(&self) -> u64 {
        self.rate() as u64 * BYTES_PER_SAMPLE as u64
    }
    fn elapsed(&self) -> Option<f64> {
        self.take.as_ref().map(|t| (self.control.frames.load(Ordering::Acquire).saturating_sub(t.start_frame)) as f64 / self.rate() as f64)
    }

    fn emit(&mut self, kind: EventKind) {
        let ev = Event::now(self.elapsed(), kind);
        log::info!("{}", ev.message());
        let line = ev.log_line();
        if let Some(t) = self.take.as_mut() {
            for d in t.disks.iter_mut() {
                if let Some(f) = d.events_file.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
        self.recent.push_back(ev.clone());
        while self.recent.len() > 200 {
            self.recent.pop_front();
        }
        self.subscribers.retain(|s| s.send(ev.clone()).is_ok());
    }

    fn set_error(&mut self, msg: String) {
        log::error!("{msg}");
        self.last_error = Some(msg.clone());
        self.emit(EventKind::Info(msg));
    }

    fn set_destinations(&mut self, dests: Vec<Destination>) {
        self.disks = dests
            .into_iter()
            .map(|dest| DiskState {
                free_bytes: crate::disk::free_space(&dest.path).unwrap_or(0),
                dest,
                failed: false,
                failed_reason: None,
                bytes: Arc::new(AtomicU64::new(0)),
                last_bytes: 0,
                last_rate_at: Instant::now(),
                rate_bps: 0,
            })
            .collect();
        self.config.destinations = self.disks.iter().map(|d| d.dest.clone()).collect();
    }

    fn refresh_disks(&mut self) {
        for d in self.disks.iter_mut() {
            if let Ok(f) = crate::disk::free_space(&d.dest.path) {
                d.free_bytes = f;
            }
        }
    }

    fn disk_inputs(&self) -> Vec<(usize, DiskInput)> {
        self.disks
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.failed && d.dest.throughput_bps.is_some())
            .map(|(i, d)| (i, DiskInput { id: d.dest.path.display().to_string(), throughput_bps: d.dest.throughput_bps.unwrap_or(0), free_bytes: d.free_bytes }))
            .collect()
    }

    fn plan(&self) -> Result<(Assignment, Vec<usize>), String> {
        if self.disks.is_empty() {
            return Err("no destination disks configured".into());
        }
        let inputs = self.disk_inputs();
        if inputs.is_empty() {
            return Err("no usable destination: every disk is failed or unmeasured".into());
        }
        let (idx, ins): (Vec<usize>, Vec<DiskInput>) = inputs.into_iter().unzip();
        let a = assign(self.channels, self.bytes_per_channel_second(), &ins, self.config.throughput_safety).map_err(|e| e.to_string())?;
        Ok((a, idx))
    }

    // ---- take lifecycle -----------------------------------------------------

    /// `continuation`: restart in place after a disk failure (no pre-roll, no arming).
    fn start_take(&mut self, continuation: bool) {
        if self.take.is_some() {
            return;
        }
        let (assignment, disk_idx) = match self.plan() {
            Ok(p) => p,
            Err(e) => {
                self.set_error(format!("cannot start: {e}"));
                if continuation {
                    self.control.state.store(STATE_IDLE, Ordering::Release);
                }
                return;
            }
        };
        let now = Local::now();
        let name = self.unique_take_name(&now.format("%Y-%m-%d_%H%M%S").to_string(), &disk_idx);
        let rate = self.rate();

        // Create take folders first; a disk that cannot even take a folder is failed.
        let mut take_disks: Vec<(usize, PathBuf)> = Vec::new();
        for (ri, range) in assignment.ranges.iter().enumerate() {
            let di = disk_idx[range.disk];
            let dir = self.disks[di].dest.path.join(&self.config.session).join(&name);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.fail_disk(di, format!("create take folder: {e}"));
                return self.start_take(continuation); // re-plan without it
            }
            let _ = ri;
            take_disks.push((di, dir));
        }

        // Arm: wait for the input callback to fix the start frame.
        let (start_frame, preroll_frames) = if continuation {
            (self.control.frames.load(Ordering::Acquire), 0)
        } else {
            self.control.state.store(STATE_ARMING, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.control.state.load(Ordering::Acquire) != STATE_RECORDING {
                if Instant::now() > deadline {
                    self.control.state.store(STATE_IDLE, Ordering::Release);
                    self.set_error("cannot start: input is not delivering audio".into());
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            (self.control.start_frame.load(Ordering::Relaxed), self.control.preroll_frames.load(Ordering::Relaxed))
        };
        let start_time = now - chrono::Duration::milliseconds((preroll_frames as f64 / rate as f64 * 1000.0) as i64);

        // Open files and spawn writers.
        let (msg_tx, msg_rx) = crossbeam_channel::unbounded();
        let block = self.config.effective_block_bytes();
        let mut disks_out: Vec<TakeDisk> = Vec::new();
        let mut paths = vec![String::new(); self.channels];
        let mut files_manifest = Vec::new();
        for (range, (di, dir)) in assignment.ranges.iter().zip(take_disks.iter()) {
            let mut chans = Vec::with_capacity(range.count);
            let mut failed: Option<String> = None;
            for ch in range.first..range.first + range.count {
                let fname = format!("ch{:03}_{}.wav", ch + 1, sanitize(&self.channel_names[ch]));
                let path = dir.join(&fname);
                let spec = WavSpec {
                    sample_rate: rate,
                    channels: 1,
                    bits_per_sample: 24,
                    description: format!("{} ch{:03} {}", self.config.session, ch + 1, self.channel_names[ch]),
                    originator: "MASSCAP".into(),
                    originator_reference: name.clone(),
                    start_time,
                };
                // Stagger first flushes across the channel set (see ChannelFile::create).
                let first_block = (block as u64 * (ch as u64 + 1) / self.channels as u64) as usize;
                match ChannelFile::create(path.clone(), spec, block, first_block, &mut self.hdr) {
                    Ok(file) => {
                        paths[ch] = path.display().to_string();
                        files_manifest.push(ManifestFile { channel: ch + 1, name: self.channel_names[ch].clone(), disk: self.disks[*di].dest.path.display().to_string(), file: fname });
                        let consumer = self.consumers[ch].take().expect("consumer available");
                        chans.push(WriterChannel { index: ch, consumer, file });
                    }
                    Err(e) => {
                        failed = Some(format!("create {}: {e}", path.display()));
                        break;
                    }
                }
            }
            if let Some(err) = failed {
                // Hand consumers back, tear down what we opened, fail the disk, re-plan.
                for c in chans {
                    self.consumers[c.index] = Some(c.consumer);
                }
                for d in disks_out {
                    let _ = d.cmd.send(WriterCmd::Stop { end_frame: start_frame });
                    if let Some(h) = d.handle {
                        let _ = h.join();
                    }
                }
                // collect consumers from those writers
                while let Ok(WriterMsg::Done { consumers, .. }) = msg_rx.try_recv() {
                    for (i, c) in consumers {
                        self.consumers[i] = Some(c);
                    }
                }
                self.fail_disk(*di, err);
                return self.start_take(continuation);
            }
            let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
            let fail_after = self.fail_injection.filter(|(d, _)| *d == *di).map(|(_, b)| b);
            self.disks[*di].bytes.store(0, Ordering::Relaxed);
            self.disks[*di].last_bytes = 0;
            let handle = writer::spawn(WriterParams {
                disk: *di,
                channels: chans,
                cmd: cmd_rx,
                msg: msg_tx.clone(),
                channel_bytes: self.channel_bytes.clone(),
                disk_bytes: self.disks[*di].bytes.clone(),
                header_interval: Duration::from_secs(5),
                fail_after_bytes: fail_after,
            });
            let open = |n: &str| OpenOptions::new().create(true).append(true).open(dir.join(n)).ok();
            disks_out.push(TakeDisk {
                disk: *di,
                dir: dir.clone(),
                first: range.first,
                count: range.count,
                cmd: cmd_tx,
                handle: Some(handle),
                events_file: open("events.txt"),
                markers_file: open("markers.txt"),
            });
        }
        for b in self.channel_bytes.iter() {
            b.store(0, Ordering::Relaxed);
        }
        self.meters.reset_clips();
        for m in self.meter_state.iter_mut() {
            m.clips_seen = 0;
        }

        self.take = Some(Take { name: name.clone(), start_time, start_frame, preroll_frames, disks: disks_out, msg_rx, markers: 0 });
        self.last_error = None;
        {
            let mut s = self.status.write();
            s.channel_paths = paths;
        }
        self.write_manifest(None, None, Some(files_manifest));
        self.emit(EventKind::TakeStarted { take: name, channels: self.channels, preroll_seconds: preroll_frames as f64 / rate as f64 });
        let ranges: Vec<(String, usize, usize)> = self
            .take
            .as_ref()
            .unwrap()
            .disks
            .iter()
            .map(|d| (self.disks[d.disk].dest.path.display().to_string(), d.first, d.count))
            .collect();
        for (disk, first_channel, count) in ranges {
            self.emit(EventKind::DiskAssigned { disk, first_channel, count });
        }
    }

    /// A take folder name that exists on none of the candidate disks. Two takes
    /// within one second (e.g. a failover restart) get `-2`, `-3`, ... appended.
    fn unique_take_name(&self, base: &str, disk_idx: &[usize]) -> String {
        let exists = |n: &str| disk_idx.iter().any(|&di| self.disks[di].dest.path.join(&self.config.session).join(n).exists());
        if !exists(base) {
            return base.to_string();
        }
        for k in 2.. {
            let n = format!("{base}-{k}");
            if !exists(&n) {
                return n;
            }
        }
        unreachable!()
    }

    fn fail_disk(&mut self, di: usize, reason: String) {
        if !self.disks[di].failed {
            self.disks[di].failed = true;
            self.disks[di].failed_reason = Some(reason.clone());
            let disk = self.disks[di].dest.path.display().to_string();
            self.emit(EventKind::DiskFailed { disk, error: reason });
        }
    }

    /// Stop the running take. Returns whether a take was running.
    fn stop_take(&mut self, reason: StopReason) -> bool {
        let Some(mut take) = self.take.take() else { return false };
        let end_frame = self.control.frames.load(Ordering::Acquire);
        for d in take.disks.iter() {
            let _ = d.cmd.send(WriterCmd::Stop { end_frame });
        }
        let mut pending = take.disks.len();
        let deadline = Instant::now() + Duration::from_secs(90);
        while pending > 0 && Instant::now() < deadline {
            match take.msg_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(WriterMsg::Done { consumers, disk, ok }) => {
                    for (i, c) in consumers {
                        self.consumers[i] = Some(c);
                    }
                    if !ok && !self.disks[disk].failed {
                        self.fail_disk(disk, "write failed while finalizing".into());
                    }
                    pending -= 1;
                }
                Ok(WriterMsg::Failed { disk, error }) => self.fail_disk(disk, error),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        for d in take.disks.iter_mut() {
            if let Some(h) = d.handle.take() {
                let _ = h.join();
            }
        }
        // Log the stop into the take's own files before closing them.
        self.take = Some(take);
        let frames = end_frame.saturating_sub(self.take.as_ref().unwrap().start_frame);
        let name = self.take.as_ref().unwrap().name.clone();
        self.emit(EventKind::TakeStopped { take: name, reason: reason.clone() });
        self.write_manifest(Some(frames), Some(format!("{reason:?}")), None);
        let take = self.take.take().unwrap();
        drop(take);

        let restart = matches!(reason, StopReason::DiskFailed { .. });
        if restart {
            // Stay in Recording so the rings keep everything; new take begins at end_frame.
            self.start_take(true);
        } else {
            self.control.state.store(STATE_IDLE, Ordering::Release);
        }
        {
            let mut s = self.status.write();
            if self.take.is_none() {
                s.take_name = None;
                s.channel_paths = vec![String::new(); self.channels];
            }
        }
        true
    }

    fn on_writer_msg(&mut self, msg: WriterMsg) {
        match msg {
            WriterMsg::Failed { disk, error } => {
                self.fail_disk(disk, error);
                let name = self.disks[disk].dest.path.display().to_string();
                self.stop_take(StopReason::DiskFailed { disk: name });
            }
            WriterMsg::Done { consumers, .. } => {
                // Only arrives outside stop_take if a writer exited unexpectedly.
                for (i, c) in consumers {
                    self.consumers[i] = Some(c);
                }
            }
        }
    }

    fn mark(&mut self, label: String) {
        let Some(elapsed) = self.elapsed() else { return };
        let frames = self.control.frames.load(Ordering::Acquire);
        let take = self.take.as_mut().unwrap();
        take.markers += 1;
        let index = take.markers;
        let rel = frames - take.start_frame;
        let line = format!("{index}\t{}\t{rel}\t{label}", format_hms(elapsed));
        for d in take.disks.iter_mut() {
            if let Some(f) = d.markers_file.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
        self.emit(EventKind::Marker { index, label });
    }

    fn write_manifest(&mut self, frames: Option<u64>, stop_reason: Option<String>, files: Option<Vec<ManifestFile>>) {
        let Some(take) = self.take.as_ref() else { return };
        let files = files.unwrap_or_else(|| {
            let mut v = Vec::new();
            for d in &take.disks {
                for ch in d.first..d.first + d.count {
                    v.push(ManifestFile {
                        channel: ch + 1,
                        name: self.channel_names[ch].clone(),
                        disk: self.disks[d.disk].dest.path.display().to_string(),
                        file: format!("ch{:03}_{}.wav", ch + 1, sanitize(&self.channel_names[ch])),
                    });
                }
            }
            v
        });
        let m = Manifest {
            take: &take.name,
            session: &self.config.session,
            start_time: take.start_time.to_rfc3339(),
            end_time: frames.map(|_| Local::now().to_rfc3339()),
            sample_rate: self.rate(),
            bits_per_sample: 24,
            channels: self.channels,
            preroll_frames: take.preroll_frames,
            frames,
            stop_reason,
            disks: take
                .disks
                .iter()
                .map(|d| ManifestDisk { path: self.disks[d.disk].dest.path.display().to_string(), take_dir: d.dir.display().to_string(), first_channel: d.first + 1, count: d.count })
                .collect(),
            files,
        };
        let json = serde_json::to_string_pretty(&m).unwrap_or_default();
        for d in &take.disks {
            let _ = std::fs::write(d.dir.join("take.json"), &json);
        }
    }

    // ---- periodic -------------------------------------------------------------

    fn tick(&mut self) {
        let now = Instant::now();
        let rate = self.rate() as f64;
        let frames_now = self.control.frames.load(Ordering::Acquire);
        let th = self.config.thresholds.clone();
        let silence_frames = (th.silence_seconds as f64 * rate) as u64;

        let mut meters = Vec::with_capacity(self.channels);
        let mut drops_total = 0u64;
        let mut clips_total = 0u32;
        let mut silent_count = 0usize;
        let mut events = Vec::new();
        let mut max_fill = 0f32;

        for ch in 0..self.channels {
            let (peak, rms, clips, last_signal) = self.meters.take_interval(ch);
            let st = &self.stats[ch];
            let dropped = st.dropped_frames();
            max_fill = max_fill.max(st.fill());
            let t = &mut self.meter_state[ch];

            // peak hold: hold 1.5 s then decay 20 dB/s
            if peak >= t.hold_dbfs || t.hold_at.map(|a| now.duration_since(a) > Duration::from_millis(1500)).unwrap_or(true) {
                if peak >= t.hold_dbfs {
                    t.hold_dbfs = peak;
                    t.hold_at = Some(now);
                } else {
                    t.hold_dbfs = (t.hold_dbfs - 0.66).max(peak);
                }
            }
            if clips > t.clips_seen {
                t.clips_seen = clips;
                t.clip_until = Some(now + Duration::from_millis(1500));
                if t.last_clip_event.map(|l| now.duration_since(l) > Duration::from_secs(2)).unwrap_or(true) {
                    t.last_clip_event = Some(now);
                    events.push(EventKind::Clip { channel: ch });
                }
            }
            if dropped > t.drops_seen {
                events.push(EventKind::Drop { channel: ch, frames: dropped - t.drops_seen });
                t.drops_seen = dropped;
                t.drop_until = Some(now + Duration::from_secs(3));
            }
            let silent_now = frames_now.saturating_sub(last_signal) >= silence_frames && frames_now >= silence_frames;
            if silent_now != t.silent {
                t.silent = silent_now;
                events.push(if silent_now { EventKind::SignalLost { channel: ch } } else { EventKind::SignalReturned { channel: ch } });
            }
            let state = if t.drop_until.map(|u| now < u).unwrap_or(false) {
                ChannelState::Drop
            } else if t.clip_until.map(|u| now < u).unwrap_or(false) {
                ChannelState::Clip
            } else if t.silent {
                ChannelState::Silent
            } else if t.hold_dbfs > th.hot_dbfs {
                ChannelState::Hot
            } else {
                ChannelState::Ok
            };
            if t.silent {
                silent_count += 1;
            }
            drops_total += dropped;
            clips_total += clips;
            meters.push(ChannelMeter { peak_dbfs: peak, rms_dbfs: rms, hold_dbfs: t.hold_dbfs, clips_total: clips, dropped_frames_total: dropped, state });
        }
        for e in events {
            self.emit(e);
        }
        if max_fill > 0.8 {
            static LAST_WARN: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            let mut g = LAST_WARN.lock().unwrap();
            if g.map(|l| now.duration_since(l) > Duration::from_secs(10)).unwrap_or(true) {
                *g = Some(now);
                drop(g);
                self.emit(EventKind::CacheFill { fraction: max_fill });
            }
        }

        // disk rates
        for d in self.disks.iter_mut() {
            let dt = now.duration_since(d.last_rate_at).as_secs_f64();
            if dt >= 1.0 {
                let b = d.bytes.load(Ordering::Relaxed);
                d.rate_bps = ((b.saturating_sub(d.last_bytes)) as f64 / dt) as u64;
                d.last_bytes = b;
                d.last_rate_at = now;
            }
        }

        // remaining time
        let bpcs = self.bytes_per_channel_second() as f64;
        let (remaining, ranges, not_ready): (Option<f64>, Vec<(usize, usize, usize)>, Option<String>) = match &self.take {
            Some(t) => {
                let r = t.disks.iter().map(|d| self.disks[d.disk].free_bytes as f64 / (d.count as f64 * bpcs)).fold(f64::INFINITY, f64::min);
                (Some(r), t.disks.iter().map(|d| (d.disk, d.first, d.count)).collect(), None)
            }
            None => match self.plan() {
                Ok((a, idx)) => {
                    let ranges = a.ranges.iter().map(|r| (idx[r.disk], r.first, r.count)).collect();
                    let r = a.ranges.iter().map(|r| self.disks[idx[r.disk]].free_bytes as f64 / (r.count as f64 * bpcs)).fold(f64::INFINITY, f64::min);
                    (Some(r), ranges, None)
                }
                Err(e) => (None, vec![], Some(e)),
            },
        };

        let disks: Vec<DiskStatus> = self
            .disks
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let r = ranges.iter().find(|(di, _, _)| *di == i);
                DiskStatus {
                    path: d.dest.path.display().to_string(),
                    free_bytes: d.free_bytes,
                    throughput_bps: d.dest.throughput_bps,
                    failed: d.failed,
                    first_channel: r.map(|(_, f, _)| *f),
                    count: r.map(|(_, _, c)| *c).unwrap_or(0),
                    bytes_written: d.bytes.load(Ordering::Relaxed),
                    write_rate_bps: d.rate_bps,
                }
            })
            .collect();

        let mut s = self.status.write();
        s.state = if self.take.is_some() { RecordState::Recording } else { RecordState::Idle };
        s.session = self.config.session.clone();
        s.take_name = self.take.as_ref().map(|t| t.name.clone());
        s.elapsed_seconds = self.elapsed().unwrap_or(0.0);
        s.remaining_seconds = remaining;
        s.free_bytes_total = self.disks.iter().filter(|d| !d.failed).map(|d| d.free_bytes).sum();
        s.cache_fill = max_fill;
        s.drops_total = drops_total;
        s.clips_total = clips_total;
        s.silent_count = silent_count;
        s.markers = self.take.as_ref().map(|t| t.markers).unwrap_or(0);
        s.meters = meters;
        for (i, b) in self.channel_bytes.iter().enumerate() {
            s.channel_bytes[i] = b.load(Ordering::Relaxed);
        }
        s.disks = disks;
        s.last_error = self.last_error.clone();
        s.recent_events = self.recent.clone();
        s.not_ready = not_ready;
    }
}
