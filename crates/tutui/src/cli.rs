//! Shared command-line entry so every binary built on tutui behaves the same.

use crate::config::{self, LoadedRun};
use crate::runner::{self, Source};
use crate::scenario::Registry;
use crate::ui;
use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind};
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(about = "Real-time load-test dashboard", version)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Directory scanned for run configs when no subcommand is given.
    #[arg(long, default_value = "runs")]
    runs_dir: PathBuf,
    /// Where event logs and summaries are written.
    #[arg(long, default_value = "reports")]
    report_dir: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Run one config file.
    Run {
        config: PathBuf,
        /// No dashboard: stream log lines to stdout and print the summary at the end.
        #[arg(long)]
        headless: bool,
    },
    /// Replay a recorded *.events.jsonl file.
    Replay {
        events: PathBuf,
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// Re-judge the replay with this run config's thresholds.
        #[arg(long)]
        thresholds_from: Option<PathBuf>,
    },
    /// Render one Markdown report from one or more *.summary.json files.
    Report {
        summaries: Vec<PathBuf>,
        /// Write here instead of stdout.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// List scenarios this binary provides.
    Scenarios,
}

pub async fn main(registry: Registry) -> Result<()> {
    let cli = Cli::parse();
    let registry = Arc::new(registry);
    match cli.command {
        Some(Command::Scenarios) => {
            for id in registry.ids() {
                let s = registry.get(id).expect("listed id");
                println!("{id:<28} {}", s.description());
            }
            Ok(())
        }
        Some(Command::Run { config, headless }) => {
            let run = LoadedRun::load(&config)?;
            if headless {
                let outcome = runner::run_headless(
                    Source::Live {
                        registry: registry.clone(),
                        run: &run,
                    },
                    &cli.report_dir,
                )
                .await;
                return finish(outcome);
            }
            let mut terminal = ratatui::init();
            let outcome = runner::run(
                &mut terminal,
                Source::Live {
                    registry: registry.clone(),
                    run: &run,
                },
                &cli.report_dir,
            )
            .await;
            ratatui::restore();
            finish(outcome)
        }
        Some(Command::Replay {
            events,
            speed,
            thresholds_from,
        }) => {
            let thresholds = match thresholds_from {
                Some(p) => LoadedRun::load(&p)?.config.thresholds,
                None => Vec::new(),
            };
            let mut terminal = ratatui::init();
            let outcome = runner::run(
                &mut terminal,
                Source::Replay {
                    path: &events,
                    speed,
                    thresholds,
                },
                &cli.report_dir,
            )
            .await;
            ratatui::restore();
            finish(outcome)
        }
        Some(Command::Report { summaries, output }) => {
            anyhow::ensure!(!summaries.is_empty(), "pass at least one *.summary.json");
            let loaded: Vec<crate::report::RunSummary> = summaries
                .iter()
                .map(|p| Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?))
                .collect::<Result<_>>()?;
            let md = crate::report::render_markdown_many(&loaded);
            match output {
                Some(path) => {
                    std::fs::write(&path, md)?;
                    println!("wrote {}", path.display());
                }
                None => print!("{md}"),
            }
            Ok(())
        }
        None => picker(registry, &cli.runs_dir, &cli.report_dir).await,
    }
}

fn finish(outcome: Result<runner::RunOutcome>) -> Result<()> {
    let outcome = outcome?;
    print!("{}", crate::report::render_text(&outcome.summary));
    println!(
        "\nsummary:  {}\nmarkdown: {}\nevents:   {}",
        outcome.summary_path.display(),
        outcome.markdown_path.display(),
        outcome.events_path.display()
    );
    Ok(())
}

async fn picker(registry: Arc<Registry>, runs_dir: &std::path::Path, report_dir: &std::path::Path) -> Result<()> {
    let runs = config::discover(runs_dir)?;
    anyhow::ensure!(!runs.is_empty(), "no run configs found in {}", runs_dir.display());
    let mut terminal = ratatui::init();
    let mut selected = 0usize;
    let mut error: Option<String> = None;
    let mut term_events = EventStream::new();
    let mut last_text: Option<String> = None;
    loop {
        terminal.draw(|f| ui::draw_picker(f, &runs, selected, error.as_deref()))?;
        let Some(Ok(TermEvent::Key(key))) = term_events.next().await else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Up | KeyCode::Char('k') => selected = (selected + runs.len() - 1) % runs.len(),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1) % runs.len(),
            KeyCode::Enter => {
                drop(term_events);
                match runner::run(
                    &mut terminal,
                    Source::Live {
                        registry: registry.clone(),
                        run: &runs[selected],
                    },
                    report_dir,
                )
                .await
                {
                    Ok(o) => {
                        last_text = Some(format!(
                            "{}\nsummary:  {}\nmarkdown: {}\nevents:   {}\n",
                            crate::report::render_text(&o.summary),
                            o.summary_path.display(),
                            o.markdown_path.display(),
                            o.events_path.display()
                        ));
                        error = None;
                    }
                    Err(e) => error = Some(format!("{e:#}")),
                }
                term_events = EventStream::new();
            }
            _ => {}
        }
    }
    ratatui::restore();
    if let Some(t) = last_text {
        print!("{t}");
    }
    Ok(())
}
