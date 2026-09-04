use crate::app::{App, RunStatus, View};
use crate::metrics::{fmt_num, Aggregate, Metric};
use crate::protocol::{LogLevel, MetricKind};
use ratatui::prelude::*;
use ratatui::symbols;
use ratatui::widgets::{Axis, Block, Borders, Cell, Chart, Dataset, GraphType, List, ListItem, Paragraph, Row, Table, Wrap};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let log_height = if app.show_logs { area.height / 4 } else { 3 };
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(8), Constraint::Length(log_height.max(3))]).areas(area);
    draw_header(frame, header, app);
    match app.view {
        View::Live => draw_live(frame, body, app),
        View::Phases => draw_phases(frame, body, app),
    }
    draw_logs(frame, footer, app);
}

fn status_span(status: &RunStatus) -> Span<'static> {
    match status {
        RunStatus::Starting => Span::styled(" STARTING ", Style::default().fg(Color::Black).bg(Color::Yellow)),
        RunStatus::Running => Span::styled(" RUNNING ", Style::default().fg(Color::Black).bg(Color::Green)),
        RunStatus::Stopping => Span::styled(" STOPPING ", Style::default().fg(Color::Black).bg(Color::Yellow)),
        RunStatus::Done => Span::styled(" DONE ", Style::default().fg(Color::Black).bg(Color::Cyan)),
        RunStatus::Failed(_) => Span::styled(" FAILED ", Style::default().fg(Color::White).bg(Color::Red)),
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let secs = app.elapsed_seconds();
    let phase = app.store.current_phase().unwrap_or("-");
    let line = Line::from(vec![
        status_span(&app.status),
        Span::raw("  "),
        Span::styled(&app.run_name, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  scenario {}  phase ", app.scenario_id)),
        Span::styled(phase, Style::default().fg(Color::Magenta)),
        Span::raw(format!(
            "  elapsed {:02}:{:02}:{:02}  events {}",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60,
            app.events_seen
        )),
    ]);
    let verdicts = app.verdicts();
    let mut line = line;
    if let Some(overall) = &verdicts.overall {
        let (txt, color) = match overall.pass {
            Some(true) => (" PASS ", Color::Green),
            Some(false) => (" FAIL ", Color::Red),
            None => (" n/a ", Color::DarkGray),
        };
        line.spans.push(Span::raw("  "));
        line.spans.push(Span::styled(txt, Style::default().fg(Color::Black).bg(color)));
        if let Some(c) = &verdicts.ceiling {
            line.spans
                .push(Span::styled(format!(" ceiling {c}"), Style::default().fg(Color::Green)));
        }
    }
    let help = Line::from(Span::styled(
        if app.is_finished() {
            "q quit   ↑↓ metric   tab live/phases   l logs"
        } else {
            "s stop scenario   q quit   ↑↓ metric   tab live/phases   l logs"
        },
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(
        Paragraph::new(vec![line, help]).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn draw_live(frame: &mut Frame, area: Rect, app: &App) {
    let [left, right] = Layout::horizontal([Constraint::Length(44), Constraint::Min(30)]).areas(area);
    draw_metric_list(frame, left, app);
    let Some(name) = app.selected_metric() else {
        frame.render_widget(
            Paragraph::new("waiting for the scenario to declare metrics…").block(Block::default().borders(Borders::ALL)),
            right,
        );
        return;
    };
    let Some(metric) = app.store.get(name) else { return };
    let [chart_area, detail_area] = Layout::vertical([Constraint::Percentage(65), Constraint::Min(6)]).areas(right);
    draw_chart(frame, chart_area, app, metric);
    draw_detail(frame, detail_area, metric);
}

fn draw_metric_list(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .store
        .names()
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let m = app.store.get(name).expect("declared metric");
            let kind = match m.spec.kind {
                MetricKind::Counter => "Σ",
                MetricKind::Gauge => "≈",
                MetricKind::Histogram => "⧗",
            };
            let style = if i == app.selected {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(vec![
                Line::from(vec![Span::styled(format!("{kind} {name}"), style.add_modifier(Modifier::BOLD))]),
                Line::from(Span::styled(
                    format!("   {}", m.headline()),
                    style.fg(if i == app.selected { Color::Black } else { Color::Gray }),
                )),
            ])
        })
        .collect();
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(" metrics ")),
        area,
    );
}

fn window_start(app: &App) -> u64 {
    app.store.current_second().saturating_sub(app.chart_window_seconds)
}

/// One line on the chart: legend name, colour, (second, value) points.
type Series<'a> = (&'a str, Color, Vec<(f64, f64)>);

fn draw_chart(frame: &mut Frame, area: Rect, app: &App, metric: &Metric) {
    let start = window_start(app) as f64;
    let end = (app.store.current_second().max(1)) as f64;
    let unit = metric.spec.unit.clone().unwrap_or_default();

    let (datasets_data, y_label): (Vec<Series>, String) = match &metric.aggregate {
        Aggregate::Counter { per_second, .. } => {
            let pts: Vec<(f64, f64)> = per_second
                .iter()
                .enumerate()
                .filter(|(i, _)| *i as f64 >= start)
                .map(|(i, v)| (i as f64, *v))
                .collect();
            (vec![("per second", Color::Green, pts)], "/s".into())
        }
        Aggregate::Gauge { per_second, .. } => {
            let pts: Vec<(f64, f64)> = per_second
                .iter()
                .enumerate()
                .filter(|(i, _)| *i as f64 >= start)
                .filter_map(|(i, v)| v.map(|v| (i as f64, v)))
                .collect();
            (vec![("value", Color::Cyan, pts)], unit)
        }
        Aggregate::Histogram { windows, .. } => {
            let sel = |f: fn(&crate::metrics::WindowPoint) -> f64| {
                windows
                    .iter()
                    .filter(|w| w.second as f64 >= start)
                    .map(|w| (w.second as f64, f(w)))
                    .collect::<Vec<_>>()
            };
            (
                vec![
                    ("p50", Color::Green, sel(|w| w.p50)),
                    ("p95", Color::Yellow, sel(|w| w.p95)),
                    ("p99", Color::Red, sel(|w| w.p99)),
                ],
                unit,
            )
        }
    };

    let y_max = datasets_data
        .iter()
        .flat_map(|(_, _, d)| d.iter().map(|p| p.1))
        .fold(0.0_f64, f64::max);
    let y_max = if y_max <= 0.0 { 1.0 } else { y_max * 1.1 };
    let datasets: Vec<Dataset> = datasets_data
        .iter()
        .map(|(name, color, data)| {
            Dataset::default()
                .name(*name)
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(*color))
                .data(data)
        })
        .collect();

    let title = format!(" {}  {} ", metric.spec.name, metric.spec.description.clone().unwrap_or_default());
    let chart = Chart::new(datasets)
        .block(Block::default().borders(Borders::ALL).title(title))
        .x_axis(
            Axis::default()
                .bounds([start, end])
                .labels(vec![
                    format!("{start:.0}s"),
                    format!("{:.0}s", (start + end) / 2.0),
                    format!("{end:.0}s"),
                ])
                .style(Style::default().fg(Color::DarkGray)),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, y_max])
                .labels(vec!["0".to_string(), fmt_num(y_max / 2.0), format!("{} {y_label}", fmt_num(y_max))])
                .style(Style::default().fg(Color::DarkGray)),
        );
    frame.render_widget(chart, area);
}

