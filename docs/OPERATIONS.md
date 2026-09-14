# WPressForge Operations Runbook

This document covers operational procedures for deploying, maintaining, upgrading, and recovering WPressForge control plane nodes and managed worker nodes.

---

## 1. System Architecture Overview

WPressForge follows a decoupled control-plane / node-agent architecture:

- **Panel (`wp-panel`)**: Central control plane serving the responsive web UI and REST API. It maintains application state in an async SQLite database (`data/panel.db` in WAL mode), executes scheduled cron tasks, and issues transactional jobs to agent nodes.
- **Agent (`wp-agent`)**: Privileged local daemon running on each managed Linux node (Ubuntu 22.04/24.04, Debian 12). The agent owns container orchestration (Docker/Podman for PHP-FPM and isolated services), Nginx virtual hosts, MySQL database provisioning, restic backup snapshots, and WP-CLI execution.
- **Communication Security**: Mutual trust established via bearer API tokens and SHA-256 TLS certificate fingerprint verification (`rustls`). No shell access or SSH keys are required between the panel and active nodes under standard operations.

```
                    ┌─────────────────────────┐
                    │      WPressForge        │
                    │   Panel (wp-panel)      │
                    │  SQLite DB (WAL mode)   │
                    └────────────┬────────────┘
                                 │
           HTTPS + Bearer Token  │ TLS Fingerprint Pinning
                                 ▼
                    ┌─────────────────────────┐
                    │     Managed Node        │
                    │   Agent (wp-agent)      │
                    ├─────────────────────────┤
                    │  • Nginx Edge Reverse   │
                    │  • Docker PHP-FPM Ctrs  │
                    │  • MariaDB / MySQL      │
                    │  • Restic Repositories  │
                    └─────────────────────────┘
```

---

## 2. Disaster Recovery: Panel Control Plane

The panel stores all server credentials, site metadata, user accounts, audit trails, and backup destination configurations.

### 2.1 Critical State Files
- `data/panel.db`: SQLite database file (includes WAL and SHM files).
- `data/secrets.key`: 32-byte master encryption key used by AES-256-GCM to seal server auth tokens and S3 credentials.

> [!CAUTION]
> If `data/secrets.key` is lost, sealed destination credentials (S3 keys, restic passwords) cannot be decrypted. Always back up `secrets.key` alongside database backups.

### 2.2 Backup Procedure
To create a transactionally safe live backup of SQLite while `wp-panel` is running:

```bash
# 1. Perform SQLite online backup into destination directory
sqlite3 /var/lib/wp-panel/data/panel.db ".backup '/var/backups/panel/panel-$(date +%Y%m%d%H%M%S).db'"

# 2. Securely archive the secrets encryption key
cp /var/lib/wp-panel/data/secrets.key /var/backups/panel/secrets.key

# 3. Encrypt and upload offsite (e.g. S3 / external cold storage)
tar czf - -C /var/backups/panel . | gpg -c - > /opt/offsite/panel-dr-$(date +%Y%m%d).tar.gz.enc
```

### 2.3 Recovery Procedure on a Fresh Host
1. Install system prerequisites (Rust runtime or prebuilt `wp-panel` binary, `sqlite3`).
2. Provision service user and directories:
   ```bash
   useradd -r -s /usr/sbin/nologin wppanel
   mkdir -p /var/lib/wp-panel/data /var/lib/wp-panel/static
   chown -R wppanel:wppanel /var/lib/wp-panel
   ```
3. Restore `data/secrets.key` (permissions `0600`, owned by `wppanel`).
4. Restore `data/panel.db` to `/var/lib/wp-panel/data/panel.db`.
5. Start `wp-panel` service. Schema migrations run automatically upon startup.
6. Verify node health check from the UI at `/servers` or via API `GET /api/v1/servers`.

---

## 3. Backup & Restore Runbook: Managed Sites

WPressForge uses `restic` for content and database snapshots, written directly from agent nodes to remote repositories (AWS S3, Cloudflare R2, Backblaze B2, Wasabi, or MinIO).

### 3.1 Backup Scopes
- **Full**: Includes both MySQL database dump (`database.sql`) and file system root (`public_html/`).
- **Files Only**: Only captures `public_html/` contents, excluding server-level caches (`wp-content/cache`).
- **Database Only**: Captures single-transaction MySQL logical dump.

### 3.2 Scheduled Backups & Retention
Schedules are defined per site with retention policies enforced using restic forget rules:
- `--keep-hourly 24`
- `--keep-daily 7`
- `--keep-weekly 4`
- `--keep-monthly 6`

