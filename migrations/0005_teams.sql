-- M7: Teams, roles, per-site access control.

-- Per-site access: which users can see/manage which sites.
-- Owners and admins implicitly see all sites; operators and viewers
-- need an explicit grant.
CREATE TABLE site_users (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id  INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    user_id  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 'manage' or 'view'. Owners/admins skip this table entirely.
    access   TEXT NOT NULL DEFAULT 'view',
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX site_users_uniq ON site_users(site_id, user_id);
CREATE INDEX site_users_user_idx ON site_users(user_id);

-- Email-based invitation tokens.
CREATE TABLE invitation_tokens (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    email      TEXT NOT NULL,
    role       TEXT NOT NULL DEFAULT 'operator',
    token      TEXT NOT NULL UNIQUE,
    invited_by INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TEXT NOT NULL,
    used_at    TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX invitation_tokens_token_idx ON invitation_tokens(token);
