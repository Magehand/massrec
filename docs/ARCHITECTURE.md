# MASSCAP — architecture and viability assessment

A pure multichannel disk recorder for Windows: up to 300 channels in, one
24-bit mono RF64/BWF WAV per channel out, aggressive RAM buffering, files
spread across several disks. Not a DAW. No playback, no mixing, no monitoring.

This document is milestone 1 of a viability study. It records the design, the
numbers it rests on, the hardware facts that bound it, and the risks that only
a run on real hardware can close.

## 1. Requirements as settled

| Topic | Decision |
|---|---|
| Channels | Sized for 300; first deployment is one card at 256. |
| Format | Follow the device sample rate (operating point 96 kHz). Write 24-bit packed PCM. Never set the device rate. |
| Files | One mono RF64-capable BWF WAV per channel per take. Header rewritten every 5 s. |
| Layout | `<disk>\<session>\<YYYY-MM-DD_HHMMSS>\chNNN_<Name>.wav` plus `markers.txt`, `events.txt`, `take.json` in every take folder. |
| Cache | Per-channel ring buffers, 60 % of installed RAM by default. Overflow writes silence and logs a drop. |
| Disks | Operator picks N destinations. Each is measured once with a 10 GB direct-I/O write. Channels are assigned as contiguous ranges, planned at 50 % of measured throughput and proportioned so all disks fill at the same time. |
| Disk failure | Stop the take, finalize, immediately start a new take on the surviving disks. |
| Pre-roll | 10 s default, up to 60 s. |
| I/O | Unbuffered, write-through, 4 MB aligned blocks per channel. |
| Dropped | Solo/cue, input trim, per-channel arm, remote view, header repair on start, installer. |
| Licence | GPL-3.0-or-later, which makes the Steinberg ASIO SDK usable without a signed agreement. |

## 2. The numbers

Per channel at 96 kHz, 24-bit: 288 000 B/s. Per channel at 48 kHz: 144 000 B/s.

| Channels | Rate | Total write | 1 hour | 4 GiB per file after |
|---|---|---|---|---|
| 256 | 48 kHz | 36.9 MB/s | 133 GB | 7 h 45 min |
| 300 | 48 kHz | 43.2 MB/s | 156 GB | 7 h 45 min |
| 256 | 96 kHz | 73.7 MB/s | 265 GB | 3 h 52 min |
| 300 | 96 kHz | 86.4 MB/s | 311 GB | 3 h 52 min |

RF64 is therefore mandatory at 96 kHz for any service longer than about four
hours. The writer produces plain RIFF while the file is small and rewrites the
same 4096-byte header block as RF64 the moment it crosses the limit, so short
takes stay maximally compatible.

RAM cache at 300 channels, 96 kHz (86.4 MB/s):

| Cache | Buffered audio |
|---|---|
| 8 GB | 1 min 33 s |
| 16 GB | 3 min 05 s |
| 26 GB | 5 min 01 s |
| 38 GB (60 % of 64 GB) | 7 min 20 s |
| 77 GB (60 % of 128 GB) | 14 min 50 s |

On top of the cache, each channel has one 4 MB staging block, 1.2 GB for 300
channels. This is where audio sits between leaving the ring and hitting the
disk; the ring itself only holds real backlog. A 32 GB machine is comfortable.

A single SATA SSD sustains 400 to 500 MB/s sequential; a decent NVMe drive 1 to
3 GB/s. One disk carries 300 channels at 96 kHz five times over. Disk splitting
in this design exists for capacity and for isolating a failing drive, not for
speed.

## 3. Data path

```
 ASIO / sim callback (device thread, real-time)
   │  per block: for each channel
   │    meters.feed()            peak, sum-of-squares, clip run, last-signal frame (atomics)
   │    ring[ch].push_i24()      convert to packed 24-bit, memcpy into the ring
   │    idle: ring[ch].trim_to(preroll)
   ▼
 300 × SPSC byte rings (cache.rs)         ← the RAM cache, one Producer / one Consumer each
   │
   ▼
 one writer thread per destination disk (writer.rs)
   │  pulls whole frames into a 4 MB aligned staging block per channel
   │  flushes each full block with one unbuffered, write-through write
   │  rewrites the 4096-byte header of every file every 5 s
   ▼
 chNNN_Name.wav  (RF64/BWF, io.rs + wav.rs)

 supervisor thread (engine.rs)   30 Hz: meters → states → events; disk free space; status snapshot
                                 owns take lifecycle, sidecars, failover
```

