//! RF64 / BWF WAV header layout.
//!
//! The header is a fixed 4096-byte block so that data starts sector-aligned and
//! the header can be rewritten with a single unbuffered write at any time:
//!
//! ```text
//! 0     RIFF|RF64  size  WAVE
//! 12    JUNK|ds64  28    riffLo riffHi dataLo dataHi sampleLo sampleHi table=0
//! 48    fmt        16    PCM mono 24-bit
//! 72    bext       602   Broadcast Wave metadata (time reference = sample of day)
//! 682   JUNK       3398  padding
//! 4088  data       size
//! 4096  <samples>
//! ```
//!
//! While the file is under 4 GiB the header is a plain RIFF file that every
//! tool reads. Once it grows past that the same block is rewritten as RF64 with
//! the ds64 chunk carrying the 64-bit sizes.

use chrono::{DateTime, Local, Timelike};

pub const HEADER_LEN: usize = 4096;
const BEXT_LEN: usize = 602;
const DATA_OFFSET: usize = HEADER_LEN;

/// What goes into the header besides sizes.
#[derive(Clone, Debug)]
pub struct WavSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    /// Free text stored in bext Description (truncated to 256 bytes).
    pub description: String,
    pub originator: String,
    pub originator_reference: String,
    /// Wall-clock time of the first sample. Drives bext date/time and time reference.
    pub start_time: DateTime<Local>,
}

impl WavSpec {
    pub fn block_align(&self) -> u16 {
        self.channels * (self.bits_per_sample / 8)
    }
    pub fn byte_rate(&self) -> u32 {
        self.sample_rate * self.block_align() as u32
    }
    /// Samples since midnight local time of the start instant (BWF time reference).
    pub fn time_reference(&self) -> u64 {
        let t = self.start_time.time();
        let secs = t.num_seconds_from_midnight() as u64;
        let nanos = t.nanosecond() as u64;
        secs * self.sample_rate as u64 + nanos * self.sample_rate as u64 / 1_000_000_000
    }
}

fn put_str(dst: &mut [u8], s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(dst.len());
    dst[..n].copy_from_slice(&b[..n]);
}

/// Build the 4096-byte header for a file containing `data_bytes` of samples.
pub fn build_header(spec: &WavSpec, data_bytes: u64) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    let pad = data_bytes & 1;
    let riff_size = (DATA_OFFSET as u64 - 8) + data_bytes + pad;
    let rf64 = riff_size > u32::MAX as u64;
    let frames = data_bytes / spec.block_align() as u64;

    // RIFF / RF64
    h[0..4].copy_from_slice(if rf64 { b"RF64" } else { b"RIFF" });
    h[4..8].copy_from_slice(&(if rf64 { u32::MAX } else { riff_size as u32 }).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");

    // ds64 / JUNK
    h[12..16].copy_from_slice(if rf64 { b"ds64" } else { b"JUNK" });
    h[16..20].copy_from_slice(&28u32.to_le_bytes());
    if rf64 {
        h[20..28].copy_from_slice(&riff_size.to_le_bytes());
        h[28..36].copy_from_slice(&data_bytes.to_le_bytes());
        h[36..44].copy_from_slice(&frames.to_le_bytes());
    }
    // 44..48 table length = 0

    // fmt
    h[48..52].copy_from_slice(b"fmt ");
    h[52..56].copy_from_slice(&16u32.to_le_bytes());
    h[56..58].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[58..60].copy_from_slice(&spec.channels.to_le_bytes());
    h[60..64].copy_from_slice(&spec.sample_rate.to_le_bytes());
    h[64..68].copy_from_slice(&spec.byte_rate().to_le_bytes());
    h[68..70].copy_from_slice(&spec.block_align().to_le_bytes());
    h[70..72].copy_from_slice(&spec.bits_per_sample.to_le_bytes());

    // bext
    h[72..76].copy_from_slice(b"bext");
    h[76..80].copy_from_slice(&(BEXT_LEN as u32).to_le_bytes());
    let b = &mut h[80..80 + BEXT_LEN];
    put_str(&mut b[0..256], &spec.description);
    put_str(&mut b[256..288], &spec.originator);
    put_str(&mut b[288..320], &spec.originator_reference);
    put_str(&mut b[320..330], &spec.start_time.format("%Y-%m-%d").to_string());
    put_str(&mut b[330..338], &spec.start_time.format("%H:%M:%S").to_string());
    let tr = spec.time_reference();
    b[338..342].copy_from_slice(&(tr as u32).to_le_bytes());
    b[342..346].copy_from_slice(&((tr >> 32) as u32).to_le_bytes());
    b[346..348].copy_from_slice(&1u16.to_le_bytes()); // version 1
    // 348..412 UMID, 412..422 loudness, 422..602 reserved: zero

    // padding JUNK
    let junk_start = 80 + BEXT_LEN; // 682
    let junk_len = DATA_OFFSET - junk_start - 8 - 8; // 3398
    h[junk_start..junk_start + 4].copy_from_slice(b"JUNK");
    h[junk_start + 4..junk_start + 8].copy_from_slice(&(junk_len as u32).to_le_bytes());

    // data
    let d = DATA_OFFSET - 8;
    h[d..d + 4].copy_from_slice(b"data");
    h[d + 4..d + 8].copy_from_slice(&(if rf64 { u32::MAX } else { data_bytes as u32 }).to_le_bytes());
    h
}

