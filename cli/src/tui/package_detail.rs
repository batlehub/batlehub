use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::api::package::{PackageStatus, PackageSummary};

use super::list_nav::ListNav;
use super::App;

/// The text beside the version list: `w`'s Why or `o`'s owners.
pub struct Pane {
    pub title: String,
    pub text: String,
}

#[derive(Default)]
pub struct PackageDetailWidget {
    pub nav: ListNav<PackageSummary>,
    pub pane: Option<Pane>,
}

impl PackageDetailWidget {
    pub fn new() -> Self {
        Self::default()
    }
}

pub fn render(f: &mut Frame, app: &App, registry: &str, name: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    let help_area = chunks[1];
    let main_area = match &app.package_detail.pane {
        Some(pane) => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(chunks[0]);
            f.render_widget(
                Paragraph::new(pane.text.as_str())
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .title(pane.title.as_str())
                            .borders(Borders::ALL),
                    ),
                cols[1],
            );
            cols[0]
        }
        None => chunks[0],
    };

    let items: Vec<ListItem> = app
        .package_detail
        .nav
        .items
        .iter()
        .map(|p| {
            let (status_str, status_color) = match &p.status {
                PackageStatus::Available => ("✓ available", Color::Green),
                PackageStatus::Blocked { reason } => {
                    let _ = reason;
                    ("✗ blocked  ", Color::Red)
                }
            };
            let reason_suffix = match &p.status {
                PackageStatus::Blocked { reason } => format!("  ({})", reason),
                _ => String::new(),
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<20}", p.version),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(status_str, Style::default().fg(status_color)),
                Span::styled(reason_suffix, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("   {:>8} dl", p.access_count),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = format!(
        " {registry} > {name} — {} version(s) ",
        app.package_detail.nav.items.len()
    );
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, main_area, &mut app.package_detail.nav.state.clone());

    let help = Paragraph::new(
        " Esc:back  ↑↓:navigate  w:why  o:owners  y:yank  u:unyank  d:delete  p:pin  P:unpin  ?:help",
    )
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(help, help_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(v: &str, status: PackageStatus) -> PackageSummary {
        PackageSummary {
            registry: "npm-proxy".to_owned(),
            name: "left-pad".to_owned(),
            version: v.to_owned(),
            artifact: None,
            status,
            access_count: 0,
        }
    }

    #[test]
    fn set_items_selects_first_when_non_empty() {
        let mut w = PackageDetailWidget::new();
        w.nav.set_items(vec![
            version("1.0.0", PackageStatus::Available),
            version("1.1.0", PackageStatus::Available),
        ]);
        assert_eq!(w.nav.state.selected(), Some(0));
        assert_eq!(w.nav.selected().unwrap().version, "1.0.0");
    }

    #[test]
    fn set_items_empty_clears_selection() {
        let mut w = PackageDetailWidget::new();
        w.nav
            .set_items(vec![version("1.0.0", PackageStatus::Available)]);
        assert_eq!(w.nav.state.selected(), Some(0));

        w.nav.set_items(vec![]);
        assert_eq!(w.nav.state.selected(), None);
        assert!(w.nav.selected().is_none());
    }

    #[test]
    fn next_and_prev_wrap_around() {
        let mut w = PackageDetailWidget::new();
        w.nav.set_items(vec![
            version("1.0.0", PackageStatus::Available),
            version(
                "1.1.0",
                PackageStatus::Blocked {
                    reason: "cve".into(),
                },
            ),
        ]);

        w.nav.next();
        assert_eq!(w.nav.state.selected(), Some(1));
        w.nav.next();
        assert_eq!(w.nav.state.selected(), Some(0));

        w.nav.prev();
        assert_eq!(w.nav.state.selected(), Some(1));
    }

    #[test]
    fn next_and_prev_on_empty_list_are_noops() {
        let mut w = PackageDetailWidget::new();
        w.nav.next();
        w.nav.prev();
        assert_eq!(w.nav.state.selected(), None);
    }
}
