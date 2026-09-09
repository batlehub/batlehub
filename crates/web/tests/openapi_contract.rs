//! The contract gate (RFC 0004 §6.1, §10).
//!
//! Every documented success response must declare a body. A `200` with only a
//! `description` makes `@hey-api/openapi-ts` emit `unknown`, which is why
//! `ui/` used to hand-write mirrors of four DTOs, and why the docs site's API
//! reference is blank for those endpoints.
//!
//! This walks the generated `ApiDoc` rather than linting the source, so it
//! fails on the next undocumented response and cannot be satisfied by a
//! comment.

use utoipa::openapi::path::Operation;
use utoipa::openapi::{PathItem, RefOr, Response};

/// Statuses that must carry a schema. `204` is excluded because "no content"
/// is the body.
const MUST_HAVE_BODY: [&str; 2] = ["200", "201"];

fn response_has_schema(response: &Response) -> bool {
    response
        .content
        .values()
        .any(|content| content.schema.is_some())
}

fn operations(item: &PathItem) -> Vec<(&'static str, &Operation)> {
    [
        ("GET", item.get.as_ref()),
        ("PUT", item.put.as_ref()),
        ("POST", item.post.as_ref()),
        ("DELETE", item.delete.as_ref()),
        ("OPTIONS", item.options.as_ref()),
        ("HEAD", item.head.as_ref()),
        ("PATCH", item.patch.as_ref()),
        ("TRACE", item.trace.as_ref()),
    ]
    .into_iter()
    .filter_map(|(method, operation)| operation.map(|operation| (method, operation)))
    .collect()
}

fn offenders_for(path: &str, item: &PathItem) -> Vec<String> {
    let mut out = Vec::new();
    for (method, operation) in operations(item) {
        for (status, response) in &operation.responses.responses {
            if !MUST_HAVE_BODY.contains(&status.as_str()) {
                continue;
            }
            let missing = match response {
                RefOr::T(response) => !response_has_schema(response),
                // A `$ref` to a shared component response is a declared body.
                RefOr::Ref(_) => false,
            };
            if missing {
                out.push(format!("{method} {path} -> {status}"));
            }
        }
    }
    out
}

#[test]
fn every_success_response_declares_a_body() {
    let spec = batlehub_web::openapi_spec();

    let mut offenders: Vec<String> = spec
        .paths
        .paths
        .iter()
        .flat_map(|(path, item)| offenders_for(path, item))
        .collect();
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "{} documented success response(s) declare no body. \
         Add `body = T` to the `utoipa::path` responses(...) clause; where the \
         handler returns an ad-hoc `json!`, give it a named DTO deriving \
         `ToSchema` in the same module (RFC 0004 §6.1).\n{}",
        offenders.len(),
        offenders.join("\n"),
    );
}

/// A guard on the guard: if route collection ever silently returns an empty
/// spec, the assertion above would pass vacuously.
#[test]
fn the_spec_is_not_empty() {
    let spec = batlehub_web::openapi_spec();
    assert!(
        spec.paths.paths.len() > 100,
        "expected the full route set, got {} paths",
        spec.paths.paths.len(),
    );
}

// ── The status a handler declares is the status it sends ─────────────────────
//
// `every_success_response_declares_a_body` walks the *generated* spec, which
// is the right place to ask whether a response has a schema. It cannot ask
// the other question: whether the status in the annotation is the status the
// handler actually produces. Nothing generates that — the annotation is
// written by hand beside a body that is written separately, and the two drift
// silently, because a wrong number in a document nobody executes breaks
// nothing until a client believes it.
//
// This is the gate for that, and it is a **source** lint rather than a spec
// one: it reads every `#[utoipa::path(…)]` in this crate, takes the `2xx`
// statuses it declares, and compares them with the `2xx` statuses the handler
// that follows actually constructs.
//
// What it can and cannot see, stated plainly: it matches literal
// `HttpResponse::Ok`/`Created` and `StatusCode::OK`/`CREATED` inside the
// handler body, including the literal a handler hands to a shared responder
// like `publish_and_respond`. A handler that computes its status from a
// variable is invisible to it and is skipped rather than guessed at. That is
// the honest bound: this catches the mistake that has actually happened
// twice — an annotation edited without the body, or a body edited without the
// annotation — and does not pretend to prove the general case.

