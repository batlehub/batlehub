// ── Pure helper functions ─────────────────────────────────────────────────────

/// Normalise a NuGet package ID to lower-case (IDs are case-insensitive in the protocol).
///
/// Delegates to `RegistryKind::canonical_package_name`, which is where the rule
/// is defined once: the explore catalogue compares a search hit's display
/// spelling against a stored name through that same function, and a second
/// definition here could drift from it.
pub fn normalize_id(id: &str) -> String {
    batlehub_core::entities::RegistryKind::Nuget
        .canonical_package_name(id)
        .into_owned()
}
