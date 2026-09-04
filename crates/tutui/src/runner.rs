//! Drives one run: spawns the scenario, feeds its events into the App, renders
//! the dashboard, handles keys, and writes the report when it ends.

use crate::app::{App, RunStatus, View};
use crate::config::LoadedRun;
use crate::protocol::{Event, LogLevel};
use crate::report::{self, EventLog};
use crate::scenario::{Recorder, Registry, RunContext};
use crate::ui;
use anyhow::{Context, Result};
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub struct RunOutcome {
    pub summary: report::RunSummary,
    pub summary_path: PathBuf,
    pub markdown_path: PathBuf,
    pub events_path: PathBuf,
    /// The user pressed q while the scenario was still running.
    pub user_quit: bool,
}

/// Source of events for the dashboard: a live scenario or a recorded file.
pub enum Source<'a> {
    Live {
        registry: Arc<Registry>,
        run: &'a LoadedRun,
    },
    Replay {
        path: &'a Path,
        speed: f64,
        thresholds: Vec<crate::verdict::Threshold>,
    },
}

struct Session {
    app: App,
    events: tokio::sync::mpsc::UnboundedReceiver<Event>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    event_log: EventLog,
    stem: String,
    started_at: chrono::DateTime<chrono::Local>,
    run_start: Instant,
}

async fn start(source: Source<'_>, report_dir: &Path) -> Result<Session> {
    let started_at = chrono::Local::now();
    let (run_name, scenario_id, declared, chart_window, params, thresholds, description) = match &source {
        Source::Live { registry, run } => {
            let scenario = registry.get(&run.config.scenario).with_context(|| {
                format!(
                    "unknown scenario '{}' (available: {})",
                    run.config.scenario,
                    registry.ids().collect::<Vec<_>>().join(", ")
                )
            })?;
            (
                run.config.name.clone(),
                scenario.id().to_string(),
                scenario.metrics(),
                run.config.chart_window_seconds,
                run.config.params.clone(),
                run.config.thresholds.clone(),
                run.config.description.clone(),
            )
        }
        Source::Replay { path, thresholds, .. } => (
            format!("replay {}", path.file_name().and_then(|s| s.to_str()).unwrap_or("?")),
            "replay".into(),
            Vec::new(),
            120,
            serde_json::Value::Null,
            thresholds.clone(),
            None,
        ),
    };

    let slug: String = run_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let stem = format!("{}-{slug}", started_at.format("%Y%m%dT%H%M%S"));
    let event_log = EventLog::create(report_dir, &stem)?;
    let app = App::new(&run_name, &scenario_id, declared, chart_window, thresholds, description);
    let cancel = CancellationToken::new();

    let (recorder, events) = Recorder::new();
    let scenario_task = match source {
        Source::Live { registry, run } => {
            let scenario_id = run.config.scenario.clone();
            let ctx = RunContext {
                params: with_base_dir(params, &run.base_dir()),
                recorder: recorder.clone(),
                cancel: cancel.clone(),
            };
            tokio::spawn(async move {
                let scenario = registry.get(&scenario_id).expect("checked above");
                recorder.emit(Event::Hello {
                    scenario: scenario.id().into(),
                    metrics: scenario.metrics(),
                    params: ctx.params.clone(),
                });
                match scenario.run(ctx).await {
                    Ok(summary) => recorder.emit(Event::Done { summary }),
                    Err(e) => recorder.emit(Event::Error { message: format!("{e:#}") }),
                }
            })
        }
        Source::Replay { path, speed, .. } => {
            let path = path.to_path_buf();
            tokio::spawn(async move { crate::replay::feed(&path, speed, recorder).await })
        }
    };
    Ok(Session {
        app,
        events,
        cancel,
        task: scenario_task,
        event_log,
        stem,
        started_at,
        run_start: Instant::now(),
    })
}

async fn close(mut s: Session, report_dir: &Path, user_quit: bool) -> Result<RunOutcome> {
    s.cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.task).await;
    while let Ok(ev) = s.events.try_recv() {
        s.event_log.append(s.run_start.elapsed().as_millis(), &ev)?;
        s.app.apply(ev);
    }
    s.event_log.flush()?;
    let summary = report::summarise(&s.app, s.started_at);
    let summary_path = report::write_summary(report_dir, &s.stem, &summary)?;
    let markdown_path = report::write_markdown(report_dir, &s.stem, &summary)?;
    Ok(RunOutcome {
        summary,
        summary_path,
        markdown_path,
        events_path: s.event_log.path,
        user_quit,
    })
}

