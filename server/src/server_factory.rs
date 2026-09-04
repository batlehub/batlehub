use std::path::PathBuf;
use std::sync::Arc;

use actix_web::{web, App, HttpServer};
use anyhow::Context;
use metrics_exporter_prometheus::PrometheusHandle;
use tracing_actix_web::{DefaultRootSpanBuilder, RootSpanBuilder, TracingLogger};
use utoipa::OpenApi as _;
use utoipa_actix_web::AppExt;

use batlehub_adapters::auth::OidcSsoFlow;
use batlehub_config::schema::{IpBlockingConfig, NotificationsConfig};
use batlehub_core::ports::{
    AuthProvider, BetaChannelPort, IpBlockStore, NotificationPort, TeamNamespacePort,
    UserBlockRepository, UserTokenRepository,
};
use batlehub_core::services::{
    AdminService, BackendVersions, GrantAdminService, LocalRegistryService, ProxyMetrics,
    ProxyService, QuotaService, SbomService,
};
use batlehub_web::handlers::back_office::ops::eviction::EvictionServiceMap;
use batlehub_web::handlers::back_office::ops::warming::WarmingServiceMap;
use batlehub_web::services::{BannerService, ConfigReloadService, NotificationService};
use batlehub_web::{
    configure_app, healthz, livez, prometheus_metrics, security_headers, AccessConfigLock, ApiDoc,
    CargoIndexMap, CliBinaryPath, HostRoutingMiddlewareFactory, IpBlockMiddlewareFactory,
    ProxyTrust, RateLimitMiddlewareFactory, RateLimitService, RegistryHostMap, RegistryMap,
    RegistryModeMap, SumDbMap, UpstreamMap, UserBlockMiddlewareFactory, VulnDbMap,
};

// ── Tracing span builder ──────────────────────────────────────────────────────

pub(super) struct BatleHubSpanBuilder;

impl RootSpanBuilder for BatleHubSpanBuilder {
    // Hand-written rather than `tracing_actix_web::root_span!`, for one field.
    //
    // The macro sets `http.target` from `uri().path_and_query()`
    // (`tracing-actix-web-0.7.22/src/root_span_macro.rs:110`), so the **query
    // string of every request lands in the root span at INFO** — the fmt
    // subscriber, the OTLP exporter, and anything shipping either. That is
    // ordinarily unremarkable and became a problem when RFC 0012 started
    // putting a credential in a query parameter: `bh_sig` is a bearer
    // capability for its lifetime, and logging it hands that capability to
    // whoever reads the logs.
    //
    // The macro offers no way to override the field — its trailing `$($field)*`
    // only appends, and re-declaring `http.target` makes `span!` reject the
    // duplicate. So the fields are set here instead, identically **except**
    // that the target is the path alone. `http.route` already carries the
    // matched pattern, and no consumer of these spans wants a query string
    // enough to justify logging credentials.
    //
    // Kept faithful to the macro on purpose: the same field set, the same
    // names, the same `Empty` placeholders that `DefaultRootSpanBuilder::
    // on_request_end` later records into (`http.status_code`,
    // `otel.status_code`, `exception.message`, `exception.details`). Drop one
    // and that call silently stops recording it.
    //
    // `ConnectionInfo::scheme`/`::host` are `clippy.toml`-disallowed so no
    // request-handling code reads a forwarded header without going through
    // `proxy_trust`. This only labels a span — a spoofed host mislabels a
    // trace, it does not decide routing, URLs or a ban.
    #[allow(clippy::disallowed_methods)]
    fn on_request_start(request: &actix_web::dev::ServiceRequest) -> tracing::Span {
        use tracing_actix_web::root_span_macro::private::{
            extract_otel_trace_id, get_request_id, http_flavor, http_method_str, http_scheme,
        };

        let user_agent = request
            .headers()
            .get("User-Agent")
            .map(|h| h.to_str().unwrap_or(""))
            .unwrap_or("");
        let http_route: std::borrow::Cow<'static, str> = request
            .match_pattern()
            .map(Into::into)
            .unwrap_or_else(|| "default".into());
        let http_method = http_method_str(request.method());
        let connection_info = request.connection_info();
        let request_id = get_request_id(request);
        // The one deviation: path only, never `path_and_query`.
        let http_target = request.uri().path();
        let otel_trace_id = extract_otel_trace_id(request);

        // Two arms because a field's value is fixed at macro expansion, and the
        // upstream builder pre-populates `trace_id` *before* span creation so it
        // is visible to `on_new_span` rather than recorded after the fact.
        macro_rules! request_span {
            ($trace_id:expr) => {
                tracing::span!(
                    tracing::Level::INFO,
                    "HTTP request",
                    http.method = %http_method,
                    http.route = %http_route,
                    http.flavor = %http_flavor(request.version()),
                    http.scheme = %http_scheme(connection_info.scheme()),
                    http.host = %connection_info.host(),
                    http.client_ip = %request.connection_info().realip_remote_addr().unwrap_or(""),
                    http.user_agent = %user_agent,
                    http.target = %http_target,
                    http.status_code = tracing::field::Empty,
                    otel.name = %format!("{} {}", http_method, http_route),
                    otel.kind = "server",
                    otel.status_code = tracing::field::Empty,
                    trace_id = $trace_id,
                    request_id = %request_id,
                    exception.message = tracing::field::Empty,
                    exception.details = tracing::field::Empty,
                )
            };
        }

        // `field::display` and `field::Empty` are both `Value`s, so one arm
        // covers each case — the upstream macro duplicates its whole field list
        // instead, because `%tid` is not an expression.
        match otel_trace_id {
            Some(tid) => request_span!(tracing::field::display(tid)),
            None => request_span!(tracing::field::Empty),
        }
    }

