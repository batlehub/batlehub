use super::*;
use batlehub_core::error::CoreError;
use batlehub_core::ports::RegistryClient;

#[tokio::test]
async fn resolve_metadata_repodata_returns_download_url() {
    let mut server = mockito::Server::new_async().await;
    let repodata = serde_json::json!({
        "packages": {
            "numpy-1.26.0-py311h0.tar.bz2": {
                "name": "numpy",
                "version": "1.26.0",
                "build": "py311h0",
                "sha256": "deadbeef"
            }
        },
        "packages.conda": {}
    });
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(repodata.to_string())
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();

    let pkg = PackageId::new("my-conda", "numpy-1.26.0-py311h0", "linux-64")
        .with_artifact("numpy-1.26.0-py311h0.tar.bz2");
    let meta = client.resolve_metadata(&pkg).await.unwrap();
    assert_eq!(meta.checksum.as_deref(), Some("deadbeef"));
    assert!(meta
        .download_url
        .as_deref()
        .unwrap()
        .ends_with("linux-64/numpy-1.26.0-py311h0.tar.bz2"));
}

/// The proxy route files a package under its name and version and puts the
/// subdir in the selector; the client reads the platform from there.
#[tokio::test]
async fn resolve_metadata_repodata_returns_download_url_under_the_proxy_routes_coordinate() {
    let mut server = mockito::Server::new_async().await;
    let repodata = serde_json::json!({
        "packages": {
            "numpy-1.26.0-py311h0.tar.bz2": {
                "name": "numpy",
                "version": "1.26.0",
                "build": "py311h0",
                "sha256": "deadbeef"
            }
        },
        "packages.conda": {}
    });
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(repodata.to_string())
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();

    let pkg = PackageId::new("my-conda", "numpy", "1.26.0")
        .with_artifact("linux-64/numpy-1.26.0-py311h0.tar.bz2");
    let meta = client.resolve_metadata(&pkg).await.unwrap();
    assert_eq!(meta.checksum.as_deref(), Some("deadbeef"));
    assert!(meta
        .download_url
        .as_deref()
        .unwrap()
        .ends_with("linux-64/numpy-1.26.0-py311h0.tar.bz2"));
}

#[tokio::test]
async fn list_versions_aggregates_across_platforms() {
    let mut server = mockito::Server::new_async().await;

    // noarch: numpy 1.26.0
    let noarch = serde_json::json!({
        "packages": {
            "numpy-1.26.0-pyhd8ed1ab_0.tar.bz2": { "name": "numpy", "version": "1.26.0", "build": "pyhd8ed1ab_0" }
        },
        "packages.conda": {}
    });
    // linux-64: numpy 1.26.0 and 1.25.2 (a binary build + older version)
    let linux64 = serde_json::json!({
        "packages": {},
        "packages.conda": {
            "numpy-1.26.0-py311h0_0.conda": { "name": "numpy", "version": "1.26.0", "build": "py311h0_0" },
            "numpy-1.25.2-py311h0_0.conda": { "name": "numpy", "version": "1.25.2", "build": "py311h0_0" }
        }
    });
    let _m1 = server
        .mock("GET", "/noarch/repodata.json")
        .with_status(200)
        .with_body(noarch.to_string())
        .create_async()
        .await;
    let _m2 = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_body(linux64.to_string())
        .create_async()
        .await;
    // Other platforms return 404 — should be silently skipped
    let _m3 = server
        .mock("GET", "/osx-64/repodata.json")
        .with_status(404)
        .create_async()
        .await;
    let _m4 = server
        .mock("GET", "/osx-arm64/repodata.json")
        .with_status(404)
        .create_async()
        .await;
    let _m5 = server
        .mock("GET", "/win-64/repodata.json")
        .with_status(404)
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let versions = client.list_versions("numpy").await.unwrap();

    // Sorted, deduplicated: 1.25.2 before 1.26.0 (lexicographic)
    assert_eq!(versions, vec!["1.25.2", "1.26.0"]);
}

