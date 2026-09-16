//! What every protocol module needs: the deterministic artifact bytes, the
//! digests a wire format names, and the upstream latency knob.

use actix_web::HttpRequest;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::time::Duration;

/// The address the proxy reached this mock on, for the self-referential URLs a
/// registry document has to carry — a packument's tarball, the sparse index's
/// `dl`, a Composer `dist.url`.
///
/// `disallowed_methods` forbids `connection_info()` in the server, where
/// believing `X-Forwarded-Host` from any peer decides the URLs it advertises.
/// Here it is the correct call and the only one available: a test double on
/// loopback with nothing to spoof, which cannot know its own address any other
/// way. Allowed once, with the reason, rather than at eleven call sites — this
/// crate is its own workspace, so `cargo clippy --workspace` at the root never
/// sees it.
#[allow(clippy::disallowed_methods)]
pub fn host(req: &HttpRequest) -> String {
    req.connection_info().host().to_string()
}

pub async fn delay(ms: u64) {
    if ms > 0 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

/// An artifact's bytes: incompressible, and **the same every time** for a
/// coordinate.
///
/// It used to be `rand::thread_rng()`, which made every fetch of the same
/// tarball a different file. That is wrong twice over. The obvious way is that
/// the packument cannot then advertise a digest, and the proxy verifies the
/// one it is given: every artifact read answered 502 and every scenario that
/// checked for a 200 had been failing since integrity checking landed. The
/// subtler way is that a cache is supposed to return the bytes it stored, and
/// an upstream whose answer changes per request makes "the same artifact"
/// meaningless — a cache hit and a cache miss would be distinguishable by
/// content, which is not a property any real registry has.
///
/// An xorshift seeded from the coordinate rather than a hash chain: this runs
/// on the packument path too, where it is the thing being timed, and the bytes
/// only have to be reproducible and not compress away — not unpredictable.
pub fn artifact_bytes(name: &str, version: &str, size: usize) -> Vec<u8> {
    let mut state = 0xcbf2_9ce4_8422_2325u64; // FNV-1a offset basis
    for byte in name
        .bytes()
        .chain(b"@".iter().copied())
        .chain(version.bytes())
    {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x1000_0000_01b3);
    }
    state |= 1; // xorshift is degenerate from zero

    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        buf.extend_from_slice(&state.to_le_bytes());
    }
    buf.truncate(size);
    buf
}

/// The hex sha1 of `bytes` — the algorithm npm's `dist.shasum` names.
pub fn sha1_hex(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes); // NOSONAR -- required by legacy `dist.shasum` wire formats
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hex sha256 of `bytes` — what a RubyGems compact-index `|checksum:` is.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
