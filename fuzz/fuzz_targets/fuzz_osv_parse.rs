#![no_main]
//! OSV's `/v1/query` answer is upstream input (RFC 0041 §6.8, T11), and
//! `parse_vulns` is where it becomes a severity: the `MAL-` rule, the twin
//! grouping of decision 21 and the `cvss` crate's vector parser, in one call.
//! Input: a first line taken as the queried PURL, the rest as JSON (a bare
//! array is wrapped as `vulns`). Properties:
//!
//! 1. **No panic**, whatever the JSON or the vector strings in it.
//! 2. **Nothing lowers `MAL-`.** A record whose id or any alias is `MAL-` is
//!    `critical` and named by a `MAL-` id.
//! 3. **Every match is a record of the answer**, named by one of its names,
//!    and there are never more matches than records.

use libfuzzer_sys::fuzz_target;

use batlehub_adapters::vulnerability::osv::parse_vulns;
use batlehub_core::entities::Severity;

fuzz_target!(|data: &[u8]| {
    let split = data.iter().position(|&b| b == b'\n').unwrap_or(0);
    let purl = String::from_utf8_lossy(&data[..split]);
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&data[split..]) else {
        return;
    };
    if json.is_array() {
        json = serde_json::json!({ "vulns": json });
    }
    let matches = parse_vulns(&purl, &json);

    let records = json
        .get("vulns")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or_default();
    assert!(matches.len() <= records.len());
    for m in &matches {
        let carried = records.iter().any(|r| {
            r["id"].as_str() == Some(&m.osv_id)
                || r["aliases"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(&m.osv_id)))
        });
        assert!(carried, "{} names no record of the answer", m.osv_id);
        let malicious =
            m.osv_id.starts_with("MAL-") || m.aliases.iter().any(|a| a.starts_with("MAL-"));
        if malicious {
            assert_eq!(m.severity, Severity::Critical, "{} lowered", m.osv_id);
            assert!(
                m.osv_id.starts_with("MAL-"),
                "malicious {} not named MAL-",
                m.osv_id
            );
        }
    }
});