    fn on_request_end<B: actix_web::body::MessageBody>(
        span: tracing::Span,
        outcome: &anyhow::Result<actix_web::dev::ServiceResponse<B>, actix_web::Error>,
    ) {
        let status = match outcome {
            Ok(resp) => resp.status(),
            Err(err) => err.as_response_error().status_code(),
        };
        if status.is_client_error() {
            tracing::info!(
                http.status_code = status.as_u16(),
                "upstream/client error (not a backend fault)"
            );
        } else if status.is_server_error() {
            tracing::warn!(http.status_code = status.as_u16(), "backend error");
        }
        DefaultRootSpanBuilder::on_request_end(span, outcome);
    }
}

// ── Server startup params ─────────────────────────────────────────────────────

pub(super) struct ServerParams {
    pub bind_addr: String,
    pub static_dir: Option<String>,
    pub cli_binary_path: Option<PathBuf>,
    pub cors_allowed_origins: Vec<String>,
    pub db_pool: sqlx::PgPool,
    pub proxy_svc: Arc<ProxyService>,
    pub admin_svc: Arc<AdminService>,
    pub token_repo: Arc<dyn UserTokenRepository>,
    pub access_config: AccessConfigLock,
    /// `[search] readmes`, shared with the reload path so turning prose search
    /// off takes effect without a restart (RFC 0007-bis §4.1).
    pub search_config: batlehub_web::SearchConfigLock,
    pub registry_map: RegistryMap,
    pub upstream_map: UpstreamMap,
    pub oidc_sso_flows: Vec<OidcSsoFlow>,
    /// Every configured OIDC provider name, SSO-enabled or not — the allow-list
    /// `POST /api/v1/auth/tokens` checks the caller against.
    pub oidc_provider_names: batlehub_web::OidcProviderNames,
    /// One-time store for in-flight OIDC authorization requests.
    pub login_states: Arc<dyn batlehub_core::ports::LoginStateStore>,
    pub warming_map: WarmingServiceMap,
    pub eviction_map: EvictionServiceMap,
    pub proxy_metrics: Arc<ProxyMetrics>,
    /// `None` when `[stats] metrics_enabled = false`: the recorder is never
    /// installed, and `/metrics` answers 503 rather than publishing.
    pub prometheus_handle: Option<PrometheusHandle>,
    pub sbom_svc: Arc<SbomService>,
    pub notification_svc: Option<Arc<NotificationService>>,
    pub notification_store: Arc<dyn NotificationPort>,
    pub notifications_config: Option<NotificationsConfig>,
    pub local_svc: Arc<LocalRegistryService>,
    pub quota_svc: Arc<QuotaService>,
    pub stats_history: Arc<dyn batlehub_core::ports::StatsHistoryRepository>,
    pub registry_mode_map: RegistryModeMap,
    pub repo_signer_map: batlehub_web::RepoSignerMap,
    pub ip_block_store: Arc<dyn IpBlockStore>,
    pub user_block_repo: Arc<dyn UserBlockRepository>,
    pub beta_channel_store: Arc<dyn BetaChannelPort>,
    pub team_namespace_store: Arc<dyn TeamNamespacePort>,
    /// RFC 0015 §6.3 — the package and version policy tiers, written through the
    /// admin API because §4.1 says a config file cannot enumerate them.
    pub policy_repo: Arc<dyn batlehub_core::ports::PolicyRepository>,
    pub ip_blocking_cfg: Option<IpBlockingConfig>,
    /// Resolved `[server].trusted_proxies` (or the deprecated
    /// `[ip_blocking]` fallback). Registered as `app_data` so the middleware
    /// stack and the outbound URL helper share one verdict per request.
    pub proxy_trust: ProxyTrust,
    /// `host -> registry` routing table for host-based ingress. Empty when the
    /// feature is unconfigured, which makes the middleware a no-op.
    pub registry_host_map: RegistryHostMap,
    pub cargo_index_map: CargoIndexMap,
    pub vuln_db_map: VulnDbMap,
    pub sumdb_map: SumDbMap,
    pub rate_limit_svc: Arc<RateLimitService>,
    pub auth_providers: Vec<Arc<dyn AuthProvider>>,
    pub reload_svc: Arc<ConfigReloadService>,
    pub banner_svc: Arc<BannerService>,
    pub storage_admin_repo: Arc<dyn batlehub_core::ports::StorageAdminRepository>,
}

