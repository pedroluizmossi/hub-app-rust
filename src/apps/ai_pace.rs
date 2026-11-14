//! Forecast of when a usage limit runs out, from the pace of the latest readings.
//!
//! The % used is a counter: it only goes up until the window renews, and it has no
//! measurement noise. The pace over an interval is the difference between its ends divided
//! by the time. Since usage comes in bursts, the pace is measured over two intervals (the
//! two-window "burn rate"): the long one gives the forecast shown, which doesn't jump with
//! every reading; the short one says whether the spending is still going on now. Only both
//! together agitate the mascot: an isolated burst or usage that already stopped don't count.
use std::collections::VecDeque;

/// Short interval: "is it spending now?".
const SHORT: i64 = 20 * 60;
/// Long interval: "is it a pattern or was it a burst?". Also how much history is kept.
const LONG: i64 = 60 * 60;
/// Minimum history for each pace to count (below that, two readings become a false alarm).
const SHORT_MIN: i64 = 5 * 60;
const LONG_MIN: i64 = 15 * 60;
/// Drop in the % used (in points) that means the window renewed.
const DROP: f64 = 1.0;
/// Change in the renewal time (s) that means another window.
const RESET_SHIFT: i64 = 10 * 60;

/// Smallest change (in points) that fills the whole height of the chart: below that the
/// line is proportionally lower, so half a point doesn't look like a jump.
const SPARK_RANGE: f64 = 5.0;

/// The limit runs out before it renews, if the pace keeps up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forecast {
    /// When it reaches 100% (seconds since the epoch), by the long pace.
    pub ends_at: i64,
    /// The short pace also points to running out before the renewal.
    pub strained: bool,
}

/// Recent readings of a usage window: (time, % used), oldest to newest.
#[derive(Default)]
pub struct History {
    samples: VecDeque<(i64, f64)>,
    resets_at: i64,
}

impl History {
    /// Stores a reading taken at `at`. Starts over when the window renewed.
    pub fn record(&mut self, at: i64, used: f64, resets_at: i64) {
        let renewed = self.samples.back().is_some_and(|&(_, last)| used < last - DROP)
            || (self.resets_at - resets_at).abs() > RESET_SHIFT;
        if renewed { self.samples.clear(); }
        self.resets_at = resets_at;
        if self.samples.back().is_some_and(|&(t, _)| at <= t) { return; }
        self.samples.push_back((at, used));
        while self.samples.front().is_some_and(|&(t, _)| at - t > LONG) { self.samples.pop_front(); }
    }

    /// Chart of the last hour as an SVG path in a 1000×100 box: the newest reading sits on
    /// the right edge and the height follows how much the usage went up. Empty with fewer
    /// than two readings.
    pub fn spark(&self) -> String {
        let (Some(&(end, _)), true) = (self.samples.back(), self.samples.len() >= 2) else { return String::new() };
        let low = self.samples.iter().map(|&(_, u)| u).fold(f64::INFINITY, f64::min);
        let high = self.samples.iter().map(|&(_, u)| u).fold(low, f64::max);
        let range = (high - low).max(SPARK_RANGE);
        self.samples.iter().enumerate().map(|(i, &(t, u))| {
            let x = 1000.0 - (end - t) as f64 * 1000.0 / LONG as f64;
            let y = 98.0 - (u - low) / range * 96.0;
            format!("{} {x:.1} {y:.1}", if i == 0 { "M" } else { "L" })
        }).collect::<Vec<_>>().join(" ")
    }

    /// Pace (points per second) since the oldest reading within `span`. If the readings
    /// are spaced further apart than `span`, the one before the last is used.
    fn rate(&self, span: i64, min: i64) -> Option<f64> {
        let &(now, used) = self.samples.back()?;
        let older = self.samples.len() - 1;
        let &(t, u) = self.samples.iter().take(older).find(|&&(t, _)| now - t <= span)
            .or_else(|| older.checked_sub(1).and_then(|i| self.samples.get(i)))?;
        (now - t >= min).then(|| (used - u) / (now - t) as f64)
    }

