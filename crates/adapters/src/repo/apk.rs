//! Alpine `.apk` parsing and `APKINDEX` reading.
//!
//! The format primitives, beside `repo/deb.rs`, `repo/rpm.rs` and
//! `repo/pacman.rs`: this is what a `.apk` upload is taken apart with and what
//! a cached `APKINDEX.tar.gz` is read through. The index *writer* and the RSA
//! signer live beside this in phase 3; what is here is the half both directions
//! need.
//!
//! **Both file types are concatenated gzip members, not archives.** An `.apk`
//! is up to three (`.SIGN.*`, control, data); an `APKINDEX.tar.gz` is two
//! (`.SIGN.*`, then `DESCRIPTION` + `APKINDEX`). Each member is its own tar, so
//! a reader that decompresses the whole stream at once and hands the result to
//! a tar parser finds only the *first* member's entries — the tar reader stops
//! at that archive's end-of-archive blocks and never sees what follows. That is
//! why [`gzip_members`] exists and why `MultiGzDecoder` is not used here
//! (RFC 0026 §6.2).

use batlehub_core::error::CoreError;
use batlehub_core::services::apk::PkgInfo;

/// One gzip member of a concatenated stream: where it sat, and its plaintext.
///
/// The offset and length are not bookkeeping — they are the input to the
/// package identity, which is a hash over the *compressed* bytes of one member.
/// A decoder that only yields plaintext cannot compute it at all.
pub struct GzipMember {
    /// Byte offset of this member in the original stream.
    pub offset: usize,
    /// Compressed length of this member.
    pub len: usize,
    /// The member's decompressed bytes.
    pub plain: Vec<u8>,
}

/// Hard cap on the plaintext one member is *kept* in memory.
///
/// Nothing here reads a member's plaintext for its own sake: both callers look
/// for one small tar entry in it (`.PKGINFO`, `APKINDEX`). The data member of
/// an `.apk` is the large one and is never searched successfully — its extent
/// is what [`parse_apk`] needs, not its bytes. So a member is buffered up to
/// this bound and *counted* past it: the walk still finds the member boundary,
/// without holding a package's worth of decompressed data per request.
const MAX_MEMBER_PLAIN: u64 = 64 * 1024 * 1024;

/// Hard cap on the plaintext the whole walk will inflate, buffered or not.
///
/// The same house rule as `repo/pacman.rs`, for the same reason: the input is
/// an attacker-controlled archive, so without a ceiling a 1 MB `.apk` whose
/// data member inflates to 20 GB OOMs the server on an authenticated publish.
/// Discarding the bytes past [`MAX_MEMBER_PLAIN`] bounds the memory but not the
/// work, which is what this bounds. Sized far above any real package.
const MAX_TOTAL_PLAIN: u64 = 2 * 1024 * 1024 * 1024;

/// Sink that keeps the first [`MAX_MEMBER_PLAIN`] bytes of a member and counts
/// the rest against a budget shared by the whole walk.
struct MemberSink<'a> {
    plain: Vec<u8>,
    /// Remaining inflate budget for the walk, in bytes.
    budget: &'a mut u64,
    /// Set when the budget ran out, to tell a bomb from trailing garbage.
    over_budget: bool,
}