### 3.3 Restoring from the Panel UI
1. Navigate to **Sites** > Select Site > **Backups** tab.
2. Locate the desired snapshot in the snapshot table.
3. Select the restore scope: **Full**, **Files Only**, or **Database Only**.
4. Click **Restore**. A background job (`backup.restore`) is dispatched to the target node.
5. The agent will:
   - Temporarily stop the site container.
   - Restore file trees or pipe the logical SQL dump into MariaDB.
   - Run `wp core verify-checksums` and fix file ownership permissions (`chown -R <site_user>:<site_user>`).
   - Restart the PHP-FPM container and reload Nginx.

### 3.4 Out-of-Band Emergency Restore (Manual Restic CLI)
If the panel is unreachable or destroyed, site data can be restored directly on the node or any recovery machine using the standard `restic` binary.

1. Export destination environment variables:
   ```bash
   export RESTIC_REPOSITORY="s3:https://s3.us-east-1.amazonaws.com/my-wp-backups/site-42"
   export RESTIC_PASSWORD="<site-restic-password>"
   export AWS_ACCESS_KEY_ID="<key-id>"
   export AWS_SECRET_ACCESS_KEY="<secret-key>"
   ```

2. List available snapshots:
   ```bash
   restic snapshots
   ```

3. Restore files to a temporary directory:
   ```bash
   mkdir -p /tmp/site-recovery
   restic restore <snapshot-id> --target /tmp/site-recovery
   ```

4. Restore database:
   ```bash
   mysql -u <db_user> -p'<db_pass>' <db_name> < /tmp/site-recovery/database.sql
   ```

5. Move files into place:
   ```bash
   rsync -av /tmp/site-recovery/public_html/ /var/www/vhosts/<domain>/public_html/
   chown -R <uid>:<gid> /var/www/vhosts/<domain>/public_html
   ```

---

## 4. Node Agent Upgrade Procedure

The agent binary (`wp-agent`) runs as a systemd daemon on each managed node. Upgrades should be executed sequentially across the server pool.

### 4.1 Zero-Downtime Rolling Upgrade
Worker containers (PHP-FPM) and Nginx run independently of `wp-agent`. Upgrading or restarting `wp-agent` **does not interrupt active HTTP traffic** on hosted WordPress sites.

1. Transfer the new binary to the target node:
   ```bash
   scp target/release/wp-agent admin@node1.example.com:/tmp/wp-agent.new
   ```

2. Verify file integrity and test execution:
   ```bash
   ssh admin@node1.example.com
   chmod +x /tmp/wp-agent.new
   /tmp/wp-agent.new --version
   ```

3. Replace binary atomically and restart service:
   ```bash
   sudo mv /tmp/wp-agent.new /usr/local/bin/wp-agent
   sudo systemctl restart wp-agent
   sudo systemctl status wp-agent
   ```

4. Verify panel connection:
   Check the server status in the panel dashboard. The heartbeat and metrics collector should report green within 30 seconds.

### 4.2 Certificate Fingerprint Rotation
If the agent's TLS certificate is renewed or regenerated:
1. Obtain the new certificate SHA-256 fingerprint:
   ```bash
   openssl x509 -in /etc/wp-agent/cert.pem -noout -fingerprint -sha256
   # Format: SHA256 Fingerprint=AB:CD:EF:...
   ```
2. Update the server record in `wp-panel` (`servers.cert_fingerprint`) via UI or database.
3. The panel will now verify the new certificate pinning and resume encrypted communication.

---

## 5. Operational Triage Matrix

| Symptom | Probable Cause | Diagnostic Command | Remediation |
|---|---|---|---|
| **502 Bad Gateway** | PHP-FPM container crashed or stopped | `docker ps -a --filter name=wp-<site_id>` | Restart container: `docker restart wp-<site_id>` or via Panel Site Actions > Restart. Check memory limits in limits tab. |
| **504 Gateway Timeout** | PHP max execution timeout or slow upstream DB query | `docker logs --tail 100 wp-<site_id>` and inspect `php-slow.log` | Check database locks via `SHOW FULL PROCESSLIST`. Increase memory or workers if saturated. |
| **Error Establishing Database Connection** | MariaDB down or MySQL user credentials desynced | `systemctl status mariadb` and test connection with `wp config get --path=...` | Verify MySQL daemon is active. Inspect `/var/www/vhosts/<domain>/wp-config.php` credentials. |
| **Disk Exhaustion (`100% full`)** | Nginx cache unpurged or Docker logs ballooned | `df -h` and `du -sh /var/lib/docker/containers/*` | Prune cache: `wp-panel` Clear Cache or clean Docker logs: `truncate -s 0 /var/lib/docker/containers/*/*-json.log`. |
| **Agent Disconnect (Red in Panel)** | Firewall blocking port 8443 or expired token | `systemctl status wp-agent` and check `ufw status` | Verify port 8443 is allowed from panel IP. Check journalctl: `journalctl -u wp-agent -n 100`. |
