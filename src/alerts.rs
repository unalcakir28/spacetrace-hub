//! Deciding what is worth waking someone up for, and telling them.
//!
//! Two rules shape this. A threshold that fires every time a snapshot arrives
//! is noise, so a rule that has already fired for a target is held back for a
//! cooldown. And a rule only fires on evidence the hub actually has: a forecast
//! rule stays quiet when the trend is not solid enough to extrapolate, rather
//! than guessing.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::{self, AlertKind, AlertRule};
use crate::fleet::Target;
use crate::html;

/// How long a rule stays quiet for a target after firing for it.
pub const COOLDOWN_SECONDS: i64 = 6 * 3600;

/// A rule that matched, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Firing {
    pub rule_id: i64,
    pub host: String,
    pub root: String,
    pub message: String,
    pub webhook_url: String,
}

/// Decide whether one rule fires for one target, ignoring cooldown.
///
/// Split out from the cooldown and delivery so the decision itself is testable
/// without a database or a network.
pub fn evaluate(rule: &AlertRule, target: &Target) -> Option<String> {
    if !rule.enabled || !rule.matches(&target.host, &target.root) {
        return None;
    }

    match rule.kind {
        AlertKind::FreeBelowPercent => {
            // Needs measured capacity; a snapshot that never knew it must not
            // be read as a full disk.
            let free = target.free_fraction()? * 100.0;
            (free < rule.threshold).then(|| {
                format!(
                    "{}:{} has {:.1}% free (below {:.0}%), {} of {} available",
                    target.host,
                    target.root,
                    free,
                    rule.threshold,
                    html::bytes(target.latest.fs_available.unwrap_or(0)),
                    html::bytes(target.latest.fs_total.unwrap_or(0)),
                )
            })
        }
        AlertKind::GrowthAbovePerDay => {
            let rate = target.bytes_per_day()?;
            (rate > rule.threshold).then(|| {
                format!(
                    "{}:{} is growing by {}/day (above {}/day), now {}",
                    target.host,
                    target.root,
                    html::bytes(rate as u64),
                    html::bytes(rule.threshold as u64),
                    html::bytes(target.latest.total_size),
                )
            })
        }
        AlertKind::FullWithinDays => {
            // `days_until_full` is already None for an unreliable trend, which
            // is what keeps this from forecasting off noise.
            let days = target.days_until_full?;
            (days <= rule.threshold).then(|| {
                format!(
                    "{}:{} fills in about {} at the current rate (within {:.0} days), {} free",
                    target.host,
                    target.root,
                    html::horizon(Some(days)),
                    rule.threshold,
                    html::bytes(target.latest.fs_available.unwrap_or(0)),
                )
            })
        }
    }
}

/// Every rule that fires for these targets and is not in cooldown.
///
/// Recording the event is what starts the cooldown, so this both decides and
/// records; delivery happens separately because it can be slow and can fail.
pub fn collect(conn: &Connection, targets: &[Target], now: i64) -> Result<Vec<(i64, Firing)>> {
    let rules = db::list_rules(conn)?;
    let mut out = Vec::new();

    for target in targets {
        for rule in &rules {
            let Some(message) = evaluate(rule, target) else {
                continue;
            };
            if let Some(last) = db::last_fired(conn, rule.id, &target.host, &target.root)? {
                if now - last < COOLDOWN_SECONDS {
                    continue;
                }
            }
            let event_id =
                db::record_event(conn, rule.id, &target.host, &target.root, now, &message)?;
            out.push((
                event_id,
                Firing {
                    rule_id: rule.id,
                    host: target.host.clone(),
                    root: target.root.clone(),
                    message,
                    webhook_url: rule.webhook_url.clone(),
                },
            ));
        }
    }
    Ok(out)
}

