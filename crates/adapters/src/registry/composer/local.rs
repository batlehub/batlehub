use bytes::Bytes;

use super::models::{ComposerJson, ComposerPackageMeta};

// ── Publish helpers ───────────────────────────────────────────────────────────

/// Parse `composer.json` from the root of a Composer ZIP artifact.
///
/// The `version` field may be absent in `composer.json` (Packagist injects it),
/// so the caller may supply it via `version_override`.
pub fn parse_composer_zip(
    data: &Bytes,
    version_override: Option<&str>,
) -> anyhow::Result<ComposerPackageMeta> {
    use std::io::Cursor;

    let cursor = Cursor::new(data.as_ref());
    let mut archive = zip::ZipArchive::new(cursor)?;

    // composer.json may live at the root or in a single top-level directory.
    let json_content = find_composer_json(&mut archive)?;

    // Parse once into a generic Value; the typed struct is derived from it.
    let composer_json: serde_json::Value = serde_json::from_str(&json_content)
        .map_err(|e| anyhow::anyhow!("invalid composer.json: {e}"))?;
    let parsed: ComposerJson = serde_json::from_value(composer_json.clone())
        .map_err(|e| anyhow::anyhow!("invalid composer.json fields: {e}"))?;

    let version = version_override
        .map(str::to_owned)
        .or(parsed.version)
        .ok_or_else(|| {
            anyhow::anyhow!("composer.json has no 'version' field and no version was provided")
        })?;

    // Validate name: exactly "vendor/package" with safe characters in each segment.
    // A bare contains('/') check would allow traversal sequences like "a/../../etc".
    let (vendor_seg, rest) = parsed.name.split_once('/').ok_or_else(|| {
        anyhow::anyhow!(
            "composer package name '{}' must be in vendor/package format",
            parsed.name
        )
    })?;
    if rest.contains('/')
        || !is_valid_composer_name_segment(vendor_seg)
        || !is_valid_composer_name_segment(rest)
    {
        anyhow::bail!(
            "composer package name '{}' contains invalid characters or extra path components",
            parsed.name
        );
    }

    Ok(ComposerPackageMeta {
        name: parsed.name,
        version,
        description: parsed.description,
        composer_json,
    })
}

/// Returns true when every character in `s` is alphanumeric, a hyphen, underscore, or dot.
/// Used to validate both Composer package name segments and ZIP path components.
pub(super) fn is_valid_composer_name_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub(super) fn find_composer_json(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
) -> anyhow::Result<String> {
    use std::io::Read;

    // Try root-level composer.json first.
    if let Ok(mut f) = archive.by_name("composer.json") {
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        return Ok(s);
    }

    // Fall back to a single top-level directory (GitHub zipball: vendor-pkg-abc123/composer.json).
    // Collect ALL candidates; if more than one top-level directory contains a composer.json the
    // archive is ambiguous and we reject it rather than silently picking a non-deterministic one.
    let candidates: Vec<String> = archive
        .file_names()
        .filter_map(Result::ok)
        .filter(|n| {
            let mut parts = n.splitn(3, '/');
            parts.next(); // top-level dir
            parts.next() == Some("composer.json") && parts.next().is_none()
        })
        .map(std::borrow::Cow::into_owned)
        .collect();

    let nested = match candidates.len() {
        0 => anyhow::bail!("composer.json not found in ZIP archive"),
        1 => candidates.into_iter().next().expect("invariant: len == 1"),
        _ => anyhow::bail!(
            "ambiguous ZIP: multiple top-level directories contain composer.json ({})",
            candidates.join(", ")
        ),
    };

    let mut f = archive.by_name(&nested)?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}
