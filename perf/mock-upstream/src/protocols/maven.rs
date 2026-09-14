//! A Maven 2 repository: `maven-metadata.xml`, the pom and the jar.

use actix_web::{route, web, HttpResponse};

use crate::support::*;
use crate::Args;

/// `GET /maven/{group/path}/{artifact}/maven-metadata.xml`
#[route("/maven/{path:.*}/maven-metadata.xml", method = "GET", method = "HEAD")]
async fn maven_metadata(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let path = path.into_inner();
    let (group, artifact) = path.rsplit_once('/').unwrap_or(("com.example", &path));
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata>
  <groupId>{}</groupId>
  <artifactId>{artifact}</artifactId>
  <versioning>
    <latest>1.2.0</latest>
    <release>1.2.0</release>
    <versions><version>1.0.0</version><version>1.1.0</version><version>1.2.0</version></versions>
    <lastUpdated>20240101000000</lastUpdated>
  </versioning>
</metadata>
"#,
        group.replace('/', ".")
    );
    HttpResponse::Ok().content_type("text/xml").body(body)
}

/// `GET /maven/{path}` — any other file under the repository: the jar, the pom.
#[route("/maven/{path:.*}", method = "GET", method = "HEAD")]
async fn maven_file(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let path = path.into_inner();
    if path.ends_with(".pom") {
        return HttpResponse::Ok()
            .content_type("text/xml")
            .body("<project><modelVersion>4.0.0</modelVersion></project>\n".to_owned());
    }
    HttpResponse::Ok()
        .content_type("application/java-archive")
        .body(artifact_bytes(&path, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(maven_metadata).service(maven_file);
}
