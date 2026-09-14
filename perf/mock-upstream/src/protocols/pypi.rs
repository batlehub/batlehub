//! PyPI: the Simple API (PEP 503) and the files it links to.
//!
//! Two routes. `/simple/{name}/` is the HTML listing the proxy parses and
//! rewrites — every `href` is made to point back at the registry, which is the
//! per-request rewrite path a soak wants under load — and `/packages/{file}` is
//! the wheel. The `#sha256=` fragment is not decoration: the proxy verifies a
//! wheel against it, so it is computed from the bytes `packages` will serve.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::{artifact_bytes, delay, host, sha256_hex};
use crate::Args;

const VERSIONS: [&str; 3] = ["1.0.0", "1.1.0", "1.2.0"];

/// `GET /pypi/simple/{name}/` — the PEP 503 page.
#[route("/pypi/simple/{name}/", method = "GET", method = "HEAD")]
async fn simple_page(
    req: HttpRequest,
    name: web::Path<String>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let name = name.into_inner();
    let host = host(&req);
    let mut body = format!(
        "<!DOCTYPE html>\n<html><head><title>Links for {name}</title></head><body>\n\
         <h1>Links for {name}</h1>\n"
    );
    // The proxy hands us the PEP 503 *normalised* name (`-`), and a wheel
    // filename carries the *escaped* one (`_`). Emitting the normalised name in
    // the filename produces a page whose links no client can ask for: the
    // pre-flight reported "the simple page names no such file".
    let escaped = name.replace('-', "_");
    for v in VERSIONS {
        let filename = format!("{escaped}-{v}-py3-none-any.whl");
        let digest = sha256_hex(&artifact_bytes(&filename, "", args.artifact_size_kb * 1024));
        body.push_str(&format!(
            "<a href=\"http://{host}/pypi/packages/{filename}#sha256={digest}\">{filename}</a><br/>\n"
        ));
    }
    body.push_str("</body></html>\n");
    HttpResponse::Ok().content_type("text/html").body(body)
}

/// `GET /pypi/packages/{filename}` — the wheel the page linked to.
#[route("/pypi/packages/{filename}", method = "GET", method = "HEAD")]
async fn package_file(filename: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let filename = filename.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&filename, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(simple_page).service(package_file);
}
