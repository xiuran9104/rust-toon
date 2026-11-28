-- 0033: 国密 identity rollout — per-user SM3 identity UUIDs and an SM3
-- password hash for the seeded admin. Idempotent: every UPDATE is keyed on
-- the exact pre-change value, and the startup pass backfills any user that
-- reaches a NULL identity_uuid (see system-server bootstrap).

-- 1) Per-user identity UUID derived from SM3("rust-toon-user:" || id) in
--    Rust. SQL cannot compute SM3, so baseline users carry literals here;
--    the gateway startup pass backfills any user created outside the
--    baseline. The API layer presents this UUID as the user id everywhere
--    (JWT sub, cache keys, token/log joins).
ALTER TABLE system_users ADD COLUMN IF NOT EXISTS identity_uuid uuid;

CREATE UNIQUE INDEX IF NOT EXISTS system_users_identity_uuid_key
    ON system_users (identity_uuid);

UPDATE system_users SET identity_uuid = '178d1f4e-ea86-414e-8a65-bbbf89ed94c0'::uuid
WHERE id = 1 AND identity_uuid IS NULL;

-- 2) Replace the seeded admin's bcrypt hash with PBKDF2-HMAC-SM3("admin123").
--    Only the untouched baseline hash is replaced; changed passwords survive.
--    The $sm3$ encoding (108 chars) needs more room than bcrypt's 60.
ALTER TABLE system_users ALTER COLUMN password TYPE varchar(128);

UPDATE system_users
SET password = '$sm3$16384$59504b42457c776e6118130a0d043f36$fd7809291e0537a9b7a2351022faa0f357adfa166dd243bd9585a088d0dd0039',
    updater = 'migration', update_time = now()
WHERE id = 1
  AND password = '$2a$04$.vd8nPeLwxt6hnSzmAoAyul8BOLX7Cib6QhcxRe30rfvrIPQHH1OG';
