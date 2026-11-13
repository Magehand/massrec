#![allow(clippy::field_reassign_with_default)]
//! End-to-end: simulated input → engine → files on two "disks".

use std::path::Path;
use std::time::{Duration, Instant};

use masscap_engine::config::{Config, Destination, InputKind};
use masscap_engine::events::EventKind;
use masscap_engine::input::sim::{SimConfig, SimInput, SimScenario};
use masscap_engine::wav::{parse_header, HEADER_LEN};
use masscap_engine::{Engine, RecordState};

const RATE: u32 = 48_000;
const CH: usize = 16;

fn config(dests: &[&Path]) -> Config {
    let mut c = Config::default();
    c.session = "TestSession".into();
    c.input = InputKind::Sim;
    c.channels = CH;
    c.sim_sample_rate = RATE;
    c.preroll_seconds = 0.5;
    c.cache_bytes = Some(64 * 1024 * 1024);
    c.block_bytes = 12_288 * 4; // small blocks so short takes produce several writes
    c.destinations = dests
        .iter()
        .map(|p| Destination { path: p.to_path_buf(), volume_serial: String::new(), throughput_bps: Some(400_000_000), measured_at: None })
        .collect();
    c
}

fn sim() -> Box<SimInput> {
    Box::new(SimInput::new(SimConfig {
        sample_rate: RATE,
        channels: CH,
        block_frames: 256,
        realtime: true,
        stop_after_frames: None,
        scenario: SimScenario { silent: vec![3], clipping: vec![5], hot: vec![] },
    }))
}

fn wait_state(e: &Engine, want: RecordState, timeout: Duration) {
    let t0 = Instant::now();
    while e.status().state != want {
        assert!(t0.elapsed() < timeout, "timed out waiting for {want:?}; last error {:?}", e.status().last_error);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wav_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false)).collect();
    v.sort();
    v
}

fn take_dirs(dest: &Path) -> Vec<std::path::PathBuf> {
    let session = dest.join("TestSession");
    let mut v: Vec<_> = std::fs::read_dir(&session).unwrap().map(|e| e.unwrap().path()).collect();
    v.sort();
    v
}

#[test]
fn records_a_take_split_over_two_disks() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let engine = Engine::new(config(&[a.path(), b.path()]), sim()).unwrap();
    let events = engine.subscribe();

    std::thread::sleep(Duration::from_millis(900)); // let pre-roll accumulate
    let st = engine.status();
    assert_eq!(st.channels, CH);
    assert!(st.remaining_seconds.is_some(), "planned assignment should give a remaining time");
    assert_eq!(st.disks.len(), 2);
    assert_eq!(st.disks[0].count + st.disks[1].count, CH);

    engine.start_take();
    wait_state(&engine, RecordState::Recording, Duration::from_secs(3));
    std::thread::sleep(Duration::from_millis(700));
    engine.mark("verse");
    std::thread::sleep(Duration::from_millis(1300));
    let st = engine.status();
    assert!(st.elapsed_seconds > 2.0 && st.elapsed_seconds < 3.5, "elapsed {}", st.elapsed_seconds);
    assert!(st.channel_paths.iter().all(|p| !p.is_empty()));
    assert!(st.meters[5].clips_total > 0, "clipping channel should register clips");
    engine.stop_take();
    wait_state(&engine, RecordState::Idle, Duration::from_secs(10));

    // Files: 8 per disk, identical lengths, valid headers, ≥ 2.0 s + pre-roll of audio.
    let ta = take_dirs(a.path());
    let tb = take_dirs(b.path());
    assert_eq!(ta.len(), 1);
    assert_eq!(tb.len(), 1);
    assert_eq!(ta[0].file_name(), tb[0].file_name(), "same take folder name on both disks");
    let fa = wav_files(&ta[0]);
    let fb = wav_files(&tb[0]);
    assert_eq!(fa.len() + fb.len(), CH);
    assert!(fa[0].file_name().unwrap().to_str().unwrap().starts_with("ch001_"));
    let mut lens = Vec::new();
    for f in fa.iter().chain(fb.iter()) {
        let data = std::fs::read(f).unwrap();
        let h = parse_header(&data).unwrap();
        assert_eq!(h.sample_rate, RATE);
        assert_eq!(h.bits_per_sample, 24);
        assert_eq!(data.len() as u64, masscap_engine::wav::file_len(h.data_bytes));
        lens.push(h.data_bytes);
    }
    assert!(lens.iter().all(|&l| l == lens[0]), "all channels must have identical length: {lens:?}");
    let seconds = lens[0] as f64 / 3.0 / RATE as f64;
    assert!(seconds > 2.3 && seconds < 3.6, "recorded {seconds} s (2.0 s + ~0.5 s pre-roll expected)");

    // Silent channel is all zeros; clipping channel is not.
    let silent = std::fs::read(fa.iter().chain(fb.iter()).find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("ch004_")).unwrap()).unwrap();
    assert!(silent[HEADER_LEN..].iter().all(|&x| x == 0));
    let clip = std::fs::read(fa.iter().chain(fb.iter()).find(|p| p.file_name().unwrap().to_str().unwrap().starts_with("ch006_")).unwrap()).unwrap();
    assert!(clip[HEADER_LEN..].iter().any(|&x| x != 0));

    // Sidecars on both disks.
    for d in [&ta[0], &tb[0]] {
        let markers = std::fs::read_to_string(d.join("markers.txt")).unwrap();
        assert_eq!(markers.lines().count(), 1);
        assert!(markers.starts_with("1\t00:00:0"));
        assert!(markers.trim_end().ends_with("\tverse"));
        let ev = std::fs::read_to_string(d.join("events.txt")).unwrap();
        assert!(ev.contains("record start"));
        assert!(ev.contains("stopped"));
        let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.join("take.json")).unwrap()).unwrap();
        assert_eq!(manifest["channels"], CH);
        assert_eq!(manifest["disks"].as_array().unwrap().len(), 2);
        assert_eq!(manifest["files"].as_array().unwrap().len(), CH);
        assert_eq!(manifest["stop_reason"], "Operator");
        assert_eq!(manifest["frames"].as_u64().unwrap(), lens[0] / 3);
    }

    let kinds: Vec<String> = events.try_iter().map(|e| format!("{:?}", e.kind).split([' ', '{']).next().unwrap().to_string()).collect();
    assert!(kinds.contains(&"TakeStarted".to_string()));
    assert!(kinds.contains(&"Marker".to_string()));
    assert!(kinds.contains(&"TakeStopped".to_string()));
    assert!(kinds.contains(&"Clip".to_string()));
    engine.shutdown();
}

