-- M3: Backup destinations, schedules, and restore support.

-- Extend backup_destinations with credential fields.
ALTER TABLE backup_destinations ADD COLUMN access_key_id TEXT;
ALTER TABLE backup_destinations ADD COLUMN secret_sealed  TEXT;   -- SecretBox::seal
ALTER TABLE backup_destinations ADD COLUMN restic_password_sealed TEXT;
ALTER TABLE backup_destinations ADD COLUMN repo_prefix TEXT NOT NULL DEFAULT 'wp';

-- Extend backup_schedules with scheduling state.
ALTER TABLE backup_schedules ADD COLUMN scope TEXT NOT NULL DEFAULT 'full';
ALTER TABLE backup_schedules ADD COLUMN interval_minutes INTEGER NOT NULL DEFAULT 1440;
ALTER TABLE backup_schedules ADD COLUMN last_run_at TEXT;
ALTER TABLE backup_schedules ADD COLUMN next_run_at TEXT;
CREATE INDEX backup_schedules_due_idx ON backup_schedules(enabled, next_run_at);

-- Extend backups with size and repo tracking.
ALTER TABLE backups ADD COLUMN files_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE backups ADD COLUMN db_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE backups ADD COLUMN restic_repo TEXT;
