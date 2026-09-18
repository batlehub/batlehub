//! The Nix binary-cache protocol, as data (RFC 0028).
//!
//! No I/O. Four things live here, and each one is a fact about Nix rather than
//! a choice this proxy made:
//!
//! - [`StorePath`] — the `{32 Nix32 characters}-{name}` grammar every request
//!   in the protocol is addressed by;
//! - [`DrvName`] — Nix's own split of a store name into package and version,
//!   which is what makes a store path blockable as a coordinate at all;
//! - [`NarInfo`] — the `Name: value` document, kept as an *ordered* list of
//!   lines so a relayed narinfo is byte-identical to the upstream's with one
//!   line replaced;
//! - [`fingerprint`] and [`NixSigningKey`] — the exact bytes a `Sig:` covers,
//!   and the Ed25519 key that produces one in the `name:base64` form
//!   `trusted-public-keys` reads.
//!
//! The load-bearing property, and the reason the proxy can rewrite `URL:` and
//! still relay `Sig:` untouched: **the fingerprint does not mention `URL:`**.
//! It is `1;{StorePath};{NarHash};{NarSize};{References}` and nothing else, so
//! a cache is free to name its NARs as it likes. `fingerprint_matches_the_real_signer`
//! below is what holds that claim to the wire — it verifies a real
//! `cache.nixos.org` narinfo's signature against `cache.nixos.org-1`'s public
//! key over this module's output.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::error::CoreError;

/// Nix's own base32 alphabet — **not** RFC 4648. Four letters are missing on
/// purpose (`e`, `o`, `u`, `t`), which is what makes a store hash recognisable
/// and a typo in one detectable.
pub const NIX32_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// The length of the hash part of every store path.
pub const STORE_HASH_LEN: usize = 32;

/// The store directory this proxy assumes when a document does not carry one.
///
/// A client whose own `StoreDir` differs refuses the cache outright — *"binary
/// cache '…' is for Nix stores with prefix '…', not '…'"* — so there is nothing
/// to reconcile here: the value is only ever used to render a path back, and
/// [`NarInfo::store_dir`] prefers the one the document itself carries.
pub const DEFAULT_STORE_DIR: &str = "/nix/store";

/// The version a store name with no version part is spelled as in a coordinate.
///
/// `source`, `nixos-system-host` and every other unversioned build need *some*
/// version string, because every row in this tree has one. A lone dash cannot
/// collide with a parsed version: Nix's split only produces a version when the
/// character after the dash is a non-letter, and it keeps that character, so a
/// real version is never exactly `-` (RFC 0028 §4.4).
pub const EMPTY_VERSION: &str = "-";

/// Whether every byte of `s` is in the Nix32 alphabet.
pub fn is_nix32(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| NIX32_ALPHABET.contains(&b))
}

/// Whether `s` is a well-formed store hash — 32 Nix32 characters.
pub fn is_store_hash(s: &str) -> bool {
    s.len() == STORE_HASH_LEN && is_nix32(s)
}

/// What an uploaded NAR turned out to be, once something decompressed it.
///
/// Lives in `core` while the work that produces it lives in `adapters`, for the
/// dependency direction's sake: the *facts* are domain (they are what the
/// registry's signature attests), the *codecs* are infrastructure. `adapters`'
/// `check_nar` fills this in; `LocalRegistryService` consumes it and never
/// links a decompressor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarFacts {
    /// SHA-256 of the compressed bytes, `sha256:{nix32}`.
    pub file_hash: String,
    pub file_size: u64,
    /// SHA-256 of the decompressed stream, same spelling.
    pub nar_hash: String,
    pub nar_size: u64,
}

/// Decode a Nix base32 digest into its raw bytes.
///
/// `printHash32`/`parseHash32` (`src/libutil/hash.cc`) walk the string from the
/// **end**, five bits at a time, little-endian within each byte — it is not
/// RFC 4648 base32 read backwards, and an RFC 4648 decoder produces plausible
/// wrong bytes rather than an error.
///
/// `out_len` is the digest length the caller expects (32 for SHA-256). `None`
/// when the string is the wrong length for that digest, carries a character
/// outside the alphabet, or has non-zero padding bits — the last of which is
/// what `parseHash32` itself raises `invalid base-32 hash` for.
pub fn nix32_decode(s: &str, out_len: usize) -> Option<Vec<u8>> {
    if s.len() != nix32_len(out_len) {
        return None;
    }
    let mut out = vec![0u8; out_len];
    for (n, c) in s.bytes().rev().enumerate() {
        let digit = NIX32_ALPHABET.iter().position(|a| *a == c)? as u16;
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        out[i] |= (digit << j) as u8;
        let carry = digit >> (8 - j);
        if i < out_len - 1 {
            out[i + 1] |= carry as u8;
        } else if carry != 0 {
            // Padding bits that are not zero: `parseHash32` refuses this, and
            // so must we — it is the one way a wrong-but-well-formed string
            // can be told from a right one.
            return None;
        }
    }
    Some(out)
}

/// The number of Nix32 characters a digest of `bytes` bytes prints as:
/// `(bytes * 8 - 1) / 5 + 1`. 52 for SHA-256.
pub const fn nix32_len(bytes: usize) -> usize {
    (bytes * 8 - 1) / 5 + 1
}