#[tokio::test]
async fn list_versions_returns_empty_for_unknown_package() {
    let mut server = mockito::Server::new_async().await;
    let repodata = serde_json::json!({ "packages": {}, "packages.conda": {} });
    for platform in ["noarch", "linux-64", "osx-64", "osx-arm64", "win-64"] {
        server
            .mock("GET", &*format!("/{platform}/repodata.json"))
            .with_status(200)
            .with_body(repodata.to_string())
            .create_async()
            .await;
    }
    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let versions = client.list_versions("nonexistent-pkg").await.unwrap();
    assert!(versions.is_empty());
}

#[tokio::test]
async fn resolve_metadata_parses_timestamp() {
    let mut server = mockito::Server::new_async().await;
    // timestamp = 1697145600000 ms → 2023-10-12T20:00:00Z
    let repodata = serde_json::json!({
        "packages": {
            "numpy-1.26.0-py311h0.tar.bz2": {
                "name": "numpy",
                "version": "1.26.0",
                "build": "py311h0",
                "sha256": "abc",
                "timestamp": 1697145600000i64
            }
        },
        "packages.conda": {}
    });
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_body(repodata.to_string())
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("reg", "numpy-1.26.0-py311h0", "linux-64")
        .with_artifact("numpy-1.26.0-py311h0.tar.bz2");
    let meta = client.resolve_metadata(&pkg).await.unwrap();
    assert!(
        meta.published_at.is_some(),
        "published_at should be parsed from timestamp"
    );
    let ts = meta.published_at.unwrap();
    assert_eq!(ts.timestamp(), 1697145600);
}

#[tokio::test]
async fn resolve_metadata_no_timestamp_gives_none() {
    let mut server = mockito::Server::new_async().await;
    let repodata = serde_json::json!({
        "packages": {
            "bzip2-1.0.8-h5.tar.bz2": {
                "name": "bzip2",
                "version": "1.0.8",
                "build": "h5",
                "sha256": "def"
            }
        },
        "packages.conda": {}
    });
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_body(repodata.to_string())
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let pkg =
        PackageId::new("reg", "bzip2-1.0.8-h5", "linux-64").with_artifact("bzip2-1.0.8-h5.tar.bz2");
    let meta = client.resolve_metadata(&pkg).await.unwrap();
    assert!(
        meta.published_at.is_none(),
        "published_at should be None when timestamp is absent"
    );
}

#[tokio::test]
async fn repodata_404_returns_not_found() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(404)
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("reg", "repodata", "linux-64");
    let err = client.resolve_metadata(&pkg).await.unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[cfg(feature = "local-registry")]
#[test]
fn parse_conda_metadata_tar_bz2() {
    use bzip2::write::BzEncoder;
    use bzip2::Compression;
    use std::io::Write;

    let index_json = serde_json::json!({
        "name": "test-pkg",
        "version": "1.0.0",
        "build": "py311h0_0",
        "build_number": 0,
        "depends": ["python >=3.11"],
        "subdir": "linux-64"
    });
    let index_bytes = serde_json::to_vec(&index_json).unwrap();

    // Build a tar archive
    let mut tar_bytes = Vec::new();
    {
        let mut tar_builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(index_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "info/index.json", index_bytes.as_slice())
            .unwrap();
        tar_builder.finish().unwrap();
    }

    // Compress with bzip2
    let mut encoder = BzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&tar_bytes).unwrap();
    let compressed = encoder.finish().unwrap();

    let info = parse_conda_metadata(&compressed).unwrap();
    assert_eq!(info.name, "test-pkg");
    assert_eq!(info.version, "1.0.0");
    assert_eq!(info.build, "py311h0_0");
    assert_eq!(info.depends, vec!["python >=3.11"]);
}