The callback never allocates, never locks and never touches the file system.
Its cost is a copy plus a peak/RMS pass, about 0.3 ms per 512-frame block for
300 channels in the simulated run.

### Rings

Each ring stores packed 24-bit bytes so the writer copies bytes straight to
disk. Two 64-bit counters (`produced`, `consumed`) give an absolute stream
position for every byte ever pushed. That position is what keeps 300 files
sample-aligned through every edge case:

* **Overflow.** If the ring is full the block is dropped and counted. As soon as
  space frees, the producer inserts the same number of zero frames before any
  new audio. The file keeps its timeline; the gap is audible silence, visible in
  `events.txt` as `write underrun (N smp)` and in the UI as DISK DROP.
* **Pre-roll.** While idle the producer trims its own ring to the pre-roll
  length after every push, so the ring always holds the last N seconds. Nothing
  else reads the ring in that state, which is what makes the trim safe.
* **Arming.** Start sets the shared state to *Arming*. On its next block the
  callback trims once more, records the absolute start frame, and flips to
  *Recording*. Writers start only after that acknowledgement. Producer and
  consumer never move the read cursor concurrently.
* **Stop.** The supervisor fixes an end frame, every writer drains every channel
  to exactly that frame, finalizes, and returns its consumers. Only then does the
  state return to *Idle*, so trimming cannot begin while a writer is reading.

### Writers

The staging block is 4 MB rounded down to a multiple of 12 288 bytes (the least
common multiple of a 3-byte frame and a 4096-byte sector), so blocks are both
whole frames and sector-aligned and the file offset after every block stays
aligned. The first block of each channel is shorter by a per-channel amount so
300 channels do not all flush in the same millisecond; after that they are
naturally spread. Measured on the simulated run this turns a 1.2 GB burst every
14 s into a steady 85 MB/s.

On finalize the partial last block is zero-padded to a sector, written, the
header is rewritten with the true sizes, and the file is truncated to its exact
length. A crash leaves files with a header at most 5 s stale, and every byte
before that header's claimed length is valid audio.

### Disk assignment (`assign.rs`)

Inputs per disk: measured sustained throughput and free space. Steps:

1. `cap_i = floor(0.5 × throughput_i / bytes_per_channel_second)`. If
   `Σ cap_i < channels` refuse to start and say by how much.
2. Water-fill: `n_i = channels × free_i / Σ free`, clamping any disk that
   exceeds its cap and redistributing the rest, until stable.
3. Round to integers by largest remainder without exceeding caps.
4. Emit contiguous ranges in configured disk order; the take's time remaining is
   `min_i free_i / (n_i × bytes_per_channel_second)`.

The plan is recomputed at every take start and shown in the UI beforehand.

### Failover

A write error on disk D: the writer reports it and starts discarding its
channels' data (so their rings do not overflow), the supervisor runs the normal
stop at the current frame, survivors finalize cleanly, D is marked failed, and a
new take starts in *continuation* mode: no arming, no pre-roll, start frame
equal to the previous end frame. Channels that lived on D lose only the audio
between D's last successful write and the end frame; everything else is
contiguous across the two take folders. Take names are made unique with a
`-2` suffix when the restart lands in the same second.

## 4. Hardware and driver facts (September 2026)

* Audinate's own **Dante PCIe-R** and the **Yamaha AIC128-D** receive 128
  channels. The "256" in Audinate marketing is in plus out.
* **Focusrite RedNet PCIeNX**: 128×128 stock, **256×256 with a paid Dante
  Activator upgrade**, up to 96 kHz. Appears as "Focusrite Thunderbolt ASIO".
  Sample rate is set from the Focusrite Notifier, not Dante Controller.
* **MARIAN Clara E**: 512×512 at 48 kHz, 256×256 at 96 kHz, own driver, and the
  only card whose vendor documents joining two cards into one ASIO device.
* **Conclusion**: at 96 kHz no single card reaches 300 receive channels. One
  PCIeNX gives 256. 300 needs two cards, and two non-MARIAN cards means two ASIO
  drivers in one host, which PortAudio, RtAudio and Rust's `cpal` all refuse
  (one driver per process). JUCE does it at the COM level. The engine here is
  device-agnostic and sized for 300, but the ASIO backend is one driver.
