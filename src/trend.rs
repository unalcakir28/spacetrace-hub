//! Growth rate and "when does it fill up".
//!
//! A least-squares line through a target's snapshot history. Deliberately
//! boring maths, with one thing that is not: the fit reports how well it fits,
//! and callers are expected to refuse to forecast from a bad fit. A confident
//! wrong date is worse than no date, and disk usage is frequently not linear —
//! a log rotation or a one-off restore will happily produce a line whose slope
//! means nothing.

/// One observation of a target's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Point {
    /// Unix seconds.
    pub at: i64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Trend {
    /// Slope of the fitted line. Negative means the target is shrinking.
    pub bytes_per_day: f64,
    /// Coefficient of determination, `0.0..=1.0`. How much of the variation
    /// the straight line actually explains.
    pub r2: f64,
    pub samples: usize,
    /// Time between the first and last observation.
    pub span_days: f64,
    /// Size at the most recent observation.
    pub latest_bytes: u64,
}

/// Below this, the history is a coincidence rather than a trend.
pub const MIN_SAMPLES: usize = 3;

/// Below this span the slope is dominated by noise: two scans an hour apart
/// during a build say nothing about next week.
pub const MIN_SPAN_DAYS: f64 = 1.0;

/// Below this fit quality, refuse to extrapolate.
pub const MIN_R2: f64 = 0.5;

impl Trend {
    /// Whether this trend is solid enough to forecast from.
    pub fn is_reliable(&self) -> bool {
        self.samples >= MIN_SAMPLES && self.span_days >= MIN_SPAN_DAYS && self.r2 >= MIN_R2
    }

    /// Days until `available` bytes are gone at this rate.
    ///
    /// `None` when the target is not growing, when the trend is not reliable,
    /// or when the answer is so far out that stating it would be false
    /// precision.
    pub fn days_until_full(&self, available: u64) -> Option<f64> {
        if !self.is_reliable() || self.bytes_per_day <= 0.0 {
            return None;
        }
        let days = available as f64 / self.bytes_per_day;
        // Beyond a few years the linear model has no claim to being right.
        if !days.is_finite() || days > 3650.0 {
            return None;
        }
        Some(days)
    }
}