#[cfg(feature = "local-registry")]
#[test]
fn parse_conda_metadata_conda_format() {
    use std::io::Write;

    let index_json = serde_json::json!({
        "name": "test-pkg",
        "version": "2.0.0",
        "build": "py311h0_1",
        "build_number": 1,
        "depends": ["python >=3.11"],
        "subdir": "linux-64"
    });
    let index_bytes = serde_json::to_vec(&index_json).unwrap();

    // tar containing info/index.json
    let mut tar_bytes = Vec::new();
    {
        let mut tar_builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(index_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "info/index.json", index_bytes.as_slice())
            .unwrap();
        tar_builder.finish().unwrap();
    }

    // zstd-compress the tar
    let mut encoder = zstd::Encoder::new(Vec::new(), 0).unwrap();
    encoder.write_all(&tar_bytes).unwrap();
    let zst_bytes = encoder.finish().unwrap();

    // ZIP containing info-test.tar.zst
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("info-test.tar.zst", options).unwrap();
        zip.write_all(&zst_bytes).unwrap();
        zip.finish().unwrap();
    }

    let info = parse_conda_metadata(&zip_bytes).unwrap();
    assert_eq!(info.name, "test-pkg");
    assert_eq!(info.version, "2.0.0");
    assert_eq!(info.build, "py311h0_1");
    assert_eq!(info.build_number, 1);
    assert_eq!(info.depends, vec!["python >=3.11"]);
}

#[cfg(feature = "local-registry")]
#[test]
fn parse_conda_metadata_conda_format_missing_info_entry() {
    let mut zip_bytes = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zip = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("not-info.txt", options).unwrap();
        zip.finish().unwrap();
    }

    let err = parse_conda_metadata(&zip_bytes).unwrap_err();
    assert!(matches!(err, CoreError::Registry(_)));
}

// ── lookup_file_in_repodata (via resolve_metadata) ─────────────────────────

#[tokio::test]
async fn lookup_file_in_repodata_404_returns_not_found() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(404)
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("reg", "numpy-1.26.0-py311h0", "linux-64")
        .with_artifact("numpy-1.26.0-py311h0.tar.bz2");
    let err = client.resolve_metadata(&pkg).await.unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn lookup_file_in_repodata_non_success_returns_registry_error() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(500)
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("reg", "numpy-1.26.0-py311h0", "linux-64")
        .with_artifact("numpy-1.26.0-py311h0.tar.bz2");
    let err = client.resolve_metadata(&pkg).await.unwrap_err();
    assert!(matches!(err, CoreError::Registry(_)));
}

#[tokio::test]
async fn lookup_file_in_repodata_invalid_json_returns_registry_error() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_body("not json")
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("reg", "numpy-1.26.0-py311h0", "linux-64")
        .with_artifact("numpy-1.26.0-py311h0.tar.bz2");
    let err = client.resolve_metadata(&pkg).await.unwrap_err();
    assert!(matches!(err, CoreError::Registry(_)));
}

#[tokio::test]
async fn lookup_file_in_repodata_filename_not_found_returns_not_found() {
    let mut server = mockito::Server::new_async().await;
    let repodata = serde_json::json!({ "packages": {}, "packages.conda": {} });
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_body(repodata.to_string())
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("reg", "numpy-1.26.0-py311h0", "linux-64")
        .with_artifact("numpy-1.26.0-py311h0.tar.bz2");
    let err = client.resolve_metadata(&pkg).await.unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

// ── fetch_platform_versions ────────────────────────────────────────────────

#[tokio::test]
async fn fetch_platform_versions_invalid_json_returns_empty() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/linux-64/repodata.json")
        .with_status(200)
        .with_body("not json")
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = CondaRegistryClient::new(server.url(), &opts).unwrap();
    let base = server.url();
    let versions = client
        .fetch_platform_versions(&base, "linux-64", "numpy")
        .await;
    assert!(versions.is_empty());
}
