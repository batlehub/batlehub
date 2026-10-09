//! CPU profiling for `perf/scripts/profile.sh`, compiled only with the
//! `profiling` feature. Never in a published image.
//!
//! Two endpoints, both administrator-only:
//!
//! - `GET /debug/pprof/folded?seconds=N&frequency=F` samples this process's CPU
//!   for `N` seconds and answers the stacks in folded format (`a;b;c count`),
//!   which `inferno-flamegraph` and speedscope read as they are. The sampler is
//!   pprof-rs on `SIGPROF`, so it counts CPU time only: an idle worker waiting
//!   on the upstream costs nothing here, which is the point of a CPU profile.
//!   One window at a time; a second concurrent request gets `409`.
//! - `GET /debug/routes` answers and **resets** a count of every request by
//!   `"METHOD pattern"`, the pattern being the one actix's router matched
//!   (`HttpRequest::match_pattern`, as `crates/web/tests/authz_matrix.rs` uses
//!   it). The driver drains it before and after each arm, so what a profile
//!   covers is measured rather than inferred from the arm's URL.
//!
//! - `GET /debug/wait` answers and **resets** where the requests' wall time
//!   went, summed over every request that finished since the last call:
//!   `wall`, `polled` (the request's future was running on a thread), `db`
//!   (sqlx's own elapsed time for each statement run inside the request) and
//!   `upstream` (`record_upstream_duration` in `batlehub-core`), in
//!   nanoseconds, with the counts. A CPU profile cannot see time spent
//!   waiting; this is the half it misses. See [`WaitLayer`].
//!
//! The arm-by-arm design is what attributes CPU to a path. Tagging samples
//! with the current route does not work under tokio: a task moves between
//! worker threads, and a per-thread tag is wrong as soon as it does.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::{get, web, HttpResponse};
use batlehub_web::extractors::AuthIdentity;
use tracing::field::{Field, Visit};
use tracing::{span, Event, Level, Subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

static ROUTES: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// Count the request under the pattern the router matched. Outermost, so it
/// sees every request, including the `404`s an arm with a wrong URL produces
/// (`<unmatched>`). The profiler's own endpoints are left out.
pub async fn tally_route(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let method = req.method().clone();
    let res = next.call(req).await?;
    let pattern = res
        .request()
        .match_pattern()
        .unwrap_or_else(|| "<unmatched>".to_owned());
    if !pattern.starts_with("/debug/") {
        *ROUTES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(format!("{method} {pattern}"))
            .or_default() += 1;
    }
    Ok(res)
}

#[get("/debug/routes")]
pub async fn routes(identity: AuthIdentity) -> HttpResponse {
    if !identity.0.is_admin() {
        return HttpResponse::Forbidden().finish();
    }
    let drained = std::mem::take(&mut *ROUTES.lock().unwrap_or_else(PoisonError::into_inner));
    HttpResponse::Ok().json(drained)
}

// ── Where the wall time goes ─────────────────────────────────────────────────

/// The root span `BatleHubSpanBuilder` opens for every request.
const REQUEST_SPAN: &str = "HTTP request";
/// The target `record_upstream_duration` emits its TRACE event under.
const UPSTREAM_TARGET: &str = "batlehub::upstream";

const WAIT_FIELDS: [&str; 7] = [
    "requests",
    "wall_ns",
    "polled_ns",
    "db_ns",
    "db_statements",
    "upstream_ns",
    "upstream_calls",
];
static WAIT: Mutex<[u64; 7]> = Mutex::new([0; 7]);

fn add_wait(values: [u64; 7]) {
    let mut totals = WAIT.lock().unwrap_or_else(PoisonError::into_inner);
    for (total, v) in totals.iter_mut().zip(values) {
        *total = total.saturating_add(v);
    }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Carried by each request span.
struct RequestClock {
    opened: Instant,
    entered: Option<Instant>,
    polled: Duration,
    db: Duration,
    db_statements: u64,
}

/// What the layer needs enabled whatever `RUST_LOG` says: the request span,
/// sqlx's per-statement event (the one `db_metrics` also reads) and the
/// upstream event.
pub fn wait_filter() -> Targets {
    Targets::new()
        .with_target("batlehub::server_factory", Level::INFO)
        .with_target("sqlx::query", Level::DEBUG)
        .with_target(UPSTREAM_TARGET, Level::TRACE)
}

/// Splits each request's wall time into the parts a CPU profile cannot tell
/// apart.
///
/// - **wall** — the request span's lifetime, open to close;
/// - **polled** — the time the span was *entered*, which is the time its
///   future was being polled on a thread. `polled − CPU` is time a poll spent
///   blocked without burning CPU: a `std` lock, synchronous file I/O, a page
///   fault — blocking inside async code, which a CPU profile shows as nothing;
/// - **db** — sqlx's elapsed time for each statement run inside the request;
/// - **upstream** — every upstream call's duration. Summed globally rather
///   than per span, because an artifact's duration is recorded when its body
///   stream drains, which can be after the request span has closed; in a
///   window that runs one arm, every upstream call is that arm's.
///
/// `wall − polled − db − upstream` is the rest of the awaiting: tokio locks
/// and semaphores, channels, `spawn_blocking` (all of `tokio::fs`).
pub struct WaitLayer;

#[derive(Default)]
struct Elapsed(Option<f64>);

impl Visit for Elapsed {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.0 = Some(value);
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

impl<S> Layer<S> for WaitLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != REQUEST_SPAN {
            return;
        }
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(RequestClock {
                opened: Instant::now(),
                entered: None,
                polled: Duration::ZERO,
                db: Duration::ZERO,
                db_statements: 0,
            });
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(clock) = span.extensions_mut().get_mut::<RequestClock>() {
                clock.entered = Some(Instant::now());
            }
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(clock) = span.extensions_mut().get_mut::<RequestClock>() {
                if let Some(at) = clock.entered.take() {
                    clock.polled += at.elapsed();
                }
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut elapsed = Elapsed::default();
        event.record(&mut elapsed);
        let Some(secs) = elapsed.0 else {
            return;
        };
        let took = Duration::from_secs_f64(secs.max(0.0));
        if event.metadata().target() == UPSTREAM_TARGET {
            add_wait([0, 0, 0, 0, 0, nanos(took), 1]);
            return;
        }
        // A statement outside any request — the scan worker, the samplers —
        // is not a request's wait, and is left out.
        let Some(scope) = ctx.event_scope(event) else {
            return;
        };
        for span in scope.from_root() {
            if let Some(clock) = span.extensions_mut().get_mut::<RequestClock>() {
                clock.db += took;
                clock.db_statements += 1;
                return;
            }
        }
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let Some(clock) = span.extensions_mut().remove::<RequestClock>() else {
            return;
        };
        add_wait([
            1,
            nanos(clock.opened.elapsed()),
            nanos(clock.polled),
            nanos(clock.db),
            clock.db_statements,
            0,
            0,
        ]);
    }
}

#[get("/debug/wait")]
pub async fn wait(identity: AuthIdentity) -> HttpResponse {
    if !identity.0.is_admin() {
        return HttpResponse::Forbidden().finish();
    }
    let drained = std::mem::take(&mut *WAIT.lock().unwrap_or_else(PoisonError::into_inner));
    let body: BTreeMap<&str, u64> = WAIT_FIELDS.into_iter().zip(drained).collect();
    HttpResponse::Ok().json(body)
}

#[get("/debug/pprof/folded")]
pub async fn folded(
    identity: AuthIdentity,
    q: web::Query<BTreeMap<String, String>>,
) -> HttpResponse {
    if !identity.0.is_admin() {
        return HttpResponse::Forbidden().finish();
    }
    let param = |k: &str, default: u64| q.get(k).and_then(|v| v.parse().ok()).unwrap_or(default);
    let seconds = param("seconds", 15).clamp(1, 300);
    // 199 Hz rather than a round 200 or 100, so the sampler does not run in
    // lockstep with a periodic timer in the process and over- or under-count it.
    let frequency = param("frequency", 199).clamp(1, 1000) as i32;
    // No blocklist: the `framehop-unwinder` feature unwinds without allocating
    // or calling into libgcc, so a sample that lands in libc (memcpy, the
    // allocator) is unwound and counted instead of dropped.
    let guard = match pprof::ProfilerGuardBuilder::default()
        .frequency(frequency)
        .build()
    {
        Ok(guard) => guard,
        Err(e) => return HttpResponse::Conflict().body(format!("profiler busy: {e}\n")),
    };
    actix_web::rt::time::sleep(Duration::from_secs(seconds)).await;
    let report = match guard.report().build() {
        Ok(report) => report,
        Err(e) => return HttpResponse::InternalServerError().body(format!("{e}\n")),
    };
    HttpResponse::Ok()
        .insert_header(("X-Profile-Frequency", frequency.to_string()))
        .content_type("text/plain; charset=utf-8")
        .body(fold(&report))
}

/// Folded stacks, root first, one line per distinct stack.
fn fold(report: &pprof::Report) -> String {
    let mut out = String::new();
    for (frames, count) in &report.data {
        let stack: Vec<String> = frames
            .frames
            .iter()
            .rev()
            .flat_map(|inlined| inlined.iter().rev())
            .map(frame_label)
            // The sampler's own frames sit at the leaf of every stack.
            .take_while(|label| !label.contains("pprof::backtrace"))
            .collect();
        let _ = writeln!(out, "{} {count}", stack.join(";"));
    }
    out
}

/// A frame's name, plus where it is when the name alone does not say.
///
/// An inlined frame carries no module path — an `async fn` handler shows up as
/// `{async_fn#0}`, a middleware's as `call` or `poll` — so the name cannot say
/// whose code it is. The source file can: every frame from this workspace gets
/// `[crates/…/file.rs:line]`, which is what `perf/scripts/profile_report.py`
/// reads to find the nearest BatleHub frame, and a dependency frame with a bare
/// name gets its crate's file.
fn frame_label(symbol: &pprof::Symbol) -> String {
    let name = simplify(&symbol.name());
    let Some(file) = symbol.filename.as_deref().and_then(|f| f.to_str()) else {
        return name;
    };
    if let Some(path) = workspace_relative(file) {
        return match symbol.lineno {
            Some(line) => format!("{name} [{path}:{line}]"),
            None => format!("{name} [{path}]"),
        };
    }
    if name.contains("::") {
        return name;
    }
    // `…/index.crates.io-<hash>/tokio-1.47.1/src/x.rs` → `tokio-1.47.1/src/x.rs`;
    // `/rustc/<hash>/library/core/src/x.rs` → `core/src/x.rs`.
    let short = ["/index.crates.io-", "/library/"]
        .iter()
        .find_map(|marker| {
            let rest = &file[file.find(marker)? + marker.len()..];
            Some(if *marker == "/library/" {
                rest
            } else {
                rest.split_once('/')?.1
            })
        })
        .unwrap_or(file);
    format!("{name} [{short}]")
}

/// `crates/web/src/x.rs` for a file of this workspace — absolute under the
/// root this binary was built from, or relative as cargo passes a workspace
/// member's sources — and `None` for anything else.
fn workspace_relative(file: &str) -> Option<&str> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .to_str()?;
    let rel = file
        .strip_prefix(root)
        .map(|r| r.trim_start_matches('/'))
        .unwrap_or(file);
    ["crates/", "server/", "cli/"]
        .iter()
        .any(|dir| rel.starts_with(dir))
        .then_some(rel)
}

/// A frame name without its generic arguments.
///
/// Debuginfo names spell every type parameter out, and under actix and tokio a
/// single frame measured 100 KB (`with_mut<tokio::runtime::task::core::Stage<
/// actix_server::…>>`): one arm's profile was 97 MB for 148 stacks. A `<…>`
/// group that follows an identifier, a `::` or a closure's `}` is an argument
/// list and is dropped; a `<` that opens the name is the qualified-path form,
/// `<Foo as Trait>::call`, and is kept because it says which impl ran. A `->`
/// inside a dropped group is not a closing bracket. `;` (`[u8; 32]`) would
/// split a folded frame in two, so it becomes `,`.
fn simplify(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(256));
    let mut depth = 0usize;
    let mut prev = '\0';
    for c in name.chars() {
        if depth > 0 {
            match c {
                '<' => depth += 1,
                '>' if prev != '-' => depth -= 1,
                _ => {}
            }
        } else if c == '<'
            && out
                .chars()
                .last()
                .is_some_and(|l| l.is_alphanumeric() || matches!(l, '_' | ':' | '}'))
        {
            depth = 1;
        } else {
            out.push(if c == ';' { ',' } else { c });
        }
        prev = c;
    }
    let out = out.replace("::::", "::");
    out.strip_suffix("::").map(str::to_owned).unwrap_or(out)
}

