//! Aggregation of raw observations into what the dashboard and report need:
//! per-second series for charts, HDR histograms for percentiles, label
//! breakdowns, and per-phase snapshots. Pure: time is passed in.

use crate::protocol::{Labels, MetricKind, MetricSpec, Observation};
use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Histogram values are stored as micro-units so sub-millisecond latencies keep precision.
const HIST_SCALE: f64 = 1000.0;
const HIST_MAX: u64 = 3_600_000 * 1000; // one hour in micro-units

fn new_hist() -> Histogram<u64> {
    Histogram::new_with_bounds(1, HIST_MAX, 3).expect("valid histogram bounds")
}

fn to_hist_units(v: f64) -> u64 {
    ((v * HIST_SCALE).round().max(1.0) as u64).min(HIST_MAX)
}

fn from_hist_units(v: u64) -> f64 {
    v as f64 / HIST_SCALE
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Percentiles {
    pub count: u64,
    pub min: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

impl Percentiles {
    pub fn of_histogram(h: &Histogram<u64>) -> Self {
        Self::of(h)
    }

    fn of(h: &Histogram<u64>) -> Self {
        if h.is_empty() {
            return Self::default();
        }
        Self {
            count: h.len(),
            min: from_hist_units(h.min()),
            p50: from_hist_units(h.value_at_quantile(0.50)),
            p90: from_hist_units(h.value_at_quantile(0.90)),
            p95: from_hist_units(h.value_at_quantile(0.95)),
            p99: from_hist_units(h.value_at_quantile(0.99)),
            max: from_hist_units(h.max()),
            mean: h.mean() / HIST_SCALE,
        }
    }
}

/// One second of a histogram metric, for the chart.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct WindowPoint {
    pub second: u64,
    pub count: u64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

#[derive(Debug)]
pub enum Aggregate {
    Counter {
        total: f64,
        per_second: Vec<f64>,
        by_label: BTreeMap<String, f64>,
    },
    Gauge {
        last: f64,
        per_second: Vec<Option<f64>>,
        by_label: BTreeMap<String, f64>,
    },
    Histogram {
        all: Histogram<u64>,
        current_window: Histogram<u64>,
        windows: Vec<WindowPoint>,
        by_label: BTreeMap<String, Histogram<u64>>,
    },
}

#[derive(Debug)]
pub struct Metric {
    pub spec: MetricSpec,
    pub aggregate: Aggregate,
}

impl Metric {
    fn new(spec: MetricSpec) -> Self {
        let aggregate = match spec.kind {
            MetricKind::Counter => Aggregate::Counter {
                total: 0.0,
                per_second: vec![0.0],
                by_label: BTreeMap::new(),
            },
            MetricKind::Gauge => Aggregate::Gauge {
                last: f64::NAN,
                per_second: vec![None],
                by_label: BTreeMap::new(),
            },
            MetricKind::Histogram => Aggregate::Histogram {
                all: new_hist(),
                current_window: new_hist(),
                windows: Vec::new(),
                by_label: BTreeMap::new(),
            },
        };
        Self { spec, aggregate }
    }

    fn record(&mut self, value: f64, labels: &Labels) {
        let label_key = label_key(labels);
        match &mut self.aggregate {
            Aggregate::Counter {
                total,
                per_second,
                by_label,
            } => {
                *total += value;
                if let Some(last) = per_second.last_mut() {
                    *last += value;
                }
                if let Some(k) = label_key {
                    *by_label.entry(k).or_default() += value;
                }
            }
            Aggregate::Gauge {
                last,
                per_second,
                by_label,
            } => {
                *last = value;
                if let Some(slot) = per_second.last_mut() {
                    *slot = Some(value);
                }
                if let Some(k) = label_key {
                    by_label.insert(k, value);
                }
            }
            Aggregate::Histogram {
                all,
                current_window,
                by_label,
                ..
            } => {
                let v = to_hist_units(value);
                let _ = all.record(v);
                let _ = current_window.record(v);
                if let Some(k) = label_key {
                    let _ = by_label.entry(k).or_insert_with(new_hist).record(v);
                }
            }
        }
    }

    /// Close the current one-second bucket and open the next.
    fn roll(&mut self, closing_second: u64) {
        match &mut self.aggregate {
            Aggregate::Counter { per_second, .. } => per_second.push(0.0),
            Aggregate::Gauge { per_second, last, .. } => {
                let carry = if last.is_nan() { None } else { Some(*last) };
                per_second.push(carry);
            }
            Aggregate::Histogram {
                current_window, windows, ..
            } => {
                if !current_window.is_empty() {
                    let p = Percentiles::of(current_window);
                    windows.push(WindowPoint {
                        second: closing_second,
                        count: p.count,
                        p50: p.p50,
                        p95: p.p95,
                        p99: p.p99,
                    });
                }
                current_window.reset();
            }
        }
    }

    pub fn percentiles(&self) -> Option<Percentiles> {
        match &self.aggregate {
            Aggregate::Histogram { all, .. } => Some(Percentiles::of(all)),
            _ => None,
        }
    }

    /// Headline number for list views: total, last value, or p95.
    pub fn headline(&self) -> String {
        match &self.aggregate {
            Aggregate::Counter { total, per_second, .. } => {
                let closed = per_second.len().saturating_sub(1).clamp(1, 5);
                let recent: f64 = per_second.iter().rev().skip(1).take(5).sum::<f64>() / closed as f64;
                format!("{} total  {:.1}/s", fmt_num(*total), recent)
            }
            Aggregate::Gauge { last, .. } => {
                if last.is_nan() {
                    "-".into()
                } else {
                    fmt_num(*last)
                }
            }
            Aggregate::Histogram { all, .. } => {
                if all.is_empty() {
                    "n=0".into()
                } else {
                    let p = Percentiles::of(all);
                    let unit = self.spec.unit.as_deref().unwrap_or("");
                    format!("n={}  p50 {}{unit}  p95 {}{unit}", p.count, fmt_num(p.p50), fmt_num(p.p95))
                }
            }
        }
    }
}

fn label_key(labels: &Labels) -> Option<String> {
    if labels.is_empty() {
        return None;
    }
    Some(labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "))
}

pub fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        "-".into()
    } else if v.abs() >= 10_000.0 {
        format!("{:.0}", v)
    } else if v.abs() >= 100.0 {
        format!("{:.1}", v)
    } else {
        format!("{:.2}", v)
    }
}

/// Shape of a gauge over a phase. `slope` is the least-squares trend in units per second:
/// positive on a queue means the consumer is not keeping up.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GaugeStats {
    pub samples: u64,
    pub last: f64,
    pub max: f64,
    pub mean: f64,
    pub slope: f64,
}

