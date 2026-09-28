-- Silicon Extend: the down step from the 1.1.0 service to 1.0.0.
--
-- Run it once, with the service stopped, before the 1.0.0 image starts:
--
--   psql "$EXTEND_DATABASE_URL" -v ON_ERROR_STOP=1 -f deploy/rollback/1.1-to-1.0.sql
--
-- It changes every world schema at version 4 or later (production and each test environment) and the
-- global schema, in one transaction. Running it twice does no harm.
--
-- Why it is needed:
-- - 1.0.0 accepts any grant on a device once the Silicon acts in the device's Team. Grants from
--   other Teams are set aside in rollback_1_1_grants (with the time they were set aside), and
--   sessions running under them end. 1.1.0 puts each back when it starts again, unless the Carbon
--   revoked that Silicon on that device meanwhile, or the pair ended.
-- - 1.0.0 would retry requests routed to another Carbon as if the Silicon using the device were
--   the recipient. Those still pending are marked failed. Their 1.0 columns never name another
--   side (to_id is the hidden text, session_id is NULL), so nothing else needs hiding.
-- - Open wake requests end: 1.0.0 knows nothing of them.
-- - A rotated credential the app hasn't confirmed is dropped; the confirmed one keeps working.
-- - "Pair with another Carbon" codes still waiting are deleted: 1.0.0 would pair them as new
--   devices, splitting one physical device into two.
--
-- While 1.0.0 runs, the 1.1.0 triggers stay and keep it safe: devices stay personal and get an
-- instance, grants get a Team, one lock per physical device, and requests get the holder columns.
-- Schema 5's in_use_indicator column stays, including hidden choices. 1.0.0 ignores it; the
-- instance rows its inserts create default to shown. Rolling forward preserves existing choices.

BEGIN;

DO $rollback$
DECLARE
    s text;
BEGIN
    FOR s IN SELECT schema_name FROM extend_global.schema_versions WHERE version >= 4 ORDER BY schema_name LOOP
        -- Grants from other Teams.
        EXECUTE format($q$CREATE TABLE IF NOT EXISTS %1$I.rollback_1_1_grants AS
            SELECT a.*, now() AS stashed_at FROM %1$I.device_access a WITH NO DATA$q$, s);
        EXECUTE format($q$INSERT INTO %1$I.rollback_1_1_grants
            SELECT a.*, now() FROM %1$I.device_access a JOIN %1$I.devices d USING (device_id) WHERE a.team <> d.team$q$, s);
        EXECUTE format($q$DELETE FROM %1$I.device_access a USING %1$I.devices d
            WHERE a.device_id = d.device_id AND a.team <> d.team$q$, s);
        EXECUTE format($q$UPDATE %1$I.sessions x SET state = 'ended', ended_at = now(), end_reason = 'access_removed', idle_ends_at = NULL
            FROM %1$I.devices d WHERE d.device_id = x.device_id AND x.state <> 'ended' AND x.team <> d.team$q$, s);
        EXECUTE format($q$DELETE FROM %1$I.device_locks l USING %1$I.sessions x
            WHERE x.session_id = l.session_id AND x.state = 'ended'$q$, s);
        -- Requests routed to another Carbon: never retried by 1.0.0.
        EXECUTE format($q$UPDATE %1$I.requests SET delivery = 'failed',
                last_error = COALESCE(last_error, 'withdrawn by the rollback to 1.0.0')
            WHERE routed_to = 'carbon' AND delivery = 'pending'$q$, s);
        -- Wake requests.
        EXECUTE format($q$UPDATE %1$I.wake_requests SET state = 'withdrawn', ended_at = now(), end_reason = 'rollback'
            WHERE state = 'open'$q$, s);
        -- Rotated credentials not yet confirmed.
        EXECUTE format($q$UPDATE %1$I.devices SET next_credential_digest = NULL
            WHERE next_credential_digest IS NOT NULL$q$, s);
    END LOOP;
    -- "Pair with another Carbon" codes still waiting.
    DELETE FROM extend_global.enrollments WHERE instance_id IS NOT NULL AND paired_device_id IS NULL;
END
$rollback$;

COMMIT;
