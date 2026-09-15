//! Battery history: one sample a minute persisted to disk, the drain model
//! learnt from those samples, and the record of how earlier estimates
//! turned out.
//!
//! The kernel's `power_now` is a snapshot of this instant's draw, so an
//! estimate built on it alone swings with every burst of CPU work. The
//! model here instead watches how fast the charge has actually been moving
//! during the current session, blends that with the instant reading, and
//! shapes the rest of the curve with the drain observed at each 10% band
//! on earlier sessions (lithium cells drain faster in percent terms near
//! the bottom). Every sample also records the prediction made at that
//! moment, so once the session has run on, the prediction can be scored
//! against what the battery really did.

use crate::power::{BatteryInfo, PowerState};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Charge is split into ten bands: band 0 is 0–9%, band 9 is 90–100%.
pub const BANDS: usize = 10;
/// Samples older than this are dropped when the history loads.
const MAX_AGE_SECS: u64 = 30 * 24 * 3600;
/// A silence longer than this (suspend, app closed) ends a session.
pub const SESSION_GAP_SECS: u64 = 10 * 60;
/// How far back the session-rate window looks.
const RECENT_WINDOW_SECS: u64 = 30 * 60;
/// Inside that window, a minute this old counts half as much as the latest
/// one: a change in draw that lasts is adopted within about ten minutes,
/// while a brief burst barely registers.
const RATE_HALF_LIFE_MINUTES: f64 = 8.0;
/// Minutes of observation before the observed rate outweighs the instant
/// reading entirely.
const FULL_TRUST_MINUTES: f64 = 20.0;
/// At least this many samples a minute apart are kept, and one more is
/// taken whenever the percent or the state changes.
const SAMPLE_INTERVAL_SECS: u64 = 60;
/// A prediction is only scored once the session has run this much longer
/// and moved this many percent, so the realized rate means something.
const SCORE_MIN_SECS: u64 = 15 * 60;
const SCORE_MIN_PERCENT: f64 = 3.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    /// Unix seconds.
    pub t: u64,
    pub percent: u8,
    pub energy_wh: f64,
    pub energy_full_wh: f64,
    pub power_w: f64,
    pub state: PowerState,
    /// Minutes to empty or to full the model predicted at this moment.
    pub predicted_min: Option<u32>,
    /// The same, from the instant power reading alone.
    pub instant_min: Option<u32>,
}

impl Sample {
    /// Where the charge stands, as precisely as the kernel allows: the
    /// energy counter has micro-watt-hour resolution, the percent one point.
    pub fn level(&self) -> f64 {
        if self.energy_full_wh > 0.0 && self.energy_wh > 0.0 {
            self.energy_wh / self.energy_full_wh * 100.0
        } else {
            self.percent as f64
        }
    }

    fn band(&self) -> usize {
        (self.percent as usize / 10).min(BANDS - 1)
    }
}

/// Charge progress between two samples of one session, in the direction of
/// that session's state: percent drained on battery, percent gained while
/// charging. Negative means the charge moved the wrong way.
fn progress(from: &Sample, to: &Sample) -> f64 {
    match from.state {
        PowerState::Charging => to.level() - from.level(),
        _ => from.level() - to.level(),
    }
}

fn hours_between(from: &Sample, to: &Sample) -> f64 {
    to.t.saturating_sub(from.t) as f64 / 3600.0
}

/// How the estimate's rate was arrived at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// Nothing observed yet; the instant power reading alone.
    Instant,
    /// The rate observed this session, blended with the instant reading.
    Observed,
    /// Observed rate plus the per-band curve from earlier sessions.
    Learned,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    pub state: PowerState,
    /// Model minutes to empty or to full.
    pub minutes: Option<u32>,
    /// Naive minutes from the instant reading.
    pub instant_minutes: Option<u32>,
    /// The blended rate driving the current band, percent per hour.
    pub rate_pct_hr: Option<f64>,
    /// The rate observed this session, percent per hour, and over how long.
    pub observed_pct_hr: Option<f64>,
    pub observed_minutes: u32,
    pub basis: Basis,
}

