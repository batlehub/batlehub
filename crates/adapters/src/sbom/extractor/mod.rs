use bytes::Bytes;

use batlehub_core::ports::ExtractedManifest;

mod anchor;
mod cargo;
mod composer;
mod conda;
mod galaxy;
mod goproxy;
mod maven;
mod npm;
mod nuget;
mod pypi;
mod readme;
mod rubygems;
mod terraform;

/// Archive-based SBOM manifest extractor.
///
/// Parses dependency manifests embedded in package archives, and the licence
/// the same manifest declares (RFC 0004-bis §13.1).
/// Requires the `sbom` feature (which enables flate2, tar, zip, quick-xml).
pub struct ArchiveSbomExtractor;

impl batlehub_core::ports::SbomExtractor for ArchiveSbomExtractor {
    fn extract(&self, data: &Bytes, registry_type: &str) -> ExtractedManifest {
        match registry_type {
            "cargo" => cargo::extract_cargo_manifest(data),
            "npm" => npm::extract_npm_manifest(data),
            "maven" => maven::extract_maven_manifest(data),
            "pypi" => pypi::extract_pypi_manifest(data),
            "nuget" => nuget::extract_nuget_manifest(data),
            // The five below answer only for the README: their archives carry
            // no manifest this reads dependencies or a licence out of, which is
            // why `README_EXTRACTION_TYPES` and `LICENSE_EXTRACTION_TYPES` are
            // different lists (RFC 0007 §5.2).
            "goproxy" => goproxy::extract_goproxy_manifest(data),
            "composer" => composer::extract_composer_manifest(data),
            "terraform" => terraform::extract_terraform_manifest(data),
            "conda" => conda::extract_conda_manifest(data),
            "rubygems" => rubygems::extract_rubygems_manifest(data),
            "galaxy" => galaxy::extract_galaxy_manifest(data),
            // The remaining registry types have no parser at all, so they report
            // an unknown licence rather than an absent one — which is why
            // `license_gate.allow_unknown` defaults to true — and no README.
            _ => ExtractedManifest::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use batlehub_core::ports::SbomExtractor;

    use super::*;

    #[test]
    fn extract_returns_empty_for_unknown_type() {
        let extractor = ArchiveSbomExtractor;
        let data = Bytes::from_static(b"not an archive");
        assert_eq!(
            extractor.extract(&data, "unknown"),
            ExtractedManifest::default()
        );
    }

    /// The README half of the same contract.
    ///
    /// A different list from the licence one, and deliberately so: five kinds
    /// answer here and nowhere else, because their archives carry a README and
    /// no machine-readable manifest. A type listed with no parser would make the
    /// published support table claim coverage dispatch cannot deliver, which is
    /// the failure RFC 0009 was written about.
    #[test]
    fn dispatch_matches_the_declared_readme_types() {
        let mut dispatched = [
            "cargo",
            "npm",
            "pypi",
            "nuget",
            "goproxy",
            "composer",
            "terraform",
            "conda",
            "rubygems",
            "galaxy",
        ];
        dispatched.sort_unstable();
        let mut declared = batlehub_core::ports::README_EXTRACTION_TYPES.to_vec();
        declared.sort_unstable();
        assert_eq!(
            dispatched.as_slice(),
            declared.as_slice(),
            "update README_EXTRACTION_TYPES when adding or removing a README parser"
        );
    }

    /// The adapter's list and the user-facing `readme_support()` answer the same
    /// question from two sides, and an operator reads the second one. A kind
    /// whose support says the archive is read must have a parser here, and a
    /// parser here must belong to a kind that says so.
    #[test]
    fn readme_support_matches_the_extractors() {
        use batlehub_core::entities::RegistryKind;

        for kind in RegistryKind::ALL {
            let reads_archive = kind.readme_support().reads_the_archive();
            let has_parser = batlehub_core::ports::README_EXTRACTION_TYPES.contains(&kind.as_str());
            assert_eq!(
                reads_archive, has_parser,
                "{kind}: readme_support() says reads_the_archive = {reads_archive}, \
                 but README_EXTRACTION_TYPES says {has_parser}"
            );
        }
    }

    /// The dispatch above and `LICENSE_EXTRACTION_TYPES` must not drift.
    ///
    /// The config warning that tells an operator their `license_gate` cannot
    /// see a licence is derived from the const, so a parser added here without
    /// updating it warns about a registry type that now works — and a type
    /// added there without a parser silences a warning that is still true.
    /// Either way the operator is told something false about their policy,
    /// which is the failure mode RFC 0004-bis exists to stop.
    #[test]
    fn dispatch_matches_the_declared_extraction_types() {
        // Mirror of the match arms, in the same order as the const.
        let dispatched = ["cargo", "maven", "npm", "nuget", "pypi"];
        assert_eq!(
            dispatched.as_slice(),
            batlehub_core::ports::LICENSE_EXTRACTION_TYPES,
            "update LICENSE_EXTRACTION_TYPES when adding or removing a parser"
        );
    }
}