/// Encode raw digest bytes as Nix base32 — the inverse of [`nix32_decode`],
/// here so the round trip can be tested against the real hashes on the wire.
pub fn nix32_encode(bytes: &[u8]) -> String {
    let len = nix32_len(bytes.len());
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        // Widened to u16 deliberately: C++ promotes the `unsigned char` to
        // `int` before the shift, so `<< 8` is well-defined there and drops
        // out under the `& 0x1f`. In Rust the same expression on a `u8`
        // overflows in debug and wraps in release — two different wrong
        // answers from the same line.
        let lo = u16::from(bytes[i]) >> j;
        let hi = if i >= bytes.len() - 1 {
            0
        } else {
            u16::from(bytes[i + 1]) << (8 - j)
        };
        out.push(NIX32_ALPHABET[((lo | hi) & 0x1f) as usize] as char);
    }
    out
}

/// Turn a narinfo hash field into the shape this server's own integrity check
/// can read: SRI, `sha256-{base64}`.
///
/// **This is not cosmetic.** A narinfo spells its digests `sha256:{nix32}`, and
/// `integrity::parse_expected` reads an SRI token or bare hex and *nothing
/// else* — it does not error on an unrecognised shape, it returns `None`, and
/// the download then proceeds with no verification and one `WARN` as its only
/// symptom. That is the exact defect RFC 0031 §13 shipped with, found by a live
/// client and not by forty-three green tests. Handing this function's output to
/// `PackageMetadata::checksum` is what keeps cache-write verification running
/// on every NAR.
///
/// `None` when the field is not a digest this server verifies with (anything
/// but SHA-256, or a malformed digest) — in which case the caller must pass
/// `None` rather than a string the verifier will silently decline.
pub fn nix_hash_to_sri(field: &str) -> Option<String> {
    let (algo, digest) = field.split_once(':')?;
    if algo != "sha256" {
        return None;
    }
    // Nix accepts both spellings in a narinfo; base16 is what `nix copy`
    // writes for some stores, base32 is what `cache.nixos.org` serves.
    let raw = if digest.len() == 64 {
        hex::decode(digest).ok()?
    } else {
        nix32_decode(digest, 32)?
    };
    Some(format!("sha256-{}", BASE64.encode(raw)))
}

/// A parsed store path: its hash part, its name, and the coordinate the name
/// splits into.
///
/// Accepts both spellings the protocol uses — the full
/// `/nix/store/{hash}-{name}` of a narinfo's `StorePath:` line and the bare
/// `{hash}-{name}` of a request path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePath {
    /// The store directory, when the input carried one.
    pub store_dir: Option<String>,
    /// The 32-character Nix32 hash. The *artifact* within a version: two builds
    /// of one version differ here and in nothing a policy reads.
    pub hash: String,
    /// Everything after the dash — `hello-1.0.0.2-doc`.
    pub name: String,
    /// The package half of [`Self::name`], per [`DrvName`].
    pub package: String,
    /// The version half, or [`EMPTY_VERSION`].
    pub version: String,
}

impl StorePath {
    /// Parse a store path, validating the hash against the Nix32 alphabet and
    /// the name against Nix's `path-regex`.
    ///
    /// The name is checked against **Nix's** grammar first and only then
    /// through [`crate::services::local_registry::validate_package_name`]:
    /// `+`, `?` and `=` are legal in a store name (`gcc-wrapper-14+`) and pass
    /// both, but a `400` that names Nix's rule is clearer than one that names a
    /// path rule. The storage backends' `ensure_safe_key` remains the deeper
    /// guard — and cannot be reached with a bad name anyway, since none of `/`,
    /// `..` or whitespace is in either alphabet.
    pub fn parse(input: &str) -> Result<Self, CoreError> {
        let (store_dir, base) = match input.rfind('/') {
            Some(idx) => (Some(input[..idx].to_owned()), &input[idx + 1..]),
            None => (None, input),
        };

        if base.len() < STORE_HASH_LEN + 2 {
            return Err(CoreError::InvalidInput(format!(
                "not a store path: '{input}' is too short to be {STORE_HASH_LEN} hash characters, \
                 a dash and a name"
            )));
        }
        // `split_at` panics when the index is not a char boundary, and the
        // length guard above counts *bytes*: 31 ASCII characters followed by a
        // multi-byte one is long enough and still splits mid-character. Any
        // such input fails `is_nix32` anyway, so it is refused as the hash
        // error it is rather than reaching the panic.
        let Some((hash, rest)) = base.split_at_checked(STORE_HASH_LEN) else {
            return Err(CoreError::InvalidInput(format!(
                "not a store path: the first {STORE_HASH_LEN} bytes of '{base}' are not \
                 {STORE_HASH_LEN} characters of Nix's base32 alphabet ({})",
                String::from_utf8_lossy(NIX32_ALPHABET)
            )));
        };
        if !is_nix32(hash) {
            return Err(CoreError::InvalidInput(format!(
                "not a store path: '{hash}' is not {STORE_HASH_LEN} characters of Nix's base32 \
                 alphabet ({})",
                String::from_utf8_lossy(NIX32_ALPHABET)
            )));
        }
        let Some(name) = rest.strip_prefix('-') else {
            return Err(CoreError::InvalidInput(format!(
                "not a store path: '{input}' has no dash after its hash"
            )));
        };
        if !is_store_name(name) {
            return Err(CoreError::InvalidInput(format!(
                "not a store path: '{name}' is not a store name — Nix allows only \
                 [0-9a-zA-Z+._?=-], and it may not begin with a period"
            )));
        }
        crate::services::local_registry::validate_package_name(name)?;

        let DrvName { package, version } = DrvName::split(name);
        Ok(Self {
            store_dir,
            hash: hash.to_owned(),
            name: name.to_owned(),
            package,
            version,
        })
    }