// ── HTTP server ───────────────────────────────────────────────────────────────

pub(super) async fn run_actix_server(p: ServerParams) -> anyhow::Result<()> {
    let ServerParams {
        bind_addr,
        static_dir,
        cli_binary_path,
        cors_allowed_origins,
        db_pool,
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        upstream_map,
        oidc_sso_flows,
        oidc_provider_names,
        login_states,
        warming_map,
        eviction_map,
        proxy_metrics,
        prometheus_handle,
        sbom_svc,
        notification_svc,
        notification_store,
        notifications_config,
        local_svc,
        quota_svc,
        stats_history,
        registry_mode_map,
        repo_signer_map,
        ip_block_store,
        user_block_repo,
        beta_channel_store,
        team_namespace_store,
        policy_repo,
        ip_blocking_cfg,
        proxy_trust,
        registry_host_map,
        cargo_index_map,
        vuln_db_map,
        sumdb_map,
        rate_limit_svc,
        auth_providers,
        reload_svc,
        banner_svc,
        storage_admin_repo,
        search_config,
    } = p;

    let notification_svc_for_shutdown = notification_svc.clone();

    HttpServer::new(move || {
        let configure = configure_app(
            proxy_svc.clone(),
            admin_svc.clone(),
            token_repo.clone(),
            Some(db_pool.clone()),
            access_config.clone(),
            registry_map.clone(),
            upstream_map.clone(),
            oidc_sso_flows.clone(),
            oidc_provider_names.clone(),
            Arc::clone(&login_states),
            warming_map.clone(),
            eviction_map.clone(),
            Arc::clone(&proxy_metrics),
            prometheus_handle.clone(),
            Some(sbom_svc.clone()),
            notification_svc.clone(),
            Arc::clone(&notification_store),
            notifications_config.clone(),
            Some(Arc::clone(&storage_admin_repo)),
            search_config.clone(),
        );
        let static_dir_inner = static_dir.clone();
        let cli_binary_path_inner = cli_binary_path.clone();

        let (app, openapi) = App::new()
            .into_utoipa_app()
            .openapi(ApiDoc::openapi())
            .configure(configure)
            .split_for_parts();

        let mut app = app
            .app_data(web::Data::new(cargo_index_map.clone()))
            .app_data(web::Data::new(vuln_db_map.clone()))
            .app_data(web::Data::new(sumdb_map.clone()))
            .app_data(web::Data::new(local_svc.clone()))
            // RFC 0017 §4.1 — the grants editor. Built here rather than in
            // `main.rs` because it is assembled entirely from handles this
            // closure already holds, and it holds no state of its own: it is a
            // write funnel over `hot.grant_repo`, the local backend (for the
            // one question validation needs — does this version exist?) and the
            // ownership list (for §4.3's refusal).
            .app_data(web::Data::new(Arc::new(GrantAdminService::new(
                local_svc.hot.clone(),
                Some(Arc::new(BackendVersions(Arc::clone(&local_svc.backend)))
                    as Arc<dyn batlehub_core::services::VersionLookup>),
                local_svc.ownership.clone(),
            ))))
            .app_data(web::Data::new(Arc::clone(&quota_svc)))
            .app_data(web::Data::new(Arc::clone(&stats_history)))
            .app_data(web::Data::new(registry_mode_map.clone()))
            .app_data(web::Data::new(repo_signer_map.clone()))
            .app_data(web::Data::new(Arc::clone(&ip_block_store)))
            .app_data(web::Data::new(Arc::clone(&user_block_repo)))
            .app_data(web::Data::new(Arc::clone(&beta_channel_store)))
            .app_data(web::Data::new(Arc::clone(&team_namespace_store)))
            .app_data(web::Data::new(Arc::clone(&policy_repo)))
            .app_data(web::Data::new(Arc::clone(&reload_svc)))
            .app_data(web::Data::new(Arc::clone(&banner_svc)))
            .app_data(web::Data::new(proxy_trust.clone()))
            .app_data(web::Data::new(registry_host_map.clone()))
            .service(prometheus_metrics)
            .service(healthz)
            .service(livez);

        if let Some(path) = cli_binary_path_inner {
            app = app.app_data(web::Data::new(CliBinaryPath(path)));
        }

        let cors = crate::watcher::build_cors(&cors_allowed_origins);
        let enabled = ip_blocking_cfg.as_ref().is_some_and(|c| c.enabled);
        let ip_block_cfg_for_mw = ip_blocking_cfg.clone().unwrap_or_default();

        app.wrap(TracingLogger::<BatleHubSpanBuilder>::new())
            .wrap(RateLimitMiddlewareFactory::new(rate_limit_svc.clone()))
            .wrap(UserBlockMiddlewareFactory::new(Arc::clone(
                &user_block_repo,
            )))
            .wrap(batlehub_web::AuthMiddlewareFactory::new(
                auth_providers.clone(),
            ))
            .wrap(cors)
            .wrap(actix_web::middleware::Condition::new(
                enabled,
                IpBlockMiddlewareFactory::new(Arc::clone(&ip_block_store), ip_block_cfg_for_mw),
            ))
            // Outside the IP-block layer so its 403 — and the rate limiter's 429,
            // and anything the static-file service returns — carry the baseline
            // headers too, not just handler responses.
            .wrap(security_headers())
            // The CSP the baseline set cannot carry, scoped to the one prefix the
            // objections to a global policy do not reach. Inside the host-routing
            // wrap below, and deliberately so: it reads the path to decide, and
            // `npm.acme.io/simple/foo/` only looks like a proxy path *after* that
            // rewrite. See `protocol_document_csp`.
            .wrap(actix_web::middleware::from_fn(
                batlehub_web::protocol_document_csp,
            ))
            // Outermost, so the URI rewrite lands before route matching and the
            // proxy-trust verdict before anything that reads a forwarded header.
            // `.wrap` builds inside-out, so this must stay the last call.
            .wrap(HostRoutingMiddlewareFactory::new(
                registry_host_map.clone(),
                proxy_trust.clone(),
            ))
            // The API reference's bundle is part of the console's build output
            // and is served from this origin, so which document `/scalar`
            // answers with depends on whether that output is actually here. A
            // server configured without `static_dir` gets the degraded page
            // rather than a CDN fallback — see `batlehub_web::SCALAR_BUNDLE_PATH`.
            .service(batlehub_web::scalar(
                openapi,
                static_dir_inner.as_deref().map(std::path::Path::new),
            ))
            .configure(move |cfg| {
                if let Some(ref dir) = static_dir_inner {
                    // Still no CSP *header* here, for the two reasons that have
                    // not changed: it cannot be global — `/proxy/**`, `/scalar`
                    // and the console each need a different policy, and the
                    // first two now get theirs from the middleware wrapped
                    // above — and the `actix_files::Files` service behind
                    // `configure_spa` is not a `ServiceFactory`, so it
                    // cannot be wrapped individually either. The SPA carries its own policy in a
                    // `<meta http-equiv>` tag, generated at build time by
                    // `ui/build/csp.ts` so `connect-src` can follow the configured
                    // API origin. `frame-ancestors` is ignored in meta form, which
                    // is why `security_headers()` sends `X-Frame-Options: DENY`.
                    //
                    // What *is* new: the document is served by `configure_spa`
                    // rather than straight off disk, so the built policy can be
                    // narrowed to the running config on the way out — see
                    // `crates/web/src/spa.rs` for why that narrowing can only
                    // ever subtract, and for the deep-link fallback that sits
                    // behind the file service so `/packages/npm/chalk` resolves
                    // to the console rather than to a 404.
                    // Document, static files and the deep-link fallback, in the
                    // one place that knows their order matters.
                    batlehub_web::configure_spa(cfg, std::path::PathBuf::from(dir));
                }
            })
    })
    .bind(&bind_addr)
    .with_context(|| format!("binding to {bind_addr}"))?
    .run()
    .await
    .context("HTTP server error")?;

    if let Some(svc) = &notification_svc_for_shutdown {
        svc.shutdown().await;
    }

    Ok(())
}

