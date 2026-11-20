-- A cargo crate published as `MyCrate` before names were folded keeps that row
-- (and its storage key `{registry}/MyCrate/{version}`), and every request names
-- it in lowercase. `stored_name_ignore_case` finds the stored spelling through
-- this index instead of listing every name of the registry.
CREATE INDEX IF NOT EXISTS idx_local_packages_registry_lower_name
    ON local_packages (registry, lower(name));
