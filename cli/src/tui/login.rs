use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::api::auth::parse_oidc_paste;
use crate::config::ConfigFile;

use super::App;

#[derive(Debug, Clone, PartialEq)]
pub enum LoginMethod {
    StaticToken,
    Oidc,
    Kubernetes,
}

pub struct LoginWidget {
    pub method: LoginMethod,
    pub token_input: Input,
    pub path_input: Input,
    /// OIDC authorization URL fetched when switching to the OIDC tab.
    pub oidc_url: Option<String>,
    /// Feedback message shown in the footer.
    pub status: Option<String>,
}

impl LoginWidget {
    pub fn new() -> Self {
        Self {
            method: LoginMethod::StaticToken,
            token_input: Input::default(),
            path_input: Input::default(),
            oidc_url: None,
            status: None,
        }
    }

    /// Save the current input to the `-P` profile (`default` when none), as
    /// `auth login` does (RFC 0040 §4.10). Returns `true` on success (caller
    /// should navigate back).
    pub fn save_to_config(&mut self, profile: Option<&str>) -> bool {
        let mut cfg = match ConfigFile::load() {
            Ok(c) => c,
            Err(e) => {
                self.status = Some(format!("Failed to load config: {e}"));
                return false;
            }
        };
        if !self.apply_to(&mut cfg, profile) {
            return false;
        }
        match cfg.save() {
            Ok(()) => true,
            Err(e) => {
                self.status = Some(format!("Failed to save config: {e}"));
                false
            }
        }
    }

    /// Write the input into `profile`'s entry of `cfg`; `false` with a status
    /// when the input is empty.
    fn apply_to(&mut self, cfg: &mut ConfigFile, profile: Option<&str>) -> bool {
        let target = match profile {
            Some(n) => cfg.profiles.entry(n.to_owned()).or_default(),
            None => &mut cfg.default,
        };
        match self.method {
            LoginMethod::StaticToken => {
                let token = self.token_input.value().to_string();
                if token.is_empty() {
                    self.status = Some("Token cannot be empty.".into());
                    return false;
                }
                target.token = Some(token);
                target.oidc_refresh_token = None;
                target.oidc_expires_at = None;
                target.kubernetes_token_path = None;
            }
            LoginMethod::Oidc => {
                let raw = self.token_input.value().trim().to_string();
                if raw.is_empty() {
                    self.status = Some("Paste the token or the full redirect URL.".into());
                    return false;
                }
                // No state check here, unlike `auth login`: this screen only
                // accepts a paste, it never starts the flow, so there is no
                // locally-generated CSRF value to compare the echoed
                // `oidc_state` against. The server still refuses a state it did
                // not issue or has already redeemed.
                let paste = parse_oidc_paste(&raw);
                target.token = Some(paste.access_token);
                target.oidc_refresh_token = paste.refresh_token;
                target.oidc_expires_at = paste.expires_at;
                // The provider the redirect names, so refreshes go back to the
                // endpoint that issued this token rather than to `flows.first()`.
                target.oidc_provider = paste.provider;
                target.kubernetes_token_path = None;
            }
            LoginMethod::Kubernetes => {
                let path = self.path_input.value().trim().to_string();
                if path.is_empty() {
                    self.status = Some("Token path cannot be empty.".into());
                    return false;
                }
                target.kubernetes_token_path = Some(path);
                target.token = None;
                target.oidc_refresh_token = None;
                target.oidc_expires_at = None;
            }
        }
        true
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match self.method {
            LoginMethod::StaticToken | LoginMethod::Oidc => {
                self.token_input
                    .handle_event(&crossterm::event::Event::Key(key));
            }
            LoginMethod::Kubernetes => {
                self.path_input
                    .handle_event(&crossterm::event::Event::Key(key));
            }
        }
    }
}

pub fn render(f: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar
            Constraint::Min(0),    // content
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    let tab_area = outer[0];
    let content_area = outer[1];
    let footer_area = outer[2];

    // ── Tab bar ────────────────────────────────────────────────────────────────
    let active = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let inactive = Style::default().fg(Color::DarkGray);

    let tabs = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "[1] Static Token",
            if app.login.method == LoginMethod::StaticToken {
                active
            } else {
                inactive
            },
        ),
        Span::raw("   "),
        Span::styled(
            "[2] OIDC",
            if app.login.method == LoginMethod::Oidc {
                active
            } else {
                inactive
            },
        ),
        Span::raw("   "),
        Span::styled(
            "[3] Kubernetes",
            if app.login.method == LoginMethod::Kubernetes {
                active
            } else {
                inactive
            },
        ),
    ]);
    let tab_bar =
        Paragraph::new(tabs).block(Block::default().title(" Login ").borders(Borders::ALL));
    f.render_widget(tab_bar, tab_area);

    // ── Content ────────────────────────────────────────────────────────────────
    match app.login.method {
        LoginMethod::StaticToken => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Enter your static bearer token:",
                    Style::default().fg(Color::Gray),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(
                        app.login.token_input.value(),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
                ]),
            ];
            let block = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
            f.render_widget(block, content_area);
        }
        LoginMethod::Oidc => {
            let url_line = app
                .login
                .oidc_url
                .as_deref()
                .unwrap_or("(fetching OIDC URL — press 2 again if it does not appear)");
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Open this URL in your browser:",
                    Style::default().fg(Color::Gray),
                )),
                Line::from(""),
                Line::from(Span::styled(url_line, Style::default().fg(Color::Cyan))),
                Line::from(""),
                Line::from(Span::styled(
                    "Then paste the oidc_access_token value (or the full redirect URL):",
                    Style::default().fg(Color::Gray),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(
                        app.login.token_input.value(),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
                ]),
            ];
            let block = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: false });
            f.render_widget(block, content_area);
        }
        LoginMethod::Kubernetes => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Path to the Kubernetes service account token file:",
                    Style::default().fg(Color::Gray),
                )),
                Line::from(Span::styled(
                    "(default: /var/run/secrets/kubernetes.io/serviceaccount/token)",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::raw("> "),
                    Span::styled(
                        app.login.path_input.value(),
                        Style::default().fg(Color::White),
                    ),
                    Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
                ]),
            ];
            let block = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
            f.render_widget(block, content_area);
        }
    }

    // ── Footer ─────────────────────────────────────────────────────────────────
    let footer_text = if let Some(ref msg) = app.login.status {
        msg.as_str()
    } else {
        " Enter:save  Esc:back  1/2/3:switch method"
    };
    let footer_style = if app.login.status.is_some() {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(Paragraph::new(footer_text).style(footer_style), footer_area);
}

