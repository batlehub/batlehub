mod admin_stats;
mod help;
mod ide_setup;
mod list_nav;
mod login;
mod package_detail;
mod package_list;
mod publish_form;
mod registry_list;
mod setup_wizard;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::api::{package::PackageQuery, BatleHubClient};
use tui_input::Input;
use uuid::Uuid;

use admin_stats::AdminStatsWidget;
use ide_setup::IdeSetupWidget;
use login::LoginWidget;
use package_detail::PackageDetailWidget;
use package_list::PackageListWidget;
use publish_form::PublishFormWidget;
use registry_list::RegistryListWidget;
use setup_wizard::SetupWizardWidget;

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    RegistryList,
    PackageList { registry: String },
    PackageDetail { registry: String, name: String },
    PublishWizard,
    SetupWizard,
    IdeSetup,
    Login,
    Help,
    AdminStats,
}

pub struct App {
    pub screen: Screen,
    pub prev_screen: Option<Screen>,
    pub registry_list: RegistryListWidget,
    pub package_list: PackageListWidget,
    pub package_detail: PackageDetailWidget,
    pub publish_form: PublishFormWidget,
    pub setup_wizard: SetupWizardWidget,
    pub ide_setup: IdeSetupWidget,
    pub login: LoginWidget,
    pub admin_stats: AdminStatsWidget,
    pub client: BatleHubClient,
    pub status_msg: Option<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new(client: BatleHubClient) -> Self {
        Self {
            screen: Screen::RegistryList,
            prev_screen: None,
            registry_list: RegistryListWidget::new(),
            package_list: PackageListWidget::new(),
            package_detail: PackageDetailWidget::new(),
            publish_form: PublishFormWidget::new(),
            setup_wizard: SetupWizardWidget::new(),
            ide_setup: IdeSetupWidget::new(),
            login: LoginWidget::new(),
            admin_stats: AdminStatsWidget::new(),
            client,
            status_msg: None,
            should_quit: false,
        }
    }

    pub fn go_back(&mut self) {
        if let Some(prev) = self.prev_screen.take() {
            self.screen = prev;
        } else {
            self.screen = Screen::RegistryList;
        }
    }
}

pub async fn run(client: BatleHubClient) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(client);

    // Load initial registry list
    match app.client.list_registries().await {
        Ok(registries) => app.registry_list.nav.set_items(registries),
        Err(e) => app.status_msg = Some(format!("Error loading registries: {e}")),
    }

    let result = event_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn is_quit_key(key: &event::KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, app))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if is_quit_key(&key) {
                    break;
                }
                handle_key(app, key).await;
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

async fn handle_key(app: &mut App, key: event::KeyEvent) {
    match &app.screen.clone() {
        Screen::RegistryList => handle_registry_list(app, key).await,
        Screen::PackageList { registry } => handle_package_list(app, key, registry.clone()).await,
        Screen::PackageDetail { registry, name } => {
            handle_package_detail(app, key, registry.clone(), name.clone()).await
        }
        Screen::PublishWizard => handle_publish_form(app, key),
        Screen::SetupWizard => handle_setup_wizard(app, key),
        Screen::IdeSetup => handle_ide_setup(app, key),
        Screen::Login => handle_login(app, key).await,
        Screen::Help => {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                app.go_back();
            }
        }
        Screen::AdminStats => handle_admin_stats(app, key).await,
    }
}

async fn open_registry(app: &mut App, registry: String) {
    match app
        .client
        .list_packages(PackageQuery {
            registry: Some(registry.clone()),
            name: None,
            blocked: false,
            page: 0,
            per_page: 100,
        })
        .await
    {
        Ok(resp) => {
            app.package_list.set_items(resp.items, resp.total);
            app.prev_screen = Some(Screen::RegistryList);
            app.screen = Screen::PackageList { registry };
        }
        Err(e) => app.status_msg = Some(format!("Error: {e}")),
    }
}

async fn handle_registry_list(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.registry_list.nav.prev(),
        KeyCode::Down | KeyCode::Char('j') => app.registry_list.nav.next(),
        KeyCode::Enter => {
            if let Some(reg) = app.registry_list.nav.selected() {
                let registry = reg.name.clone();
                open_registry(app, registry).await;
            }
        }
        KeyCode::Char('p') => {
            app.prev_screen = Some(Screen::RegistryList);
            app.screen = Screen::PublishWizard;
        }
        KeyCode::Char('s') => {
            let server_url = app.client.base_url.clone();
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".to_string());
            // The TUI already holds the registry list, so its snippets name the
            // real registries — and their own hosts when host-routed.
            let targets = crate::api::registry::RegistryTargets::new(
                &server_url,
                &app.registry_list.nav.items,
            );
            let items =
                crate::api::setup::scan_project_types(std::path::Path::new(&cwd), &targets, 2);
            app.setup_wizard.set_items(items, cwd);
            app.prev_screen = Some(Screen::RegistryList);
            app.screen = Screen::SetupWizard;
        }
        KeyCode::Char('i') => {
            let server_url = app.client.base_url.clone();
            let items = crate::api::ide::detect_ides(&server_url, &app.registry_list.nav.items);
            app.ide_setup.set_items(items);
            app.prev_screen = Some(Screen::RegistryList);
            app.screen = Screen::IdeSetup;
        }
        KeyCode::Char('L') => {
            app.login = LoginWidget::new();
            app.prev_screen = Some(Screen::RegistryList);
            app.screen = Screen::Login;
        }
        KeyCode::Char('?') => {
            app.prev_screen = Some(Screen::RegistryList);
            app.screen = Screen::Help;
        }
        KeyCode::Char('A') => {
            app.prev_screen = Some(Screen::RegistryList);
            app.screen = Screen::AdminStats;
            load_admin_stats(app).await;
        }
        _ => {}
    }
}