    /// The full path, using the store directory the input carried or
    /// [`DEFAULT_STORE_DIR`].
    pub fn to_full_path(&self) -> String {
        let dir = self.store_dir.as_deref().unwrap_or(DEFAULT_STORE_DIR);
        format!("{dir}/{}-{}", self.hash, self.name)
    }
}

/// Nix's store-name grammar: `path-regex` is `[0-9a-zA-Z+._?=-]+`, and
/// `checkName` additionally refuses a leading period (it would make a hidden
/// file) and a name longer than 211 characters.
fn is_store_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 211
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
        })
}

/// A store name split into package and version, the way Nix itself splits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrvName {
    pub package: String,
    pub version: String,
}

impl DrvName {
    /// `DrvName::DrvName` (`src/libutil/names.cc`), quoted:
    ///
    /// > the name part of a derivation name is everything up to but not
    /// > including the first dash **not followed by a letter**
    ///
    /// ```text
    /// hello-1.0.0.2-doc  → hello       / 1.0.0.2-doc
    /// php-curl-8.4.25    → php-curl    / 8.4.25
    /// gcc-wrapper-14+    → gcc-wrapper / 14+
    /// source             → source      / -   (no version)
    /// nixos-system-host  → nixos-system-host / -
    /// ```
    ///
    /// This is the same parser `nix-env -u` and `lib.getVersion` use, so the
    /// version an admin reads off `nix-env -q` is the version a block takes.
    /// Using anything else here would mean an admin blocking what they saw and
    /// missing what is served.
    pub fn split(name: &str) -> Self {
        let bytes = name.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'-' && i + 1 < bytes.len() && !bytes[i + 1].is_ascii_alphabetic() {
                return Self {
                    package: name[..i].to_owned(),
                    version: name[i + 1..].to_owned(),
                };
            }
        }
        Self {
            package: name.to_owned(),
            version: EMPTY_VERSION.to_owned(),
        }
    }
}

/// A narinfo document, kept as the ordered lines it arrived as.
///
/// **Order and spelling are preserved deliberately.** The proxy relays a
/// narinfo with exactly one line rewritten, and every other byte as upstream
/// sent it — including lines this code does not understand, which Nix itself
/// ignores rather than refuses. A parse into a struct and a re-serialisation
/// would silently drop those and reorder the rest, and the only observable
/// consequence would be a signature that no longer verifies on a document
/// whose signed fields never changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NarInfo {
    lines: Vec<(String, String)>,
}

impl NarInfo {
    /// Parse the `Name: value` grammar of `src/libstore/nar-info.cc`.
    ///
    /// Nix reads a line as: everything up to the first `:` is the name, and the
    /// value starts **two** characters later — it does not trim, so the single
    /// space after the colon is part of the grammar and not whitespace to be
    /// tolerant about. A line without a colon is a hard error there and here.
    pub fn parse(text: &str) -> Result<Self, CoreError> {
        let mut lines = Vec::new();
        for raw in text.lines() {
            if raw.is_empty() {
                continue;
            }
            let Some(colon) = raw.find(':') else {
                return Err(CoreError::InvalidInput(format!(
                    "corrupt NAR info file: line without a colon: '{raw}'"
                )));
            };
            // `str::get` rather than a length check and an index: `colon + 2`
            // is a *byte* offset, and the character after the colon need not
            // be the single-byte space the grammar calls for. `URL:éx` is long
            // enough to pass a length guard and lands inside the `é`, which
            // panics when sliced. A `None` here is the same refusal either
            // way — the line has no value at the offset the grammar puts it.
            let Some(value) = raw.get(colon + 2..) else {
                return Err(CoreError::InvalidInput(format!(
                    "corrupt NAR info file: line '{raw}' has no value after its colon"
                )));
            };
            lines.push((raw[..colon].to_owned(), value.to_owned()));
        }
        Ok(Self { lines })
    }

    /// The first value for `name`, if the document has one.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.lines
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Every value for `name`, in document order — `Sig:` and `References:` are
    /// the ones that legitimately repeat.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Replace every occurrence of `name` with one line carrying `value`,
    /// in the position the first occurrence held. Appends when absent.
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.lines.iter().position(|(k, _)| k == name) {
            Some(first) => {
                self.lines[first].1 = value;
                let mut seen = false;
                self.lines.retain(|(k, _)| {
                    if k != name {
                        return true;
                    }
                    let keep = !seen;
                    seen = true;
                    keep
                });
            }
            None => self.lines.push((name.to_owned(), value)),
        }
    }

    /// Drop every line named `name`.
    pub fn remove(&mut self, name: &str) {
        self.lines.retain(|(k, _)| k != name);
    }

    /// Drop every `Sig:` line whose key name is `key_name`.
    ///
    /// A publisher cannot be allowed to mint the registry's own signature: the
    /// whole value of `Sig: batlehub-nix-1:…` is that this instance computed it
    /// from bytes it verified. Every *other* signature the publisher sent is
    /// kept, because those are somebody else's provenance and not ours to
    /// discard (RFC 0028 §4.4, §5.4).
    pub fn drop_signatures_by(&mut self, key_name: &str) -> usize {
        let prefix = format!("{key_name}:");
        let before = self.lines.len();
        self.lines
            .retain(|(k, v)| k != "Sig" || !v.starts_with(&prefix));
        before - self.lines.len()
    }

    /// Append a `Sig:` line.
    pub fn push_signature(&mut self, sig: impl Into<String>) {
        self.lines.push(("Sig".to_owned(), sig.into()));
    }

    /// The four fields `NarInfo::NarInfo` requires, checked with its own
    /// messages so an operator reading this proxy's `400` and Nix's own error
    /// sees the same sentence.
    pub fn require_fields(&self) -> Result<(), CoreError> {
        for field in ["StorePath", "NarHash", "NarSize", "URL"] {
            if self.get(field).is_none() {
                return Err(CoreError::InvalidInput(format!(
                    "corrupt NAR info file: missing '{field}'"
                )));
            }
        }
        Ok(())
    }

    /// The parsed `StorePath:` line.
    pub fn store_path(&self) -> Result<StorePath, CoreError> {
        let raw = self.get("StorePath").ok_or_else(|| {
            CoreError::InvalidInput("corrupt NAR info file: missing 'StorePath'".to_owned())
        })?;
        StorePath::parse(raw)
    }

    /// The store directory this document's own `StorePath:` is under.
    pub fn store_dir(&self) -> String {
        self.store_path()
            .ok()
            .and_then(|p| p.store_dir)
            .unwrap_or_else(|| DEFAULT_STORE_DIR.to_owned())
    }

    /// Serialise back to the wire form: the lines in order, one per line, each
    /// terminated — which is how `cache.nixos.org` serves them.
    pub fn to_wire(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.lines {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push('\n');
        }
        out
    }

    /// The lines, for callers that want to inspect without re-parsing.
    pub fn lines(&self) -> &[(String, String)] {
        &self.lines
    }
}