pub fn handle_key(app: &mut App, key: KeyEvent) -> Option<ShouldGoBack> {
    match key.code {
        KeyCode::Esc => return Some(ShouldGoBack),
        KeyCode::Char('1') => {
            app.login.method = LoginMethod::StaticToken;
            app.login.status = None;
            app.login.token_input = Input::default();
            app.login.path_input = Input::default();
        }
        KeyCode::Char('3') => {
            app.login.method = LoginMethod::Kubernetes;
            app.login.status = None;
            app.login.token_input = Input::default();
            app.login.path_input = Input::default();
        }
        KeyCode::Enter => {
            if app.login.save_to_config(app.profile.as_deref()) {
                return Some(ShouldGoBack);
            }
        }
        _ => app.login.handle_key(key),
    }
    None
}

/// Marker returned when the login screen wants to navigate back.
pub struct ShouldGoBack;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::BatleHubClient;
    use crossterm::event::KeyModifiers;

    fn make_app() -> App {
        let client = BatleHubClient::new("http://localhost:8080", None).expect("client");
        App::new(client)
    }

    #[test]
    fn handle_key_static_token_routes_to_token_input() {
        let mut w = LoginWidget::new();
        w.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(w.token_input.value(), "a");
        assert_eq!(w.path_input.value(), "");
    }

    #[test]
    fn handle_key_kubernetes_routes_to_path_input() {
        let mut w = LoginWidget::new();
        w.method = LoginMethod::Kubernetes;
        w.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(w.path_input.value(), "a");
        assert_eq!(w.token_input.value(), "");
    }

    #[test]
    fn a_token_lands_in_the_selected_profile_and_leaves_default_alone() {
        let mut cfg = ConfigFile::default();
        cfg.default.token = Some("default-token".into());
        let mut w = LoginWidget::new();
        w.token_input = Input::default().with_value("work-token".into());

        assert!(w.apply_to(&mut cfg, Some("work")));
        assert_eq!(cfg.profiles["work"].token.as_deref(), Some("work-token"));
        assert_eq!(cfg.default.token.as_deref(), Some("default-token"));
    }

    #[test]
    fn save_to_config_empty_static_token_fails_with_message() {
        let mut w = LoginWidget::new();
        assert!(!w.apply_to(&mut ConfigFile::default(), None));
        assert_eq!(w.status.as_deref(), Some("Token cannot be empty."));
    }

    #[test]
    fn save_to_config_empty_oidc_paste_fails_with_message() {
        let mut w = LoginWidget::new();
        w.method = LoginMethod::Oidc;
        assert!(!w.apply_to(&mut ConfigFile::default(), None));
        assert_eq!(
            w.status.as_deref(),
            Some("Paste the token or the full redirect URL.")
        );
    }

    #[test]
    fn save_to_config_empty_kubernetes_path_fails_with_message() {
        let mut w = LoginWidget::new();
        w.method = LoginMethod::Kubernetes;
        assert!(!w.apply_to(&mut ConfigFile::default(), None));
        assert_eq!(w.status.as_deref(), Some("Token path cannot be empty."));
    }

    #[test]
    fn module_handle_key_esc_returns_should_go_back() {
        let mut app = make_app();
        let result = handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(result.is_some());
    }

    #[test]
    fn module_handle_key_switches_method_and_clears_status() {
        let mut app = make_app();
        app.login.status = Some("err".to_owned());
        app.login.method = LoginMethod::Oidc;

        let result = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        );
        assert!(result.is_none());
        assert_eq!(app.login.method, LoginMethod::StaticToken);
        assert!(app.login.status.is_none());

        let result = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
        );
        assert!(result.is_none());
        assert_eq!(app.login.method, LoginMethod::Kubernetes);
    }

    #[test]
    fn module_handle_key_enter_with_empty_token_does_not_go_back() {
        let mut app = make_app();
        let result = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(result.is_none());
        assert_eq!(app.login.status.as_deref(), Some("Token cannot be empty."));
    }

    #[test]
    fn module_handle_key_switching_tabs_clears_leftover_input() {
        let mut app = make_app();
        // Type a static token, then switch to Kubernetes — leftover text must
        // not still be sitting in token_input/path_input for the new tab to
        // pick up.
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(app.login.token_input.value(), "x");

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE),
        );
        assert_eq!(app.login.method, LoginMethod::Kubernetes);
        assert_eq!(app.login.token_input.value(), "");
        assert_eq!(app.login.path_input.value(), "");

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert_eq!(app.login.path_input.value(), "y");

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        );
        assert_eq!(app.login.method, LoginMethod::StaticToken);
        assert_eq!(app.login.token_input.value(), "");
        assert_eq!(app.login.path_input.value(), "");
    }

    #[test]
    fn module_handle_key_default_forwards_to_input() {
        let mut app = make_app();
        let result = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );
        assert!(result.is_none());
        assert_eq!(app.login.token_input.value(), "z");
    }
}
