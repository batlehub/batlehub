#![no_main]
//! `services::nix`: the narinfo document, the fingerprint a `Sig:` covers, and
//! the edits the proxy and the publish path make between reading a narinfo
//! and serving it.
//!
//! The input is two documents — raw text, or a well-formed template with
//! fuzzer-chosen values in the fields the fingerprint covers. For each one
//! that parses:
//!
//! 1. **Round trip** — `parse(to_wire(x)) == x`. The proxy relays a narinfo
//!    as its lines; a line that reads back differently is a relayed document
//!    that is not the one upstream signed.
//! 2. **The fingerprint decodes** — split on `;` it is exactly
//!    `1`, the full store path, `NarHash`, `NarSize` as a number and the
//!    references, and the references split on `,` are exactly the sorted set
//!    of `{store dir}/{base}`. A fingerprint that decodes is injective, so no
//!    `Sig:` over one narinfo verifies a narinfo that claims something else.
//! 3. **Injective across the pair** — two documents with one fingerprint claim
//!    one (StorePath, NarHash, NarSize, set of References). Implied by 2 (each
//!    fingerprint decodes to its own claim), so it cannot fail while 2 holds;
//!    it is the attack stated directly, and costs one comparison.
//! 4. **One registry signature** — after `drop_signatures_by(our key)` and
//!    appending ours, exactly one `Sig:` is under our key name, and it
//!    verifies over the fingerprint with our public key. A publisher cannot
//!    smuggle a second one through.
//! 5. **`rewrite_url` never moves the fingerprint** — the property the whole
//!    proxy design rests on: `URL:` is rewritten and `Sig:` relayed untouched.

use std::collections::BTreeSet;

use libfuzzer_sys::fuzz_target;

use batlehub_core::services::nix::{
    fingerprint, rewrite_url, verify_signature, NarInfo, NixSigningKey, DEFAULT_STORE_DIR,
};

const KEY_NAME: &str = "batlehub-nix-1";

/// What a fingerprint is supposed to say, read without the fingerprint.
#[derive(Debug, PartialEq, Eq)]
struct Claim {
    path: String,
    nar_hash: String,
    nar_size: u64,
    refs: BTreeSet<String>,
}

fn claim(info: &NarInfo) -> Claim {
    let path = info
        .store_path()
        .expect("fingerprint succeeded, so StorePath parses");
    let dir = path
        .store_dir
        .clone()
        .unwrap_or_else(|| DEFAULT_STORE_DIR.to_owned());
    Claim {
        path: path.to_full_path(),
        nar_hash: info.get("NarHash").unwrap().to_owned(),
        nar_size: info.get("NarSize").unwrap().parse().unwrap(),
        refs: info
            .get_all("References")
            .iter()
            .flat_map(|l| l.split_whitespace())
            .map(|b| format!("{dir}/{b}"))
            .collect(),
    }
}

fn check_one(text: &str) -> Option<(String, Claim)> {
    let mut info = NarInfo::parse(text).ok()?;

    // 1. round trip
    let wire = info.to_wire();
    let again = NarInfo::parse(&wire).expect("to_wire output must parse");
    assert_eq!(again, info, "round trip changed the document:\n{wire}");

    let fp = fingerprint(&info).ok()?;
    let want = claim(&info);

    // 2. the fingerprint decodes to the claim
    let parts: Vec<&str> = fp.split(';').collect();
    assert_eq!(parts.len(), 5, "fingerprint has extra fields: {fp:?}");
    assert_eq!(parts[0], "1");
    assert_eq!(parts[1], want.path, "{fp:?}");
    assert_eq!(parts[2], want.nar_hash, "{fp:?}");
    assert_eq!(parts[3], want.nar_size.to_string(), "{fp:?}");
    let refs: Vec<&str> = if parts[4].is_empty() {
        Vec::new()
    } else {
        parts[4].split(',').collect()
    };
    let sorted: Vec<&str> = want.refs.iter().map(String::as_str).collect();
    assert_eq!(refs, sorted, "{fp:?}");

    // 5. rewrite_url never moves the fingerprint
    let hash = info.store_path().unwrap().hash;
    let mut rewritten = info.clone();
    if rewrite_url(&mut rewritten, &hash).is_ok() {
        assert_eq!(fingerprint(&rewritten).ok().as_deref(), Some(fp.as_str()));
    }

    // 4. exactly one registry signature, and it verifies
    let key = NixSigningKey::from_seed([7; 32], KEY_NAME);
    info.drop_signatures_by(KEY_NAME);
    let sig = key.sign_fingerprint(&fp);
    info.push_signature(sig.clone());
    let ours = info
        .get_all("Sig")
        .into_iter()
        .filter(|s| s.split(':').next() == Some(KEY_NAME))
        .count();
    assert_eq!(ours, 1, "{}", info.to_wire());
    assert!(verify_signature(&fp, &sig, &[key.public_key_line()]));
    // ...and the signed document still carries the same fingerprint.
    assert_eq!(fingerprint(&info).ok().as_deref(), Some(fp.as_str()));

    Some((fp, want))
}

/// One field of a template document: `None` keeps the well-formed default, so
/// most inputs reach the fingerprint and only the chosen fields are hostile.
#[derive(Debug, arbitrary::Arbitrary)]
struct Fields {
    store_dir: Option<String>,
    hash: Option<String>,
    name: Option<String>,
    nar_hash: Option<String>,
    nar_size: Option<String>,
    url: Option<String>,
    refs: Vec<Option<String>>,
    sigs: Vec<String>,
}

impl Fields {
    fn render(&self) -> String {
        let mut doc = format!(
            "StorePath: {}/{}-{}\nURL: {}\nNarHash: {}\nNarSize: {}\n",
            self.store_dir.as_deref().unwrap_or("/nix/store"),
            self.hash
                .as_deref()
                .unwrap_or("0001npbf2n4z3pjy6vm2mw8ywkqixxs6"),
            self.name.as_deref().unwrap_or("hello-1.0"),
            self.url.as_deref().unwrap_or("nar/x.nar.zst"),
            self.nar_hash
                .as_deref()
                .unwrap_or("sha256:075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x"),
            self.nar_size.as_deref().unwrap_or("226848"),
        );
        if !self.refs.is_empty() {
            doc.push_str("References:");
            for r in &self.refs {
                doc.push(' ');
                doc.push_str(
                    r.as_deref()
                        .unwrap_or("ghpayap4j5fqg9ryyzrfdj9ygdi01iw9-aeson-2.2.4.1-doc"),
                );
            }
            doc.push('\n');
        }
        for sig in &self.sigs {
            doc.push_str(&format!("Sig: {sig}\n"));
        }
        doc
    }
}

/// Raw text exercises the line grammar; the template keeps the fingerprinted
/// fields reachable, which raw bytes almost never are.
#[derive(Debug, arbitrary::Arbitrary)]
enum Input {
    Raw(String, String),
    Template(Fields, Fields),
}

fuzz_target!(|input: Input| {
    let (a, b) = match input {
        Input::Raw(a, b) => (a, b),
        Input::Template(a, b) => (a.render(), b.render()),
    };
    let a = check_one(&a);
    let b = check_one(&b);
    // 3. injective across the pair
    if let (Some((fa, ca)), Some((fb, cb))) = (a, b) {
        if fa == fb {
            assert_eq!(ca, cb, "one fingerprint, two claims: {fa:?}");
        }
    }
});
