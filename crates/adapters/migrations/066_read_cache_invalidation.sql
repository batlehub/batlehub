-- Read-cache invalidation (crates/adapters/src/db/read_cache.rs).
--
-- Every replica caches the reads a download repeats and LISTENs on
-- `batlehub_invalidate`; these triggers NOTIFY it, with the table's name as
-- the payload, whenever a change could alter a cached answer. NOTIFY is
-- delivered at commit and folded per transaction, so a bulk write is one
-- message per table.
--
-- Row triggers with a WHEN clause, not statement triggers: the access-event
-- write runs `INSERT INTO package_statuses … ON CONFLICT DO NOTHING` on every
-- download, and a statement trigger would fire for each one. What is cached
-- only as a hit (tokens, dedup mappings) needs no INSERT trigger.

CREATE OR REPLACE FUNCTION batlehub_notify_invalidate() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('batlehub_invalidate', TG_TABLE_NAME);
    RETURN NULL;
END;
$$;

-- package_statuses: only a row that is, or was, a block.
CREATE TRIGGER read_cache_package_statuses_ins AFTER INSERT ON package_statuses
    FOR EACH ROW WHEN (NEW.status = 'blocked')
    EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_package_statuses_upd AFTER UPDATE ON package_statuses
    FOR EACH ROW WHEN ((OLD.status = 'blocked' OR NEW.status = 'blocked')
        AND (OLD.status IS DISTINCT FROM NEW.status
             OR OLD.block_reason IS DISTINCT FROM NEW.block_reason
             OR OLD.package_artifact IS DISTINCT FROM NEW.package_artifact))
    EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_package_statuses_del AFTER DELETE ON package_statuses
    FOR EACH ROW WHEN (OLD.status = 'blocked')
    EXECUTE FUNCTION batlehub_notify_invalidate();

-- package_flags: any change; pushes are rare and batched.
CREATE TRIGGER read_cache_package_flags AFTER INSERT OR UPDATE OR DELETE ON package_flags
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();

-- local_packages: a new published row can change a cached `public`.
CREATE TRIGGER read_cache_local_packages_ins AFTER INSERT ON local_packages
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_local_packages_upd AFTER UPDATE ON local_packages
    FOR EACH ROW WHEN (OLD.visibility IS DISTINCT FROM NEW.visibility
                       OR OLD.status IS DISTINCT FROM NEW.status)
    EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_local_packages_del AFTER DELETE ON local_packages
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();

-- user_tokens: not the once-a-minute last_used_at write, unless it moves
-- last_used_ip, which `token_new_source` compares against.
CREATE TRIGGER read_cache_user_tokens_upd AFTER UPDATE ON user_tokens
    FOR EACH ROW WHEN (OLD.revoked_at IS DISTINCT FROM NEW.revoked_at
                       OR OLD.last_used_ip IS DISTINCT FROM NEW.last_used_ip
                       OR OLD.expires_at IS DISTINCT FROM NEW.expires_at
                       OR OLD.role IS DISTINCT FROM NEW.role
                       OR OLD.groups IS DISTINCT FROM NEW.groups
                       OR OLD.user_id IS DISTINCT FROM NEW.user_id
                       OR OLD.token_hash IS DISTINCT FROM NEW.token_hash)
    EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_user_tokens_del AFTER DELETE ON user_tokens
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();

-- user_blocks: "not blocked" is cached too, so every change.
CREATE TRIGGER read_cache_user_blocks AFTER INSERT OR UPDATE OR DELETE ON user_blocks
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();

-- The dedup mapping: a re-pointed or removed ref, index row or backend.
CREATE TRIGGER read_cache_dedup_refs_upd AFTER UPDATE ON artifact_dedup_refs
    FOR EACH ROW WHEN (OLD.content_hash IS DISTINCT FROM NEW.content_hash)
    EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_dedup_refs_del AFTER DELETE ON artifact_dedup_refs
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_dedup_index_upd AFTER UPDATE ON artifact_dedup_index
    FOR EACH ROW WHEN (OLD.content_key IS DISTINCT FROM NEW.content_key)
    EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_dedup_index_del AFTER DELETE ON artifact_dedup_index
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_artifact_storage_upd AFTER UPDATE ON artifact_storage
    FOR EACH ROW WHEN (OLD.backend_name IS DISTINCT FROM NEW.backend_name)
    EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_artifact_storage_del AFTER DELETE ON artifact_storage
    FOR EACH ROW EXECUTE FUNCTION batlehub_notify_invalidate();

-- TRUNCATE skips row triggers.
CREATE TRIGGER read_cache_package_statuses_trunc AFTER TRUNCATE ON package_statuses
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_package_flags_trunc AFTER TRUNCATE ON package_flags
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_local_packages_trunc AFTER TRUNCATE ON local_packages
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_user_tokens_trunc AFTER TRUNCATE ON user_tokens
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_user_blocks_trunc AFTER TRUNCATE ON user_blocks
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_dedup_refs_trunc AFTER TRUNCATE ON artifact_dedup_refs
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_dedup_index_trunc AFTER TRUNCATE ON artifact_dedup_index
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
CREATE TRIGGER read_cache_artifact_storage_trunc AFTER TRUNCATE ON artifact_storage
    FOR EACH STATEMENT EXECUTE FUNCTION batlehub_notify_invalidate();