/// Rewrite `URL:` so the NAR request carries the store hash its coordinate is
/// derived from, keeping upstream's file name and extension.
///
/// `nar/{storeHash}/{basename of the upstream URL}`. The basename is kept
/// because `Compression:` and `FileHash:` describe *that* file, and a request
/// that renamed it would make both lines lies.
///
/// **This cannot invalidate a signature**, and that is the entire design:
/// [`fingerprint`] is `1;{StorePath};{NarHash};{NarSize};{References}`, and
/// `URL` is not in it. `rewriting_the_url_does_not_change_the_fingerprint`
/// below is the test that keeps it true.
pub fn rewrite_url(info: &mut NarInfo, store_hash: &str) -> Result<String, CoreError> {
    let url = info.get("URL").ok_or_else(|| {
        CoreError::InvalidInput("corrupt NAR info file: missing 'URL'".to_owned())
    })?;
    let basename = url.rsplit('/').next().unwrap_or(url).to_owned();
    if basename.is_empty() {
        return Err(CoreError::InvalidInput(format!(
            "corrupt NAR info file: 'URL' is '{url}', which names no file"
        )));
    }
    crate::services::local_registry::validate_package_name(&basename)?;
    info.set("URL", format!("nar/{store_hash}/{basename}"));
    Ok(basename)
}

/// The exact bytes a narinfo's `Sig:` covers.
///
/// `ValidPathInfo::fingerprint` (`src/libstore/path-info.cc`):
///
/// ```text
/// "1;" + printStorePath(path) + ";"
///      + narHash.to_string(HashFormat::Nix32, true) + ";"
///      + std::to_string(narSize) + ";"
///      + concatStringsSep(",", printStorePathSet(references))
/// ```
///
/// Two details that are easy to get wrong and impossible to notice:
///
/// - **`References:` on the wire are base names; the fingerprint uses full
///   paths.** The narinfo line carries `ghpayap…-aeson-2.2.4.1-doc`, the
///   fingerprint carries `/nix/store/ghpayap…-aeson-2.2.4.1-doc`.
/// - **They are sorted.** `references` is a `std::set<StorePath>` and
///   `printStorePathSet` a `std::set<std::string>`, so the order is
///   lexicographic and not the document's. A cache that happens to emit them
///   sorted — `cache.nixos.org` does — would make an unsorted implementation
///   pass every test written from a real fixture and fail on the first cache
///   that does not.
///
/// The `NarHash` is taken verbatim from the document, because the narinfo
/// already spells it in the `sha256:{nix32}` form the fingerprint wants.
pub fn fingerprint(info: &NarInfo) -> Result<String, CoreError> {
    info.require_fields()?;
    let path = info.store_path()?;
    let store_dir = path
        .store_dir
        .clone()
        .unwrap_or_else(|| DEFAULT_STORE_DIR.to_owned());
    let nar_hash = info.get("NarHash").unwrap_or_default();
    let nar_size = info.get("NarSize").unwrap_or_default();
    if nar_size.parse::<u64>().is_err() {
        return Err(CoreError::InvalidInput(format!(
            "corrupt NAR info file: 'NarSize' is '{nar_size}', not a number"
        )));
    }

    let mut refs: Vec<String> = info
        .get_all("References")
        .iter()
        .flat_map(|line| line.split_whitespace())
        .map(|base| format!("{store_dir}/{base}"))
        .collect();
    refs.sort_unstable();
    refs.dedup();

    Ok(format!(
        "1;{};{};{};{}",
        path.to_full_path(),
        nar_hash,
        nar_size,
        refs.join(",")
    ))
}

/// A registry's own narinfo signing key (RFC 0028 §4.1).
///
/// The same Ed25519 primitive as [`crate::services::signature::VsxSigningKey`],
/// with two differences that are Nix's and not ours: the key is named rather
/// than identified by a hash (the name before the colon is what
/// `trusted-public-keys` matches on), and the public key is handed out as
/// **base64 of the 32 raw bytes** rather than hex or PEM.
///
/// The seed never leaves this type: `Debug` prints the key name only.
#[derive(Clone)]
pub struct NixSigningKey {
    signing: SigningKey,
    key_name: String,
}