impl Estimate {
    fn idle(state: PowerState) -> Self {
        Self {
            state,
            minutes: None,
            instant_minutes: None,
            rate_pct_hr: None,
            observed_pct_hr: None,
            observed_minutes: 0,
            basis: Basis::Instant,
        }
    }

    /// "3 hr 12 min left", "1 hr 05 min to full", "Fully charged".
    pub fn headline(&self, battery: &BatteryInfo) -> String {
        match (self.state, self.minutes) {
            (PowerState::OnBattery, Some(m)) => format!("{} left", duration_text(m)),
            (PowerState::OnBattery, None) => "Calculating runtime…".into(),
            (PowerState::Charging, Some(m)) => format!("{} to full", duration_text(m)),
            (PowerState::Charging, None) => "Charging…".into(),
            (PowerState::PluggedIn, _) if battery.percent >= 99 => "Fully charged".into(),
            (PowerState::PluggedIn, _) => format!("Holding at {}%", battery.percent),
        }
    }

    /// The compact form for the sidebar: "3 hr 12 min", "1 hr 05 min to full".
    pub fn short(&self, battery: &BatteryInfo) -> String {
        match (self.state, self.minutes) {
            (PowerState::OnBattery, Some(m)) => duration_text(m),
            (PowerState::OnBattery, None) => "Calculating…".into(),
            (PowerState::Charging, Some(m)) => format!("{} to full", duration_text(m)),
            (PowerState::Charging, None) => "Charging".into(),
            (PowerState::PluggedIn, _) if battery.percent >= 99 => "Full".into(),
            (PowerState::PluggedIn, _) => "Not charging".into(),
        }
    }

    pub fn basis_text(&self) -> &'static str {
        match self.basis {
            Basis::Instant => "From this instant's power reading only",
            Basis::Observed => "From the drain observed this session",
            Basis::Learned => "Observed drain, shaped by earlier sessions",
        }
    }
}

pub fn duration_text(minutes: u32) -> String {
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{} hr {:02} min", minutes / 60, minutes % 60)
    }
}

/// One past prediction next to what the battery went on to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    pub t: u64,
    pub percent: u8,
    pub state: PowerState,
    pub predicted_min: u32,
    pub instant_min: Option<u32>,
    /// Minutes the rest of the charge would really have taken at the rate
    /// realized after this moment.
    pub realized_min: u32,
}

