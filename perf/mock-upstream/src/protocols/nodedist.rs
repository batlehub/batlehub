//! The Node.js distribution tree: `index.tab`, `index.json` and the tarballs.
//!
//! Two listings, because two families of client read different ones — nvm reads
//! the TSV, fnm and mise read the JSON — and the proxy filters both. The TSV's
//! header is what `IndexTab::parse` keys its columns on, so it carries Node's
//! real eleven columns rather than the three this mock fills in.

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay};
use crate::Args;

const RELEASES: [(&str, &str, &str); 3] = [
    ("v22.11.0", "2024-10-29", "Jod"),
    ("v22.10.0", "2024-10-16", "-"),
    ("v20.18.0", "2024-10-03", "Iron"),
];

/// `GET /nodedist/index.tab` — the TSV nvm reads.
#[route("/nodedist/index.tab", method = "GET", method = "HEAD")]
async fn index_tab(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let mut body =
        String::from("version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n");
    for (version, date, lts) in RELEASES {
        body.push_str(&format!(
            "{version}\t{date}\theaders,linux-x64,src\t10.9.0\t12.4.254.21\t1.49.1\t\
             1.3.0.1-motley\t3.0.15+quic\t127\t{lts}\t-\n"
        ));
    }
    HttpResponse::Ok().content_type("text/plain").body(body)
}

/// `GET /nodedist/index.json` — the same releases, for fnm and mise.
#[route("/nodedist/index.json", method = "GET", method = "HEAD")]
async fn index_json(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let rows: Vec<_> = RELEASES
        .iter()
        .map(|(version, date, lts)| {
            serde_json::json!({
                "version": version,
                "date": date,
                "files": ["headers", "linux-x64", "src"],
                "npm": "10.9.0",
                "lts": if *lts == "-" { serde_json::Value::Bool(false) }
                       else { serde_json::Value::String((*lts).to_owned()) },
                "security": false,
            })
        })
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::Value::Array(rows).to_string())
}

/// `GET /nodedist/{version}/{file}` — a tarball, a checksum file, anything.
#[route("/nodedist/{version}/{file}", method = "GET", method = "HEAD")]
async fn dist_file(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (version, file) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(
            &file,
            &version,
            args.artifact_size_kb * 1024,
        ))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(index_tab)
        .service(index_json)
        .service(dist_file);
}
