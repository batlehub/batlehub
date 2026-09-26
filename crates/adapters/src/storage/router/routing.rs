use std::collections::HashMap;

/// Extract the registry name from an artifact key and look up its assigned backend.
/// Falls back to `default_name` when the registry has no explicit assignment or
/// when the key does not carry an `"artifact:"` prefix.
///
/// A staging key routes as the artifact it was made for, so the staged bytes
/// land on the backend where promoting them is a rename.
pub fn route_key_to_backend<'a>(
    key: &str,
    registry_assignments: &'a HashMap<String, String>,
    default_name: &'a str,
) -> &'a str {
    let key = batlehub_core::ports::staged_destination(key).unwrap_or(key);
    let registry = key
        .strip_prefix("artifact:")
        .and_then(|k| k.split('/').next())
        .unwrap_or("");

    registry_assignments
        .get(registry)
        .map(|s| s.as_str())
        .unwrap_or(default_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_by_registry_assignment() {
        let mut assignments = HashMap::new();
        assignments.insert("cargo".to_string(), "s3".to_string());
        assert_eq!(
            route_key_to_backend("artifact:cargo/tokio/1.0.0", &assignments, "default"),
            "s3"
        );
        assert_eq!(
            route_key_to_backend("artifact:npm/lodash/4.0.0", &assignments, "default"),
            "default"
        );
    }

    #[test]
    fn a_staging_key_routes_as_its_destination() {
        let mut assignments = HashMap::new();
        assignments.insert("cargo".to_string(), "s3".to_string());
        let staged = batlehub_core::ports::staging_key_for("artifact:cargo/tokio/1.0.0");
        assert_eq!(route_key_to_backend(&staged, &assignments, "default"), "s3");
    }

    #[test]
    fn falls_back_for_key_without_artifact_prefix() {
        let assignments = HashMap::new();
        assert_eq!(
            route_key_to_backend("some/other/key", &assignments, "default"),
            "default"
        );
    }

    #[test]
    fn empty_assignments_always_returns_default() {
        let assignments = HashMap::new();
        assert_eq!(
            route_key_to_backend("artifact:maven/org.apache/log4j/2.0", &assignments, "fs"),
            "fs"
        );
    }
}