/// No terminal: log lines go to stdout, everything else only to the report.
pub async fn run_headless(source: Source<'_>, report_dir: &Path) -> Result<RunOutcome> {
    let mut s = start(source, report_dir).await?;
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                s.app.tick();
                s.event_log.flush()?;
                let phase = s.app.store.current_phase().unwrap_or("-");
                println!("{:>5}s  [{}] {}", s.app.elapsed_seconds(), phase, s.app.store.names().iter().filter_map(|n| s.app.store.get(n).map(|m| format!("{n}: {}", m.headline()))).collect::<Vec<_>>().join("  |  "));
            }
            ev = s.events.recv() => match ev {
                Some(ev) => {
                    s.event_log.append(s.run_start.elapsed().as_millis(), &ev)?;
                    if let Event::Log { level, message } = &ev {
                        println!("{:>5}s  {:?}: {message}", s.app.elapsed_seconds(), level);
                    }
                    if let Event::Phase { name } = &ev {
                        println!("{:>5}s  == phase {name} ==", s.app.elapsed_seconds());
                    }
                    s.app.apply(ev);
                    if s.app.is_finished() { break }
                }
                None => break,
            },
            _ = &mut ctrl_c => {
                println!("interrupt: stopping scenario");
                s.cancel.cancel();
            }
        }
    }
    close(s, report_dir, false).await
}

pub async fn run(terminal: &mut DefaultTerminal, source: Source<'_>, report_dir: &Path) -> Result<RunOutcome> {
    let mut s = start(source, report_dir).await?;
    let Session {
        app,
        events,
        cancel,
        event_log,
        run_start,
        ..
    } = &mut s;
    let run_start = *run_start;

    let mut term_events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    let mut user_quit = false;
    let mut scenario_done = false;

    loop {
        terminal.draw(|f| ui::draw_dashboard(f, app))?;
        tokio::select! {
            _ = ticker.tick() => {
                app.tick();
                event_log.flush()?;
            }
            ev = events.recv(), if !scenario_done => match ev {
                Some(ev) => {
                    event_log.append(run_start.elapsed().as_millis(), &ev)?;
                    app.apply(ev);
                    // Drain whatever else is queued so a burst renders in one frame.
                    while let Ok(ev) = events.try_recv() {
                        event_log.append(run_start.elapsed().as_millis(), &ev)?;
                        app.apply(ev);
                    }
                }
                None => {
                    scenario_done = true;
                    if !app.is_finished() {
                        app.status = RunStatus::Failed("scenario ended without a done/error event".into());
                    }
                }
            },
            key = term_events.next() => {
                let Some(Ok(TermEvent::Key(key))) = key else { continue };
                if key.kind != KeyEventKind::Press { continue }
                match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        if app.is_finished() || scenario_done { break }
                        user_quit = true;
                        app.status = RunStatus::Stopping;
                        app.push_log(LogLevel::Warn, "stop requested; waiting for the scenario to unwind (q again to force)");
                        if cancel.is_cancelled() { break }
                        cancel.cancel();
                    }
                    (KeyCode::Char('s'), _) if !app.is_finished() => {
                        app.status = RunStatus::Stopping;
                        app.push_log(LogLevel::Warn, "stop requested");
                        cancel.cancel();
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), _) => app.select_prev(),
                    (KeyCode::Down, _) | (KeyCode::Char('j'), _) => app.select_next(),
                    (KeyCode::Tab, _) => app.view = if app.view == View::Live { View::Phases } else { View::Live },
                    (KeyCode::Char('l'), _) => app.show_logs = !app.show_logs,
                    _ => {}
                }
            }
        }
    }

    close(s, report_dir, user_quit).await
}

/// Scenarios resolve relative paths against the config's directory; expose it under a reserved key.
fn with_base_dir(mut params: serde_json::Value, base: &Path) -> serde_json::Value {
    if let serde_json::Value::Object(map) = &mut params {
        map.entry("_base_dir")
            .or_insert_with(|| serde_json::Value::String(base.display().to_string()));
    }
    params
}
