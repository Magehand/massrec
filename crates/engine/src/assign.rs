//! Channel-to-disk assignment.
//!
//! Each destination disk has a measured sustained write throughput and a free
//! space figure. Channels are laid out as contiguous ranges in disk order.
//!
//! Rules:
//! 1. A disk may never be planned above `safety` (default 0.5) of its measured
//!    throughput. If the sum of those budgets cannot carry every channel the
//!    assignment fails and nothing is recorded.
//! 2. Within the budgets, channels are spread in proportion to free space so
//!    that every disk runs out at the same moment. The take's "time remaining"
//!    is then a single honest number.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct DiskInput {
    pub id: String,
    pub throughput_bps: u64,
    pub free_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskRange {
    pub disk: usize,
    /// First channel index (0-based, inclusive).
    pub first: usize,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assignment {
    pub ranges: Vec<DiskRange>,
    /// Seconds until the first disk fills at the planned rate.
    pub runout_seconds: f64,
}

impl Assignment {
    pub fn disk_for(&self, channel: usize) -> Option<usize> {
        self.ranges.iter().find(|r| channel >= r.first && channel < r.first + r.count).map(|r| r.disk)
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AssignError {
    #[error("no destination disks configured")]
    NoDisks,
    #[error("insufficient disk throughput: {needed} channels need {needed_bps} B/s, budget across disks is {available_bps} B/s ({available} channels)")]
    InsufficientThroughput { needed: usize, needed_bps: u64, available: usize, available_bps: u64 },
    #[error("no free space on any destination")]
    NoSpace,
}

pub fn assign(channels: usize, bytes_per_channel_second: u64, disks: &[DiskInput], safety: f64) -> Result<Assignment, AssignError> {
    if disks.is_empty() {
        return Err(AssignError::NoDisks);
    }
    if channels == 0 {
        return Ok(Assignment { ranges: vec![], runout_seconds: f64::INFINITY });
    }
    let b = bytes_per_channel_second.max(1) as f64;
    let cap: Vec<usize> = disks.iter().map(|d| ((d.throughput_bps as f64 * safety) / b).floor() as usize).collect();
    let total_cap: usize = cap.iter().sum();
    if total_cap < channels {
        return Err(AssignError::InsufficientThroughput {
            needed: channels,
            needed_bps: channels as u64 * bytes_per_channel_second,
            available: total_cap,
            available_bps: disks.iter().map(|d| (d.throughput_bps as f64 * safety) as u64).sum(),
        });
    }
    if disks.iter().all(|d| d.free_bytes == 0) {
        return Err(AssignError::NoSpace);
    }

    // Water-filling: proportional to free space, clamped at throughput caps.
    let n = disks.len();
    let mut fixed = vec![false; n];
    let mut want = vec![0f64; n];
    let mut remaining = channels as f64;
    loop {
        let free_sum: f64 = (0..n).filter(|&i| !fixed[i]).map(|i| disks[i].free_bytes as f64).sum();
        if free_sum <= 0.0 {
            // Nothing left with free space but channels remain: spill onto capped disks anyway.
            break;
        }
        let mut changed = false;
        for i in 0..n {
            if fixed[i] {
                continue;
            }
            want[i] = remaining * disks[i].free_bytes as f64 / free_sum;
            if want[i] > cap[i] as f64 {
                want[i] = cap[i] as f64;
                fixed[i] = true;
                remaining -= cap[i] as f64;
                changed = true;
                break; // recompute shares with this disk removed
            }
        }
        if !changed {
            break;
        }
    }

    // Integer rounding by largest remainder, never exceeding caps.
    let mut counts: Vec<usize> = want.iter().map(|w| w.floor() as usize).collect();
    let mut short = channels - counts.iter().sum::<usize>();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| (want[b] - want[b].floor()).partial_cmp(&(want[a] - want[a].floor())).unwrap());
    while short > 0 {
        let mut progressed = false;
        for &i in &order {
            if short == 0 {
                break;
            }
            if counts[i] < cap[i] {
                counts[i] += 1;
                short -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break; // cannot happen given total_cap >= channels
        }
    }

    let mut ranges = Vec::new();
    let mut first = 0;
    let mut runout = f64::INFINITY;
    for (i, &c) in counts.iter().enumerate() {
        if c == 0 {
            continue;
        }
        ranges.push(DiskRange { disk: i, first, count: c });
        first += c;
        runout = runout.min(disks[i].free_bytes as f64 / (c as f64 * b));
    }
    Ok(Assignment { ranges, runout_seconds: runout })
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: u64 = 288_000; // 96 kHz * 3 bytes
    const GB: u64 = 1_000_000_000;

    fn disk(id: &str, mbps: u64, free_gb: u64) -> DiskInput {
        DiskInput { id: id.into(), throughput_bps: mbps * 1_000_000, free_bytes: free_gb * GB }
    }

    fn total(a: &Assignment) -> usize {
        a.ranges.iter().map(|r| r.count).sum()
    }

    #[test]
    fn single_disk_takes_all() {
        let a = assign(300, B, &[disk("d", 500, 1000)], 0.5).unwrap();
        assert_eq!(a.ranges, vec![DiskRange { disk: 0, first: 0, count: 300 }]);
        let expected = 1000.0 * GB as f64 / (300.0 * B as f64);
        assert!((a.runout_seconds - expected).abs() < 1.0);
    }

    #[test]
    fn equal_disks_split_evenly_and_contiguously() {
        let a = assign(300, B, &[disk("a", 500, 1000), disk("b", 500, 1000)], 0.5).unwrap();
        assert_eq!(a.ranges, vec![DiskRange { disk: 0, first: 0, count: 150 }, DiskRange { disk: 1, first: 150, count: 150 }]);
    }

    #[test]
    fn free_space_sets_proportion_so_disks_run_out_together() {
        let a = assign(300, B, &[disk("a", 500, 1000), disk("b", 500, 500)], 0.5).unwrap();
        assert_eq!(total(&a), 300);
        assert_eq!(a.ranges[0].count, 200);
        assert_eq!(a.ranges[1].count, 100);
        // both disks: free / (count*B) equal
        let ta = 1000.0 * GB as f64 / (200.0 * B as f64);
        assert!((a.runout_seconds - ta).abs() < 1.0);
    }

    #[test]
    fn throughput_cap_limits_a_fast_small_disk() {
        // disk a is slow: 0.5*20 MB/s = 10 MB/s -> 34 channels max
        let a = assign(300, B, &[disk("a", 20, 4000), disk("b", 500, 1000)], 0.5).unwrap();
        assert_eq!(total(&a), 300);
        assert_eq!(a.ranges[0].count, 34);
        assert_eq!(a.ranges[1].count, 266);
    }

    #[test]
    fn insufficient_throughput_is_an_error() {
        let e = assign(300, B, &[disk("a", 20, 4000), disk("b", 20, 1000)], 0.5).unwrap_err();
        assert!(matches!(e, AssignError::InsufficientThroughput { needed: 300, available: 68, .. }));
    }

    #[test]
    fn four_disks_rounding_sums_exactly() {
        let disks = [disk("a", 300, 700), disk("b", 300, 333), disk("c", 300, 1200), disk("d", 300, 91)];
        let a = assign(300, B, &disks, 0.5).unwrap();
        assert_eq!(total(&a), 300);
        let mut next = 0;
        for r in &a.ranges {
            assert_eq!(r.first, next);
            next += r.count;
        }
        for ch in 0..300 {
            assert!(a.disk_for(ch).is_some());
        }
    }

    #[test]
    fn no_disks_is_an_error() {
        assert_eq!(assign(10, B, &[], 0.5).unwrap_err(), AssignError::NoDisks);
    }
}