fn draw_detail(frame: &mut Frame, area: Rect, metric: &Metric) {
    let unit = metric.spec.unit.clone().unwrap_or_default();
    let mut rows: Vec<Row> = Vec::new();
    match &metric.aggregate {
        Aggregate::Counter { total, by_label, .. } => {
            rows.push(Row::new(vec![Cell::from("total"), Cell::from(fmt_num(*total))]));
            for (k, v) in by_label {
                rows.push(Row::new(vec![Cell::from(k.clone()), Cell::from(fmt_num(*v))]));
            }
        }
        Aggregate::Gauge { last, by_label, .. } => {
            rows.push(Row::new(vec![Cell::from("last"), Cell::from(fmt_num(*last))]));
            for (k, v) in by_label {
                rows.push(Row::new(vec![Cell::from(k.clone()), Cell::from(fmt_num(*v))]));
            }
        }
        Aggregate::Histogram { by_label, .. } => {
            if let Some(p) = metric.percentiles() {
                rows.push(Row::new(vec![
                    Cell::from("overall"),
                    Cell::from(format!(
                        "n={}  min {}  p50 {}  p90 {}  p95 {}  p99 {}  max {}  mean {} {unit}",
                        p.count,
                        fmt_num(p.min),
                        fmt_num(p.p50),
                        fmt_num(p.p90),
                        fmt_num(p.p95),
                        fmt_num(p.p99),
                        fmt_num(p.max),
                        fmt_num(p.mean)
                    )),
                ]));
            }
            for (k, h) in by_label {
                let p = crate::metrics::Percentiles::of_histogram(h);
                rows.push(Row::new(vec![
                    Cell::from(k.clone()),
                    Cell::from(format!(
                        "n={}  p50 {}  p95 {}  p99 {} {unit}",
                        p.count,
                        fmt_num(p.p50),
                        fmt_num(p.p95),
                        fmt_num(p.p99)
                    )),
                ]));
            }
        }
    }
    let table =
        Table::new(rows, [Constraint::Length(28), Constraint::Min(20)]).block(Block::default().borders(Borders::ALL).title(" breakdown "));
    frame.render_widget(table, area);
}