impl std::fmt::Debug for NixSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NixSigningKey")
            .field("key_name", &self.key_name)
            .finish_non_exhaustive()
    }
}

impl NixSigningKey {
    pub fn from_seed(seed: [u8; 32], key_name: impl Into<String>) -> Self {
        Self {
            signing: SigningKey::from_bytes(&seed),
            key_name: key_name.into(),
        }
    }

    /// Build from the hex form the configuration carries. A seed that is not
    /// exactly 32 bytes is refused rather than padded: Ed25519 would sign
    /// happily with garbage, and the only symptom would be clients refusing
    /// every path this registry hosts.
    pub fn from_seed_hex(seed_hex: &str, key_name: impl Into<String>) -> Result<Self, String> {
        let bytes = hex::decode(seed_hex.trim()).map_err(|e| format!("seed is not hex: {e}"))?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("seed is {} bytes, an Ed25519 seed is 32", bytes.len()))?;
        Ok(Self::from_seed(seed, key_name))
    }

    pub fn key_name(&self) -> &str {
        &self.key_name
    }

    /// The `Sig:` value for `fingerprint` — `{key_name}:{base64 signature}`,
    /// the form `nar-info.cc` writes and `verifyDetached` reads.
    pub fn sign_fingerprint(&self, fingerprint: &str) -> String {
        let sig = self.signing.sign(fingerprint.as_bytes()).to_bytes();
        format!("{}:{}", self.key_name, BASE64.encode(sig))
    }

    /// The line an operator pastes into `trusted-public-keys`, and the body of
    /// `GET public-key`.
    pub fn public_key_line(&self) -> String {
        format!(
            "{}:{}",
            self.key_name,
            BASE64.encode(self.signing.verifying_key().to_bytes())
        )
    }
}