impl Checkpoint {
    pub fn error_min(&self) -> i64 {
        self.predicted_min as i64 - self.realized_min as i64
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Accuracy {
    pub checkpoints: usize,
    /// Mean absolute error of the model, in minutes.
    pub model_error_min: f64,
    /// Mean absolute error of the instant reading, in minutes.
    pub instant_error_min: Option<f64>,
    /// Signed mean error: positive means the model promised too much.
    pub model_bias_min: f64,
    /// Share of model predictions within ten percent of what happened.
    pub within_ten_percent: f64,
}

#[derive(Debug, Default)]
pub struct History {
    samples: Vec<Sample>,
    /// `None` keeps the history in memory only (demo data, tests).
    path: Option<PathBuf>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl History {
    pub fn load() -> Self {
        let path = data_path();
        let mut samples: Vec<Sample> = fs::read_to_string(&path)
            .map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default();
        samples.sort_by_key(|s| s.t);
        let cutoff = now().saturating_sub(MAX_AGE_SECS);
        let before = samples.len();
        samples.retain(|s| s.t >= cutoff);
        let pruned = samples.len() != before;
        let history = Self {
            samples,
            path: Some(path),
        };
        if pruned {
            history.rewrite();
        }
        history
    }

    pub fn in_memory(samples: Vec<Sample>) -> Self {
        Self {
            samples,
            path: None,
        }
    }

    /// Eight hours of plausible use for a computer with no battery: a
    /// morning on the adapter, a discharge through the afternoon, a top-up.
    pub fn demo(now: u64) -> Self {
        let mut samples = Vec::new();
        let start = now.saturating_sub(8 * 3600);
        let full = 67.2;
        let mut level = 100.0_f64;
        let mut t = start;
        while t <= now {
            let elapsed = (t - start) as f64 / 60.0;
            let (state, delta, power) = if elapsed < 60.0 {
                (PowerState::PluggedIn, 0.0, 0.0)
            } else if elapsed < 360.0 {
                // A drain that speeds up as the cell empties, with a lumpy
                // burst of work in the middle.
                let burst = if (170.0..200.0).contains(&elapsed) {
                    1.8
                } else {
                    1.0
                };
                let shape = 1.0 + (100.0 - level) / 200.0;
                let rate = 14.0 * shape * burst;
                (PowerState::OnBattery, -rate / 60.0, rate / 100.0 * full)
            } else if level < 96.0 {
                (PowerState::Charging, 40.0 / 60.0, 28.0)
            } else {
                (PowerState::PluggedIn, 0.0, 0.0)
            };
            level = (level + delta).clamp(0.0, 100.0);
            samples.push(Sample {
                t,
                percent: level.round() as u8,
                energy_wh: level / 100.0 * full,
                energy_full_wh: full,
                power_w: power,
                state,
                predicted_min: None,
                instant_min: None,
            });
            t += SAMPLE_INTERVAL_SECS;
        }
        let mut history = Self::in_memory(Vec::new());
        // Replay the samples so each carries the prediction the model would
        // have made at that moment, which is what the accuracy view scores.
        for sample in samples {
            let battery = BatteryInfo {
                percent: sample.percent,
                status: match sample.state {
                    PowerState::OnBattery => "Discharging",
                    PowerState::Charging => "Charging",
                    PowerState::PluggedIn => "Full",
                }
                .into(),
                power_watts: sample.power_w,
                energy_now_wh: sample.energy_wh,
                energy_full_wh: sample.energy_full_wh,
                ..Default::default()
            };
            let estimate = history.estimate(&battery, sample.t);
            history.samples.push(Sample {
                predicted_min: estimate.minutes,
                instant_min: estimate.instant_minutes,
                ..sample
            });
        }
        history
    }

    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// Take a sample now, unless the last one is recent and nothing has
    /// changed. Returns whether a sample was added.
    pub fn record(&mut self, battery: &BatteryInfo, estimate: &Estimate, now: u64) -> bool {
        let state = battery.state();
        if let Some(last) = self.samples.last() {
            let unchanged = last.percent == battery.percent && last.state == state;
            if unchanged && now < last.t + SAMPLE_INTERVAL_SECS {
                return false;
            }
            if now <= last.t {
                return false;
            }
        }
        let sample = Sample {
            t: now,
            percent: battery.percent,
            energy_wh: battery.energy_now_wh,
            energy_full_wh: battery.energy_full_wh,
            power_w: battery.power_watts,
            state,
            predicted_min: estimate.minutes,
            instant_min: estimate.instant_minutes,
        };
        self.append_line(&sample);
        self.samples.push(sample);
        true
    }

    fn append_line(&self, sample: &Sample) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(line) = serde_json::to_string(sample) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }

    fn rewrite(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let text: String = self
            .samples
            .iter()
            .filter_map(|s| serde_json::to_string(s).ok())
            .map(|line| line + "\n")
            .collect();
        let _ = fs::write(path, text);
    }

    /// Contiguous runs of one state with no long silence inside them.
    pub fn sessions(&self) -> Vec<&[Sample]> {
        let mut sessions = Vec::new();
        let mut start = 0;
        for i in 1..=self.samples.len() {
            let ends = i == self.samples.len()
                || self.samples[i].state != self.samples[start].state
                || self.samples[i].t.saturating_sub(self.samples[i - 1].t) > SESSION_GAP_SECS;
            if ends {
                sessions.push(&self.samples[start..i]);
                start = i;
            }
        }
        sessions
    }

    /// The session still running at `now`, if it is in `state`.
    fn current_session(&self, state: PowerState, now: u64) -> Option<&[Sample]> {
        let session = self.sessions().pop()?;
        let last = session.last()?;
        (last.state == state && now.saturating_sub(last.t) <= SESSION_GAP_SECS).then_some(session)
    }

    /// The rate over the last half hour of a session, percent per hour,
    /// weighted towards the latest minutes, and the minutes it was observed
    /// over. Movement and time are summed separately before dividing, so a
    /// gauge that updates in steps averages out rather than spiking.
    fn recent_rate(session: &[Sample], now: u64) -> Option<(f64, u32)> {
        let from = now.saturating_sub(RECENT_WINDOW_SECS);
        let (mut moved, mut hours) = (0.0, 0.0);
        let (mut first, mut last) = (None, None);
        for pair in session.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if b.t < from {
                continue;
            }
            let age_minutes = now.saturating_sub(b.t) as f64 / 60.0;
            let weight = 0.5_f64.powf(age_minutes / RATE_HALF_LIFE_MINUTES);
            moved += weight * progress(a, b);
            hours += weight * hours_between(a, b);
            first.get_or_insert(a);
            last = Some(b);
        }
        let span = hours_between(first?, last?) * 60.0;
        if span < 1.0 || moved <= 0.0 || hours <= 0.0 {
            return None;
        }
        Some((moved / hours, span.round() as u32))
    }

    /// Percent per hour observed in each band across every session of
    /// `state`, weighted by time spent there.
    pub fn band_rates(&self, state: PowerState) -> [Option<f64>; BANDS] {
        let mut moved = [0.0_f64; BANDS];
        let mut hours = [0.0_f64; BANDS];
        for session in self.sessions().into_iter().filter(|s| s[0].state == state) {
            for pair in session.windows(2) {
                let band = pair[0].band();
                moved[band] += progress(&pair[0], &pair[1]);
                hours[band] += hours_between(&pair[0], &pair[1]);
            }
        }
        let mut rates = [None; BANDS];
        for band in 0..BANDS {
            // Five minutes and a full percent of movement before a band
            // is trusted.
            if hours[band] * 60.0 >= 5.0 && moved[band] >= 1.0 {
                rates[band] = Some(moved[band] / hours[band]);
            }
        }
        rates
    }

    /// The estimate for `battery` at `now`.
    pub fn estimate(&self, battery: &BatteryInfo, now: u64) -> Estimate {
        let state = battery.state();
        if state == PowerState::PluggedIn {
            return Estimate::idle(state);
        }
        let instant_minutes = battery.instant_minutes();
        let points = remaining_points(battery.percent, state);
        let instant_rate = instant_minutes
            .filter(|m| *m > 0)
            .map(|m| points / (m as f64 / 60.0));
        let observed = self
            .current_session(state, now)
            .and_then(|session| Self::recent_rate(session, now));
        let (current, observed_pct_hr, observed_minutes, basis) = match (observed, instant_rate) {
            (Some((rate, minutes)), Some(instant)) => {
                let trust = (minutes as f64 / FULL_TRUST_MINUTES).min(1.0);
                (
                    trust * rate + (1.0 - trust) * instant,
                    Some(rate),
                    minutes,
                    Basis::Observed,
                )
            }
            (Some((rate, minutes)), None) => (rate, Some(rate), minutes, Basis::Observed),
            (None, Some(instant)) => (instant, None, 0, Basis::Instant),
            (None, None) => {
                return Estimate {
                    instant_minutes,
                    ..Estimate::idle(state)
                };
            }
        };
        let bands = self.band_rates(state);
        let here = (battery.percent as usize / 10).min(BANDS - 1);
        // The curve is only "shaped" once a band other than the one being
        // crossed right now has been seen; the current band's own history
        // says nothing the observed rate does not.
        let shaped = bands
            .iter()
            .enumerate()
            .any(|(band, rate)| band != here && rate.is_some());
        let (minutes, basis) = if shaped {
            (
                walk(battery.percent, state, current, &bands),
                Basis::Learned,
            )
        } else {
            (Some((points / current * 60.0).round() as u32), basis)
        };
        Estimate {
            state,
            minutes,
            instant_minutes,
            rate_pct_hr: Some(current),
            observed_pct_hr,
            observed_minutes,
            basis,
        }
    }

    /// Every past prediction that can now be scored, oldest first.
    pub fn checkpoints(&self) -> Vec<Checkpoint> {
        let mut out = Vec::new();
        for session in self.sessions() {
            if session[0].state == PowerState::PluggedIn {
                continue;
            }
            let Some(last) = session.last() else {
                continue;
            };
            for sample in session {
                let Some(predicted) = sample.predicted_min else {
                    continue;
                };
                if last.t.saturating_sub(sample.t) < SCORE_MIN_SECS {
                    continue;
                }
                let moved = progress(sample, last);
                if moved < SCORE_MIN_PERCENT {
                    continue;
                }
                let rate = moved / hours_between(sample, last);
                let realized = remaining_points(sample.percent, sample.state) / rate * 60.0;
                out.push(Checkpoint {
                    t: sample.t,
                    percent: sample.percent,
                    state: sample.state,
                    predicted_min: predicted,
                    instant_min: sample.instant_min,
                    realized_min: realized.round() as u32,
                });
            }
        }
        out
    }

    /// How the predictions since `since` have fared, or `None` with nothing
    /// scored yet.
    pub fn accuracy(&self, since: u64) -> Option<Accuracy> {
        let points: Vec<Checkpoint> = self
            .checkpoints()
            .into_iter()
            .filter(|c| c.t >= since)
            .collect();
        if points.is_empty() {
            return None;
        }
        let n = points.len() as f64;
        let model_error_min = points
            .iter()
            .map(|c| c.error_min().unsigned_abs() as f64)
            .sum::<f64>()
            / n;
        let model_bias_min = points.iter().map(|c| c.error_min() as f64).sum::<f64>() / n;
        let instant: Vec<f64> = points
            .iter()
            .filter_map(|c| {
                c.instant_min
                    .map(|m| (m as i64 - c.realized_min as i64).unsigned_abs() as f64)
            })
            .collect();
        let instant_error_min =
            (!instant.is_empty()).then(|| instant.iter().sum::<f64>() / instant.len() as f64);
        let within = points
            .iter()
            .filter(|c| c.error_min().unsigned_abs() as f64 <= 0.10 * c.realized_min.max(1) as f64)
            .count() as f64;
        Some(Accuracy {
            checkpoints: points.len(),
            model_error_min,
            instant_error_min,
            model_bias_min,
            within_ten_percent: within / n,
        })
    }
}

/// Percent still to travel: down to empty on battery, up to full charging.
fn remaining_points(percent: u8, state: PowerState) -> f64 {
    match state {
        PowerState::Charging => (100 - percent.min(100)) as f64,
        _ => percent as f64,
    }
}

/// Minutes to walk the remaining charge one percent at a time. The current
/// band moves at `current`; the others at their learned rate, scaled so the
/// learned curve passes through today's rate where the two meet. With no
/// learned rate for the band being crossed, the current rate stands in.
fn walk(percent: u8, state: PowerState, current: f64, bands: &[Option<f64>; BANDS]) -> Option<u32> {
    if current <= 0.0 {
        return None;
    }
    let here = (percent as usize / 10).min(BANDS - 1);
    let anchor = bands[here].or_else(|| {
        let known: Vec<f64> = bands.iter().flatten().copied().collect();
        (!known.is_empty()).then(|| known.iter().sum::<f64>() / known.len() as f64)
    });
    let scale = anchor
        .map(|learned| (current / learned).clamp(0.5, 2.0))
        .unwrap_or(1.0);
    let points: Vec<u8> = match state {
        PowerState::Charging => (percent..100).collect(),
        _ => (1..=percent).collect(),
    };
    let mut hours = 0.0;
    for point in points {
        let band = (point as usize / 10).min(BANDS - 1);
        let rate = if band == here {
            current
        } else {
            bands[band].map(|r| r * scale).unwrap_or(current)
        };
        hours += 1.0 / rate;
    }
    Some((hours * 60.0).round() as u32)
}

fn data_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("raven-power/history.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(percent: u8, status: &str, power: f64) -> BatteryInfo {
        BatteryInfo {
            percent,
            status: status.into(),
            power_watts: power,
            energy_now_wh: percent as f64,
            energy_full_wh: 100.0,
            ..Default::default()
        }
    }

