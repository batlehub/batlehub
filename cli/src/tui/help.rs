use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::App;

pub fn render(f: &mut Frame, _app: &App) {
    let area = f.area();

    // Centre a box
    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(10),
            Constraint::Min(0),
            Constraint::Percentage(10),
        ])
        .split(area);
    let hchunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(15),
            Constraint::Min(0),
            Constraint::Percentage(15),
        ])
        .split(vchunks[1]);
    let box_area = hchunks[1];

    let lines = vec![
        Line::from(Span::styled(
            "Keyboard shortcuts",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Global",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  q / Ctrl-C   Quit"),
        Line::from("  ?            This help"),
        Line::from("  Esc          Go back"),
        Line::from(""),
        Line::from(Span::styled(
            "Registry list",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  ↑ / k        Move up"),
        Line::from("  ↓ / j        Move down"),
        Line::from("  Enter        Open registry / browse packages"),
        Line::from("  p            Open publish wizard"),
        Line::from("  s            Open setup wizard (scan current dir)"),
        Line::from("  i            Open IDE setup (detect editor → registry)"),
        Line::from("  L            Open login screen (OIDC / Kubernetes / static token)"),
        Line::from(""),
        Line::from(Span::styled(
            "Package list",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  Enter        View version details"),
        Line::from("  /            Toggle search filter"),
        Line::from(""),
        Line::from(Span::styled(
            "Package detail (versions)",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  w            Why: is the selected version pullable, and your refusals"),
        Line::from("  o            Owners of the package"),
        Line::from("  y / d        Yank / delete selected version (asks first)"),
        Line::from("  u            Unyank selected version"),
        Line::from("  p / P        Pin / unpin selected version against retention"),
        Line::from("  Esc          Close the side pane, then go back"),
        Line::from(""),
        Line::from(Span::styled(
            "Publish wizard",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  Tab          Next field"),
        Line::from("  Shift-Tab    Previous field"),
        Line::from("  Enter        Path: read name and version; Version: publish"),
        Line::from("  Esc          Cancel"),
        Line::from(""),
        Line::from(Span::styled(
            "Setup wizard / IDE setup",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  ↑ / k        Move up"),
        Line::from("  ↓ / j        Move down"),
        Line::from("  (IDE setup)  * marks a registry type not configured yet"),
        Line::from(""),
        Line::from(Span::styled(
            "Login",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  1            Static token"),
        Line::from("  2            OIDC (browser login)"),
        Line::from("  3            Kubernetes service account"),
        Line::from("  Enter        Save to the -P profile and reconnect"),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc or ? to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::White)),
        )
        .alignment(Alignment::Left);

    f.render_widget(para, box_area);
}