/// Final on-disk length for `data_bytes` of samples (RIFF pad byte included).
pub fn file_len(data_bytes: u64) -> u64 {
    DATA_OFFSET as u64 + data_bytes + (data_bytes & 1)
}

/// Parsed view of a header produced by [`build_header`]; used by tests and tools.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedHeader {
    pub rf64: bool,
    pub riff_size: u64,
    pub data_bytes: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub time_reference: u64,
    pub data_offset: usize,
}

pub fn parse_header(h: &[u8]) -> Option<ParsedHeader> {
    if h.len() < HEADER_LEN {
        return None;
    }
    let u32at = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().unwrap());
    let u16at = |o: usize| u16::from_le_bytes(h[o..o + 2].try_into().unwrap());
    let u64at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap());
    let rf64 = match &h[0..4] {
        b"RIFF" => false,
        b"RF64" => true,
        _ => return None,
    };
    if &h[8..12] != b"WAVE" || &h[48..52] != b"fmt " || &h[4088..4092] != b"data" {
        return None;
    }
    let (riff_size, data_bytes) = if rf64 {
        if &h[12..16] != b"ds64" {
            return None;
        }
        (u64at(20), u64at(28))
    } else {
        (u32at(4) as u64, u32at(4092) as u64)
    };
    Some(ParsedHeader {
        rf64,
        riff_size,
        data_bytes,
        sample_rate: u32at(60),
        channels: u16at(58),
        bits_per_sample: u16at(70),
        time_reference: u32at(80 + 338) as u64 | ((u32at(80 + 342) as u64) << 32),
        data_offset: DATA_OFFSET,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn spec() -> WavSpec {
        WavSpec {
            sample_rate: 96_000,
            channels: 1,
            bits_per_sample: 24,
            description: "ch001 Kick In".into(),
            originator: "MASSCAP".into(),
            originator_reference: "take".into(),
            start_time: Local.with_ymd_and_hms(2026, 9, 6, 10, 30, 0).unwrap(),
        }
    }

    #[test]
    fn small_file_is_plain_riff() {
        let h = build_header(&spec(), 3_000);
        let p = parse_header(&h).unwrap();
        assert!(!p.rf64);
        assert_eq!(p.data_bytes, 3_000);
        assert_eq!(p.riff_size, 4096 - 8 + 3_000);
        assert_eq!(p.sample_rate, 96_000);
        assert_eq!(p.bits_per_sample, 24);
        assert_eq!(&h[12..16], b"JUNK");
        assert_eq!(p.time_reference, 10 * 3600 * 96_000 + 30 * 60 * 96_000);
    }

    #[test]
    fn odd_size_gets_pad_byte() {
        let h = build_header(&spec(), 3);
        let p = parse_header(&h).unwrap();
        assert_eq!(p.riff_size, 4096 - 8 + 3 + 1);
        assert_eq!(file_len(3), 4096 + 4);
    }

    #[test]
    fn upgrades_to_rf64_past_4gib() {
        let just_under = u32::MAX as u64 - (4096 - 8) - 1;
        assert!(!parse_header(&build_header(&spec(), just_under)).unwrap().rf64);
        let over = 5_000_000_000u64;
        let h = build_header(&spec(), over);
        let p = parse_header(&h).unwrap();
        assert!(p.rf64);
        assert_eq!(p.data_bytes, over);
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), u32::MAX);
        assert_eq!(u32::from_le_bytes(h[4092..4096].try_into().unwrap()), u32::MAX);
        assert_eq!(u64::from_le_bytes(h[36..44].try_into().unwrap()), over / 3);
    }
}
