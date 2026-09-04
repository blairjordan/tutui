//! Dashboard state: a reducer over scenario events plus view selection.

use crate::metrics::MetricStore;
use crate::protocol::{Event, LogLevel, MetricSpec};
use crate::verdict::{self, Threshold, Verdicts};
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Instant;

const LOG_CAPACITY: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Starting,
    Running,
    Stopping,
    Done,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub second: u64,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Live,
    Phases,
}

pub struct App {
    pub run_name: String,
    pub scenario_id: String,
    pub started: Instant,
    pub status: RunStatus,
    pub store: MetricStore,
    pub logs: VecDeque<LogLine>,
    pub selected: usize,
    pub view: View,
    pub show_logs: bool,
    pub summary: Value,
    pub events_seen: u64,
    pub chart_window_seconds: u64,
    pub thresholds: Vec<Threshold>,
    pub description: Option<String>,
}

impl App {
    pub fn new(
        run_name: &str,
        scenario_id: &str,
        metrics: Vec<MetricSpec>,
        chart_window_seconds: u64,
        thresholds: Vec<Threshold>,
        description: Option<String>,
    ) -> Self {
        Self {
            run_name: run_name.into(),
            scenario_id: scenario_id.into(),
            started: Instant::now(),
            status: RunStatus::Starting,
            store: MetricStore::new(metrics),
            logs: VecDeque::with_capacity(LOG_CAPACITY),
            selected: 0,
            view: View::Live,
            show_logs: true,
            summary: Value::Null,
            events_seen: 0,
            chart_window_seconds,
            thresholds,
            description,
        }
    }

    pub fn elapsed_seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Called on every UI tick so buckets close even when no events arrive.
    pub fn tick(&mut self) {
        self.store.advance_to(self.elapsed_seconds());
    }

    pub fn apply(&mut self, event: Event) {
        self.tick();
        self.events_seen += 1;
        match event {
            Event::Hello { scenario, metrics, .. } => {
                self.scenario_id = scenario;
                for m in metrics {
                    self.store.declare(m);
                }
                self.status = RunStatus::Running;
            }
            Event::Phase { name } => {
                self.store.begin_phase(&name);
                self.push_log(LogLevel::Info, format!("phase: {name}"));
                if self.status == RunStatus::Starting {
                    self.status = RunStatus::Running;
                }
            }
            Event::Observe(o) => {
                self.store.record(&o);
                if self.status == RunStatus::Starting {
                    self.status = RunStatus::Running;
                }
            }
            Event::Batch { observations } => {
                for o in &observations {
                    self.store.record(o);
                }
                if self.status == RunStatus::Starting {
                    self.status = RunStatus::Running;
                }
            }
            Event::Log { level, message } => self.push_log(level, message),
            Event::Done { summary } => {
                self.summary = summary;
                self.status = RunStatus::Done;
                self.push_log(LogLevel::Info, "scenario finished");
            }
            Event::Error { message } => {
                self.push_log(LogLevel::Error, message.clone());
                self.status = RunStatus::Failed(message);
            }
        }
    }

    pub fn push_log(&mut self, level: LogLevel, message: impl Into<String>) {
        if self.logs.len() == LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(LogLine {
            second: self.elapsed_seconds(),
            level,
            message: message.into(),
        });
    }

    pub fn select_next(&mut self) {
        let n = self.store.names().len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    pub fn select_prev(&mut self) {
        let n = self.store.names().len();
        if n > 0 {
            self.selected = (self.selected + n - 1) % n;
        }
    }

    pub fn selected_metric(&self) -> Option<&str> {
        self.store.names().get(self.selected).map(String::as_str)
    }

    pub fn verdicts(&self) -> Verdicts {
        verdict::evaluate(
            &self.thresholds,
            &self.store.overall_summary(),
            &self.store.phase_summaries(),
            self.store.current_second(),
        )
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.status, RunStatus::Done | RunStatus::Failed(_))
    }
}
