//! MASSCAP engine: a pure multichannel disk recorder core.
//!
//! Data path: input callback → [`cache`] rings (one per channel, lock-free) →
//! per-disk [`writer`] threads → RF64/BWF files ([`wav`]) via direct I/O ([`io`]).
//! Channel-to-disk placement is computed by [`assign`]. [`engine`] ties it
//! together with take lifecycle, pre-roll, markers and failure handling.

// Index loops over parallel per-channel arrays are the clearest form here.
#![allow(clippy::needless_range_loop, clippy::type_complexity)]

pub mod assign;
pub mod cache;
pub mod config;
pub mod disk;
pub mod engine;
pub mod events;
pub mod input;
pub mod io;
pub mod meter;
pub mod wav;
pub mod writer;

/// Bytes per sample on disk and in the cache (24-bit packed PCM).
pub const BYTES_PER_SAMPLE: usize = 3;

/// Full-scale magnitude of a 24-bit sample held in the low bits of an `i32`.
pub const FULL_SCALE: i32 = 1 << 23;

pub use engine::{Engine, EngineStatus, RecordState};
pub use events::Event;
