-- M1: Security foundations.
-- login_attempts, TOTP columns, agent fingerprint, totp_confirmed_at.

-- Login throttling
CREATE TABLE IF NOT EXISTS login_attempts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ip         TEXT NOT NULL,
    email      TEXT NOT NULL,
    success    INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS login_attempts_ip_idx ON login_attempts(ip, created_at);

-- Agent certificate fingerprint for TLS pinning
ALTER TABLE servers ADD COLUMN agent_fingerprint TEXT;

-- TOTP 2FA confirmation timestamp
ALTER TABLE users ADD COLUMN totp_confirmed_at TEXT;