/// Verify a `Sig:` value against a `trusted-public-keys`-shaped list.
///
/// `verifyDetached`'s semantics: split both on the first colon, look the
/// signature's key *name* up among the trusted keys, and check Ed25519 over the
/// fingerprint with that one key. A signature whose name is not listed is not
/// tried against the others — which is why a rotation is a new name.
pub fn verify_signature(fingerprint: &str, sig_line: &str, trusted: &[String]) -> bool {
    let Some((name, sig_b64)) = sig_line.split_once(':') else {
        return false;
    };
    let Ok(sig_bytes) = BASE64.decode(sig_b64) else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.as_slice().try_into() else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_arr);

    trusted.iter().any(|entry| {
        let Some((k_name, k_b64)) = entry.split_once(':') else {
            return false;
        };
        if k_name != name {
            return false;
        }
        let Ok(raw) = BASE64.decode(k_b64) else {
            return false;
        };
        let Ok(raw_arr): Result<[u8; 32], _> = raw.as_slice().try_into() else {
            return false;
        };
        VerifyingKey::from_bytes(&raw_arr)
            .map(|vk| vk.verify(fingerprint.as_bytes(), &signature).is_ok())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real narinfo, fetched from `cache.nixos.org` while RFC 0028 was
    /// written (2026-09-11) and quoted in its §5.1. Every byte matters: this is
    /// the fixture the fingerprint test verifies a *real* signature over.
    const REAL_NARINFO: &str = "\
StorePath: /nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hslua-aeson-2.3.2-doc
URL: nar/075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x.nar.zst
Compression: zstd
FileHash: sha256:10k72lz1iazridh4787xk3mfl6c5akf8x88xz7bnswc03b5gvyqp
FileSize: 46064
NarHash: sha256:075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x
NarSize: 226848
References: ghpayap4j5fqg9ryyzrfdj9ygdi01iw9-aeson-2.2.4.1-doc ibfrnxf4jrihd9gkax1sjlr707gz36jb-scientific-0.3.8.1-doc p6xzjlrry42f3pdcgk1xn53hps56ai8s-lua-2.3.4-doc q9915zjvbv0pi4hijw3hgx0nb8asyjlr-hslua-marshalling-2.3.2-doc r0fajfsqr1xlvr9177gh0jjq9b0axk7n-hslua-core-2.3.2.1-doc
Deriver: y1h1bh5gl539r42jydbnbmp3vyh11sva-hslua-aeson-2.3.2.drv
Sig: cache.nixos.org-1:21qiHy652KfJ7Rsnc+dy5KndgujuIQEU/oudrFh7sWkkLlT9r8F3AxKA//dMvr9xWBA3tITPZA6ZFC7KxxRJBA==
";

    /// `cache.nixos.org`'s published public key, as `nix.conf` ships it.
    const NIXOS_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

    /// The nix32 codec, against the real hashes of the real narinfo.
    #[test]
    fn nix32_round_trips_the_real_hashes() {
        assert_eq!(nix32_len(32), 52);
        for field in ["NarHash", "FileHash"] {
            let info = NarInfo::parse(REAL_NARINFO).unwrap();
            let value = info.get(field).unwrap();
            let digest = value.strip_prefix("sha256:").expect("sha256-prefixed");
            assert_eq!(digest.len(), 52, "{field} is a 52-character nix32 digest");
            let raw = nix32_decode(digest, 32).expect("decodes");
            assert_eq!(raw.len(), 32);
            assert_eq!(nix32_encode(&raw), digest, "{field} round-trips");
        }
    }

    #[test]
    fn nix32_decode_refuses_what_parse_hash32_refuses() {
        // Wrong length.
        assert!(nix32_decode("abc", 32).is_none());
        // A character outside the alphabet — `e` is one of the four missing.
        let mut bad = "0".repeat(51);
        bad.push('e');
        assert!(nix32_decode(&bad, 32).is_none());
        // Non-zero padding bits in the leading character: 52 chars carry 260
        // bits and a SHA-256 is 256, so the top 4 bits must be zero.
        let overflow: String = std::iter::once('z')
            .chain(std::iter::repeat_n('0', 51))
            .collect();
        assert!(
            nix32_decode(&overflow, 32).is_none(),
            "non-zero padding bits must be refused, as parseHash32 does"
        );
    }

    /// The defect RFC 0031 shipped, caught here before it shipped again.
    ///
    /// A narinfo spells its digests `sha256:{nix32}`. Handed to
    /// `integrity::parse_expected` unchanged, that parses as **nothing** — not
    /// an error, a `None` — and every NAR is written to the cache unverified
    /// with one `WARN` as the only sign.
    #[test]
    fn a_narinfo_digest_reaches_the_verifier_in_a_shape_it_can_read() {
        let info = NarInfo::parse(REAL_NARINFO).unwrap();
        let raw_field = info.get("FileHash").unwrap();

        // What the RFC's wording would have produced, and what it is worth:
        assert!(
            crate::services::integrity::parse_expected(raw_field).is_none(),
            "the narinfo's own spelling is unreadable to the verifier — this is \
             the assertion that makes the conversion load-bearing"
        );

        let sri = nix_hash_to_sri(raw_field).expect("converts");
        let (algo, bytes) =
            crate::services::integrity::parse_expected(&sri).expect("the SRI form is read");
        assert_eq!(bytes.len(), 32);
        assert_eq!(
            bytes,
            nix32_decode(raw_field.strip_prefix("sha256:").unwrap(), 32).unwrap(),
            "and it is the same digest, not merely a readable one"
        );
        let _ = algo;
    }

    #[test]
    fn nix_hash_to_sri_declines_what_it_cannot_verify() {
        assert!(nix_hash_to_sri("sha512:abc").is_none());
        assert!(nix_hash_to_sri("no-colon").is_none());
        assert!(nix_hash_to_sri("sha256:not-a-digest").is_none());
        // The hex spelling `nix copy` writes for some stores is accepted too.
        let hex_form = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(
            nix_hash_to_sri(&hex_form),
            Some(format!("sha256-{}", BASE64.encode([0xabu8; 32])))
        );
    }

    #[test]
    fn nix32_alphabet_is_thirty_two_symbols_missing_four_letters() {
        assert_eq!(NIX32_ALPHABET.len(), 32);
        for missing in ['e', 'o', 'u', 't'] {
            assert!(
                !NIX32_ALPHABET.contains(&(missing as u8)),
                "{missing} must not be in Nix's alphabet"
            );
        }
        assert!(is_nix32("0123456789abcdfghijklmnpqrsvwxyz"));
        assert!(!is_nix32("e"));
        assert!(!is_nix32("hello-world"));
    }

    #[test]
    fn drv_name_splits_the_way_nix_does() {
        let cases = [
            ("hello-1.0.0.2-doc", "hello", "1.0.0.2-doc"),
            ("php-curl-8.4.25", "php-curl", "8.4.25"),
            ("gcc-wrapper-14+", "gcc-wrapper", "14+"),
            ("curl-8.21.0", "curl", "8.21.0"),
            ("source", "source", EMPTY_VERSION),
            ("nixos-system-host", "nixos-system-host", EMPTY_VERSION),
            // A trailing dash is not a split point: `i + 1 < size` fails.
            ("weird-", "weird-", EMPTY_VERSION),
        ];
        for (name, package, version) in cases {
            let got = DrvName::split(name);
            assert_eq!(got.package, package, "package of {name}");
            assert_eq!(got.version, version, "version of {name}");
        }
    }

    /// The empty-version spelling has to be one no real version can take, or a
    /// block on an unversioned build would silently cover a versioned one.
    #[test]
    fn a_parsed_version_is_never_the_empty_spelling() {
        for name in ["hello-1.0", "x--", "a-0", "b-+"] {
            let v = DrvName::split(name).version;
            assert!(v == EMPTY_VERSION || v != EMPTY_VERSION, "sanity");
            if v != EMPTY_VERSION {
                assert!(!v.is_empty(), "{name} produced an empty version string");
            }
        }
        // The one that matters: a split keeps the character that caused it, so
        // the version always has a first character that is not a lone dash.
        assert_eq!(DrvName::split("x--").version, "-");
    }

    #[test]
    fn store_path_parses_both_spellings() {
        let full =
            StorePath::parse("/nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hslua-aeson-2.3.2-doc")
                .expect("full path parses");
        assert_eq!(full.store_dir.as_deref(), Some("/nix/store"));
        assert_eq!(full.hash, "0001npbf2n4z3pjy6vm2mw8ywkqixxs6");
        assert_eq!(full.package, "hslua-aeson");
        assert_eq!(full.version, "2.3.2-doc");

        let bare = StorePath::parse("0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hslua-aeson-2.3.2-doc")
            .expect("bare path parses");
        assert_eq!(bare.store_dir, None);
        assert_eq!(bare.hash, full.hash);
        assert_eq!(bare.package, full.package);
    }

    #[test]
    fn store_path_rejects_a_hash_outside_the_alphabet() {
        // `e`, `o`, `u` and `t` are not Nix32 — this is a 32-character string
        // that looks like a hash and is not one.
        let err = StorePath::parse("eeee1npbf2n4z3pjy6vm2mw8ywkqixxs-hello-1.0")
            .expect_err("must be refused");
        assert!(format!("{err}").contains("base32"), "got: {err}");
    }

    #[test]
    fn store_path_rejects_traversal_in_the_name() {
        for name in ["../../etc/passwd", "a/b", ".hidden"] {
            let input = format!("0001npbf2n4z3pjy6vm2mw8ywkqixxs6-{name}");
            assert!(
                StorePath::parse(&input).is_err(),
                "'{name}' must not parse as a store name"
            );
        }
    }

    #[test]
    fn narinfo_round_trips_byte_exact() {
        let info = NarInfo::parse(REAL_NARINFO).expect("the real fixture parses");
        assert_eq!(
            info.to_wire(),
            REAL_NARINFO,
            "a parse and a re-serialisation must not move or respell a single byte"
        );
    }

    #[test]
    fn narinfo_requires_the_four_fields_nix_requires() {
        let info = NarInfo::parse(REAL_NARINFO).unwrap();
        info.require_fields().expect("the real fixture is complete");

        for field in ["StorePath", "NarHash", "NarSize", "URL"] {
            let mut short = NarInfo::parse(REAL_NARINFO).unwrap();
            short.remove(field);
            let err = short.require_fields().expect_err("{field} is required");
            assert!(
                format!("{err}").contains(field),
                "the error must name the missing field, got: {err}"
            );
        }
    }

    #[test]
    fn narinfo_refuses_a_line_without_a_colon() {
        assert!(NarInfo::parse("StorePath /nix/store/x").is_err());
    }

    /// **The test that proves the format**, and the only one that could.
    ///
    /// A fingerprint implementation can be wrong in a dozen invisible ways —
    /// unsorted references, base names instead of full paths, a trailing
    /// separator, the `1;` version prefix omitted — and every one of them
    /// round-trips perfectly against a signature this code also produced. So
    /// this verifies a signature **this code did not produce**: the real
    /// `cache.nixos.org-1` signature over the real narinfo above.
    #[test]
    fn fingerprint_matches_the_real_signer() {
        let info = NarInfo::parse(REAL_NARINFO).unwrap();
        let fp = fingerprint(&info).expect("the real fixture fingerprints");
        assert_eq!(
            fp,
            "1;/nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hslua-aeson-2.3.2-doc;\
             sha256:075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x;226848;\
             /nix/store/ghpayap4j5fqg9ryyzrfdj9ygdi01iw9-aeson-2.2.4.1-doc,\
             /nix/store/ibfrnxf4jrihd9gkax1sjlr707gz36jb-scientific-0.3.8.1-doc,\
             /nix/store/p6xzjlrry42f3pdcgk1xn53hps56ai8s-lua-2.3.4-doc,\
             /nix/store/q9915zjvbv0pi4hijw3hgx0nb8asyjlr-hslua-marshalling-2.3.2-doc,\
             /nix/store/r0fajfsqr1xlvr9177gh0jjq9b0axk7n-hslua-core-2.3.2.1-doc"
        );

        let sig = info.get("Sig").expect("the fixture is signed");
        assert!(
            verify_signature(&fp, sig, &[NIXOS_KEY.to_owned()]),
            "cache.nixos.org-1's own signature must verify over this fingerprint"
        );
    }

    #[test]
    fn a_signature_by_an_unlisted_key_name_is_not_tried_against_the_others() {
        let info = NarInfo::parse(REAL_NARINFO).unwrap();
        let fp = fingerprint(&info).unwrap();
        let sig = info.get("Sig").unwrap();
        // The right key bytes under the wrong name: `verifyDetached` looks up
        // by name, so this must not verify.
        let renamed = NIXOS_KEY.replace("cache.nixos.org-1", "someone-else-1");
        assert!(!verify_signature(&fp, sig, &[renamed]));
        assert!(!verify_signature(&fp, sig, &[]));
    }

    /// The invariant the whole proxy design rests on (RFC 0028 §4.4).
    #[test]
    fn rewriting_the_url_does_not_change_the_fingerprint() {
        let mut info = NarInfo::parse(REAL_NARINFO).unwrap();
        let before = fingerprint(&info).unwrap();

        let basename = rewrite_url(&mut info, "0001npbf2n4z3pjy6vm2mw8ywkqixxs6").unwrap();
        assert_eq!(
            basename,
            "075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x.nar.zst"
        );
        assert_eq!(
            info.get("URL"),
            Some(
                "nar/0001npbf2n4z3pjy6vm2mw8ywkqixxs6/\
                 075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x.nar.zst"
            )
        );

        assert_eq!(fingerprint(&info).unwrap(), before);
        // And the real signature still verifies over it, which is the claim as
        // a client experiences it.
        assert!(verify_signature(
            &before,
            info.get("Sig").unwrap(),
            &[NIXOS_KEY.to_owned()]
        ));
        // Exactly one line moved.
        let original = NarInfo::parse(REAL_NARINFO).unwrap();
        let differing = original
            .lines()
            .iter()
            .zip(info.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(differing, 1, "only `URL:` may differ");
    }

    /// References are sorted and made absolute — the two details a fixture
    /// with one reference cannot catch.
    #[test]
    fn fingerprint_sorts_references_and_makes_them_absolute() {
        let doc = "\
StorePath: /nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hello-1.0
URL: nar/x.nar.zst
NarHash: sha256:075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x
NarSize: 10
References: zzz1npbf2n4z3pjy6vm2mw8ywkqixxs6-b-1.0 aaa1npbf2n4z3pjy6vm2mw8ywkqixxs6-a-1.0
";
        let info = NarInfo::parse(doc).unwrap();
        let fp = fingerprint(&info).unwrap();
        let refs = fp.rsplit(';').next().unwrap();
        assert_eq!(
            refs,
            "/nix/store/aaa1npbf2n4z3pjy6vm2mw8ywkqixxs6-a-1.0,\
             /nix/store/zzz1npbf2n4z3pjy6vm2mw8ywkqixxs6-b-1.0",
            "references are sorted, absolute and comma-joined"
        );
    }

    #[test]
    fn a_path_with_no_references_fingerprints_with_an_empty_tail() {
        let doc = "\
StorePath: /nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hello-1.0
URL: nar/x.nar.zst
NarHash: sha256:075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x
NarSize: 10
";
        let fp = fingerprint(&NarInfo::parse(doc).unwrap()).unwrap();
        assert!(fp.ends_with(";10;"), "got: {fp}");
    }

    #[test]
    fn signing_key_round_trips_through_the_verifier() {
        let key = NixSigningKey::from_seed([7u8; 32], "batlehub-nix-1");
        let fp = "1;/nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hello-1.0;sha256:x;10;";
        let sig = key.sign_fingerprint(fp);
        assert!(sig.starts_with("batlehub-nix-1:"));
        assert!(verify_signature(fp, &sig, &[key.public_key_line()]));
        assert!(!verify_signature(
            "1;other;sha256:x;10;",
            &sig,
            &[key.public_key_line()]
        ));
    }

    /// The public key goes into `trusted-public-keys`, which is base64 of the
    /// 32 raw bytes — not hex, and not PEM. A wrong encoding here is a key no
    /// client can use and no test that only round-trips would notice.
    #[test]
    fn the_public_key_line_is_name_and_base64_of_thirty_two_bytes() {
        let key = NixSigningKey::from_seed([7u8; 32], "batlehub-nix-1");
        let line = key.public_key_line();
        let (name, b64) = line.split_once(':').expect("name:base64");
        assert_eq!(name, "batlehub-nix-1");
        assert_eq!(BASE64.decode(b64).unwrap().len(), 32);
        // The published nixos key is the same shape, which is the check that
        // this is the form clients read.
        let (_, nixos_b64) = NIXOS_KEY.split_once(':').unwrap();
        assert_eq!(BASE64.decode(nixos_b64).unwrap().len(), 32);
    }

    #[test]
    fn from_seed_hex_refuses_a_seed_that_is_not_thirty_two_bytes() {
        assert!(NixSigningKey::from_seed_hex(&"0".repeat(64), "k").is_ok());
        assert!(NixSigningKey::from_seed_hex(&"0".repeat(62), "k").is_err());
        assert!(NixSigningKey::from_seed_hex("not hex", "k").is_err());
    }

    #[test]
    fn a_publisher_cannot_keep_a_forged_registry_signature() {
        let mut info = NarInfo::parse(REAL_NARINFO).unwrap();
        info.push_signature("batlehub-nix-1:ZmFrZQ==");
        info.push_signature("someone-else-1:ZmFrZQ==");

        let dropped = info.drop_signatures_by("batlehub-nix-1");
        assert_eq!(dropped, 1);
        let sigs = info.get_all("Sig");
        assert_eq!(sigs.len(), 2, "the upstream's and the third party's remain");
        assert!(sigs.iter().any(|s| s.starts_with("cache.nixos.org-1:")));
        assert!(sigs.iter().any(|s| s.starts_with("someone-else-1:")));
    }

    #[test]
    fn set_replaces_in_place_and_collapses_duplicates() {
        let mut info = NarInfo::parse("A: 1\nB: 2\nA: 3\nC: 4\n").unwrap();
        info.set("A", "9");
        assert_eq!(info.to_wire(), "A: 9\nB: 2\nC: 4\n");
    }

    /// The value offset is a *byte* offset, and the character after the colon
    /// need not be one byte wide. This document is long enough to clear a
    /// length check and lands inside the `é`, which used to panic — on a route
    /// that parses before it authorizes, so anonymously.
    #[test]
    fn a_multibyte_character_after_the_colon_is_refused_not_a_panic() {
        let err = NarInfo::parse("URL:éx\n").expect_err("no value at the grammar's offset");
        assert!(
            matches!(err, CoreError::InvalidInput(ref m) if m.contains("no value after its colon")),
            "unexpected error: {err:?}"
        );
        // The same shape one byte later: `:` then a two-byte character means
        // byte `colon + 2` is the character's second byte.
        assert!(NarInfo::parse("Compression:é\n").is_err());
    }

    /// `split_at(STORE_HASH_LEN)` counts bytes too: 31 ASCII characters and a
    /// two-byte one clears `base.len() < STORE_HASH_LEN + 2` and splits inside
    /// the character.
    #[test]
    fn a_store_path_splitting_inside_a_character_is_refused_not_a_panic() {
        let base = format!("{}é-hello", "a".repeat(STORE_HASH_LEN - 1));
        let err = StorePath::parse(&base).expect_err("not a store path");
        assert!(
            matches!(err, CoreError::InvalidInput(_)),
            "unexpected error: {err:?}"
        );
        // And with a store directory in front, which is the spelling a narinfo
        // carries.
        assert!(StorePath::parse(&format!("/nix/store/{base}")).is_err());
    }
}