impl GaugeStats {
    pub fn of(points: &[(u64, f64)]) -> Self {
        if points.is_empty() {
            return Self::default();
        }
        let n = points.len() as f64;
        let mean_x = points.iter().map(|p| p.0 as f64).sum::<f64>() / n;
        let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
        let var_x = points.iter().map(|p| (p.0 as f64 - mean_x).powi(2)).sum::<f64>();
        let cov = points.iter().map(|p| (p.0 as f64 - mean_x) * (p.1 - mean_y)).sum::<f64>();
        Self {
            samples: points.len() as u64,
            last: points[points.len() - 1].1,
            max: points.iter().map(|p| p.1).fold(f64::MIN, f64::max),
            mean: mean_y,
            slope: if var_x > 0.0 { cov / var_x } else { 0.0 },
        }
    }
}

/// Everything recorded during one named phase, for the end-of-run comparison table.
#[derive(Debug, Serialize, Deserialize)]
pub struct PhaseSummary {
    pub name: String,
    pub start_second: u64,
    pub end_second: Option<u64>,
    pub counters: BTreeMap<String, f64>,
    pub counter_labels: BTreeMap<String, BTreeMap<String, f64>>,
    pub histograms: BTreeMap<String, Percentiles>,
    pub gauges: BTreeMap<String, GaugeStats>,
}