    /// A steady discharge at `pct_hr` from `start_pct`, one sample a minute.
    fn discharge(t0: u64, start_pct: f64, pct_hr: f64, minutes: u64) -> Vec<Sample> {
        (0..=minutes)
            .map(|m| {
                let level = start_pct - pct_hr * m as f64 / 60.0;
                Sample {
                    t: t0 + m * 60,
                    percent: level.round() as u8,
                    energy_wh: level,
                    energy_full_wh: 100.0,
                    power_w: pct_hr,
                    state: PowerState::OnBattery,
                    predicted_min: None,
                    instant_min: None,
                }
            })
            .collect()
    }

    #[test]
    fn instant_reading_alone_when_nothing_is_observed() {
        let history = History::in_memory(Vec::new());
        let estimate = history.estimate(&battery(50, "Discharging", 10.0), 1000);
        assert_eq!(estimate.basis, Basis::Instant);
        assert_eq!(estimate.minutes, Some(300));
        assert_eq!(estimate.instant_minutes, Some(300));
    }

    #[test]
    fn observed_drain_outweighs_a_momentary_burst() {
        // Half an hour at 10%/h, then a burst that reads 30 W this instant.
        let samples = discharge(0, 60.0, 10.0, 30);
        let history = History::in_memory(samples);
        let estimate = history.estimate(&battery(55, "Discharging", 30.0), 30 * 60);
        assert_eq!(estimate.basis, Basis::Observed);
        assert_eq!(estimate.observed_minutes, 30);
        // Full trust in the observed rate: 55% at 10%/h.
        assert_eq!(estimate.minutes, Some(330));
        // The naive figure would promise far less.
        assert_eq!(estimate.instant_minutes, Some(110));
    }

