//! tutui — a generic, real-time load-test dashboard.
//!
//! Implement [`Scenario`], register it in a [`Registry`], hand the registry to
//! [`cli::main`]. Run configs are JSON files selecting a scenario id and params.

pub mod app;
pub mod cli;
pub mod config;
pub mod metrics;
pub mod process;
pub mod protocol;
pub mod replay;
pub mod report;
pub mod runner;
pub mod scenario;
pub mod ui;
pub mod verdict;

pub use async_trait::async_trait;
pub use protocol::{Labels, LogLevel, MetricKind, MetricSpec};
pub use scenario::{labels, Recorder, Registry, RunContext, Scenario};
pub use serde_json::Value;