fn draw_phases(frame: &mut Frame, area: Rect, app: &App) {
    let phases = app.store.phase_summaries();
    if phases.is_empty() {
        frame.render_widget(
            Paragraph::new("no phases reported by this scenario").block(Block::default().borders(Borders::ALL).title(" phases ")),
            area,
        );
        return;
    }
    let hist_names: Vec<String> = app
        .store
        .names()
        .iter()
        .filter(|n| app.store.get(n).is_some_and(|m| m.spec.kind == MetricKind::Histogram))
        .cloned()
        .collect();
    let counter_names: Vec<String> = app
        .store
        .names()
        .iter()
        .filter(|n| app.store.get(n).is_some_and(|m| m.spec.kind == MetricKind::Counter))
        .cloned()
        .collect();
    let mut header = vec!["phase".to_string(), "dur".to_string()];
    for c in &counter_names {
        header.push(c.clone());
    }
    for h in &hist_names {
        header.push(format!("{h} p50/p95/p99"));
    }
    let verdicts = app.verdicts();
    let rows: Vec<Row> = phases
        .iter()
        .map(|p| {
            let end = p.end_second.unwrap_or(app.store.current_second());
            let pass = verdicts.phases.iter().find(|v| v.phase == p.name).and_then(|v| v.pass);
            let style = match pass {
                Some(true) => Style::default().fg(Color::Green),
                Some(false) => Style::default().fg(Color::Red),
                None => Style::default(),
            };
            let mark = match pass {
                Some(true) => "✓ ",
                Some(false) => "✗ ",
                None => "",
            };
            let mut cells = vec![format!("{mark}{}", p.name), format!("{}s", end - p.start_second)];
            for c in &counter_names {
                let total = p.counters.get(c).copied().unwrap_or(0.0);
                let labels = p
                    .counter_labels
                    .get(c)
                    .map(|m| {
                        m.iter()
                            .map(|(l, n)| format!("{}:{}", l.rsplit('=').next().unwrap_or(l), n))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                cells.push(format!("{} {labels}", fmt_num(total)));
            }
            for h in &hist_names {
                cells.push(
                    p.histograms
                        .get(h)
                        .map(|x| format!("{}/{}/{}", fmt_num(x.p50), fmt_num(x.p95), fmt_num(x.p99)))
                        .unwrap_or_else(|| "-".into()),
                );
            }
            Row::new(cells).style(style)
        })
        .collect();
    let widths: Vec<Constraint> = header.iter().map(|_| Constraint::Min(12)).collect();
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(Style::default().add_modifier(Modifier::BOLD)))
        .block(Block::default().borders(Borders::ALL).title(" phases "));
    frame.render_widget(table, area);
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let visible = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take(visible.max(1))
        .rev()
        .map(|l| {
            let color = match l.level {
                LogLevel::Debug => Color::DarkGray,
                LogLevel::Info => Color::Gray,
                LogLevel::Warn => Color::Yellow,
                LogLevel::Error => Color::Red,
            };
            Line::from(vec![
                Span::styled(format!("{:>5}s ", l.second), Style::default().fg(Color::DarkGray)),
                Span::styled(l.message.clone(), Style::default().fg(color)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" log ")),
        area,
    );
}
