//! The fleet view: one row per target, with its trend and its headroom.
//!
//! A "target" is a `(host, root)` pair, which is exactly the identity the
//! snapshot store already uses to decide what is comparable. Everything here is
//! derived from stored snapshots; the hub keeps no separate rollup table, so
//! there is nothing that can drift out of step with the snapshots themselves.

use anyhow::Result;
use serde::Serialize;
use spacetrace_store::{ScanMeta, Store};

use crate::trend::{fit, Point, Trend};

#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub host: String,
    pub root: String,
    /// Most recent snapshot of this target.
    pub latest: ScanMeta,
    pub snapshots: usize,
    /// Growth of the *scanned root*, not of the filesystem.
    pub trend: Option<Trend>,
    /// Forecast, or `None` when the trend is not solid enough to extrapolate
    /// or the filesystem's capacity was never measured.
    pub days_until_full: Option<f64>,
    /// Size at each snapshot, oldest first. Drawn as a sparkline.
    pub history: Vec<Point>,
}

impl Target {
    pub fn key(&self) -> String {
        format!("{}:{}", self.host, self.root)
    }

    /// Fraction of the filesystem still free at the latest scan.
    pub fn free_fraction(&self) -> Option<f64> {
        self.latest.fs_free_fraction()
    }

    pub fn bytes_per_day(&self) -> Option<f64> {
        self.trend
            .filter(|t| t.is_reliable())
            .map(|t| t.bytes_per_day)
    }

    /// Lower sorts first. Targets about to fill up come before fast-growing
    /// ones, which come before everything else; within a band, less free space
    /// first. A dashboard that opens on the thing about to break is worth more
    /// than one sorted alphabetically.
    fn urgency(&self) -> (u8, f64) {
        if let Some(days) = self.days_until_full {
            return (0, days);
        }
        if let Some(free) = self.free_fraction() {
            if free < 0.10 {
                return (1, free);
            }
        }
        if let Some(rate) = self.bytes_per_day() {
            if rate > 0.0 {
                // Negate so faster growth sorts earlier.
                return (2, -rate);
            }
        }
        (3, self.free_fraction().unwrap_or(1.0))
    }
}

/// Every target the hub knows about, most urgent first.
pub fn targets(store: &Store) -> Result<Vec<Target>> {
    // `list` is already newest-first, which is what makes the first snapshot
    // seen for a target its latest.
    let all = store.list()?;

    let mut order: Vec<(String, String)> = Vec::new();
    let mut grouped: std::collections::HashMap<(String, String), Vec<ScanMeta>> =
        std::collections::HashMap::new();

    for meta in all {
        let key = (meta.host.clone(), meta.root.clone());
        if !grouped.contains_key(&key) {
            order.push(key.clone());
        }
        grouped.entry(key).or_default().push(meta);
    }

    let mut out = Vec::with_capacity(order.len());
    for key in order {
        let metas = grouped.remove(&key).unwrap_or_default();
        let Some(latest) = metas.first().cloned() else {
            continue;
        };

        let mut history: Vec<Point> = metas
            .iter()
            .map(|m| Point {
                at: m.started_at,
                bytes: m.total_size,
            })
            .collect();
        history.reverse(); // oldest first, for drawing

        let trend = fit(&history);
        // The forecast is about the filesystem's headroom, not the root's size,
        // so it needs the capacity recorded with the latest scan.
        let days_until_full = trend
            .zip(latest.fs_available)
            .and_then(|(t, available)| t.days_until_full(available));

        out.push(Target {
            host: key.0,
            root: key.1,
            snapshots: metas.len(),
            latest,
            trend,
            days_until_full,
            history,
        });
    }

    out.sort_by(|a, b| {
        let (ka, va) = a.urgency();
        let (kb, vb) = b.urgency();
        ka.cmp(&kb)
            .then(va.total_cmp(&vb))
            .then(a.key().cmp(&b.key()))
    });
    Ok(out)
}

