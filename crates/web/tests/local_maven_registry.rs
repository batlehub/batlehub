//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, TestRequest};

use batlehub_config::schema::RegistryMode;

// ══ Maven local registry tests ════════════════════════════════════════════════

async fn make_local_maven_app(mode: RegistryMode) -> impl TestService {
    // Hybrid needs the upstream client, or the fall-through resolves nothing.
    let upstream = matches!(mode, RegistryMode::Hybrid);
    build_local_registry_app(
        local_only_app_parts("local-maven", "maven", mode, upstream),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

const SAMPLE_POM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <groupId>com.example</groupId>
  <artifactId>mylib</artifactId>
  <version>1.0.0</version>
  <packaging>jar</packaging>
  <description>A test library</description>
</project>"#;

#[actix_web::test]
async fn maven_put_pom_creates_version() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/xml"))
        .set_payload(SAMPLE_POM)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);

    // maven-metadata.xml should now contain the version
    let req = TestRequest::get()
        .uri("/proxy/local-maven/maven2/com/example/mylib/maven-metadata.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(
        body.contains("<version>1.0.0</version>"),
        "metadata should contain version"
    );
    assert!(body.contains("<groupId>com.example</groupId>"));
    assert!(body.contains("<artifactId>mylib</artifactId>"));
}

#[actix_web::test]
async fn maven_put_pom_version_mismatch_returns_400() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    // URL path says 1.0.0, POM body declares 1.0.1 — must be rejected rather
    // than silently publishing under whichever version the body claims.
    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/xml"))
        .set_payload(SAMPLE_POM.replace("<version>1.0.0</version>", "<version>1.0.1</version>"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);

    // Nothing must have been published under either version.
    let req = TestRequest::get()
        .uri("/proxy/local-maven/maven2/com/example/mylib/maven-metadata.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn maven_publish_traversal_version_returns_400() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    // A lone ".." lands as the version segment (2nd-from-last) — must be
    // rejected before it reaches the storage key, per
    // `parse_maven_path_traversal_version_rejected` (unit test) exercised here
    // end-to-end over HTTP.
    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/../mylib-1.0.0.jar")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"fake-jar-bytes".as_slice())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn maven_put_jar_before_pom_is_accepted() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    let jar_bytes = b"fake-jar-bytes";
    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.jar")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(jar_bytes.as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);

    // GET the jar back
    let req = TestRequest::get()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.jar")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await, jar_bytes.as_slice());
}

#[actix_web::test]
async fn maven_put_jar_anonymous_returns_401() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    // No Authorization header at all — must be rejected by enforce_publish_policy
    // the same way the .pom branch is, not silently stored.
    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.jar")
        .set_payload(b"fake-jar-bytes".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 401);

    // Nothing must have been stored under that coordinate.
    let req = TestRequest::get()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.jar")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn maven_put_metadata_xml_is_silently_accepted() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/maven-metadata.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload("<metadata/>")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn maven_put_pom_duplicate_returns_409() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    for _ in 0..2 {
        let req = TestRequest::put()
            .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .insert_header(("Content-Type", "application/xml"))
            .set_payload(SAMPLE_POM)
            .to_request();
        let _ = call_service(&app, req).await;
    }

    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/xml"))
        .set_payload(SAMPLE_POM)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 409);
}

#[actix_web::test]
async fn maven_put_requires_auth() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom")
        .set_payload(SAMPLE_POM)
        .to_request();
    let resp = call_service(&app, req).await;
    // Anonymous has no access to this registry, returns 403 (RBAC) or 401
    assert!(resp.status() == 401 || resp.status() == 403);
}

#[actix_web::test]
async fn maven_get_metadata_empty_returns_404() {
    let app = make_local_maven_app(RegistryMode::Local).await;

    let req = TestRequest::get()
        .uri("/proxy/local-maven/maven2/com/example/unknown/maven-metadata.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn maven_put_proxy_mode_registry_rejected() {
    let app = make_local_maven_app(RegistryMode::Proxy).await;

    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(SAMPLE_POM)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}
