//! Persisted output of a run: raw event stream (replayable) and a summary.

use crate::app::App;
use crate::metrics::{Aggregate, PhaseSummary};
use crate::protocol::Event;
use crate::verdict::{Threshold, Verdicts};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct Recorded<'a> {
    t_ms: u128,
    event: &'a Event,
}

pub struct EventLog {
    writer: BufWriter<File>,
    pub path: PathBuf,
}

impl EventLog {
    pub fn create(dir: &Path, stem: &str) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{stem}.events.jsonl"));
        let writer = BufWriter::new(File::create(&path).with_context(|| format!("create {}", path.display()))?);
        Ok(Self { writer, path })
    }

    pub fn append(&mut self, t_ms: u128, event: &Event) -> Result<()> {
        serde_json::to_writer(&mut self.writer, &Recorded { t_ms, event })?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        Ok(self.writer.flush()?)
    }
}

#[derive(Serialize, Deserialize)]
pub struct MetricSummary {
    pub kind: String,
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_rate_per_second: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentiles: Option<crate::metrics::Percentiles>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_label: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
pub struct RunSummary {
    pub run: String,
    pub scenario: String,
    #[serde(default)]
    pub description: Option<String>,
    pub started_at: String,
    pub duration_seconds: u64,
    pub status: String,
    pub metrics: BTreeMap<String, MetricSummary>,
    pub phases: Vec<PhaseSummary>,
    #[serde(default)]
    pub thresholds: Vec<Threshold>,
    #[serde(default)]
    pub verdicts: Verdicts,
    pub scenario_summary: serde_json::Value,
    pub unknown_metrics: BTreeMap<String, u64>,
}

pub fn summarise(app: &App, started_at: chrono::DateTime<chrono::Local>) -> RunSummary {
    let duration = app.elapsed_seconds().max(1);
    let mut metrics = BTreeMap::new();
    for name in app.store.names() {
        let Some(m) = app.store.get(name) else { continue };
        let kind = format!("{:?}", m.spec.kind).to_lowercase();
        let s = match &m.aggregate {
            Aggregate::Counter { total, by_label, .. } => MetricSummary {
                kind,
                unit: m.spec.unit.clone(),
                total: Some(*total),
                mean_rate_per_second: Some(total / duration as f64),
                last: None,
                max: None,
                percentiles: None,
                by_label: by_label.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect(),
            },
            Aggregate::Gauge {
                last,
                per_second,
                by_label,
            } => MetricSummary {
                kind,
                unit: m.spec.unit.clone(),
                total: None,
                mean_rate_per_second: None,
                last: Some(*last),
                max: per_second
                    .iter()
                    .flatten()
                    .cloned()
                    .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.max(v)))),
                percentiles: None,
                by_label: by_label.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect(),
            },
            Aggregate::Histogram { by_label, .. } => MetricSummary {
                kind,
                unit: m.spec.unit.clone(),
                total: None,
                mean_rate_per_second: None,
                last: None,
                max: None,
                percentiles: m.percentiles(),
                by_label: by_label.iter().map(|(k, h)| (k.clone(), serde_json::json!(h.len()))).collect(),
            },
        };
        metrics.insert(name.clone(), s);
    }
    RunSummary {
        run: app.run_name.clone(),
        scenario: app.scenario_id.clone(),
        description: app.description.clone(),
        started_at: started_at.to_rfc3339(),
        duration_seconds: app.elapsed_seconds(),
        status: format!("{:?}", app.status),
        metrics,
        phases: app.store.phase_summaries(),
        thresholds: app.thresholds.clone(),
        verdicts: {
            let mut v = app.verdicts();
            if matches!(app.status, crate::app::RunStatus::Failed(_)) {
                if let Some(o) = v.overall.as_mut() {
                    o.pass = Some(false);
                }
            }
            v
        },
        scenario_summary: app.summary.clone(),
        unknown_metrics: app.store.unknown_metrics.clone(),
    }
}