async fn load_admin_stats(app: &mut App) {
    app.admin_stats.loading = true;
    app.admin_stats.error = None;
    match app.client.admin_stats().await {
        Ok(v) => app.admin_stats.set_data(v),
        Err(e) => app.admin_stats.set_error(e.to_string()),
    }
}

async fn handle_admin_stats(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => app.go_back(),
        KeyCode::Char('r') => load_admin_stats(app).await,
        _ => {}
    }
}

async fn open_package_detail(app: &mut App, registry: String, name: String) {
    match app
        .client
        .list_packages(PackageQuery {
            registry: Some(registry.clone()),
            name: Some(name.clone()),
            blocked: false,
            page: 0,
            per_page: 200,
        })
        .await
    {
        Ok(resp) => {
            let versions: Vec<_> = resp
                .items
                .into_iter()
                .filter(|p| p.name == name && p.registry == registry)
                .collect();
            app.package_detail.nav.set_items(versions);
            app.prev_screen = Some(Screen::PackageList {
                registry: registry.clone(),
            });
            app.screen = Screen::PackageDetail { registry, name };
        }
        Err(e) => app.status_msg = Some(format!("Error: {e}")),
    }
}

async fn handle_package_list(app: &mut App, key: event::KeyEvent, registry: String) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.package_list.prev(),
        KeyCode::Down | KeyCode::Char('j') => app.package_list.next(),
        KeyCode::Esc => app.go_back(),
        KeyCode::Enter => {
            if let Some(pkg) = app.package_list.selected() {
                let name = pkg.name.clone();
                open_package_detail(app, registry, name).await;
            }
        }
        KeyCode::Char('/') => {
            if app.package_list.search_active {
                app.package_list.handle_search_key(key);
            } else {
                app.package_list.toggle_search();
            }
        }
        KeyCode::Char('?') => {
            app.prev_screen = Some(Screen::PackageList { registry });
            app.screen = Screen::Help;
        }
        _ => {
            // Pass other keys to search box if active
            if app.package_list.search_active {
                app.package_list.handle_search_key(key);
            }
        }
    }
}

async fn handle_package_detail(
    app: &mut App,
    key: event::KeyEvent,
    registry: String,
    name: String,
) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.package_detail.nav.prev(),
        KeyCode::Down | KeyCode::Char('j') => app.package_detail.nav.next(),
        KeyCode::Esc => app.go_back(),
        KeyCode::Char('y') => {
            if let Some(pkg) = app.package_detail.nav.selected() {
                let version = pkg.version.clone();
                match app.client.yank_version(&registry, &name, &version).await {
                    Ok(()) => {
                        app.status_msg = Some(format!("Yanked {name}@{version}"));
                        // Refresh
                        refresh_detail(app, &registry, &name).await;
                    }
                    Err(e) => app.status_msg = Some(format!("Error: {e}")),
                }
            }
        }
        KeyCode::Char('u') => {
            if let Some(pkg) = app.package_detail.nav.selected() {
                let version = pkg.version.clone();
                match app.client.unyank_version(&registry, &name, &version).await {
                    Ok(()) => {
                        app.status_msg = Some(format!("Unyanked {name}@{version}"));
                        refresh_detail(app, &registry, &name).await;
                    }
                    Err(e) => app.status_msg = Some(format!("Error: {e}")),
                }
            }
        }
        KeyCode::Char('?') => {
            app.prev_screen = Some(Screen::PackageDetail { registry, name });
            app.screen = Screen::Help;
        }
        _ => {}
    }
}

async fn refresh_detail(app: &mut App, registry: &str, name: &str) {
    if let Ok(resp) = app
        .client
        .list_packages(PackageQuery {
            registry: Some(registry.to_string()),
            name: Some(name.to_string()),
            blocked: false,
            page: 0,
            per_page: 200,
        })
        .await
    {
        let versions: Vec<_> = resp
            .items
            .into_iter()
            .filter(|p| p.name == name && p.registry == registry)
            .collect();
        app.package_detail.nav.set_items(versions);
    }
}

fn handle_setup_wizard(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.setup_wizard.nav.prev(),
        KeyCode::Down | KeyCode::Char('j') => app.setup_wizard.nav.next(),
        KeyCode::Esc => app.go_back(),
        KeyCode::Char('?') => {
            app.prev_screen = Some(Screen::SetupWizard);
            app.screen = Screen::Help;
        }
        _ => {}
    }
}