    #[test]
    fn partial_observation_blends_with_the_instant_reading() {
        let samples = discharge(0, 60.0, 10.0, 15);
        let history = History::in_memory(samples);
        let estimate = history.estimate(&battery(57, "Discharging", 20.0), 15 * 60);
        let rate = estimate.rate_pct_hr.unwrap();
        // Fifteen of the twenty minutes of trust: three quarters of the way
        // from 20 %/h down to the observed 10.
        assert!((rate - 12.5).abs() < 0.01, "rate {rate}");
    }

    #[test]
    fn a_sustained_change_in_draw_is_adopted_within_minutes() {
        // Thirty quiet minutes at 10%/h, then ten minutes of heavy work at
        // 30%/h that is still going on.
        let mut samples = discharge(0, 60.0, 10.0, 30);
        samples.extend(discharge(31 * 60, 54.5, 30.0, 9));
        let history = History::in_memory(samples);
        let estimate = history.estimate(&battery(50, "Discharging", 30.0), 40 * 60);
        let rate = estimate.rate_pct_hr.unwrap();
        // Well past halfway to the new rate; a plain half-hour average
        // would still say 15.
        assert!((20.0..25.0).contains(&rate), "rate {rate}");
        // And a two-minute blip moves it very little.
        let mut samples = discharge(0, 60.0, 10.0, 38);
        samples.extend(discharge(39 * 60, 53.5, 30.0, 1));
        let history = History::in_memory(samples);
        let estimate = history.estimate(&battery(53, "Discharging", 30.0), 40 * 60);
        let rate = estimate.rate_pct_hr.unwrap();
        assert!(rate < 14.0, "rate {rate}");
    }

