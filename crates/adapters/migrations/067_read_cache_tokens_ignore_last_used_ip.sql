-- TD-0001 §1: a token's last-used write no longer invalidates the token cache.
--
-- 066 notified on a `last_used_ip` change because `token_new_source` compared
-- against the cached row. It now compares against the address
-- `touch_last_used` reads in its own statement, so the cached copy of that
-- column may be stale, and a token's first use (NULL → its address) no longer
-- costs every replica a `find_by_hash` on the next request.
DROP TRIGGER read_cache_user_tokens_upd ON user_tokens;
CREATE TRIGGER read_cache_user_tokens_upd AFTER UPDATE ON user_tokens
    FOR EACH ROW WHEN (OLD.revoked_at IS DISTINCT FROM NEW.revoked_at
                       OR OLD.expires_at IS DISTINCT FROM NEW.expires_at
                       OR OLD.role IS DISTINCT FROM NEW.role
                       OR OLD.groups IS DISTINCT FROM NEW.groups
                       OR OLD.user_id IS DISTINCT FROM NEW.user_id
                       OR OLD.token_hash IS DISTINCT FROM NEW.token_hash)
    EXECUTE FUNCTION batlehub_notify_invalidate();