fn handle_ide_setup(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.ide_setup.nav.prev(),
        KeyCode::Down | KeyCode::Char('j') => app.ide_setup.nav.next(),
        KeyCode::Esc => app.go_back(),
        KeyCode::Char('?') => {
            app.prev_screen = Some(Screen::IdeSetup);
            app.screen = Screen::Help;
        }
        _ => {}
    }
}

async fn handle_login(app: &mut App, key: event::KeyEvent) {
    // Switching to OIDC tab fetches the authorization URL asynchronously
    if key.code == KeyCode::Char('2') {
        app.login.method = login::LoginMethod::Oidc;
        app.login.status = None;
        app.login.token_input = Input::default();
        app.login.path_input = Input::default();
        if app.login.oidc_url.is_none() {
            let csrf = Uuid::new_v4().to_string();
            match app.client.oidc_login_url(&csrf, None, None).await {
                Ok(url) => app.login.oidc_url = Some(url),
                Err(e) => app.login.status = Some(format!("OIDC unavailable: {e}")),
            }
        }
        return;
    }

    if let Some(_back) = login::handle_key(app, key) {
        app.go_back();
        app.status_msg =
            Some("Credentials saved. Restart TUI to connect with new credentials.".into());
    }
}

fn handle_publish_form(app: &mut App, key: event::KeyEvent) {
    if key.code == KeyCode::Esc {
        app.go_back();
    } else {
        app.publish_form.handle_key(key);
    }
}

fn render(f: &mut ratatui::Frame, app: &App) {
    match &app.screen {
        Screen::RegistryList => {
            registry_list::render(f, app);
        }
        Screen::PackageList { registry } => {
            package_list::render(f, app, registry);
        }
        Screen::PackageDetail { registry, name } => {
            package_detail::render(f, app, registry, name);
        }
        Screen::PublishWizard => {
            publish_form::render(f, app);
        }
        Screen::SetupWizard => {
            setup_wizard::render(f, app);
        }
        Screen::IdeSetup => {
            ide_setup::render(f, app);
        }
        Screen::Login => {
            login::render(f, app);
        }
        Screen::Help => {
            help::render(f, app);
        }
        Screen::AdminStats => {
            admin_stats::render(f, app);
        }
    }

    // Status bar overlay
    if let Some(msg) = &app.status_msg {
        let area = f.area();
        let status_area = ratatui::layout::Rect {
            x: 0,
            y: area.height.saturating_sub(1),
            width: area.width,
            height: 1,
        };
        f.render_widget(
            ratatui::widgets::Paragraph::new(msg.as_str())
                .style(ratatui::style::Style::default().fg(ratatui::style::Color::Yellow)),
            status_area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app() -> App {
        let client = BatleHubClient::new("http://localhost:8080", None).expect("client");
        App::new(client)
    }

    #[test]
    fn go_back_returns_to_prev_screen_when_set() {
        let mut app = make_app();
        app.screen = Screen::PackageList {
            registry: "npm".to_owned(),
        };
        app.prev_screen = Some(Screen::RegistryList);

        app.go_back();
        assert_eq!(app.screen, Screen::RegistryList);
        assert!(app.prev_screen.is_none());
    }

    #[test]
    fn go_back_defaults_to_registry_list_when_no_prev() {
        let mut app = make_app();
        app.screen = Screen::Help;
        app.prev_screen = None;

        app.go_back();
        assert_eq!(app.screen, Screen::RegistryList);
    }

    #[tokio::test]
    async fn slash_key_opens_search_when_inactive() {
        let mut app = make_app();
        assert!(!app.package_list.search_active);

        handle_package_list(
            &mut app,
            event::KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            "npm".to_owned(),
        )
        .await;

        assert!(app.package_list.search_active);
    }

    #[tokio::test]
    async fn slash_key_inserts_literal_slash_while_search_is_active() {
        let mut app = make_app();
        app.package_list.toggle_search();
        assert!(app.package_list.search_active);

        handle_package_list(
            &mut app,
            event::KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            "npm".to_owned(),
        )
        .await;

        // Must still be searching, with the '/' appended to the query rather
        // than the search box being toggled closed.
        assert!(app.package_list.search_active);
        assert_eq!(app.package_list.search_input.value(), "/");
    }

    #[tokio::test]
    async fn switching_to_oidc_tab_clears_leftover_input() {
        let mut app = make_app();
        app.login.token_input =
            tui_input::Input::default().with_value("leftover-static-token".to_owned());

        handle_login(
            &mut app,
            event::KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
        )
        .await;

        assert_eq!(app.login.method, login::LoginMethod::Oidc);
        assert_eq!(app.login.token_input.value(), "");
        assert_eq!(app.login.path_input.value(), "");
    }

    #[test]
    fn is_quit_key_detects_q_and_ctrl_c() {
        assert!(is_quit_key(&event::KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE
        )));
        assert!(is_quit_key(&event::KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_quit_key(&event::KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
        assert!(!is_quit_key(&event::KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
    }
}
