//! Thresholds turn recorded numbers into pass/fail, and phases into a ceiling:
//! the last phase in which every applicable threshold held.

use crate::metrics::PhaseSummary;
use crate::protocol::Labels;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stat {
    // histogram
    P50,
    P90,
    P95,
    P99,
    Max,
    Mean,
    Count,
    // counter
    Total,
    /// counter total ÷ phase seconds
    Rate,
    /// labelled counter total ÷ unlabelled total (needs `labels`)
    Share,
    // gauge
    Last,
    /// gauge trend in units/s over the phase; > 0 on a queue means the consumer is losing
    Slope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Threshold {
    pub metric: String,
    pub stat: Stat,
    #[serde(default, skip_serializing_if = "Labels::is_empty")]
    pub labels: Labels,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Phases whose name starts with any of these are setup, not load; they are never judged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_phases: Vec<String>,
}

impl Threshold {
    pub fn describe(&self) -> String {
        let labels = if self.labels.is_empty() {
            String::new()
        } else {
            format!(
                "{{{}}}",
                self.labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",")
            )
        };
        let bound = match (self.min, self.max) {
            (Some(lo), Some(hi)) => format!("{lo} ≤ · ≤ {hi}"),
            (Some(lo), None) => format!("≥ {lo}"),
            (None, Some(hi)) => format!("≤ {hi}"),
            (None, None) => "(no bound)".into(),
        };
        format!(
            "{}{labels}.{} {bound}",
            self.metric,
            serde_json::to_value(self.stat)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub rule: String,
    /// None when the phase has no data for the metric: not judged.
    pub value: Option<f64>,
    pub pass: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseVerdict {
    pub phase: String,
    pub checks: Vec<Check>,
    /// Some(false) if any applicable check failed, Some(true) if all applicable passed, None if nothing applied.
    pub pass: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Verdicts {
    pub overall: Option<PhaseVerdict>,
    pub phases: Vec<PhaseVerdict>,
    /// Last judged phase where everything passed.
    pub ceiling: Option<String>,
    /// First judged phase where something failed.
    pub first_failure: Option<String>,
}

fn label_key(labels: &Labels) -> String {
    labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

pub fn value_of(t: &Threshold, p: &PhaseSummary, now_second: u64) -> Option<f64> {
    use Stat::*;
    match t.stat {
        P50 | P90 | P95 | P99 | Max | Mean | Count => {
            if let Some(h) = p.histograms.get(&t.metric) {
                if h.count == 0 {
                    return None;
                }
                return Some(match t.stat {
                    P50 => h.p50,
                    P90 => h.p90,
                    P95 => h.p95,
                    P99 => h.p99,
                    Max => h.max,
                    Mean => h.mean,
                    _ => h.count as f64,
                });
            }
            // Max/Mean also make sense for gauges.
            let g = p.gauges.get(&t.metric).filter(|g| g.samples > 0)?;
            match t.stat {
                Max => Some(g.max),
                Mean => Some(g.mean),
                Count => Some(g.samples as f64),
                _ => None,
            }
        }
        Total => p.counters.get(&t.metric).copied(),
        Rate => p.counters.get(&t.metric).map(|c| c / p.duration_seconds(now_second) as f64),
        Share => {
            let total = *p.counters.get(&t.metric)?;
            if total <= 0.0 {
                return None;
            }
            let part = p
                .counter_labels
                .get(&t.metric)
                .and_then(|m| m.get(&label_key(&t.labels)))
                .copied()
                .unwrap_or(0.0);
            Some(part / total)
        }
        Last => p.gauges.get(&t.metric).filter(|g| g.samples > 0).map(|g| g.last),
        Slope => p.gauges.get(&t.metric).filter(|g| g.samples > 0).map(|g| g.slope),
    }
}

fn judge(t: &Threshold, p: &PhaseSummary, now_second: u64) -> Option<Check> {
    if t.skip_phases.iter().any(|s| p.name.starts_with(s.as_str())) {
        return None;
    }
    let value = value_of(t, p, now_second);
    let pass = match value {
        None => true,
        Some(v) => t.min.is_none_or(|lo| v >= lo) && t.max.is_none_or(|hi| v <= hi),
    };
    Some(Check {
        rule: t.describe(),
        value,
        pass,
    })
}

fn judge_phase(thresholds: &[Threshold], p: &PhaseSummary, now_second: u64) -> PhaseVerdict {
    let checks: Vec<Check> = thresholds.iter().filter_map(|t| judge(t, p, now_second)).collect();
    let applicable: Vec<&Check> = checks.iter().filter(|c| c.value.is_some()).collect();
    let pass = if applicable.is_empty() {
        None
    } else {
        Some(applicable.iter().all(|c| c.pass))
    };
    PhaseVerdict {
        phase: p.name.clone(),
        checks,
        pass,
    }
}

pub fn evaluate(thresholds: &[Threshold], overall: &PhaseSummary, phases: &[PhaseSummary], now_second: u64) -> Verdicts {
    if thresholds.is_empty() {
        return Verdicts::default();
    }
    let phase_verdicts: Vec<PhaseVerdict> = phases.iter().map(|p| judge_phase(thresholds, p, now_second)).collect();
    let ceiling = phase_verdicts.iter().rev().find(|v| v.pass == Some(true)).map(|v| v.phase.clone());
    let first_failure = phase_verdicts.iter().find(|v| v.pass == Some(false)).map(|v| v.phase.clone());
    Verdicts {
        overall: Some(judge_phase(thresholds, overall, now_second)),
        phases: phase_verdicts,
        ceiling,
        first_failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MetricStore;
    use crate::protocol::{MetricSpec, Observation};

    fn obs(metric: &str, value: f64, labels: &[(&str, &str)]) -> Observation {
        Observation {
            metric: metric.into(),
            value,
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    fn rules() -> Vec<Threshold> {
        vec![
            Threshold {
                metric: "lat".into(),
                stat: Stat::P95,
                labels: Labels::new(),
                min: None,
                max: Some(100.0),
                skip_phases: vec!["setup".into()],
            },
            Threshold {
                metric: "reqs".into(),
                stat: Stat::Share,
                labels: [("status".to_string(), "200".to_string())].into(),
                min: Some(0.9),
                max: None,
                skip_phases: vec![],
            },
            Threshold {
                metric: "queue".into(),
                stat: Stat::Slope,
                labels: Labels::new(),
                min: None,
                max: Some(0.0),
                skip_phases: vec![],
            },
        ]
    }

    #[test]
    fn ceiling_is_last_passing_phase_and_setup_phases_are_skipped() {
        let mut s = MetricStore::new(vec![
            MetricSpec::histogram("lat", "ms", ""),
            MetricSpec::counter("reqs", ""),
            MetricSpec::gauge("queue", ""),
        ]);
        s.begin_phase("setup");
        s.record(&obs("lat", 5000.0, &[])); // would fail p95 but setup is skipped
        s.advance_to(1);
        s.begin_phase("c=5");
        s.record(&obs("lat", 50.0, &[]));
        s.record(&obs("reqs", 1.0, &[("status", "200")]));
        s.record(&obs("queue", 5.0, &[]));
        s.advance_to(2);
        s.record(&obs("queue", 5.0, &[]));
        s.advance_to(3);
        s.begin_phase("c=50");
        s.record(&obs("lat", 500.0, &[]));
        s.record(&obs("reqs", 1.0, &[("status", "503")]));
        s.advance_to(4);
        let v = evaluate(&rules(), &s.overall_summary(), &s.phase_summaries(), s.current_second());
        assert_eq!(v.phases[0].pass, None); // setup: lat skipped by rule, no reqs/queue data → nothing judged
        assert_eq!(v.phases[1].pass, Some(true));
        assert_eq!(v.phases[2].pass, Some(false));
        assert_eq!(v.ceiling.as_deref(), Some("c=5"));
        assert_eq!(v.first_failure.as_deref(), Some("c=50"));
    }

    #[test]
    fn gauge_without_samples_is_not_judged() {
        let mut s = MetricStore::new(vec![MetricSpec::gauge("queue", "")]);
        s.begin_phase("idle");
        s.advance_to(2);
        let rules = vec![Threshold {
            metric: "queue".into(),
            stat: Stat::Slope,
            labels: Labels::new(),
            min: None,
            max: Some(0.0),
            skip_phases: vec![],
        }];
        let v = evaluate(&rules, &s.overall_summary(), &s.phase_summaries(), 2);
        assert_eq!(v.phases[0].pass, None);
        assert_eq!(v.overall.unwrap().pass, None);
    }

    #[test]
    fn no_thresholds_means_no_verdicts() {
        let s = MetricStore::new(vec![]);
        let v = evaluate(&[], &s.overall_summary(), &s.phase_summaries(), 0);
        assert!(v.overall.is_none());
    }
}
