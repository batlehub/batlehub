//! Every SQL statement the process runs, counted, and per request.
//!
//! sqlx already reports each statement it executes as a DEBUG event under the
//! `sqlx::query` target, with its elapsed time. This layer turns those events
//! into metrics, so the numbers `tests/heavy/db_calls.sh` measures on one
//! request are visible on every request in production:
//!
//! - `batlehub_db_statements_total{verb}` and
//!   `batlehub_db_statement_duration_seconds{verb}` — every statement, including
//!   the ones no request caused (the scan worker, the samplers);
//! - `batlehub_db_statements_per_request{route}` — how many ran inside one HTTP
//!   request's span, recorded when the span closes.
//!
//! `timed_query` (crates/adapters/src/db/mod.rs) times a few named call sites;
//! this covers all of them without touching one.
//!
//! The event is enabled by this layer's own filter, whatever `RUST_LOG` says,
//! so sqlx formats a short summary per statement it would otherwise have
//! skipped — microseconds against a database round trip.

use std::sync::atomic::{AtomicU32, Ordering};

use tracing::field::{Field, Visit};
use tracing::{span, Event, Level, Subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// The name `BatleHubSpanBuilder` gives the root span of every request.
const REQUEST_SPAN: &str = "HTTP request";

/// What the layer needs enabled whatever `RUST_LOG` says: sqlx's statements,
/// and the request span they are counted against.
pub(super) fn filter() -> Targets {
    Targets::new()
        .with_target("sqlx::query", Level::DEBUG)
        .with_target("batlehub::server_factory", Level::INFO)
}

pub(super) struct DbStatementLayer;

/// Carried by each request span.
struct Tally {
    route: String,
    statements: AtomicU32,
}

#[derive(Default)]
struct Fields {
    route: Option<String>,
    summary: Option<String>,
    elapsed_secs: Option<f64>,
}

impl Visit for Fields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = Some(value);
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "summary" => self.summary = Some(value.to_owned()),
            "http.route" => self.route = Some(value.to_owned()),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `%http_route` and sqlx's `summary` (a `String`) arrive here.
        match field.name() {
            "summary" => self.summary = Some(format!("{value:?}").trim_matches('"').to_owned()),
            "http.route" => self.route = Some(format!("{value:?}")),
            _ => {}
        }
    }
}

/// The statement's first keyword, from a fixed set: a label, so it cannot be
/// allowed to grow with whatever SQL the codebase writes next.
fn verb(summary: Option<&str>) -> &'static str {
    let first = summary
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("");
    match first.to_ascii_uppercase().as_str() {
        "SELECT" => "select",
        "INSERT" => "insert",
        "UPDATE" => "update",
        "DELETE" => "delete",
        "WITH" => "with",
        "BEGIN" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" => "transaction",
        _ => "other",
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for DbStatementLayer {
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != REQUEST_SPAN {
            return;
        }
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Tally {
                route: fields.route.unwrap_or_else(|| "default".to_owned()),
                statements: AtomicU32::new(0),
            });
        }
    }

    /// The route arrives after routing, in `on_request_end`, not with the span.
    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        values.record(&mut fields);
        let Some(route) = fields.route else {
            return;
        };
        if let Some(span) = ctx.span(id) {
            if let Some(tally) = span.extensions_mut().get_mut::<Tally>() {
                tally.route = route;
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let verb = verb(fields.summary.as_deref());
        metrics::counter!("batlehub_db_statements_total", "verb" => verb).increment(1);
        if let Some(secs) = fields.elapsed_secs {
            metrics::histogram!("batlehub_db_statement_duration_seconds", "verb" => verb)
                .record(secs);
        }
        let Some(scope) = ctx.event_scope(event) else {
            return;
        };
        for span in scope {
            if let Some(tally) = span.extensions().get::<Tally>() {
                tally.statements.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let extensions = span.extensions();
        if let Some(tally) = extensions.get::<Tally>() {
            metrics::histogram!("batlehub_db_statements_per_request", "route" => tally.route.clone())
                .record(f64::from(tally.statements.load(Ordering::Relaxed)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn statements_are_counted_by_verb_and_per_request() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let subscriber = tracing_subscriber::registry().with(DbStatementLayer);
        metrics::with_local_recorder(&recorder, || {
            tracing::subscriber::with_default(subscriber, || {
                {
                    // Declared empty and recorded later, as `BatleHubSpanBuilder` does.
                    let span =
                        tracing::info_span!("HTTP request", http.route = tracing::field::Empty);
                    let _in = span.enter();
                    span.record("http.route", "/proxy/{registry}/{package}");
                    tracing::debug!(target: "sqlx::query", summary = "SELECT 1", elapsed_secs = 0.001_f64);
                    tracing::debug!(target: "sqlx::query", summary = "INSERT INTO access_events (id, …", elapsed_secs = 0.002_f64);
                }
                // Outside any request: counted, attributed to none.
                tracing::debug!(target: "sqlx::query", summary = "UPDATE scan_jobs SET leased_until …", elapsed_secs = 0.001_f64);
                // Not sqlx: ignored.
                tracing::debug!(target: "other", summary = "SELECT 1");
            });
        });
        let out = handle.render();
        assert!(
            out.contains(r#"batlehub_db_statements_total{verb="select"} 1"#),
            "{out}"
        );
        assert!(
            out.contains(r#"batlehub_db_statements_total{verb="insert"} 1"#),
            "{out}"
        );
        assert!(
            out.contains(r#"batlehub_db_statements_total{verb="update"} 1"#),
            "{out}"
        );
        assert!(
            out.contains(
                r#"batlehub_db_statements_per_request_sum{route="/proxy/{registry}/{package}"} 2"#
            ),
            "{out}"
        );
        assert!(
            out.contains(
                r#"batlehub_db_statements_per_request_count{route="/proxy/{registry}/{package}"} 1"#
            ),
            "{out}"
        );
    }

    #[test]
    fn a_verb_outside_the_set_is_other() {
        assert_eq!(verb(Some("select id from x")), "select");
        assert_eq!(verb(Some("VACUUM ANALYZE")), "other");
        assert_eq!(verb(None), "other");
    }
}