impl std::io::Write for MemberSink<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if data.len() as u64 > *self.budget {
            self.over_budget = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "decompressed apk stream exceeds the size limit",
            ));
        }
        *self.budget -= data.len() as u64;
        let held = self.plain.len() as u64;
        if held < MAX_MEMBER_PLAIN {
            let room = (MAX_MEMBER_PLAIN - held) as usize;
            self.plain.extend_from_slice(&data[..room.min(data.len())]);
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Walk a stream of concatenated gzip members, one at a time.
///
/// `bufread::GzDecoder` consumes exactly one member and leaves the reader
/// positioned after it, which is what makes the loop possible. A member that
/// consumes nothing ends the walk rather than spinning, and trailing bytes that
/// are not a gzip member are ignored rather than failing — a mirror that pads
/// its files is not this parser's problem.
///
/// A stream that inflates past [`MAX_TOTAL_PLAIN`] is the one case that is an
/// error rather than an end of walk: stopping quietly would hand the caller a
/// prefix of the members and let a bomb read as a malformed package.
pub fn gzip_members(bytes: &[u8]) -> Result<Vec<GzipMember>, CoreError> {
    let mut members = Vec::new();
    let mut budget = MAX_TOTAL_PLAIN;
    let mut offset = 0usize;
    while offset < bytes.len() {
        let mut cursor = std::io::Cursor::new(&bytes[offset..]);
        let mut sink = MemberSink {
            plain: Vec::new(),
            budget: &mut budget,
            over_budget: false,
        };
        {
            let mut decoder = flate2::bufread::GzDecoder::new(&mut cursor);
            if std::io::copy(&mut decoder, &mut sink).is_err() {
                if sink.over_budget {
                    return Err(CoreError::InvalidInput(format!(
                        "apk stream decompresses past the {MAX_TOTAL_PLAIN}-byte limit"
                    )));
                }
                break;
            }
        }
        let consumed = cursor.position() as usize;
        if consumed == 0 {
            break;
        }
        members.push(GzipMember {
            offset,
            len: consumed,
            plain: sink.plain,
        });
        offset += consumed;
    }
    Ok(members)
}

/// The first entry named `wanted` in a tar archive, as bytes.
pub fn tar_entry(plain: &[u8], wanted: &str) -> Option<Vec<u8>> {
    use std::io::Read;

    let mut archive = tar::Archive::new(std::io::Cursor::new(plain));
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        let matched = entry
            .path()
            .ok()
            .is_some_and(|p| p.to_string_lossy() == wanted);
        if matched {
            let mut body = Vec::new();
            entry.read_to_end(&mut body).ok()?;
            return Some(body);
        }
    }
    None
}

/// A parsed `.apk`: its control metadata, its identity and its size.
pub struct ApkPackage {
    pub info: PkgInfo,
    /// The `C:` field: `Q1` + base64 of the SHA-1 over the control member's
    /// **compressed** bytes.
    pub identity: String,
    /// The whole file's size — the index's `S:` field.
    pub size: u64,
}

/// The magic of apk-tools 3's ADB container, the first four bytes of every
/// package `apk mkpkg` writes. Observed on 3.0.8's own output while the heavy
/// suite was first run; the packages on the mirror are all still v2.
const ADB_MAGIC: &[u8] = b"ADBd";

