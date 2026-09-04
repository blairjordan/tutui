//! Event vocabulary shared by in-process scenarios and external processes.
//! External processes emit these as one JSON object per stdout line (docs/PROTOCOL.md).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type Labels = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// Count of things that happened; graphed as a per-second rate.
    Counter,
    /// A level sampled over time (in-flight, queue depth); graphed as its last value.
    Gauge,
    /// A distribution of measurements (latency, bytes); graphed as p50/p95/p99.
    Histogram,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSpec {
    pub name: String,
    pub kind: MetricKind,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl MetricSpec {
    pub fn counter(name: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            kind: MetricKind::Counter,
            unit: None,
            description: Some(description.into()),
        }
    }
    pub fn gauge(name: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            kind: MetricKind::Gauge,
            unit: None,
            description: Some(description.into()),
        }
    }
    pub fn histogram(name: &str, unit: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            kind: MetricKind::Histogram,
            unit: Some(unit.into()),
            description: Some(description.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub metric: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: Labels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Hello {
        scenario: String,
        #[serde(default)]
        metrics: Vec<MetricSpec>,
        #[serde(default)]
        params: Value,
    },
    Phase {
        name: String,
    },
    Observe(Observation),
    Batch {
        observations: Vec<Observation>,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    Done {
        #[serde(default)]
        summary: Value,
    },
    Error {
        message: String,
    },
}

/// Messages the dashboard writes to an external scenario's stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_observe_with_labels() {
        let e: Event = serde_json::from_str(r#"{"type":"observe","metric":"latency_ms","value":12.5,"labels":{"status":"200"}}"#).unwrap();
        let Event::Observe(o) = e else { panic!("wrong variant") };
        assert_eq!(o.metric, "latency_ms");
        assert_eq!(o.labels["status"], "200");
    }

    #[test]
    fn parses_hello_with_metric_specs() {
        let e: Event = serde_json::from_str(r#"{"type":"hello","scenario":"s","metrics":[{"name":"reqs","kind":"counter"}]}"#).unwrap();
        let Event::Hello { metrics, .. } = e else { panic!("wrong variant") };
        assert_eq!(metrics[0].kind, MetricKind::Counter);
    }
}
