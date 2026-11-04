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
use crate::cli::lifecycle::{self, Verb};
use crate::cli::publish::PublishArgs;
use crate::config::ConfigFile;
use tui_input::Input;
use uuid::Uuid;

use admin_stats::AdminStatsWidget;
use ide_setup::IdeSetupWidget;
use login::LoginWidget;
use package_detail::{PackageDetailWidget, Pane};
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

/// A destructive key waiting for its `y` (RFC 0040 §4.5, §4.10): nothing is
/// sent until it comes.
pub struct PendingConfirm {
    pub verb: Verb,
    pub registry: String,
    pub name: String,
    pub version: String,
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
    /// The `-P` profile: the one the login screen writes.
    pub profile: Option<String>,
    pub confirm: Option<PendingConfirm>,
    /// A submitted publish form, run by the event loop on the plain terminal.
    pub publish_request: Option<(PublishArgs, Option<String>)>,
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
            profile: None,
            confirm: None,
            publish_request: None,
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

pub async fn run(client: BatleHubClient, profile: Option<String>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(client);
    app.profile = profile;

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

        if let Some((args, registry)) = app.publish_request.take() {
            publish_outside(terminal, app, args, registry).await?;
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// `publish` prints as it goes, so it runs on the plain terminal, exactly as
/// the CLI's `publish` does, and the TUI comes back once it is read.
async fn publish_outside(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    args: PublishArgs,
    registry: Option<String>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let result = crate::cli::publish::run(args, &app.client, registry.as_deref()).await;
    if let Err(e) = &result {
        eprintln!("error: {e:#}");
    }
    eprint!("\nPress Enter to return to the TUI ");
    tokio::task::spawn_blocking(|| std::io::stdin().read_line(&mut String::new())).await??;

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    match result {
        Ok(()) => {
            app.status_msg = Some("Published.".into());
            app.go_back();
        }
        Err(e) => app.status_msg = Some(format!("Publish failed: {e:#}")),
    }
    Ok(())
}

async fn handle_key(app: &mut App, key: event::KeyEvent) {
    if let Some(p) = app.confirm.take() {
        if key.code == KeyCode::Char('y') {
            act(app, p.verb, p.registry, p.name, p.version).await;
        } else {
            app.status_msg = Some(format!("Did not {}.", p.verb.name()));
        }
        return;
    }
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
        KeyCode::Esc if app.package_detail.pane.is_some() => app.package_detail.pane = None,
        KeyCode::Esc => app.go_back(),
        KeyCode::Char(c @ ('y' | 'u' | 'd' | 'p' | 'P')) => {
            let verb = match c {
                'y' => Verb::Yank,
                'u' => Verb::Unyank,
                'd' => Verb::Delete,
                'p' => Verb::Pin,
                _ => Verb::Unpin,
            };
            if let Some(pkg) = app.package_detail.nav.selected() {
                let version = pkg.version.clone();
                if verb.destructive() {
                    app.confirm = Some(PendingConfirm {
                        verb,
                        registry,
                        name,
                        version,
                    });
                } else {
                    act(app, verb, registry, name, version).await;
                }
            }
        }
        KeyCode::Char('w') => show_why(app, &registry, &name).await,
        KeyCode::Char('o') => show_owners(app, &registry, &name).await,
        KeyCode::Char('?') => {
            app.prev_screen = Some(Screen::PackageDetail { registry, name });
            app.screen = Screen::Help;
        }
        _ => {}
    }
}

/// One lifecycle verb on one version, through `package <verb>`'s own requests.
async fn act(app: &mut App, verb: Verb, registry: String, name: String, version: String) {
    let coord = format!("{registry}/{name}@{version}");
    match lifecycle::apply_one(verb, (registry.clone(), name.clone(), version), &app.client).await {
        Ok(()) => {
            app.status_msg = Some(format!("{} {coord}", verb.past()));
            refresh_detail(app, &registry, &name).await;
        }
        Err(e) => app.status_msg = Some(format!("Error: {e:#}")),
    }
}

/// `why <coord>` for the selected version, then the caller's own recent
/// refusals of this package (`why --last`, narrowed to it).
async fn show_why(app: &mut App, registry: &str, name: &str) {
    let Some(version) = app.package_detail.nav.selected().map(|p| p.version.clone()) else {
        return;
    };
    let mut text = match app.client.pullability(registry, name, Some(&version)).await {
        // shortcut: against a server older than RFC 0040, `render` warns on stderr and
        // smears the screen until the next full redraw; acceptable for a pre-0040 server.
        Ok(answer) => crate::cli::why::render(registry, name, &answer),
        Err(e) => format!("Error: {e:#}"),
    };
    if let Ok(rows) = app.client.my_refusals(20).await {
        let mine: Vec<String> = rows
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["registry"] == registry && r["name"] == name)
            .map(|r| {
                format!(
                    "  @{}  {}",
                    r["version"].as_str().unwrap_or(""),
                    r["reason_code"].as_str().unwrap_or("—")
                )
            })
            .collect();
        if !mine.is_empty() {
            text.push_str("\nYour recent refusals of this package:\n");
            text.push_str(&mine.join("\n"));
        }
    }
    app.package_detail.pane = Some(Pane {
        title: format!(" Why {registry}/{name}@{version} "),
        text,
    });
}

async fn show_owners(app: &mut App, registry: &str, name: &str) {
    let text = match app.client.list_owners(registry, name).await {
        Ok(owners) if owners.is_empty() => "No owner: the package is nobody's.".to_owned(),
        Ok(owners) => owners
            .iter()
            .map(|o| format!("{}:{}  {}", o.principal_type, o.principal_id, o.role))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => format!("Error: {e:#}"),
    };
    app.package_detail.pane = Some(Pane {
        title: format!(" Owners of {registry}/{name} "),
        text,
    });
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

    if login::handle_key(app, key).is_some() {
        app.go_back();
        if key.code != KeyCode::Esc {
            reconnect(app).await;
        }
    }
}

/// Rebuild the client from the profile the login screen just wrote, so the
/// new credential is in use at once.
async fn reconnect(app: &mut App) {
    let base = app.client.base_url.clone();
    let client = match ConfigFile::load() {
        Ok(mut cfg) => crate::api::auth::resolve_token(&base, app.profile.as_deref(), &mut cfg)
            .await
            .and_then(|token| BatleHubClient::new(&base, token.as_deref())),
        Err(e) => Err(e),
    };
    match client {
        Ok(client) => {
            app.client = client;
            match app.client.list_registries().await {
                Ok(registries) => app.registry_list.nav.set_items(registries),
                Err(e) => app.status_msg = Some(format!("Signed in, but: {e:#}")),
            }
            if app.status_msg.is_none() {
                app.status_msg = Some(format!(
                    "Signed in to profile {}.",
                    app.profile.as_deref().unwrap_or("default")
                ));
            }
        }
        Err(e) => app.status_msg = Some(format!("Credentials saved, but: {e:#}")),
    }
}

fn handle_publish_form(app: &mut App, key: event::KeyEvent) {
    if key.code == KeyCode::Esc {
        app.go_back();
        return;
    }
    app.publish_form.handle_key(key);
    if std::mem::take(&mut app.publish_form.submitted) {
        use publish_form::PublishField;
        let field = |f| {
            let v = app.publish_form.value(f).trim();
            (!v.is_empty()).then(|| v.to_owned())
        };
        let args = PublishArgs {
            file: field(PublishField::FilePath).unwrap_or_default().into(),
            name: field(PublishField::Name),
            version: field(PublishField::Version),
            r#type: None,
            pom: None,
            distribution: None,
            component: None,
            platform: "noarch".to_owned(),
        };
        app.publish_request = Some((args, field(PublishField::Registry)));
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

    if let Some(p) = &app.confirm {
        render_confirm(f, p);
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

/// The modal a destructive key opens: what will happen, to what, and the
/// one fact not to miss.
fn render_confirm(f: &mut ratatui::Frame, p: &PendingConfirm) {
    use ratatui::layout::Rect;
    use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
    let mut text = format!(
        "This will {}:\n  {}/{}@{}\n",
        p.verb.name(),
        p.registry,
        p.name,
        p.version
    );
    if let Some(note) = p.verb.note() {
        text.push_str(&format!("\n{note}\n"));
    }
    text.push_str("\ny: proceed   any other key: cancel");
    let area = f.area();
    let width = area.width.saturating_sub(4).min(64);
    let modal = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + area.height.saturating_sub(9) / 2,
        width,
        height: 9.min(area.height),
    };
    f.render_widget(Clear, modal);
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(" Confirm ").borders(Borders::ALL)),
        modal,
    );
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

    fn key(c: char) -> event::KeyEvent {
        event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn on_detail(app: &mut App) {
        app.package_detail
            .nav
            .set_items(vec![crate::api::package::PackageSummary {
                registry: "npm-local".to_owned(),
                name: "left-pad".to_owned(),
                version: "1.4.0".to_owned(),
                artifact: None,
                status: crate::api::package::PackageStatus::Available,
                access_count: 0,
            }]);
        app.screen = Screen::PackageDetail {
            registry: "npm-local".to_owned(),
            name: "left-pad".to_owned(),
        };
    }

    fn screen_text(app: &App) -> String {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(100, 30)).expect("terminal");
        terminal.draw(|f| render(f, app)).expect("draw");
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn a_destructive_key_asks_in_a_modal_and_another_key_cancels() {
        let mut app = make_app();
        on_detail(&mut app);

        handle_key(&mut app, key('d')).await;
        let pending = app.confirm.as_ref().expect("delete waits for its y");
        assert_eq!(pending.verb, Verb::Delete);
        let text = screen_text(&app);
        assert!(text.contains("This will delete:"), "{text}");
        assert!(text.contains("npm-local/left-pad@1.4.0"), "{text}");
        assert!(text.contains("never be published again"), "{text}");

        // Anything but `y` cancels, and the server (none here) is never asked.
        handle_key(&mut app, key('n')).await;
        assert!(app.confirm.is_none());
        assert_eq!(app.status_msg.as_deref(), Some("Did not delete."));
    }

    #[tokio::test]
    async fn yank_confirms_and_unyank_does_not() {
        let mut app = make_app();
        on_detail(&mut app);
        handle_key(&mut app, key('y')).await;
        assert_eq!(app.confirm.as_ref().map(|p| p.verb), Some(Verb::Yank));
        handle_key(
            &mut app,
            event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;
        assert!(app.confirm.is_none());

        for (c, verb) in [('u', Verb::Unyank), ('p', Verb::Pin), ('P', Verb::Unpin)] {
            assert!(!verb.destructive(), "{c} would open a modal");
        }
    }

    #[test]
    fn the_why_pane_renders_beside_the_versions() {
        let mut app = make_app();
        on_detail(&mut app);
        app.package_detail.pane = Some(Pane {
            title: " Why npm-local/left-pad@1.4.0 ".to_owned(),
            text: "⏳ npm-local/left-pad@1.4.0 — pullable from 2026-11-04".to_owned(),
        });
        let text = screen_text(&app);
        assert!(text.contains("Why npm-local/left-pad@1.4.0"), "{text}");
        assert!(text.contains("pullable from"), "{text}");
        assert!(text.contains("1.4.0"), "{text}");
    }

    #[tokio::test]
    async fn esc_closes_the_pane_before_leaving_the_screen() {
        let mut app = make_app();
        on_detail(&mut app);
        app.package_detail.pane = Some(Pane {
            title: String::new(),
            text: String::new(),
        });
        handle_key(
            &mut app,
            event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;
        assert!(app.package_detail.pane.is_none());
        assert!(matches!(app.screen, Screen::PackageDetail { .. }));
    }

    #[test]
    fn a_submitted_publish_form_becomes_publish_args() {
        let mut app = make_app();
        app.screen = Screen::PublishWizard;
        for (i, v) in ["dist/x.tgz", "npm-local", "", "2.0.0"].iter().enumerate() {
            app.publish_form.fields[i] = tui_input::Input::default().with_value((*v).to_owned());
        }
        app.publish_form.active_field = publish_form::PublishField::Version;
        handle_publish_form(
            &mut app,
            event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        let (args, registry) = app.publish_request.take().expect("submitted");
        assert_eq!(registry.as_deref(), Some("npm-local"));
        assert_eq!(args.file, std::path::PathBuf::from("dist/x.tgz"));
        // An empty field is left to `publish`'s own detection.
        assert_eq!(args.name, None);
        assert_eq!(args.version.as_deref(), Some("2.0.0"));
        assert!(!app.publish_form.submitted);
    }

    #[tokio::test]
    async fn esc_on_the_login_screen_does_not_claim_a_sign_in() {
        let mut app = make_app();
        app.screen = Screen::Login;
        handle_login(
            &mut app,
            event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        )
        .await;
        assert_eq!(app.screen, Screen::RegistryList);
        assert!(app.status_msg.is_none());
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