/// The HTTP surface of a worker-only process (RFC 0018 §6.5): `/livez` and
/// `/metrics`, nothing that serves an artifact. `/healthz` needs the proxy
/// service and is not here; a worker's health is its heartbeat row.
pub(super) async fn run_worker_only_server(
    bind_addr: String,
    prometheus_handle: Option<PrometheusHandle>,
) -> anyhow::Result<()> {
    tracing::info!(%bind_addr, "worker-only: listening for /livez and /metrics");
    HttpServer::new(move || {
        let mut app = App::new().service(livez).service(prometheus_metrics);
        if let Some(h) = prometheus_handle.clone() {
            app = app.app_data(web::Data::new(h));
        }
        app
    })
    .bind(&bind_addr)?
    .run()
    .await?;
    Ok(())
}

#[cfg(test)]
mod span_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::Layer;

    /// What a span was created with: the values actually recorded, and the
    /// full set of field *names* declared.
    ///
    /// The two differ, and the difference is the point: a field declared
    /// `Empty` is not visited at creation, so a visitor alone cannot tell
    /// "declared, awaiting a value" from "not declared at all" — which is
    /// exactly the mistake that would silently break
    /// `DefaultRootSpanBuilder::on_request_end`.
    #[derive(Default, Clone)]
    struct Sink {
        values: Arc<Mutex<Vec<(String, String)>>>,
        declared: Arc<Mutex<Vec<String>>>,
    }

    struct Captured(Sink);

    struct Grab(Arc<Mutex<Vec<(String, String)>>>);

    impl Visit for Grab {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .lock()
                .unwrap()
                .push((field.name().to_owned(), format!("{value:?}")));
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0
                .lock()
                .unwrap()
                .push((field.name().to_owned(), value.to_owned()));
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for Captured {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            self.0.declared.lock().unwrap().extend(
                attrs
                    .metadata()
                    .fields()
                    .iter()
                    .map(|f| f.name().to_owned()),
            );
            attrs.record(&mut Grab(Arc::clone(&self.0.values)));
        }
    }

    fn fields_for(uri: &str) -> Vec<(String, String)> {
        capture(uri).0
    }

    fn capture(uri: &str) -> (Vec<(String, String)>, Vec<String>) {
        let sink = Sink::default();
        let subscriber = tracing_subscriber::registry().with(Captured(sink.clone()));
        with_default(subscriber, || {
            let req = actix_web::test::TestRequest::get()
                .uri(uri)
                .to_srv_request();
            // `TracingLogger` inserts this before calling the builder; a bare
            // test request has none, and `get_request_id` unwraps it.
            {
                use actix_web::HttpMessage as _;
                req.extensions_mut()
                    .insert(tracing_actix_web::root_span_macro::private::generate_request_id());
            }
            let _span = BatleHubSpanBuilder::on_request_start(&req);
        });
        let values = sink.values.lock().unwrap().clone();
        let declared = sink.declared.lock().unwrap().clone();
        (values, declared)
    }

    fn value_of<'a>(fields: &'a [(String, String)], name: &str) -> &'a str {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("no field {name} in {fields:?}"))
    }

    /// RFC 0012 §7.1. The reason this builder is hand-written instead of
    /// `root_span!`: a signed download URL carries a bearer capability in
    /// `bh_sig`, and the upstream macro would put it in the span at INFO.
    #[test]
    fn the_span_target_never_carries_a_query_string() {
        let fields = fields_for(
            "/proxy/tf/v1/providers/hashicorp/null/3.2.2/artifact/linux/amd64?bh_sig=1.PAYLOAD.MAC",
        );
        let target = value_of(&fields, "http.target");

        assert_eq!(
            target, "/proxy/tf/v1/providers/hashicorp/null/3.2.2/artifact/linux/amd64",
            "http.target must be the path alone"
        );
        // Belt and braces: no field anywhere in the span may carry the token.
        for (name, value) in &fields {
            assert!(
                !value.contains("bh_sig") && !value.contains("PAYLOAD"),
                "field {name} leaked the signature: {value}"
            );
        }
    }

    /// A query that is not a credential is dropped too. That is the trade this
    /// makes, and it is deliberate: `http.route` carries the matched pattern,
    /// and no consumer wanted a query enough to justify logging credentials.
    #[test]
    fn an_ordinary_query_is_dropped_as_well() {
        let fields = fields_for("/api/v1/packages?page=2&per_page=50");
        assert_eq!(value_of(&fields, "http.target"), "/api/v1/packages");
    }

    /// The field set must stay identical to the macro's, because
    /// `DefaultRootSpanBuilder::on_request_end` records into four of these by
    /// name — drop one and it silently stops recording.
    #[test]
    fn every_field_the_upstream_builder_writes_into_is_declared() {
        let (_values, names) = capture("/healthz");
        for required in [
            "http.method",
            "http.route",
            "http.flavor",
            "http.scheme",
            "http.host",
            "http.client_ip",
            "http.user_agent",
            "http.target",
            "http.status_code",
            "otel.name",
            "otel.kind",
            "otel.status_code",
            "trace_id",
            "request_id",
            "exception.message",
            "exception.details",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "missing field {required}; declared: {names:?}"
            );
        }
    }
}