/// POST the firing to its webhook.
///
/// A delivery failure is recorded against the event rather than retried: the
/// next snapshot will evaluate the rule again, and a hub that queues retries
/// for an endpoint that is simply gone becomes its own problem.
pub async fn deliver(client: &reqwest::Client, firing: &Firing) -> Result<String, String> {
    let body = serde_json::json!({
        "source": "spacetrace-hub",
        "host": firing.host,
        "root": firing.root,
        "message": firing.message,
        "rule_id": firing.rule_id,
    });

    let response = client
        .post(&firing.webhook_url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("{e}"))?;

    let status = response.status();
    if status.is_success() {
        Ok(status.as_u16().to_string())
    } else {
        Err(format!("webhook returned {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trend::{fit, Point};
    use spacetrace_store::ScanMeta;

    const DAY: i64 = 86_400;
    const GIB: u64 = 1024 * 1024 * 1024;

    fn rule(kind: AlertKind, threshold: f64) -> AlertRule {
        AlertRule {
            id: 1,
            host: None,
            root: None,
            kind,
            threshold,
            webhook_url: "https://example.com/hook".into(),
            enabled: true,
            created_at: 0,
        }
    }

    fn target(sizes: &[(i64, u64)], total: Option<u64>, available: Option<u64>) -> Target {
        let history: Vec<Point> = sizes
            .iter()
            .map(|(day, bytes)| Point {
                at: day * DAY,
                bytes: *bytes,
            })
            .collect();
        let trend = fit(&history);
        let latest_bytes = history.last().map(|p| p.bytes).unwrap_or(0);
        let latest_at = history.last().map(|p| p.at).unwrap_or(0);

        let latest = ScanMeta {
            id: 1,
            host: "nas".into(),
            root: "/var".into(),
            started_at: latest_at,
            duration_ms: 1,
            total_size: latest_bytes,
            total_alloc: latest_bytes,
            files: 1,
            dirs: 1,
            errors: 0,
            hardlinks_deduped: 0,
            scanner_version: "test".into(),
            label: None,
            fs_total: total,
            fs_available: available,
        };
        let days_until_full = trend.zip(available).and_then(|(t, a)| t.days_until_full(a));

        Target {
            host: "nas".into(),
            root: "/var".into(),
            snapshots: history.len(),
            latest,
            trend,
            days_until_full,
            history,
        }
    }

    /// A week of 1 GiB/day growth, with the given headroom left.
    fn growing(available: Option<u64>) -> Target {
        let sizes: Vec<(i64, u64)> = (0..7).map(|d| (d, d as u64 * GIB)).collect();
        target(&sizes, Some(1000 * GIB), available)
    }

    #[test]
    fn free_space_fires_only_below_the_threshold() {
        // 5% free.
        let t = target(&[(0, GIB), (1, GIB)], Some(1000 * GIB), Some(50 * GIB));

        assert!(evaluate(&rule(AlertKind::FreeBelowPercent, 10.0), &t).is_some());
        assert!(evaluate(&rule(AlertKind::FreeBelowPercent, 5.0), &t).is_none());
        assert!(evaluate(&rule(AlertKind::FreeBelowPercent, 1.0), &t).is_none());

        let message = evaluate(&rule(AlertKind::FreeBelowPercent, 10.0), &t).unwrap();
        assert!(message.contains("5.0% free"), "{message}");
        assert!(message.contains("nas:/var"), "{message}");
    }

    #[test]
    fn free_space_stays_quiet_when_capacity_was_never_measured() {
        let t = growing(None);
        assert!(
            evaluate(&rule(AlertKind::FreeBelowPercent, 90.0), &t).is_none(),
            "an unmeasured filesystem must not be reported as full"
        );
    }

    #[test]
    fn growth_fires_above_the_threshold() {
        let t = growing(Some(500 * GIB));

        // Growing 1 GiB/day.
        assert!(evaluate(&rule(AlertKind::GrowthAbovePerDay, 0.5 * GIB as f64), &t).is_some());
        assert!(evaluate(&rule(AlertKind::GrowthAbovePerDay, 2.0 * GIB as f64), &t).is_none());
    }

    #[test]
    fn growth_stays_quiet_without_a_reliable_trend() {
        // Two points: a line, but not one to act on.
        let t = target(&[(0, 0), (5, 100 * GIB)], Some(1000 * GIB), Some(500 * GIB));
        assert!(
            evaluate(&rule(AlertKind::GrowthAbovePerDay, 1.0), &t).is_none(),
            "two samples is not a trend"
        );
    }

    #[test]
    fn growth_stays_quiet_on_noise() {
        // A restore and a delete: a fitted slope here means nothing.
        let sizes = [
            (0i64, 1_000u64),
            (1, 900 * GIB),
            (2, 2_000),
            (3, 850 * GIB),
            (4, 3_000),
            (5, 1_000),
        ];
        let t = target(&sizes, Some(1000 * GIB), Some(500 * GIB));
        assert!(evaluate(&rule(AlertKind::GrowthAbovePerDay, 1.0), &t).is_none());
    }

    #[test]
    fn the_forecast_rule_fires_inside_its_horizon() {
        // 1 GiB/day with 5 GiB left: about five days.
        let t = growing(Some(5 * GIB));
        assert!(t.days_until_full.is_some());

        assert!(evaluate(&rule(AlertKind::FullWithinDays, 7.0), &t).is_some());
        assert!(evaluate(&rule(AlertKind::FullWithinDays, 2.0), &t).is_none());

        let message = evaluate(&rule(AlertKind::FullWithinDays, 7.0), &t).unwrap();
        assert!(message.contains("fills in about"), "{message}");
    }

    #[test]
    fn a_disabled_rule_never_fires() {
        let t = growing(Some(GIB));
        let mut r = rule(AlertKind::FullWithinDays, 3650.0);
        assert!(evaluate(&r, &t).is_some());
        r.enabled = false;
        assert!(evaluate(&r, &t).is_none());
    }

    #[test]
    fn a_rule_scoped_to_another_target_never_fires() {
        let t = growing(Some(GIB));
        let mut r = rule(AlertKind::FullWithinDays, 3650.0);
        assert!(evaluate(&r, &t).is_some());

        r.host = Some("someone-else".into());
        assert!(evaluate(&r, &t).is_none());

        r.host = Some("nas".into());
        r.root = Some("/srv".into());
        assert!(evaluate(&r, &t).is_none());

        r.root = Some("/var".into());
        assert!(evaluate(&r, &t).is_some());
    }

    // ------------------------------------------------------- with a database

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn a_firing_rule_is_recorded_once_and_then_held_back() {
        let conn = db();
        db::create_rule(
            &conn,
            None,
            None,
            AlertKind::FullWithinDays,
            7.0,
            "https://example.com/hook",
        )
        .unwrap();

        let targets = vec![growing(Some(5 * GIB))];
        let now = 1_000_000i64;

        let first = collect(&conn, &targets, now).unwrap();
        assert_eq!(first.len(), 1, "it should fire the first time");

        // A second snapshot moments later must not fire again.
        let second = collect(&conn, &targets, now + 60).unwrap();
        assert!(second.is_empty(), "cooldown should hold it back");

        // Once the cooldown has passed it may fire again.
        let third = collect(&conn, &targets, now + COOLDOWN_SECONDS + 1).unwrap();
        assert_eq!(third.len(), 1);

        assert_eq!(db::recent_events(&conn, 10).unwrap().len(), 2);
    }

    #[test]
    fn cooldown_is_per_target_not_per_rule() {
        let conn = db();
        db::create_rule(
            &conn,
            None,
            None,
            AlertKind::FreeBelowPercent,
            50.0,
            "https://e/h",
        )
        .unwrap();

        let mut other = growing(Some(GIB));
        other.host = "web1".into();
        let targets = vec![growing(Some(GIB)), other];
        let now = 1_000_000i64;

        let firings = collect(&conn, &targets, now).unwrap();
        assert_eq!(firings.len(), 2, "both targets are their own case");

        // And neither fires again immediately.
        assert!(collect(&conn, &targets, now + 10).unwrap().is_empty());
    }

    #[test]
    fn nothing_fires_when_no_rule_matches() {
        let conn = db();
        db::create_rule(
            &conn,
            Some("nowhere"),
            None,
            AlertKind::FreeBelowPercent,
            99.0,
            "https://e/h",
        )
        .unwrap();
        let targets = vec![growing(Some(GIB))];
        assert!(collect(&conn, &targets, 1_000_000).unwrap().is_empty());
    }

    #[test]
    fn several_rules_can_fire_for_one_target() {
        let conn = db();
        db::create_rule(
            &conn,
            None,
            None,
            AlertKind::FreeBelowPercent,
            99.0,
            "https://e/a",
        )
        .unwrap();
        db::create_rule(
            &conn,
            None,
            None,
            AlertKind::FullWithinDays,
            30.0,
            "https://e/b",
        )
        .unwrap();

        let targets = vec![growing(Some(GIB))];
        let firings = collect(&conn, &targets, 1_000_000).unwrap();
        assert_eq!(firings.len(), 2);
        // Each gets its own event row, so each has its own cooldown.
        assert_eq!(db::recent_events(&conn, 10).unwrap().len(), 2);
    }
}