#[cfg(test)]
mod tests {
    use super::{simplify, workspace_relative, WaitLayer, WAIT, WAIT_FIELDS};
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn request_wall_time_is_split_into_polled_sql_and_upstream() {
        let subscriber = tracing_subscriber::registry().with(WaitLayer);
        tracing::subscriber::with_default(subscriber, || {
            let request = tracing::info_span!("HTTP request");
            {
                let _polled = request.enter();
                tracing::debug!(target: "sqlx::query", elapsed_secs = 0.25_f64, "SELECT 1");
                tracing::trace!(target: "batlehub::upstream", elapsed_secs = 0.5_f64);
            }
            // Outside any request: a background statement, not counted.
            tracing::debug!(target: "sqlx::query", elapsed_secs = 9.0_f64, "SELECT 2");
            drop(request);
        });
        let totals: std::collections::BTreeMap<_, _> =
            WAIT_FIELDS.into_iter().zip(*WAIT.lock().unwrap()).collect();
        assert_eq!(totals["requests"], 1);
        assert_eq!(totals["db_statements"], 1);
        assert_eq!(totals["db_ns"], 250_000_000);
        assert_eq!(totals["upstream_calls"], 1);
        assert_eq!(totals["upstream_ns"], 500_000_000);
        assert!(totals["polled_ns"] <= totals["wall_ns"]);
    }