impl PhaseSummary {
    pub fn duration_seconds(&self, now_second: u64) -> u64 {
        self.end_second.unwrap_or(now_second).saturating_sub(self.start_second).max(1)
    }
}

#[derive(Debug)]
struct PhaseState {
    name: String,
    start_second: u64,
    counters: BTreeMap<String, f64>,
    counter_labels: BTreeMap<String, BTreeMap<String, f64>>,
    histograms: BTreeMap<String, Histogram<u64>>,
    gauges: BTreeMap<String, Vec<(u64, f64)>>,
}

impl PhaseState {
    fn summary(&self, end_second: Option<u64>) -> PhaseSummary {
        PhaseSummary {
            name: self.name.clone(),
            start_second: self.start_second,
            end_second,
            counters: self.counters.clone(),
            counter_labels: self.counter_labels.clone(),
            histograms: self.histograms.iter().map(|(k, h)| (k.clone(), Percentiles::of(h))).collect(),
            gauges: self.gauges.iter().map(|(k, pts)| (k.clone(), GaugeStats::of(pts))).collect(),
        }
    }
}

#[derive(Debug, Default)]
pub struct MetricStore {
    order: Vec<String>,
    metrics: BTreeMap<String, Metric>,
    current_second: u64,
    phases: Vec<PhaseState>,
    pub unknown_metrics: BTreeMap<String, u64>,
}

impl MetricStore {
    pub fn new(specs: Vec<MetricSpec>) -> Self {
        let mut s = Self::default();
        for spec in specs {
            s.declare(spec);
        }
        s
    }

    pub fn declare(&mut self, spec: MetricSpec) {
        if !self.metrics.contains_key(&spec.name) {
            self.order.push(spec.name.clone());
            self.metrics.insert(spec.name.clone(), Metric::new(spec));
        }
    }

    pub fn names(&self) -> &[String] {
        &self.order
    }

    pub fn get(&self, name: &str) -> Option<&Metric> {
        self.metrics.get(name)
    }

    pub fn current_second(&self) -> u64 {
        self.current_second
    }

    /// Advance wall-clock seconds; closes every bucket between the last known second and `second`.
    pub fn advance_to(&mut self, second: u64) {
        while self.current_second < second {
            for m in self.metrics.values_mut() {
                m.roll(self.current_second);
            }
            self.current_second += 1;
        }
    }

    pub fn record(&mut self, obs: &Observation) {
        let Some(m) = self.metrics.get_mut(&obs.metric) else {
            *self.unknown_metrics.entry(obs.metric.clone()).or_default() += 1;
            return;
        };
        m.record(obs.value, &obs.labels);
        if let Some(phase) = self.phases.last_mut() {
            match m.spec.kind {
                MetricKind::Counter => {
                    *phase.counters.entry(obs.metric.clone()).or_default() += obs.value;
                    if let Some(k) = label_key(&obs.labels) {
                        *phase.counter_labels.entry(obs.metric.clone()).or_default().entry(k).or_default() += obs.value;
                    }
                }
                MetricKind::Histogram => {
                    let _ = phase
                        .histograms
                        .entry(obs.metric.clone())
                        .or_insert_with(new_hist)
                        .record(to_hist_units(obs.value));
                }
                MetricKind::Gauge => phase
                    .gauges
                    .entry(obs.metric.clone())
                    .or_default()
                    .push((self.current_second, obs.value)),
            }
        }
    }

    pub fn begin_phase(&mut self, name: &str) {
        self.phases.push(PhaseState {
            name: name.into(),
            start_second: self.current_second,
            counters: BTreeMap::new(),
            counter_labels: BTreeMap::new(),
            histograms: BTreeMap::new(),
            gauges: BTreeMap::new(),
        });
    }

    pub fn current_phase(&self) -> Option<&str> {
        self.phases.last().map(|p| p.name.as_str())
    }