* **Dante Application Library** (network receive without a card) is capped at
  64×64 and licensed by Audinate. Not a route to 300.
* Dante devices take their **sample rate from the network**. The engine reads
  the driver's rate and never sets it.
* The Dante ASIO drivers are documented as 24-bit, but whether they report
  `Int24` packed or `Int32` is not documented. Both are handled.
* **ASIO SDK** has been dual-licensed since October 2025: GPLv3 or a free signed
  proprietary agreement. This project is GPLv3.
* **cpal 0.18** (`asio` feature) is maintained, supports Int24/Int32/Float,
  needs LLVM and the SDK at build time (auto-downloaded, or `CPAL_ASIO_DIR`).

### Running on a Mac against Dante Virtual Soundcard

The input layer has three sources: the simulator, the system audio host through
`cpal` (Core Audio on macOS, WASAPI on Windows) and ASIO. DVS appears to macOS
as a Core Audio device, so `masscap-cli record --system --device "Dante Virtual
Soundcard"` records real Dante streams with the exact engine, direct-I/O path,
assignment and failover described here. The ceiling is DVS itself: 64×64 at
44.1/48 kHz, fewer at 96 kHz (check the DVS release notes for the exact figure).
That makes a Mac a good bring-up rig for everything except the 300-channel load,
which the simulator covers.

## 5. What has been verified in the dry run

Everything below ran on macOS against the simulated input; no ASIO driver was
exercised.

* Unit tests: ring semantics including overflow-to-silence, RF64 upgrade at the
  4 GiB boundary, assignment maths for 1 to 4 disks, aligned direct I/O, clip and
  silence detection, sim faults.
* Integration: 16 channels to two destinations with pre-roll and a marker; all
  files identical in length, headers valid, sidecars present on both disks,
  manifest consistent. Injected disk failure: survivor takes all channels in a
  new take within a second; both takes' files aligned.
* Smoke: 300 channels at 96 kHz for 20 s in release build, 2 GB cache, two
  destinations. Zero drops, steady 85 MB/s, cache backlog 1 %, about one third
  of one CPU core (mostly the tone generator). Stop with 300 fsyncs took 3.7 s.
* Windows: `cargo check --target x86_64-pc-windows-msvc` for engine and CLI
  passes, covering the `FILE_FLAG_NO_BUFFERING` path, `GetDiskFreeSpaceExW`,
  `GetVolumeInformationW` and the console Ctrl-C handler. The `asio` feature is
  compiled only in CI on a Windows runner.

## 6. Risks that only hardware closes

1. **One driver per process.** Fine for one card. A second non-MARIAN card
   needs either a second capture process feeding the same rings over shared
   memory or a hand-written ASIO COM binding. Neither is started.
2. **ASIO sample type and channel naming** from the Dante driver: unverified.
   Channel names default to `In N` from the driver and can be overridden in
   `masscap.toml`.
3. **Unbuffered I/O on exotic volumes.** ReFS, network shares and some USB
   bridges reject `FILE_FLAG_NO_BUFFERING` or 4096-byte alignment. The error
   would surface at take start as a failed disk, not as data loss, but a
   buffered fallback is a five-line change if it bites.
4. **Windows scheduling.** The callback is cheap, but the machine must be
   dedicated during a recording: sleep, Windows Update and power management
   off, the card in a full-bandwidth PCIe slot. Most "recorder drops" in the
   field are this, not software.
5. **Memory on a small machine.** 300 channels want 1.2 GB of staging blocks
   regardless of the cache setting. `block_bytes` can be lowered in the config.
6. **Drain time at stop.** Finalizing 300 files means 300 header writes, 300
   truncations and 300 fsyncs. Expect several seconds on spinning disks. The
   input keeps buffering during that time, so the next take can start at once.

## 7. Verdict

Feasible for one developer. The recorder core exists and behaves as specified
under simulation at the full 300-channel, 96 kHz load. The remaining work is
hardware bring-up, which is measured in days and dominated by driver quirks,
and the channel ceiling, which is a purchasing decision (one PCIeNX for 256,
Clara E cards for 300) rather than a software one.
