use batlehub_core::ports::ExtractedManifest;
use bytes::Bytes;

use super::readme;

/// A chart archive wraps everything in one `{chart}/` directory, and the
/// chart's README is `{chart}/README.md` — the file `helm show readme` prints.
/// A subchart under `{chart}/charts/` has its own, which is one level too deep
/// to be mistaken for it (RFC 0029 §6.1).
pub(super) fn extract_helm_manifest(data: &Bytes) -> ExtractedManifest {
    ExtractedManifest {
        readme: readme::readme_from_targz(data, readme::root_readme_matcher(1)),
        ..ExtractedManifest::default()
    }
}

#[cfg(test)]
mod tests {
    use super::super::readme::fixtures::targz;
    use super::*;

    #[test]
    fn the_chart_root_readme_is_read_and_a_subcharts_is_not() {
        let data = targz(&[
            (
                "mychart/charts/dep/README.md",
                b"# the dependency".as_slice(),
            ),
            (
                "mychart/Chart.yaml",
                b"name: mychart\nversion: 0.1.0\n".as_slice(),
            ),
            ("mychart/README.md", b"# mychart".as_slice()),
        ]);
        assert_eq!(
            extract_helm_manifest(&data).readme.unwrap().content,
            "# mychart"
        );
    }
}
