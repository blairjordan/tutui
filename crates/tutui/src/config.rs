//! A run config is a JSON file naming a scenario and its parameters.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Scenario id from the binary's registry.
    pub scenario: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default = "default_chart_window")]
    pub chart_window_seconds: u64,
    /// Pass/fail rules evaluated per phase and overall; see verdict.rs.
    #[serde(default)]
    pub thresholds: Vec<crate::verdict::Threshold>,
}

fn default_chart_window() -> u64 {
    120
}

#[derive(Debug, Clone)]
pub struct LoadedRun {
    pub config: RunConfig,
    pub path: PathBuf,
}

impl LoadedRun {
    pub fn load(path: &Path) -> Result<LoadedRun> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let config: RunConfig = serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(LoadedRun {
            config,
            path: path.to_path_buf(),
        })
    }

    /// Directory the config lives in; scenarios resolve relative paths against it.
    pub fn base_dir(&self) -> PathBuf {
        self.path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Every parseable *.json under `dir`, sorted by filename.
pub fn discover(dir: &Path) -> Result<Vec<LoadedRun>> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("list {}", dir.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    Ok(paths.iter().filter_map(|p| LoadedRun::load(p).ok()).collect())
}