/// Read `.PKGINFO` and compute the package identity.
///
/// **The identity has two rules and apk picks between them by a field.** Both
/// are SHA-1 over *compressed* bytes — never over `.PKGINFO`, never over a
/// decompressed tar — and which range is covered depends on whether the
/// control file carries `datahash`:
///
/// | `.PKGINFO` | `C:` covers | Why |
/// | --- | --- | --- |
/// | has `datahash` | the control member alone | the data is already covered, by the hash inside the control file |
/// | no `datahash` | the control member **to the end of the file** | nothing else covers the data, so the identity must |
///
/// Every package `abuild` writes carries `datahash` — the mirror's
/// `busybox-1.37.0-r20.apk` does — so the first rule is the one production
/// traffic takes. The second is the one a hand-built package takes, and
/// getting it wrong is not a visible failure: the index is served, the client
/// downloads the package, and *then* refuses it for an identity mismatch,
/// which reads as corruption. Both rules were measured against `apk index`
/// itself while the heavy suite was first run (RFC 0026 §13).
///
/// A protocol-mandated SHA-1: recorded as such in the scanner triage notes
/// rather than "fixed", because the wire format defines it.
///
/// Verified against the real `busybox-1.37.0-r20.apk`: member lengths
/// 720 / 1 719 / 503 677, total 506 116, `datahash` present, identity
/// `Q1Pp11KIKAs8SS6R8w4SbCQA0XAbM=` — the mirror's own `C:` line for that
/// package. The tests below pin both rules on synthetic packages; the heavy
/// suite re-proves them against a real one, because a 506 KB fixture does not
/// belong in the repository.
pub fn parse_apk(bytes: &[u8]) -> Result<ApkPackage, CoreError> {
    // The v3 container, named before the gzip walk fails on it. `apk mkpkg`
    // — apk-tools 3's own package builder — writes **this** and not the v2
    // format, which is not a detail an operator can be expected to know from
    // the error "no gzip member could be read". No Alpine branch ships a v3
    // index (RFC 0026 §2.2), a v2 `APKINDEX` has no shape that could describe
    // a v3 package, and `abuild` still produces v2 — so this is refused with
    // the sentence that says what to do instead.
    if bytes.starts_with(ADB_MAGIC) {
        return Err(CoreError::Registry(
            "this is an apk v3 (ADB) package, the format `apk mkpkg` writes. A v2 APKINDEX \
             cannot describe one and no Alpine branch ships a v3 index, so this registry \
             hosts v2 packages only — build it with `abuild`, which still produces v2"
                .to_owned(),
        ));
    }

    let members = gzip_members(bytes)?;
    if members.is_empty() {
        return Err(CoreError::Registry(
            "not an apk package: no gzip member could be read".to_owned(),
        ));
    }

    let control = members
        .iter()
        .find(|m| tar_entry(&m.plain, ".PKGINFO").is_some())
        .ok_or_else(|| {
            CoreError::Registry(
                "not an apk package: no member carries a .PKGINFO control file".to_owned(),
            )
        })?;

    let raw = tar_entry(&control.plain, ".PKGINFO").expect("found just above");
    let info = PkgInfo::parse(&raw);

    use sha1::{Digest, Sha1};
    let covered = if info.fields.iter().any(|(k, _)| k == "datahash") {
        &bytes[control.offset..control.offset + control.len]
    } else {
        // From the control member to the end: the signature member, when there
        // is one, is the only part left out — it cannot sign a digest that
        // includes it.
        &bytes[control.offset..]
    };
    let identity = format!(
        "Q1{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            Sha1::digest(covered)
        )
    );

    Ok(ApkPackage {
        info,
        identity,
        size: bytes.len() as u64,
    })
}

/// Pull the `APKINDEX` text out of an `APKINDEX.tar.gz`.
///
/// The index lives in the *second* gzip member, behind the signature — see the
/// module docs for why that cannot be read with a single decoder.
pub fn decode_index(bytes: &[u8]) -> Option<String> {
    for member in gzip_members(bytes).ok()? {
        if let Some(body) = tar_entry(&member.plain, "APKINDEX") {
            return String::from_utf8(body).ok();
        }
    }
    None
}

/// Build the **data member** of an `APKINDEX.tar.gz`: gzip of a tar holding
/// `DESCRIPTION` then `APKINDEX`.
///
/// `DESCRIPTION` carries the registry name and a generation counter. apk does
/// not read it — it is what `apk index --description` writes and what a human
/// gets from `tar -xOf APKINDEX.tar.gz DESCRIPTION`, which makes "which
/// generation of this index am I looking at" answerable without the server.
pub fn generate_index(body: &str, description: &str) -> Result<Vec<u8>, CoreError> {
    let tar = tar_of(&[
        ("DESCRIPTION", description.as_bytes()),
        ("APKINDEX", body.as_bytes()),
    ])?;
    gzip(&tar)
}