/// The success statuses this lint can recognise, as (constructor fragment,
/// status) pairs.
const CONSTRUCTORS: [(&str, &str); 4] = [
    ("HttpResponse::Ok", "200"),
    ("StatusCode::OK", "200"),
    ("HttpResponse::Created", "201"),
    ("StatusCode::CREATED", "201"),
];

/// Every `.rs` file under this crate's `src/`.
fn source_files() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut out,
    );
    out.sort();
    out
}

/// The `2xx` statuses an attribute block declares, in source order.
fn declared_successes(attr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in attr.split("status = ").skip(1) {
        let code: String = part.chars().take_while(char::is_ascii_digit).collect();
        if code.starts_with('2') && code.len() == 3 && !out.contains(&code) {
            out.push(code);
        }
    }
    out
}

/// The next `open`…`close` group after `from`, balanced.
///
/// Two delimiters because the two things this reads are punctuated
/// differently: an attribute is `#[utoipa::path( … )]` and a handler is
/// `async fn … { … }`. Getting that wrong is how the first draft of this test
/// scanned nothing at all, which is what the `checked > 100` assertion below
/// exists to catch.
fn delimited_after(text: &str, from: usize, open: char, close: char) -> Option<&str> {
    let rest = &text[from..];
    let start = rest.find(open)?;
    let mut depth = 0usize;
    for (i, c) in rest[start..].char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(&rest[start..start + i]);
            }
        }
    }
    None
}

/// RFC 0018's re-read of this tree recorded that one publish endpoint
/// declared `201` and answered `200`. It does not any more — the shared
/// responder took a status argument in June and every publish path passes one
/// — and the point of this test is that nobody has to re-read the tree to
/// know it. It is the item that finding became.
/// The statuses a handler body literally constructs, from `CONSTRUCTORS`.
fn constructed_statuses(body: &str) -> Vec<&'static str> {
    let mut constructs: Vec<&str> = Vec::new();
    for (fragment, status) in CONSTRUCTORS {
        if body.contains(fragment) && !constructs.contains(&status) {
            constructs.push(status);
        }
    }
    constructs
}

/// Where the two disagree, in both directions, for one handler.
fn disagreements(short: &str, name: &str, declared: &[String], constructs: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for sent in constructs {
        if !declared.iter().any(|d| d == sent) {
            out.push(format!(
                "{short}::{name} sends {sent} but its utoipa::path declares {declared:?}"
            ));
        }
    }
    for d in declared {
        if !constructs.contains(&d.as_str()) {
            out.push(format!(
                "{short}::{name} declares {d} but its body only constructs {constructs:?}"
            ));
        }
    }
    out
}

/// One `#[utoipa::path(…)]` and the handler under it. `None` when this lint
/// cannot read the pair — a missing declaration, a body whose status comes
/// from somewhere it cannot follow — and a guess would be worse than a skip.
fn compare_one(text: &str, short: &str, at: &mut usize, start: usize) -> Option<Vec<String>> {
    let attr = delimited_after(text, start, '(', ')')?;
    *at = start + attr.len();

    let declared = declared_successes(attr);
    if declared.is_empty() {
        return None;
    }
    // The handler is the next `async fn` after the annotation.
    let fn_start = *at + text[*at..].find("async fn ")?;
    let name: String = text[fn_start + "async fn ".len()..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let body = delimited_after(text, fn_start, '{', '}')?;

    let constructs = constructed_statuses(body);
    if constructs.is_empty() {
        return None;
    }
    Some(disagreements(short, &name, &declared, &constructs))
}

#[test]
fn every_declared_success_status_is_one_the_handler_can_actually_send() {
    let mut offenders: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for file in source_files() {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        let short = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&file)
            .display()
            .to_string();

        let mut at = 0usize;
        while let Some(found) = text[at..].find("#[utoipa::path(") {
            let start = at + found;
            let before = at;
            match compare_one(&text, &short, &mut at, start) {
                Some(found) => {
                    checked += 1;
                    offenders.extend(found);
                }
                // `compare_one` advances `at` past the attribute before it can
                // fail; if it failed before that, do it here or the scan spins.
                None if at == before => break,
                None => {}
            }
        }
    }

    assert!(
        checked > 100,
        "only {checked} handlers were checked — the scan stopped finding them, which is a \
         broken gate rather than a clean tree"
    );
    assert!(
        offenders.is_empty(),
        "the OpenAPI annotation and the handler disagree about the status ({} case(s)).\n\
         A wrong number here breaks nothing until a client believes it, which is why it is \
         checked rather than reviewed:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}