/// Fit a line through the points. `None` when there is nothing to fit.
///
/// Points may arrive in any order; they are sorted by time here so callers do
/// not have to remember to.
pub fn fit(points: &[Point]) -> Option<Trend> {
    if points.len() < 2 {
        return None;
    }
    let mut sorted: Vec<Point> = points.to_vec();
    sorted.sort_unstable_by_key(|p| p.at);

    let first = sorted.first()?;
    let last = sorted.last()?;
    let span_days = (last.at - first.at) as f64 / 86_400.0;

    // Measure time in days from the first sample: keeps the numbers small
    // enough that f64 precision is never in question, unlike raw unix seconds
    // squared.
    let xs: Vec<f64> = sorted
        .iter()
        .map(|p| (p.at - first.at) as f64 / 86_400.0)
        .collect();
    let ys: Vec<f64> = sorted.iter().map(|p| p.bytes as f64).collect();
    let n = xs.len() as f64;

    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;

    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut syy = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        let dx = x - mean_x;
        let dy = y - mean_y;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }

    // All samples at the same instant: no slope exists.
    if sxx <= f64::EPSILON {
        return None;
    }
    let slope = sxy / sxx;

    // With no variation in y the line is flat and explains everything about a
    // constant; calling that a perfect fit is right, and it keeps a completely
    // static target from being reported as unreliable.
    let r2 = if syy <= f64::EPSILON {
        1.0
    } else {
        ((sxy * sxy) / (sxx * syy)).clamp(0.0, 1.0)
    };

    Some(Trend {
        bytes_per_day: slope,
        r2,
        samples: sorted.len(),
        span_days,
        latest_bytes: last.bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn points(values: &[(i64, u64)]) -> Vec<Point> {
        values
            .iter()
            .map(|(day, bytes)| Point {
                at: day * DAY,
                bytes: *bytes,
            })
            .collect()
    }

    #[test]
    fn a_perfectly_linear_history_gives_the_exact_slope() {
        // 1 GiB per day for a week.
        let gib = 1024u64 * 1024 * 1024;
        let p = points(&[
            (0, 0),
            (1, gib),
            (2, 2 * gib),
            (3, 3 * gib),
            (4, 4 * gib),
            (5, 5 * gib),
            (6, 6 * gib),
        ]);
        let trend = fit(&p).unwrap();

        let relative = (trend.bytes_per_day - gib as f64).abs() / gib as f64;
        assert!(relative < 1e-9, "slope off by {relative}");
        assert!((trend.r2 - 1.0).abs() < 1e-9);
        assert_eq!(trend.samples, 7);
        assert!((trend.span_days - 6.0).abs() < 1e-9);
        assert_eq!(trend.latest_bytes, 6 * gib);
        assert!(trend.is_reliable());
    }

    #[test]
    fn the_forecast_divides_headroom_by_the_rate() {
        let gib = 1024u64 * 1024 * 1024;
        let p = points(&[(0, 0), (1, gib), (2, 2 * gib), (3, 3 * gib)]);
        let trend = fit(&p).unwrap();

        // 10 GiB of headroom at 1 GiB/day.
        let days = trend.days_until_full(10 * gib).unwrap();
        assert!((days - 10.0).abs() < 1e-6, "got {days}");
    }

    #[test]
    fn a_shrinking_target_never_fills_up() {
        let gib = 1024u64 * 1024 * 1024;
        let p = points(&[(0, 10 * gib), (1, 9 * gib), (2, 8 * gib), (3, 7 * gib)]);
        let trend = fit(&p).unwrap();

        assert!(trend.bytes_per_day < 0.0);
        assert!(trend.days_until_full(gib).is_none());
    }

    #[test]
    fn a_flat_target_never_fills_up_but_still_fits() {
        let p = points(&[(0, 5000), (1, 5000), (2, 5000), (3, 5000)]);
        let trend = fit(&p).unwrap();

        assert!(trend.bytes_per_day.abs() < 1e-9);
        assert_eq!(
            trend.r2, 1.0,
            "a constant is perfectly explained by a flat line"
        );
        assert!(trend.is_reliable());
        assert!(trend.days_until_full(1_000_000).is_none());
    }

    #[test]
    fn too_few_samples_is_not_a_trend() {
        assert!(fit(&[]).is_none());
        assert!(fit(&points(&[(0, 100)])).is_none());

        // Two points fit a line, but not one worth forecasting from.
        let trend = fit(&points(&[(0, 0), (5, 5000)])).unwrap();
        assert_eq!(trend.samples, 2);
        assert!(!trend.is_reliable(), "two points is below MIN_SAMPLES");
        assert!(trend.days_until_full(10_000).is_none());
    }

    #[test]
    fn a_history_shorter_than_a_day_is_not_extrapolated() {
        // Three scans an hour apart during a build.
        let p = vec![
            Point { at: 0, bytes: 0 },
            Point {
                at: 3600,
                bytes: 1_000_000_000,
            },
            Point {
                at: 7200,
                bytes: 2_000_000_000,
            },
        ];
        let trend = fit(&p).unwrap();
        assert!(trend.span_days < MIN_SPAN_DAYS);
        assert!(!trend.is_reliable());
        assert!(trend.days_until_full(u64::MAX / 2).is_none());
    }

    #[test]
    fn a_noisy_history_is_refused_rather_than_extrapolated() {
        // A one-off restore then a delete: a line through this means nothing.
        let p = points(&[
            (0, 1_000),
            (1, 900_000_000),
            (2, 2_000),
            (3, 850_000_000),
            (4, 3_000),
            (5, 1_000),
        ]);
        let trend = fit(&p).unwrap();
        assert!(trend.r2 < MIN_R2, "r2 was {}", trend.r2);
        assert!(!trend.is_reliable());
        assert!(trend.days_until_full(1_000_000_000).is_none());
    }

    #[test]
    fn a_noisy_but_clearly_rising_history_is_still_usable() {
        // Real growth with jitter on top: the slope is meaningful.
        let gib = 1024f64 * 1024.0 * 1024.0;
        let p: Vec<Point> = (0..14)
            .map(|day| Point {
                at: day * DAY,
                // +1 GiB/day with a ±0.1 GiB wobble.
                bytes: (day as f64 * gib + ((day % 3) as f64 - 1.0) * 0.1 * gib) as u64,
            })
            .collect();
        let trend = fit(&p).unwrap();

        assert!(trend.r2 > 0.99, "r2 was {}", trend.r2);
        assert!(trend.is_reliable());
        let rate_gib = trend.bytes_per_day / gib;
        assert!((rate_gib - 1.0).abs() < 0.05, "rate was {rate_gib} GiB/day");
    }

    #[test]
    fn points_do_not_have_to_arrive_in_order() {
        let gib = 1024u64 * 1024 * 1024;
        let ordered = fit(&points(&[(0, 0), (1, gib), (2, 2 * gib), (3, 3 * gib)])).unwrap();
        let shuffled = fit(&points(&[(2, 2 * gib), (0, 0), (3, 3 * gib), (1, gib)])).unwrap();
        assert_eq!(ordered, shuffled);
    }

    #[test]
    fn samples_all_at_the_same_instant_have_no_slope() {
        let p = vec![
            Point { at: 1000, bytes: 1 },
            Point { at: 1000, bytes: 2 },
            Point { at: 1000, bytes: 3 },
        ];
        assert!(fit(&p).is_none());
    }

    #[test]
    fn an_absurdly_distant_forecast_is_withheld() {
        // 1 byte a day against a terabyte of headroom: technically 3 billion
        // days, which is not a fact about anything.
        let p = points(&[(0, 0), (2, 2), (4, 4), (6, 6)]);
        let trend = fit(&p).unwrap();
        assert!(trend.is_reliable());
        assert!(trend.days_until_full(1024u64.pow(4)).is_none());
    }

    #[test]
    fn a_full_disk_forecasts_zero_days() {
        let gib = 1024u64 * 1024 * 1024;
        let p = points(&[(0, 0), (1, gib), (2, 2 * gib), (3, 3 * gib)]);
        let trend = fit(&p).unwrap();
        assert_eq!(trend.days_until_full(0), Some(0.0));
    }

    #[test]
    fn very_large_byte_counts_do_not_lose_precision() {
        // 100 TiB growing by 1 TiB a day; naive accumulation in f32 would fail.
        let tib = 1024f64.powi(4);
        let p: Vec<Point> = (0..10)
            .map(|day| Point {
                at: day * DAY,
                bytes: (100.0 * tib + day as f64 * tib) as u64,
            })
            .collect();
        let trend = fit(&p).unwrap();
        assert!((trend.bytes_per_day / tib - 1.0).abs() < 1e-6);
        assert!((trend.r2 - 1.0).abs() < 1e-9);
    }
}