    /// The whole run folded into one PhaseSummary so thresholds evaluate identically overall and per phase.
    pub fn overall_summary(&self) -> PhaseSummary {
        let mut counters = BTreeMap::new();
        let mut counter_labels = BTreeMap::new();
        let mut histograms = BTreeMap::new();
        let mut gauges = BTreeMap::new();
        for (name, m) in &self.metrics {
            match &m.aggregate {
                Aggregate::Counter { total, by_label, .. } => {
                    counters.insert(name.clone(), *total);
                    counter_labels.insert(name.clone(), by_label.clone());
                }
                Aggregate::Histogram { all, .. } => {
                    histograms.insert(name.clone(), Percentiles::of(all));
                }
                Aggregate::Gauge { per_second, .. } => {
                    let pts: Vec<(u64, f64)> = per_second
                        .iter()
                        .enumerate()
                        .filter_map(|(i, v)| v.map(|v| (i as u64, v)))
                        .collect();
                    gauges.insert(name.clone(), GaugeStats::of(&pts));
                }
            }
        }
        PhaseSummary {
            name: "overall".into(),
            start_second: 0,
            end_second: Some(self.current_second.max(1)),
            counters,
            counter_labels,
            histograms,
            gauges,
        }
    }

    pub fn phase_summaries(&self) -> Vec<PhaseSummary> {
        let n = self.phases.len();
        self.phases
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let end = if i + 1 < n { Some(self.phases[i + 1].start_second) } else { None };
                p.summary(end)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(metric: &str, value: f64) -> Observation {
        Observation {
            metric: metric.into(),
            value,
            labels: Labels::new(),
        }
    }

    #[test]
    fn counter_buckets_per_second() {
        let mut s = MetricStore::new(vec![MetricSpec::counter("reqs", "")]);
        s.record(&obs("reqs", 1.0));
        s.record(&obs("reqs", 1.0));
        s.advance_to(1);
        s.record(&obs("reqs", 1.0));
        s.advance_to(3);
        let Aggregate::Counter { total, per_second, .. } = &s.get("reqs").unwrap().aggregate else {
            panic!()
        };
        assert_eq!(*total, 3.0);
        assert_eq!(per_second, &vec![2.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn histogram_percentiles_and_windows() {
        let mut s = MetricStore::new(vec![MetricSpec::histogram("lat", "ms", "")]);
        for v in [10.0, 20.0, 30.0, 40.0, 100.0] {
            s.record(&obs("lat", v));
        }
        s.advance_to(1);
        let m = s.get("lat").unwrap();
        let p = m.percentiles().unwrap();
        assert_eq!(p.count, 5);
        assert!((p.max - 100.0).abs() < 0.2);
        assert!((p.p50 - 30.0).abs() < 0.2);
        let Aggregate::Histogram { windows, .. } = &m.aggregate else {
            panic!()
        };
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].count, 5);
    }

    #[test]
    fn phases_partition_counters() {
        let mut s = MetricStore::new(vec![MetricSpec::counter("reqs", "")]);
        s.begin_phase("a");
        s.record(&obs("reqs", 1.0));
        s.advance_to(2);
        s.begin_phase("b");
        s.record(&obs("reqs", 1.0));
        s.record(&obs("reqs", 1.0));
        let ps = s.phase_summaries();
        assert_eq!(ps[0].counters["reqs"], 1.0);
        assert_eq!(ps[0].end_second, Some(2));
        assert_eq!(ps[1].counters["reqs"], 2.0);
    }

    #[test]
    fn gauge_slope_is_units_per_second() {
        let g = GaugeStats::of(&[(0, 10.0), (1, 20.0), (2, 30.0), (3, 40.0)]);
        assert!((g.slope - 10.0).abs() < 1e-9);
        assert_eq!(g.last, 40.0);
        assert_eq!(g.max, 40.0);
        let flat = GaugeStats::of(&[(5, 3.0), (6, 3.0)]);
        assert_eq!(flat.slope, 0.0);
    }

    #[test]
    fn unknown_metric_is_counted_not_dropped_silently() {
        let mut s = MetricStore::new(vec![]);
        s.record(&obs("nope", 1.0));
        assert_eq!(s.unknown_metrics["nope"], 1);
    }
}