    #[test]
    fn learned_bands_shape_the_lower_curve() {
        // An earlier session drained 40–50% at 10%/h and 30–40% at 20%/h.
        let mut samples = discharge(0, 49.0, 10.0, 54);
        samples.extend(discharge(2 * 3600, 39.0, 20.0, 30));
        let history = History::in_memory(samples);
        let bands = history.band_rates(PowerState::OnBattery);
        assert!((bands[4].unwrap() - 10.0).abs() < 0.1);
        assert!((bands[3].unwrap() - 20.0).abs() < 0.1);
        // A new session, much later, draining the 40s at 10%/h again: the
        // 30s should be expected to go twice as fast, the rest at today's
        // rate: 30 + 30 + 174 minutes.
        let day = 24 * 3600;
        let later = discharge(day, 49.0, 10.0, 30);
        let history = History::in_memory(history.samples.iter().cloned().chain(later).collect());
        let estimate = history.estimate(&battery(44, "Discharging", 10.0), day + 30 * 60);
        assert_eq!(estimate.basis, Basis::Learned);
        assert_eq!(estimate.minutes, Some(234));
    }

    #[test]
    fn charging_counts_up_to_full() {
        let history = History::in_memory(Vec::new());
        let estimate = history.estimate(&battery(40, "Charging", 30.0), 0);
        assert_eq!(estimate.state, PowerState::Charging);
        assert_eq!(estimate.minutes, Some(120));
        assert_eq!(
            estimate.headline(&battery(40, "Charging", 30.0)),
            "2 hr 00 min to full"
        );
    }