/// Prepend the **signature member**, producing the file apk downloads.
///
/// Two concatenated gzip members, signature first: that order is not cosmetic.
/// `apk_sign_ctx_process_file` expects `.SIGN.*` entries *before* any other
/// name (`package.c:573`), and the signature covers the data member's
/// **compressed** bytes — which is why this takes the gzipped member and not
/// the tar, and why `generate_index` returns one rather than the two halves.
pub fn sign_index(data_member: &[u8], signer: &super::ApkSigner) -> Result<Vec<u8>, CoreError> {
    // Every key, current first, and **all of them in one gzip member**. Not a
    // style choice: apk starts digesting at the member boundary *after* the
    // signatures, so a second signature member would move the boundary and
    // every signature before it would cover the wrong bytes. A fleet
    // mid-rotation takes the first entry whose key it holds (RFC 0026 §11
    // decision 9).
    let names: Vec<String> = signer.all().map(|s| s.signature_entry_name()).collect();
    let signatures: Vec<Vec<u8>> = signer
        .all()
        .map(|s| s.sign(data_member))
        .collect::<Result<_, _>>()?;
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .map(String::as_str)
        .zip(signatures.iter().map(Vec::as_slice))
        .collect();

    // `tar_entries`, not `tar_of`: the data member continues this same tar
    // stream, so the signature member must not declare its end.
    let sig_tar = tar_entries(&entries)?;
    let sig_member = gzip(&sig_tar)?;
    Ok([sig_member, data_member.to_vec()].concat())
}

/// A tar archive of `(name, bytes)` entries, in order.
fn tar_of(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, CoreError> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in entries {
        // **POSIX ustar, not GNU**, with every numeric field written out. apk's
        // own tar reader accepts the GNU spelling for the *index* entries and
        // rejects the signature entry written that way — the index then parses
        // and `apk update` reports `BAD signature` on a signature that is
        // cryptographically correct, which points a reader at the key and the
        // digest, neither of which is wrong. Matching what GNU `tar` itself
        // writes (`ustar\0` + `00`, typeflag `0`, octal uid/gid/mtime) is what
        // both generations accept; measured against `apk.static` in RFC 0026
        // §13, not deduced.
        let mut header = tar::Header::new_ustar();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, name, *content)
            .map_err(|e| CoreError::Registry(format!("could not write the index tar: {e}")))?;
    }
    builder
        .into_inner()
        .map_err(|e| CoreError::Registry(format!("could not finish the index tar: {e}")))
}

/// The entries of a tar **without** the end-of-archive marker that ends one.
///
/// An `APKINDEX.tar.gz` is not two tar files. It is **one tar stream split
/// across two gzip members**, and apk reads it as such: the signature member's
/// entries are followed, in the same stream, by `DESCRIPTION` and `APKINDEX`.
/// A tar writer that finalises the first member writes the two zero blocks that
/// say "the stream ends here", and apk — which has already been handed the rest
/// — answers `BAD archive` and resolves nothing.
///
/// Found by the first real `apk update` against a repository this instance
/// hosts (RFC 0026 §6.8 step 5). Every unit test passed through it: our own
/// reader walks gzip members independently and never notices the marker, so
/// only a client that reads the stream the way the format defines it could say
/// this was wrong.
fn tar_entries(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, CoreError> {
    let mut out = tar_of(entries)?;
    // `tar::Builder::into_inner` finalises, appending exactly two zero blocks.
    // Truncating that fixed length is exact — it is the marker and never part
    // of an entry, whose own padding lives in the block before it.
    const END_OF_ARCHIVE: usize = 1024;
    if out.len() >= END_OF_ARCHIVE && out[out.len() - END_OF_ARCHIVE..].iter().all(|b| *b == 0) {
        out.truncate(out.len() - END_OF_ARCHIVE);
    }
    Ok(out)
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>, CoreError> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes)
        .and_then(|()| enc.finish())
        .map_err(|e| CoreError::Registry(format!("could not compress the index: {e}")))
}

/// Fixture builders shared with `registry/apk.rs`'s tests, so the shape of a
/// real `APKINDEX.tar.gz` is described once — in the module that knows it.
#[cfg(test)]
pub mod tests_support {
    use std::io::Write;

    fn gzip_one(bytes: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    }