    #[test]
    fn only_this_workspace_is_ours() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let abs = format!("{}/crates/web/src/lib.rs", root.display());
        assert_eq!(workspace_relative(&abs), Some("crates/web/src/lib.rs"));
        assert_eq!(
            workspace_relative("server/src/main.rs"),
            Some("server/src/main.rs")
        );
        let dep =
            "/home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.47.1/src/lib.rs";
        assert_eq!(workspace_relative(dep), None);
        assert_eq!(
            workspace_relative("/rustc/abc/library/core/src/lib.rs"),
            None
        );
    }

    #[test]
    fn generic_arguments_are_dropped_and_the_impl_is_kept() {
        let cases = [
            (
                "tokio::loom::std::unsafe_cell::UnsafeCell<tokio::runtime::task::core::Stage<F>>::with_mut<R, G>",
                "tokio::loom::std::unsafe_cell::UnsafeCell::with_mut",
            ),
            (
                "<batlehub_web::middleware::Auth<S> as actix_service::Service<Req>>::call",
                "<batlehub_web::middleware::Auth as actix_service::Service>::call",
            ),
            ("alloc::vec::Vec::<u8>::push", "alloc::vec::Vec::push"),
            ("std::panicking::catch_unwind::do_call::<F, R>", "std::panicking::catch_unwind::do_call"),
            ("f::{closure_env#0}<fn(A) -> B, C>", "f::{closure_env#0}"),
            ("hash<[u8; 32]>", "hash"),
            ("copy_from([u8; 32])", "copy_from([u8, 32])"),
        ];
        for (raw, want) in cases {
            assert_eq!(simplify(raw), want, "{raw}");
        }
    }
}