    /// Forecast as seen at `now`. `None` if, at this pace, the window renews before running
    /// out (or if there isn't enough history to tell yet).
    pub fn forecast(&self, now: i64) -> Option<Forecast> {
        let &(at, used) = self.samples.back()?;
        let ends = |rate: f64| (rate > 0.0 && used < 100.0)
            .then(|| at.saturating_add(((100.0 - used) / rate) as i64));
        let ends_at = self.rate(LONG, LONG_MIN).and_then(ends).filter(|t| *t > now && *t < self.resets_at)?;
        let strained = self.rate(SHORT, SHORT_MIN).and_then(ends).is_some_and(|t| t < self.resets_at);
        Some(Forecast { ends_at, strained })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60;

    /// History with a reading every `step` minutes; `used(minute)` gives the % at each one.
    fn history(minutes: i64, step: i64, resets_at: i64, used: impl Fn(i64) -> f64) -> History {
        let mut h = History::default();
        for m in (0..=minutes).step_by(step as usize) { h.record(m * MIN, used(m), resets_at); }
        h
    }

    #[test]
    fn steady_pace_runs_out_before_renewal() {
        // 40% → 70% in 60 min (0.5 point/min): 30 points left, 60 more minutes.
        let h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(60 * MIN), Some(Forecast { ends_at: 120 * MIN, strained: true }));
    }

    #[test]
    fn renewal_before_running_out_has_no_forecast() {
        let h = history(60, 3, 90 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(60 * MIN), None);
    }

    #[test]
    fn stopped_usage_forecasts_but_does_not_agitate() {
        // Went up 30 points in 30 min and stopped: the long pace still points, the short one is zero.
        let h = history(60, 3, 180 * MIN, |m| 40.0 + m.min(30) as f64);
        let f = h.forecast(60 * MIN).unwrap();
        assert!(!f.strained);
        assert_eq!(f.ends_at, 120 * MIN);
    }

    #[test]
    fn isolated_burst_has_no_forecast() {
        // Idle for 45 min, then 6 points in 15 min: the short pace would run out in ~2 h, but
        // over the whole hour it was only 6 points (the long one would take over 8 h; renews in 5 h).
        let h = history(60, 3, 360 * MIN, |m| 50.0 + (m - 45).max(0) as f64 * 0.4);
        assert_eq!(h.forecast(60 * MIN), None);
    }

    #[test]
    fn little_history_has_no_forecast() {
        let h = history(12, 3, 180 * MIN, |m| 40.0 + m as f64 * 2.0);
        assert_eq!(h.forecast(12 * MIN), None);
    }

    #[test]
    fn renewal_clears_the_history() {
        let mut h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        h.record(63 * MIN, 2.0, 480 * MIN);
        assert_eq!(h.forecast(63 * MIN), None);
        assert_eq!(h.samples.len(), 1);
    }

    #[test]
    fn repeated_reading_does_not_count_twice() {
        let mut h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        let n = h.samples.len();
        h.record(60 * MIN, 70.0, 180 * MIN);
        assert_eq!(h.samples.len(), n);
    }

    #[test]
    fn sparse_readings_use_the_last_interval() {
        // One reading every 30 min: there's no reading within the short interval.
        let h = history(60, 30, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(60 * MIN), Some(Forecast { ends_at: 120 * MIN, strained: true }));
    }

    #[test]
    fn chart_of_the_last_hour() {
        // 30 min of readings: fills the right half; a 10-point rise, the whole height.
        let h = history(30, 15, 0, |m| 40.0 + m as f64 / 3.0);
        assert_eq!(h.spark(), "M 500.0 98.0 L 750.0 50.0 L 1000.0 2.0");
        // 1-point rise: a fifth of the height
        let h = history(60, 60, 0, |m| 40.0 + m as f64 / 60.0);
        assert_eq!(h.spark(), "M 0.0 98.0 L 1000.0 78.8");
        assert_eq!(history(0, 3, 0, |_| 40.0).spark(), "");
    }

    #[test]
    fn expired_forecast_goes_away() {
        let h = history(60, 3, 180 * MIN, |m| 40.0 + m as f64 * 0.5);
        assert_eq!(h.forecast(121 * MIN), None);
    }
}