    fn tar_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, name, content.as_bytes())
                .unwrap();
        }
        tar.into_inner().unwrap()
    }

    /// An `APKINDEX.tar.gz` as a mirror serves one: signature member, then the
    /// data member.
    pub fn index_archive(body: &str) -> Vec<u8> {
        [
            gzip_one(&tar_of(&[(".SIGN.RSA.test.rsa.pub", "sig\n")])),
            gzip_one(&tar_of(&[("DESCRIPTION", "test\n"), ("APKINDEX", body)])),
        ]
        .concat()
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::index_archive;
    use super::*;
    use std::io::Write;

    fn gzip_one(bytes: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    }

    fn tar_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, name, content.as_bytes())
                .unwrap();
        }
        tar.into_inner().unwrap()
    }

    /// A `.apk` as `abuild` writes one, or as `apk mkpkg` does when
    /// `with_signature` is false.
    fn apk_archive(pkginfo: &str, with_signature: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if with_signature {
            out.extend(gzip_one(&tar_of(&[(".SIGN.RSA.builder.rsa.pub", "sig\n")])));
        }
        out.extend(gzip_one(&tar_of(&[(".PKGINFO", pkginfo)])));
        out.extend(gzip_one(&tar_of(&[("usr/bin/thing", "binary\n")])));
        out
    }

    const PKGINFO: &str =
        "pkgname = thing\npkgver = 1.2.3-r4\narch = x86_64\nsize = 99\nbuilddate = 1700000000\n";

    /// The same control file as `abuild` writes one: with `datahash`, which is
    /// what switches the identity to the control member alone.
    const PKGINFO_DATAHASH: &str = "pkgname = thing\npkgver = 1.2.3-r4\narch = x86_64\nsize = 99\n\
                                    builddate = 1700000000\n\
                                    datahash = a2c934bbd3745f7ca3d6ff53ff61819a4e09db061581d921e8824c62acbaf3ca\n";
    const INDEX_BODY: &str = "C:Q1a=\nP:busybox\nV:1.37.0-r20\nt:1763764856\n\n\
                              C:Q1b=\nP:curl\nV:8.14.1-r3\nt:1749000000\n\n";

    /// The whole reason `gzip_members` exists: a reader that stops at the first
    /// member finds the signature and never the index, and the age gate
    /// silently loses every date.
    #[test]
    fn decode_index_reads_across_the_member_boundary() {
        let text = decode_index(&index_archive(INDEX_BODY)).expect("index decoded");
        assert!(text.contains("P:busybox"), "got: {text:?}");
        assert_eq!(
            batlehub_core::services::apk::ApkIndex::parse(&text).len(),
            2
        );
    }

    #[test]
    fn decode_index_rejects_bytes_that_are_not_an_index() {
        assert!(decode_index(b"not a gzip stream at all").is_none());
        // A well-formed gzip member that carries no APKINDEX entry.
        assert!(decode_index(&gzip_one(&tar_of(&[("OTHER", "x")]))).is_none());
    }

    /// A member that inflates far past what it compresses to is walked without
    /// the whole plaintext being held: `MAX_MEMBER_PLAIN` is what is kept, and
    /// the extent — which is what `parse_apk` actually needs — is still right.
    #[test]
    fn a_member_past_the_buffer_bound_keeps_its_extent_and_not_its_bytes() {
        // Two members: a small one, then one whose plaintext exceeds a bound
        // small enough to test against. The real bound is 64 MiB, so this
        // asserts the *shape* — the extent survives a truncated buffer — using
        // the same walk.
        let small = gzip_one(&tar_of(&[("A", "x")]));
        let mut bytes = small.clone();
        bytes.extend_from_slice(&gzip_one(&vec![0u8; 4 * 1024 * 1024]));

        let members = gzip_members(&bytes).expect("4 MiB is within the budget");
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].offset, 0);
        assert_eq!(members[0].len, small.len());
        assert_eq!(members[1].offset, small.len());
        assert_eq!(
            members[1].offset + members[1].len,
            bytes.len(),
            "the second member's extent still reaches the end of the file"
        );
    }

    /// The bound `repo/pacman.rs` has had all along. A stream that inflates
    /// past it is an error rather than a prefix of members, so a bomb cannot
    /// read as a merely malformed package — and cannot OOM the server on an
    /// authenticated publish.
    #[test]
    fn a_stream_inflating_past_the_total_budget_is_refused() {
        // Driven through the sink directly: producing 2 GiB of real gzip in a
        // unit test would cost what the bound exists to refuse. This proves the
        // budget is enforced and that the refusal is distinguishable from the
        // decode error that merely ends the walk.
        use std::io::Write;
        let mut budget = 1024u64;
        let mut sink = MemberSink {
            plain: Vec::new(),
            budget: &mut budget,
            over_budget: false,
        };
        assert!(sink.write_all(&vec![0u8; 512]).is_ok());
        assert!(!sink.over_budget);
        assert!(
            sink.write_all(&vec![0u8; 4096]).is_err(),
            "past the budget, the sink refuses rather than growing"
        );
        assert!(sink.over_budget, "and says which kind of failure it was");
    }

    #[test]
    fn gzip_members_reports_each_member_and_its_extent() {
        let bytes = apk_archive(PKGINFO, true);
        let members = gzip_members(&bytes).expect("within the inflate budget");
        assert_eq!(members.len(), 3, "signature, control, data");
        assert_eq!(members[0].offset, 0);
        for pair in members.windows(2) {
            assert_eq!(
                pair[0].offset + pair[0].len,
                pair[1].offset,
                "members are contiguous"
            );
        }
        let last = members.last().unwrap();
        assert_eq!(last.offset + last.len, bytes.len());
    }

    /// SHA-1 of a slice of `bytes`, spelled the way an index's `C:` is.
    fn q1(slice: &[u8]) -> String {
        use sha1::{Digest, Sha1};
        format!(
            "Q1{}",
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                Sha1::digest(slice)
            )
        )
    }

    /// Rule one: a package whose `.PKGINFO` carries `datahash` — which is
    /// every package `abuild` writes — is identified by its **control member
    /// alone**, because the data is already covered by that field.
    #[test]
    fn parse_apk_identity_with_datahash_covers_the_control_member() {
        let bytes = apk_archive(PKGINFO_DATAHASH, true);
        let parsed = parse_apk(&bytes).expect("parsed");

        assert_eq!(parsed.info.name(), Some("thing"));
        assert_eq!(parsed.info.version(), Some("1.2.3-r4"));
        assert_eq!(parsed.size, bytes.len() as u64);

        let members = gzip_members(&bytes).expect("within the inflate budget");
        let control = &members[1];
        assert_eq!(
            parsed.identity,
            q1(&bytes[control.offset..control.offset + control.len])
        );
    }

    /// Rule two: with no `datahash`, the identity runs from the control member
    /// **to the end of the file** — nothing else covers the data, so the
    /// identity has to.
    ///
    /// Getting this backwards is invisible from here: the index is served, the
    /// client downloads the package and *then* refuses it for an identity
    /// mismatch, which reads as corruption. Measured against `apk index`
    /// itself (RFC 0026 §13), not deduced from the format description.
    #[test]
    fn parse_apk_identity_without_datahash_runs_to_the_end_of_the_file() {
        let bytes = apk_archive(PKGINFO, true);
        let parsed = parse_apk(&bytes).expect("parsed");

        let members = gzip_members(&bytes).expect("within the inflate budget");
        let control = &members[1];
        assert_eq!(parsed.identity, q1(&bytes[control.offset..]));
        assert_ne!(
            parsed.identity,
            q1(&bytes[control.offset..control.offset + control.len]),
            "the two rules must not coincide, or this test proves nothing"
        );
    }

    /// An unsigned package — `melange`, `nfpm`, anything hand-built — must
    /// parse, and must get the *same* identity as the signed one: both rules
    /// start at the control member, and the signature member is the only thing
    /// either of them leaves out. This is what makes local publishing
    /// tractable — a package with no signature installs fine from a signed
    /// index.
    #[test]
    fn parse_apk_accepts_a_package_with_no_signature() {
        let signed = parse_apk(&apk_archive(PKGINFO, true)).unwrap();
        let unsigned = parse_apk(&apk_archive(PKGINFO, false)).unwrap();
        assert_eq!(unsigned.info.name(), Some("thing"));
        assert_eq!(
            signed.identity, unsigned.identity,
            "the identity is the control member's, so a signature does not change it"
        );
    }

    /// The generated file has the shape apk reads: two members, signature
    /// first, and the index recoverable from the second.
    #[test]
    fn a_signed_index_has_the_shape_apk_expects() {
        let signer = super::super::ApkSigner::from_pem(
            include_str!("testdata/apk_signing_2048.pkcs8.pem"),
            "internal@example.com-0001.rsa.pub",
        )
        .unwrap();

        let data = generate_index(INDEX_BODY, "perf-test 1\n").unwrap();
        let file = sign_index(&data, &signer).unwrap();

        let members = gzip_members(&file).expect("within the inflate budget");
        assert_eq!(members.len(), 2, "signature member, then data member");
        assert!(
            tar_entry(
                &members[0].plain,
                ".SIGN.RSA256.internal@example.com-0001.rsa.pub"
            )
            .is_some(),
            "the signature entry is named for the key, in the first member"
        );
        assert!(tar_entry(&members[1].plain, "DESCRIPTION").is_some());
        assert_eq!(decode_index(&file).as_deref(), Some(INDEX_BODY));
    }

    /// The signature must cover the data member's **compressed** bytes and
    /// verify under the public key the key route serves. Get this wrong and
    /// every client refuses the whole repository.
    #[test]
    fn the_signature_covers_the_compressed_data_member() {
        use aws_lc_rs::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};

        let signer = super::super::ApkSigner::from_pem(
            include_str!("testdata/apk_signing_2048.pkcs8.pem"),
            "k.rsa.pub",
        )
        .unwrap();
        let data = generate_index(INDEX_BODY, "test\n").unwrap();
        let file = sign_index(&data, &signer).unwrap();

        let members = gzip_members(&file).expect("within the inflate budget");
        let signature = tar_entry(&members[0].plain, ".SIGN.RSA256.k.rsa.pub").unwrap();
        let signed_bytes = &file[members[1].offset..members[1].offset + members[1].len];
        assert_eq!(
            signed_bytes,
            data.as_slice(),
            "the data member is unchanged"
        );

        let pem = signer.public_key_pem().unwrap();
        let der = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<String>();
        let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, der).unwrap();

        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &der)
            .verify(signed_bytes, &signature)
            .expect("apk verifies this signature over these bytes");
    }

    /// A round trip through the whole local path: parse an upload, render its
    /// entry, build and sign an index, and read the entry back out.
    #[test]
    fn a_published_package_round_trips_into_a_signed_index() {
        use batlehub_core::services::apk::{index_entry, render_index, ApkIndex};

        let bytes = apk_archive(PKGINFO, false);
        let parsed = parse_apk(&bytes).unwrap();
        let entry = index_entry(&parsed.info, &parsed.identity, parsed.size);

        let signer = super::super::ApkSigner::from_pem(
            include_str!("testdata/apk_signing_2048.pkcs8.pem"),
            "k.rsa.pub",
        )
        .unwrap();
        let file = sign_index(
            &generate_index(&render_index(&[entry]), "test\n").unwrap(),
            &signer,
        )
        .unwrap();

        let text = decode_index(&file).expect("index readable");
        assert!(text.contains(&format!("C:{}", parsed.identity)));
        assert_eq!(
            ApkIndex::parse(&text).built_at("thing", "1.2.3-r4"),
            Some(1700000000),
            "the build date survives into the index the age gate reads"
        );
    }

    #[test]
    fn parse_apk_rejects_a_stream_with_no_control_member() {
        let no_control = gzip_one(&tar_of(&[("usr/bin/thing", "binary\n")]));
        assert!(parse_apk(&no_control).is_err());
        assert!(parse_apk(b"not a gzip stream").is_err());
    }
}