pub fn write_summary(dir: &Path, stem: &str, summary: &RunSummary) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{stem}.summary.json"));
    fs::write(&path, serde_json::to_vec_pretty(summary)?)?;
    Ok(path)
}

pub fn write_markdown(dir: &Path, stem: &str, summary: &RunSummary) -> Result<PathBuf> {
    let path = dir.join(format!("{stem}.md"));
    fs::write(&path, render_markdown_many(std::slice::from_ref(summary)))?;
    Ok(path)
}

fn verdict_line(v: &Verdicts) -> Option<String> {
    let overall = v.overall.as_ref()?;
    let word = match overall.pass {
        Some(true) => "PASS",
        Some(false) => "FAIL",
        None => "not judged",
    };
    let mut s = format!("verdict {word}");
    if let Some(c) = &v.ceiling {
        s.push_str(&format!("  ceiling = {c}"));
    }
    if let Some(f) = &v.first_failure {
        s.push_str(&format!("  first failure = {f}"));
    }
    Some(s)
}

/// Plain-text rendering printed after the TUI exits, so results survive in scrollback.
pub fn render_text(summary: &RunSummary) -> String {
    use crate::metrics::fmt_num;
    let mut out = String::new();
    out.push_str(&format!(
        "run {} ({}) — {} — {}s\n",
        summary.run, summary.scenario, summary.status, summary.duration_seconds
    ));
    if let Some(v) = verdict_line(&summary.verdicts) {
        out.push_str(&format!("{v}\n"));
        for pv in &summary.verdicts.phases {
            for c in pv.checks.iter().filter(|c| !c.pass) {
                out.push_str(&format!(
                    "  ✗ [{}] {}  actual {}\n",
                    pv.phase,
                    c.rule,
                    c.value.map(fmt_num).unwrap_or_default()
                ));
            }
        }
    }
    for (name, m) in &summary.metrics {
        let unit = m.unit.as_deref().unwrap_or("");
        match m.kind.as_str() {
            "counter" => out.push_str(&format!(
                "  {name}: total {}  mean {:.2}/s\n",
                fmt_num(m.total.unwrap_or(0.0)),
                m.mean_rate_per_second.unwrap_or(0.0)
            )),
            "gauge" => out.push_str(&format!(
                "  {name}: last {}  max {}\n",
                fmt_num(m.last.unwrap_or(f64::NAN)),
                fmt_num(m.max.unwrap_or(f64::NAN))
            )),
            _ => {
                if let Some(p) = &m.percentiles {
                    out.push_str(&format!(
                        "  {name}: n={} p50 {}{unit} p90 {}{unit} p95 {}{unit} p99 {}{unit} max {}{unit} mean {}{unit}\n",
                        p.count,
                        fmt_num(p.p50),
                        fmt_num(p.p90),
                        fmt_num(p.p95),
                        fmt_num(p.p99),
                        fmt_num(p.max),
                        fmt_num(p.mean)
                    ));
                }
            }
        }
        for (k, v) in &m.by_label {
            out.push_str(&format!("      {k}: {v}\n"));
        }
    }
    if !summary.phases.is_empty() {
        out.push_str("\nphases:\n");
        for p in &summary.phases {
            let dur = p
                .end_second
                .map(|e| format!("{}s", e - p.start_second))
                .unwrap_or_else(|| format!("{}s+", summary.duration_seconds.saturating_sub(p.start_second)));
            out.push_str(&format!("  [{}] {dur}\n", p.name));
            for (k, v) in &p.counters {
                let labels = p
                    .counter_labels
                    .get(k)
                    .map(|m| m.iter().map(|(l, n)| format!("{l}:{n}")).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default();
                out.push_str(&format!("      {k}: {}  {labels}\n", fmt_num(*v)));
            }
            for (k, h) in &p.histograms {
                out.push_str(&format!(
                    "      {k}: n={} p50 {} p95 {} p99 {} max {}\n",
                    h.count,
                    fmt_num(h.p50),
                    fmt_num(h.p95),
                    fmt_num(h.p99),
                    fmt_num(h.max)
                ));
            }
        }
    }
    if !summary.unknown_metrics.is_empty() {
        out.push_str(&format!("\nundeclared metrics ignored: {:?}\n", summary.unknown_metrics));
    }
    out
}

fn md_num(v: Option<f64>) -> String {
    v.map(crate::metrics::fmt_num).unwrap_or_else(|| "–".into())
}

fn md_hist(p: &crate::metrics::Percentiles, unit: &str) -> String {
    format!(
        "n={} · p50 {}{unit} · p95 {}{unit} · p99 {}{unit} · max {}{unit}",
        p.count,
        crate::metrics::fmt_num(p.p50),
        crate::metrics::fmt_num(p.p95),
        crate::metrics::fmt_num(p.p99),
        crate::metrics::fmt_num(p.max)
    )
}

/// Team-facing Markdown: verdicts first, then per-phase tables, then raw metric summaries.
pub fn render_markdown(s: &RunSummary) -> String {
    use crate::metrics::fmt_num;
    let mut out = String::new();
    out.push_str(&format!("## {}\n\n", s.run));
    if let Some(d) = &s.description {
        out.push_str(&format!("{d}\n\n"));
    }
    out.push_str(&format!(
        "| | |\n|---|---|\n| scenario | `{}` |\n| started | {} |\n| duration | {}s |\n| status | {} |\n",
        s.scenario, s.started_at, s.duration_seconds, s.status
    ));
    if let Some(overall) = &s.verdicts.overall {
        let word = match overall.pass {
            Some(true) => "✅ PASS",
            Some(false) => "❌ FAIL",
            None => "– not judged",
        };
        out.push_str(&format!("| verdict | **{word}** |\n"));
        out.push_str(&format!(
            "| ceiling | **{}** |\n",
            s.verdicts.ceiling.clone().unwrap_or_else(|| "none passed".into())
        ));
        if let Some(f) = &s.verdicts.first_failure {
            out.push_str(&format!("| first failure | {f} |\n"));
        }
    }
    out.push('\n');

    if !s.thresholds.is_empty() {
        out.push_str("### Thresholds\n\n| rule | overall | ");
        for pv in &s.verdicts.phases {
            out.push_str(&format!("{} | ", pv.phase));
        }
        out.push_str("\n|---|---|");
        for _ in &s.verdicts.phases {
            out.push_str("---|");
        }
        out.push('\n');
        for (i, t) in s.thresholds.iter().enumerate() {
            out.push_str(&format!("| `{}` | ", t.describe()));
            let cell = |c: Option<&crate::verdict::Check>| match c {
                Some(c) if c.value.is_none() => "–".to_string(),
                Some(c) => format!("{} {}", if c.pass { "✅" } else { "❌" }, md_num(c.value)),
                None => "skip".into(),
            };
            let overall = s
                .verdicts
                .overall
                .as_ref()
                .and_then(|o| o.checks.iter().find(|c| c.rule == t.describe()));
            out.push_str(&format!("{} | ", cell(overall)));
            for pv in &s.verdicts.phases {
                let c = pv.checks.iter().find(|c| c.rule == t.describe());
                let _ = i;
                out.push_str(&format!("{} | ", cell(c)));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    if !s.phases.is_empty() {
        let hist: Vec<&String> = s.metrics.iter().filter(|(_, m)| m.kind == "histogram").map(|(k, _)| k).collect();
        let counters: Vec<&String> = s.metrics.iter().filter(|(_, m)| m.kind == "counter").map(|(k, _)| k).collect();
        let gauges: Vec<&String> = s.metrics.iter().filter(|(_, m)| m.kind == "gauge").map(|(k, _)| k).collect();
        out.push_str("### Phases\n\n| phase | duration | ");
        for c in &counters {
            out.push_str(&format!("{c} | "));
        }
        for h in &hist {
            out.push_str(&format!("{h} | "));
        }
        for g in &gauges {
            out.push_str(&format!("{g} (max / slope) | "));
        }
        out.push_str("\n|---|---|");
        for _ in counters.iter().chain(hist.iter()).chain(gauges.iter()) {
            out.push_str("---|");
        }
        out.push('\n');
        for p in &s.phases {
            let dur = p
                .end_second
                .map(|e| format!("{}s", e - p.start_second))
                .unwrap_or_else(|| format!("{}s", s.duration_seconds.saturating_sub(p.start_second)));
            out.push_str(&format!("| {} | {dur} | ", p.name));
            for c in &counters {
                let total = p.counters.get(*c).copied().unwrap_or(0.0);
                let labels = p
                    .counter_labels
                    .get(*c)
                    .map(|m| {
                        m.iter()
                            .map(|(l, n)| format!("{}:{}", l.rsplit('=').next().unwrap_or(l), fmt_num(*n)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "{}{} | ",
                    fmt_num(total),
                    if labels.is_empty() { String::new() } else { format!(" ({labels})") }
                ));
            }
            for h in &hist {
                let unit = s.metrics.get(*h).and_then(|m| m.unit.clone()).unwrap_or_default();
                out.push_str(&format!(
                    "{} | ",
                    p.histograms.get(*h).map(|x| md_hist(x, &unit)).unwrap_or_else(|| "–".into())
                ));
            }
            for g in &gauges {
                out.push_str(&format!(
                    "{} | ",
                    p.gauges
                        .get(*g)
                        .map(|x| format!("{} / {}{}", fmt_num(x.max), if x.slope > 0.0 { "+" } else { "" }, fmt_num(x.slope)))
                        .unwrap_or_else(|| "–".into())
                ));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("### Metrics (whole run)\n\n| metric | kind | value |\n|---|---|---|\n");
    for (name, m) in &s.metrics {
        let unit = m.unit.as_deref().unwrap_or("");
        let value = match m.kind.as_str() {
            "counter" => format!(
                "total {} · mean {:.2}/s{}",
                fmt_num(m.total.unwrap_or(0.0)),
                m.mean_rate_per_second.unwrap_or(0.0),
                if m.by_label.is_empty() {
                    String::new()
                } else {
                    format!(
                        " · {}",
                        m.by_label.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", ")
                    )
                }
            ),
            "gauge" => format!("last {} · max {}", md_num(m.last), md_num(m.max)),
            _ => m.percentiles.as_ref().map(|p| md_hist(p, unit)).unwrap_or_else(|| "–".into()),
        };
        out.push_str(&format!("| `{name}` | {} | {value} |\n", m.kind));
    }
    out.push('\n');
    if !s.scenario_summary.is_null() {
        out.push_str(&format!(
            "<details><summary>scenario summary</summary>\n\n```json\n{}\n```\n</details>\n\n",
            serde_json::to_string_pretty(&s.scenario_summary).unwrap_or_default()
        ));
    }
    out
}

pub fn render_markdown_many(runs: &[RunSummary]) -> String {
    let mut out = String::from("# Load test report\n\n");
    if runs.len() > 1 {
        out.push_str("| run | status | verdict | ceiling |\n|---|---|---|---|\n");
        for r in runs {
            let verdict = r
                .verdicts
                .overall
                .as_ref()
                .map(|o| match o.pass {
                    Some(true) => "✅",
                    Some(false) => "❌",
                    None => "–",
                })
                .unwrap_or("–");
            out.push_str(&format!(
                "| {} | {} | {verdict} | {} |\n",
                r.run,
                r.status,
                r.verdicts.ceiling.clone().unwrap_or_else(|| "–".into())
            ));
        }
        out.push('\n');
    }
    for r in runs {
        out.push_str(&render_markdown(r));
    }
    out
}