/// One target by name, with its history.
pub fn target(store: &Store, host: &str, root: &str) -> Result<Option<Target>> {
    Ok(targets(store)?
        .into_iter()
        .find(|t| t.host == host && t.root == root))
}

/// Totals across the fleet, for the cards at the top of the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub hosts: usize,
    pub targets: usize,
    pub snapshots: usize,
    pub total_size: u64,
    /// Targets whose filesystem is more than 90% used.
    pub tight: usize,
    /// Targets with a forecast inside 30 days.
    pub filling_soon: usize,
    pub newest_scan: Option<i64>,
}

pub fn summarise(targets: &[Target]) -> Summary {
    let mut hosts: Vec<&str> = targets.iter().map(|t| t.host.as_str()).collect();
    hosts.sort_unstable();
    hosts.dedup();

    Summary {
        hosts: hosts.len(),
        targets: targets.len(),
        snapshots: targets.iter().map(|t| t.snapshots).sum(),
        total_size: targets.iter().map(|t| t.latest.total_size).sum(),
        tight: targets
            .iter()
            .filter(|t| t.free_fraction().is_some_and(|f| f < 0.10))
            .count(),
        filling_soon: targets
            .iter()
            .filter(|t| t.days_until_full.is_some_and(|d| d <= 30.0))
            .count(),
        newest_scan: targets.iter().map(|t| t.latest.started_at).max(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(host: &str, root: &str, at: i64, size: u64, available: Option<u64>) -> ScanMeta {
        ScanMeta {
            id: at,
            host: host.into(),
            root: root.into(),
            started_at: at,
            duration_ms: 10,
            total_size: size,
            total_alloc: size,
            files: 1,
            dirs: 1,
            errors: 0,
            hardlinks_deduped: 0,
            scanner_version: "test".into(),
            label: None,
            fs_total: available.map(|_| 1_000_000_000_000),
            fs_available: available,
        }
    }

    fn target_from(metas: Vec<ScanMeta>) -> Target {
        // Mirrors what `targets` builds, without needing a database.
        let latest = metas.first().cloned().unwrap();
        let mut history: Vec<Point> = metas
            .iter()
            .map(|m| Point {
                at: m.started_at,
                bytes: m.total_size,
            })
            .collect();
        history.reverse();
        let trend = fit(&history);
        let days_until_full = trend
            .zip(latest.fs_available)
            .and_then(|(t, a)| t.days_until_full(a));
        Target {
            host: latest.host.clone(),
            root: latest.root.clone(),
            snapshots: metas.len(),
            latest,
            trend,
            days_until_full,
            history,
        }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn a_growing_target_gets_a_forecast() {
        let gib = 1024u64 * 1024 * 1024;
        // 1 GiB/day for a week, 10 GiB of headroom left.
        let metas: Vec<ScanMeta> = (0..7)
            .rev()
            .map(|d| meta("nas", "/var", d * DAY, (d as u64) * gib, Some(10 * gib)))
            .collect();
        let t = target_from(metas);

        assert_eq!(t.snapshots, 7);
        let rate = t.bytes_per_day().expect("a reliable rate");
        assert!((rate / gib as f64 - 1.0).abs() < 0.01, "rate {rate}");
        let days = t.days_until_full.expect("a forecast");
        assert!((days - 10.0).abs() < 0.5, "days {days}");
    }

    #[test]
    fn a_target_without_capacity_gets_no_forecast() {
        let gib = 1024u64 * 1024 * 1024;
        let metas: Vec<ScanMeta> = (0..7)
            .rev()
            .map(|d| meta("nas", "/var", d * DAY, (d as u64) * gib, None))
            .collect();
        let t = target_from(metas);

        assert!(t.bytes_per_day().is_some(), "the rate is still known");
        assert!(
            t.days_until_full.is_none(),
            "without capacity there is nothing to forecast against"
        );
        assert!(t.free_fraction().is_none());
    }

    #[test]
    fn a_single_snapshot_has_no_trend() {
        let t = target_from(vec![meta("nas", "/var", 0, 100, Some(1000))]);
        assert!(t.trend.is_none());
        assert!(t.bytes_per_day().is_none());
        assert!(t.days_until_full.is_none());
    }

    #[test]
    fn urgency_puts_the_soonest_to_fill_first() {
        let gib = 1024u64 * 1024 * 1024;
        let growing_fast: Vec<ScanMeta> = (0..7)
            .rev()
            .map(|d| meta("a", "/var", d * DAY, (d as u64) * 10 * gib, Some(20 * gib)))
            .collect();
        let growing_slow: Vec<ScanMeta> = (0..7)
            .rev()
            .map(|d| meta("b", "/var", d * DAY, (d as u64) * gib, Some(500 * gib)))
            .collect();
        let idle = vec![meta("c", "/var", 0, gib, Some(900 * gib))];

        let mut list = [
            target_from(idle),
            target_from(growing_slow),
            target_from(growing_fast),
        ];
        list.sort_by(|x, y| {
            let (ka, va) = x.urgency();
            let (kb, vb) = y.urgency();
            ka.cmp(&kb)
                .then(va.total_cmp(&vb))
                .then(x.key().cmp(&y.key()))
        });

        assert_eq!(list[0].host, "a", "2 days out must come first");
        assert_eq!(list[2].host, "c", "nothing happening comes last");
    }

    #[test]
    fn a_nearly_full_filesystem_outranks_a_merely_growing_one() {
        let gib = 1024u64 * 1024 * 1024;
        // Almost no headroom, but flat: no forecast, yet still the priority.
        let tight = vec![
            meta("tight", "/var", 0, gib, Some(2 * gib)),
            meta("tight", "/var", -DAY, gib, Some(2 * gib)),
        ];
        let mut tight = target_from(tight);
        tight.latest.fs_total = Some(1000 * gib);
        tight.latest.fs_available = Some(20 * gib); // 2% free
        tight.days_until_full = None;

        let growing: Vec<ScanMeta> = (0..7)
            .rev()
            .map(|d| meta("grow", "/var", d * DAY, (d as u64) * gib, None))
            .collect();
        let growing = target_from(growing);

        assert!(tight.urgency().0 < growing.urgency().0, "tight first");
    }

    #[test]
    fn the_summary_counts_hosts_targets_and_pressure() {
        let gib = 1024u64 * 1024 * 1024;
        let mut tight = target_from(vec![meta("a", "/var", 0, gib, Some(1))]);
        tight.latest.fs_total = Some(1000);
        tight.latest.fs_available = Some(5); // 0.5% free

        // Growing 1 GiB/day with 20 GiB left on a 100 GiB filesystem: due in
        // about 20 days, but 20% free, so it must count as "filling soon" and
        // not as "tight".
        let soon: Vec<ScanMeta> = (0..7)
            .rev()
            .map(|d| meta("a", "/srv", d * DAY, (d as u64) * gib, Some(20 * gib)))
            .collect();
        let mut soon = target_from(soon);
        soon.latest.fs_total = Some(100 * gib);

        let calm = target_from(vec![meta("b", "/data", 0, gib, Some(900 * gib))]);

        let summary = summarise(&[tight, soon, calm]);
        assert_eq!(summary.hosts, 2, "a and b");
        assert_eq!(summary.targets, 3);
        assert_eq!(summary.tight, 1);
        assert_eq!(summary.filling_soon, 1);
        // The newest scan in the fleet is `soon`s last one, on day 6.
        assert_eq!(summary.newest_scan, Some(6 * DAY));
    }

    #[test]
    fn an_empty_fleet_summarises_to_zeroes() {
        let summary = summarise(&[]);
        assert_eq!(summary.hosts, 0);
        assert_eq!(summary.targets, 0);
        assert_eq!(summary.total_size, 0);
        assert!(summary.newest_scan.is_none());
    }
}
