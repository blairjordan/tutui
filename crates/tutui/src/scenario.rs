//! The extension point: anything that can drive load and report metrics.

use crate::protocol::{Event, Labels, LogLevel, MetricSpec, Observation};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait Scenario: Send + Sync {
    /// Stable id referenced by run configs, e.g. "checkout.ramp".
    fn id(&self) -> &str;
    fn description(&self) -> &str;
    /// Metrics this scenario will report. Declared up front so the dashboard can lay out panels before data arrives.
    fn metrics(&self) -> Vec<MetricSpec>;
    /// Run to completion, honouring `ctx.cancel`. The returned value is the run summary written to the report.
    async fn run(&self, ctx: RunContext) -> anyhow::Result<Value>;
}

pub struct RunContext {
    pub params: Value,
    pub recorder: Recorder,
    pub cancel: CancellationToken,
}

impl RunContext {
    /// Deserialize params into the scenario's own typed struct.
    pub fn params<T: DeserializeOwned>(&self) -> anyhow::Result<T> {
        serde_json::from_value(self.params.clone()).map_err(|e| anyhow::anyhow!("invalid params: {e}"))
    }
}

/// Cheap, cloneable handle a scenario uses to report. Dropping the last clone ends the stream.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::UnboundedSender<Event>,
}

impl Recorder {
    pub fn new() -> (Recorder, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Recorder { tx }, rx)
    }

    pub fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn observe(&self, metric: &str, value: f64) {
        self.emit(Event::Observe(Observation {
            metric: metric.into(),
            value,
            labels: Labels::new(),
        }));
    }

    pub fn observe_labeled(&self, metric: &str, value: f64, labels: Labels) {
        self.emit(Event::Observe(Observation {
            metric: metric.into(),
            value,
            labels,
        }));
    }

    pub fn count(&self, metric: &str) {
        self.observe(metric, 1.0);
    }

    pub fn count_labeled(&self, metric: &str, labels: Labels) {
        self.observe_labeled(metric, 1.0, labels);
    }

    pub fn gauge(&self, metric: &str, value: f64) {
        self.observe(metric, value);
    }

    pub fn phase(&self, name: impl Into<String>) {
        self.emit(Event::Phase { name: name.into() });
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.emit(Event::Log {
            level,
            message: message.into(),
        });
    }

    pub fn info(&self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.log(LogLevel::Warn, message);
    }
}

pub fn labels<const N: usize>(pairs: [(&str, String); N]) -> Labels {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// Scenarios a binary makes available, looked up by id from run configs.
#[derive(Default)]
pub struct Registry {
    scenarios: Vec<Box<dyn Scenario>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(mut self, scenario: impl Scenario + 'static) -> Self {
        self.scenarios.push(Box::new(scenario));
        self
    }

    pub fn get(&self, id: &str) -> Option<&dyn Scenario> {
        self.scenarios.iter().find(|s| s.id() == id).map(|s| s.as_ref())
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.scenarios.iter().map(|s| s.id())
    }
}