#[test]
fn disk_failure_stops_and_restarts_on_survivor() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let engine = Engine::new(config(&[a.path(), b.path()]), sim()).unwrap();
    let events = engine.subscribe();
    engine.inject_disk_failure(1, 200_000);

    std::thread::sleep(Duration::from_millis(300));
    engine.start_take();
    wait_state(&engine, RecordState::Recording, Duration::from_secs(3));
    let first = engine.status().take_name.unwrap();

    // Wait for the failover: disk 1 fails, take restarts with a new name.
    let t0 = Instant::now();
    loop {
        let st = engine.status();
        if st.disks[1].failed && st.state == RecordState::Recording && st.take_name.as_deref() != Some(&first) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(15), "no failover; status {:?}", st.recent_events.iter().map(|e| e.message()).collect::<Vec<_>>());
        std::thread::sleep(Duration::from_millis(30));
    }
    let st = engine.status();
    assert_eq!(st.disks[0].count, CH, "survivor takes every channel");
    assert_eq!(st.disks[1].count, 0);
    std::thread::sleep(Duration::from_millis(800));
    engine.stop_take();
    wait_state(&engine, RecordState::Idle, Duration::from_secs(10));

    // Disk A has two takes: the truncated first one (8 files, equal lengths) and the full second (16 files).
    let ta = take_dirs(a.path());
    assert_eq!(ta.len(), 2, "{ta:?}");
    let f1 = wav_files(&ta[0]);
    let f2 = wav_files(&ta[1]);
    assert_eq!(f1.len(), CH / 2);
    assert_eq!(f2.len(), CH);
    for set in [&f1, &f2] {
        let lens: Vec<u64> = set.iter().map(|f| parse_header(&std::fs::read(f).unwrap()).unwrap().data_bytes).collect();
        assert!(lens.iter().all(|&l| l == lens[0]), "{lens:?}");
        assert!(lens[0] > 0);
    }
    let m1: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(ta[0].join("take.json")).unwrap()).unwrap();
    assert!(m1["stop_reason"].as_str().unwrap().contains("DiskFailed"));
    let m2: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(ta[1].join("take.json")).unwrap()).unwrap();
    assert_eq!(m2["preroll_frames"], 0);
    assert_eq!(m2["disks"].as_array().unwrap().len(), 1);

    let evs: Vec<_> = events.try_iter().collect();
    assert!(evs.iter().any(|e| matches!(e.kind, EventKind::DiskFailed { .. })));
    assert_eq!(evs.iter().filter(|e| matches!(e.kind, EventKind::TakeStarted { .. })).count(), 2);
    assert_eq!(evs.iter().filter(|e| matches!(e.kind, EventKind::TakeStopped { .. })).count(), 2);
    engine.shutdown();
}

#[test]
fn refuses_to_start_without_measured_disks() {
    let a = tempfile::tempdir().unwrap();
    let mut c = config(&[a.path()]);
    c.destinations[0].throughput_bps = None;
    let engine = Engine::new(c, sim()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(engine.status().not_ready.is_some());
    engine.start_take();
    std::thread::sleep(Duration::from_millis(300));
    let st = engine.status();
    assert_eq!(st.state, RecordState::Idle);
    assert!(st.last_error.as_deref().unwrap_or("").contains("cannot start"));
    engine.shutdown();
}
