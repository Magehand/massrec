# MASSCAP

A pure multichannel audio recorder for Windows. Up to 300 channels from an ASIO
device into one 24-bit RF64/BWF WAV per channel, buffered aggressively in RAM,
split across several disks. Not a DAW.

Status: viability dry run. The engine, CLI and GUI run against a built-in
simulated input on any platform, and against any system audio device through
Core Audio (macOS) or WASAPI (Windows). On a Mac with Dante Virtual Soundcard
that gives a real Dante test at up to 64 channels. The ASIO input compiles on
Windows but has not yet been exercised against a real card. See
`docs/ARCHITECTURE.md`.

## Layout

```
crates/engine   recorder core: input, rings, assignment, RF64 writers, take lifecycle
crates/cli      masscap-cli: disk-test, record, disks, list-devices, init
crates/gui      masscap: the egui recorder console
docs/           architecture and viability assessment
```

## Build

Rust stable. On macOS/Linux the `asio` feature is unavailable; everything else
works.

```
cargo test --workspace
cargo run -p masscap-cli -- record --sim --rate 96000 --channels 300 --duration 60 \
    --dest /tmp/a --dest /tmp/b --session Test --cache-mb 4000
cargo run -p masscap -- --sim --channels 300 --rate 96000

# real device, e.g. Dante Virtual Soundcard on macOS
cargo run -p masscap-cli -- list-devices
cargo run -p masscap-cli -- record --system --device "Dante Virtual Soundcard" \
    --channels 64 --duration 60 --dest /Volumes/A/MASSCAP --dest /Volumes/B/MASSCAP
```

The recorder follows the device's sample rate; channel count is capped at what
the device exposes.

Windows with ASIO (needs LLVM/clang for bindgen; the SDK is downloaded by
`asio-sys`, or set `CPAL_ASIO_DIR`):

```
powershell -File build.ps1
```

The release binaries land in `target\release\`. GitHub Actions builds the same
on every push (`.github/workflows/windows.yml`).

## Operating

1. `masscap-cli init` writes `masscap.toml` next to the exe.
2. `masscap-cli disk-test D:\MASSCAP` writes 10 GB with direct I/O, records the
   sustained rate and saves the destination. Repeat per disk.
3. `masscap-cli disks` shows free space and measured rates.
4. Start `masscap.exe`; set the session name; record with the big button or
   Ctrl+R; Space places a marker. Stopping needs a second Ctrl+R within 2 s.

Take folders: `<disk>\<session>\<YYYY-MM-DD_HHMMSS>\chNNN_<Name>.wav` with
`markers.txt`, `events.txt` and `take.json` on every disk that took part.

## Licence

GPL-3.0-or-later. See `LICENSE`.
