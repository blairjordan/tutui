use crate::config::LoadedRun;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

pub fn draw(frame: &mut Frame, runs: &[LoadedRun], selected: usize, error: Option<&str>) {
    let [header, body, footer] = Layout::vertical([Constraint::Length(2), Constraint::Min(3), Constraint::Length(2)]).areas(frame.area());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("tutui", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  choose a run config"),
        ])),
        header,
    );
    let items: Vec<ListItem> = runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let style = if i == selected {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                Style::default()
            };
            let desc = r.config.description.clone().unwrap_or_default();
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!(" {}  [{}]", r.config.name, r.config.scenario),
                    style.add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("   {desc}  ({})", r.path.display()),
                    style.fg(if i == selected { Color::Black } else { Color::Gray }),
                )),
            ])
        })
        .collect();
    frame.render_widget(List::new(items).block(Block::default().borders(Borders::ALL)), body);
    let hint = match error {
        Some(e) => Line::from(Span::styled(e, Style::default().fg(Color::Red))),
        None => Line::from(Span::styled("enter run   ↑↓ select   q quit", Style::default().fg(Color::DarkGray))),
    };
    frame.render_widget(Paragraph::new(hint), footer);
}
