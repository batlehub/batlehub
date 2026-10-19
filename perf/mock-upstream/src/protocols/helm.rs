//! A classic Helm chart repository: `index.yaml` and the archives it names.
//!
//! Eight charts of four versions, with **relative** URLs — the jetstack shape —
//! so the proxy resolves each against the repository URL the way `helm` does.
//! Each entry's `digest` is the SHA-256 of the bytes [`chart`] serves, because
//! the proxy verifies it on the cache write; a digest that was not computed
//! over these bytes would fail every fetch.

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay, sha256_hex};
use crate::Args;

const CHARTS: usize = 8;
const VERSIONS: usize = 4;

fn bytes_of(name: &str, version: &str, args: &Args) -> Vec<u8> {
    artifact_bytes(name, version, args.artifact_size_kb * 1024)
}

/// `GET /helm/index.yaml`.
#[route("/helm/index.yaml", method = "GET", method = "HEAD")]
async fn index(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let mut out = String::from("apiVersion: v1\nentries:\n");
    for c in 0..CHARTS {
        let name = format!("chart{c}");
        out.push_str(&format!("  {name}:\n"));
        for v in (0..VERSIONS).rev() {
            let version = format!("1.{v}.0");
            let digest = sha256_hex(&bytes_of(&name, &version, &args));
            out.push_str(&format!(
                "  - apiVersion: v2\n    name: {name}\n    version: {version}\n    \
                 appVersion: \"2.{v}\"\n    created: \"2020-01-02T00:00:00Z\"\n    \
                 digest: {digest}\n    urls:\n    - charts/{name}-{version}.tgz\n"
            ));
        }
    }
    out.push_str("generated: \"2020-01-02T00:00:00Z\"\n");
    HttpResponse::Ok().content_type("text/yaml").body(out)
}

/// `GET /helm/charts/{name}-{version}.tgz`.
#[route("/helm/charts/{file}", method = "GET", method = "HEAD")]
async fn chart(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let file = path.into_inner();
    let Some((name, version)) = file
        .strip_suffix(".tgz")
        .and_then(|stem| stem.rsplit_once('-'))
    else {
        return HttpResponse::NotFound().finish();
    };
    HttpResponse::Ok()
        .content_type("application/gzip")
        .body(bytes_of(name, version, &args))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(index).service(chart);
}
