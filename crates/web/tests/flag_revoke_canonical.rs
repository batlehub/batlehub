//! The revoke credential has four readers, and the endpoint cannot tell them
//! apart when they disagree.
//!
//! `DELETE /api/v1/flags/{source}/{external_id}` carries no body, so its
//! `X-Hub-Signature-256` covers [`revoke_canonical`] instead — the method and
//! the path, which binds the proof to the one flag it lifts (see that
//! function). Four things compute that string: the handler, the heavy suite's
//! `heavy_flag_revoke_canonical`, and the literal quoted in the configuration
//! guide, the incident-response runbook and RFC 0002 — where an operator reads
//! it, because a SOC integrates against the documentation and not against this
//! tree.
//!
//! # Why a gate and not a review item
//!
//! The endpoint answers an unknown source and a bad signature with the same
//! `404 unknown flag source: <name>`, deliberately, so that it does not
//! enumerate the sources. The cost is that a caller signing the old way is
//! told its *source* is missing, and the source is plainly there in the config
//! it just pushed to. When the canonical string was introduced, the in-process
//! tests were updated with the handler and the heavy suite was not; the suite
//! is CI-only and slow, so the disagreement surfaced as that misleading `404`
//! an afternoon later, in a step eight minutes into a run.
//!
//! Nothing here re-derives the answer — that is what makes it a gate rather
//! than a second opinion. It asks the *other* implementations what they
//! compute and holds them to this crate's function, in `cargo test`, at the
//! moment the function changes.
//!
//! # What is deliberately not checked
//!
//! `crates/web/tests/flags.rs` also spells the string out. It is left to spell
//! it out: it drives the real handler over HTTP, so a literal that drifts
//! fails there on its own, as an assertion about a `200` rather than about a
//! source file.

use std::path::{Path, PathBuf};
use std::process::Command;

use batlehub_web::handlers::flags::revoke_canonical;
use batlehub_web::services::verify_inbound_hmac;

/// A source name and an id with no regex meaning and no shell meaning, so a
/// failure is about the format and never about the sample.
const SOURCE: &str = "acme-soc";
const EXTERNAL_ID: &str = "SOC-2026-0412";
const SECRET: &str = "gate-secret";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/web is two levels below the repo root")
        .to_path_buf()
}

/// Run a snippet with `tests/heavy/lib.sh` sourced, and return its stdout.
///
/// Sourcing the harness is the point: the gate must read the definition the
/// suites actually call, not a copy of it kept here.
fn heavy_lib(snippet: &str) -> String {
    let root = repo_root();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("source tests/heavy/lib.sh; {snippet}"))
        .current_dir(&root)
        .output()
        .expect("bash runs the heavy harness");
    assert!(
        out.status.success(),
        "tests/heavy/lib.sh failed to run `{snippet}`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("the harness emits UTF-8")
}

/// The heavy suite signs what the handler verifies.
#[test]
fn heavy_suite_signs_the_canonical_the_handler_verifies() {
    let expected = revoke_canonical(SOURCE, EXTERNAL_ID);

    // Compared before the signature so a drift reports the two strings, not an
    // opaque "the header was rejected".
    let from_harness = heavy_lib(&format!(
        "heavy_flag_revoke_canonical {SOURCE} {EXTERNAL_ID}"
    ));
    assert_eq!(
        from_harness, expected,
        "tests/heavy/lib.sh signs bytes the handler does not verify — \
         update heavy_flag_revoke_canonical to match revoke_canonical() in \
         crates/web/src/handlers/flags.rs"
    );

    // And the whole credential, end to end: what the suite would put in the
    // header is what `authenticate` accepts.
    let header = heavy_lib(&format!(
        "heavy_flag_sign_revoke {SECRET} {SOURCE} {EXTERNAL_ID}"
    ));
    assert!(
        verify_inbound_hmac(SECRET, expected.as_bytes(), header.trim()),
        "the heavy suite's revoke signature is not one the server accepts"
    );
}

/// Every page that tells an operator what to sign quotes the current string.
///
/// The placeholders are the point: these pages are read by someone who has to
/// substitute their own source and id, so the gate asserts the *shape*, with
/// the newline written the way markdown carries it.
#[test]
fn the_documented_canonical_is_the_current_one() {
    let quoted = revoke_canonical("{source}", "{external_id}").replace('\n', "\\n");
    let root = repo_root();
    // Where an operator meets the revoke: the key's own row, the runbook step
    // that lifts a flag, and the RFC that specifies the surface.
    for page in [
        "docs/guide/configuration.md",
        "docs/operations/incident-response.md",
        "docs/rfc/0002-vulnerability-flags-and-exposure.md",
    ] {
        let text = std::fs::read_to_string(root.join(page))
            .unwrap_or_else(|e| panic!("{page} is readable: {e}"));
        assert!(
            text.contains(&quoted),
            "{page} does not quote `{quoted}`, so an operator integrating from \
             it will sign bytes the server rejects — and be told the source is \
             unknown"
        );
    }
}