    #[test]
    fn plugged_in_has_no_countdown() {
        let history = History::in_memory(Vec::new());
        let full = battery(100, "Full", 0.0);
        let estimate = history.estimate(&full, 0);
        assert_eq!(estimate.minutes, None);
        assert_eq!(estimate.headline(&full), "Fully charged");
        let held = battery(80, "Not charging", 0.0);
        assert_eq!(history.estimate(&held, 0).headline(&held), "Holding at 80%");
    }

    #[test]
    fn a_long_silence_splits_sessions() {
        let mut samples = discharge(0, 90.0, 10.0, 10);
        samples.extend(discharge(3600, 80.0, 10.0, 10));
        let history = History::in_memory(samples);
        assert_eq!(history.sessions().len(), 2);
        // Nothing current: the last sample is an hour old.
        let estimate = history.estimate(&battery(70, "Discharging", 10.0), 3 * 3600);
        assert_eq!(estimate.basis, Basis::Learned);
        assert_eq!(estimate.observed_pct_hr, None);
    }

    #[test]
    fn checkpoints_score_predictions_against_the_realized_rate() {
        let mut samples = discharge(0, 60.0, 10.0, 60);
        // The prediction at the start promised 6 hours; a naive meter, 3.
        samples[0].predicted_min = Some(360);
        samples[0].instant_min = Some(180);
        let history = History::in_memory(samples);
        let points = history.checkpoints();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].realized_min, 360);
        assert_eq!(points[0].error_min(), 0);
        let accuracy = history.accuracy(0).unwrap();
        assert_eq!(accuracy.checkpoints, 1);
        assert_eq!(accuracy.model_error_min, 0.0);
        assert_eq!(accuracy.instant_error_min, Some(180.0));
        assert_eq!(accuracy.within_ten_percent, 1.0);
    }

    #[test]
    fn record_keeps_one_sample_a_minute_unless_something_changes() {
        let mut history = History::in_memory(Vec::new());
        let estimate = Estimate::idle(PowerState::OnBattery);
        assert!(history.record(&battery(50, "Discharging", 5.0), &estimate, 100));
        assert!(!history.record(&battery(50, "Discharging", 5.0), &estimate, 120));
        assert!(history.record(&battery(49, "Discharging", 5.0), &estimate, 130));
        assert!(!history.record(&battery(49, "Discharging", 5.0), &estimate, 150));
        assert!(history.record(&battery(49, "Discharging", 5.0), &estimate, 190));
        assert!(history.record(&battery(49, "Charging", 5.0), &estimate, 195));
        assert_eq!(history.samples().len(), 4);
    }

    #[test]
    fn demo_history_scores_its_own_predictions() {
        let history = History::demo(1_800_000_000);
        assert!(history.sessions().len() >= 3);
        assert!(history.accuracy(0).is_some());
        assert!(
            history
                .band_rates(PowerState::OnBattery)
                .iter()
                .any(Option::is_some)
        );
    }
}
