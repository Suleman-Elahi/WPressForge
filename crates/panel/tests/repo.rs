//! Repository-layer tests: every `db::*` module against a real SQLite file.
//!
//! These exist because the audit found a defect that only this layer could
//! catch: `user_for_session` selected a hand-written column list that omitted
//! `totp_secret`, and because `map_row` tolerates missing columns, 2FA silently
//! became impossible to enable. Nothing above the repository could see it.
//!
//! A temp *file* is used rather than `sqlite::memory:` because an in-memory
//! database is private to a single connection, so a pooled test would silently
//! talk to several different empty databases.

use chrono::{Duration, Utc};
use std::path::PathBuf;
use wp_common::models::{
    BackupScope, CacheSettings, DatabaseMode, Environment, JobKind, JobStatus, PhpVersion,
    ResourceLimits, ServerStatus, SiteStatus, SslIssuer,
};
use wp_panel::db;
use wp_panel::secrets::SecretBox;

/// A throwaway database that removes itself, WAL sidecars included.
struct TestDb {
    pool: db::Db,
    path: PathBuf,
}

impl Drop for TestDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

async fn test_db() -> TestDb {
    let path = std::env::temp_dir().join(format!("wp-panel-repo-{}.db", uuid::Uuid::new_v4()));
    let pool = db::connect(&path).await.expect("open database");
    TestDb { pool, path }
}

async fn a_user(db: &db::Db, email: &str, role: &str) -> i64 {
    let hash = wp_panel::auth::hash_password("correct horse battery").expect("hash");
    db::users::create(db, email, "Test User", &hash, role)
        .await
        .expect("create user")
}

async fn a_server(db: &db::Db, name: &str) -> i64 {
    db::servers::create(
        db,
        db::servers::NewServer {
            name,
            agent_url: "https://10.0.0.5:8443",
            agent_token: "token-token-token-token-1234",
            hostname: "node.example.net",
            ip_address: "10.0.0.5",
            provider: Some("test"),
            region: Some("local"),
            agent_fingerprint: Some("sha256:AA:BB"),
        },
    )
    .await
    .expect("create server")
}

async fn a_site(db: &db::Db, server_id: i64, domain: &str) -> i64 {
    db::sites::create(
        db,
        db::sites::NewSite {
            server_id,
            domain,
            title: Some("Test site"),
            php_version: PhpVersion::Php84,
            database_mode: DatabaseMode::Shared,
            environment: Environment::Production,
            parent_site_id: None,
            limits: ResourceLimits::default(),
            cache: CacheSettings::default(),
            request_ssl: true,
        },
    )
    .await
    .expect("create site")
}

// ---------------------------------------------------------------------------
// users and sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn users_round_trip_and_sessions_resolve() {
    let test = test_db().await;
    let db = &test.pool;

    assert_eq!(db::users::count(db).await.unwrap(), 0);
    let id = a_user(db, "owner@example.com", "owner").await;
    assert_eq!(db::users::count(db).await.unwrap(), 1);

    let user = db::users::by_email(db, "owner@example.com")
        .await
        .unwrap()
        .expect("found by email");
    assert_eq!(user.id, id);
    assert_eq!(user.role, "owner");
    assert!(user.last_login_at.is_none());

    db::users::create_session(
        db,
        "session-token",
        id,
        "curl",
        "127.0.0.1",
        Duration::days(1),
    )
    .await
    .unwrap();

    let from_session = db::users::user_for_session(db, "session-token")
        .await
        .unwrap()
        .expect("session resolves");
    assert_eq!(from_session.id, id);

    db::users::delete_session(db, "session-token")
        .await
        .unwrap();
    assert!(
        db::users::user_for_session(db, "session-token")
            .await
            .unwrap()
            .is_none()
    );
}

/// The regression test for the defect described at the top of this file.
#[tokio::test]
async fn a_session_carries_the_totp_columns() {
    let test = test_db().await;
    let db = &test.pool;
    let id = a_user(db, "2fa@example.com", "owner").await;

    db::users::set_totp_secret(db, id, "JBSWY3DPEHPK3PXP")
        .await
        .unwrap();
    db::users::create_session(db, "tok", id, "", "", Duration::days(1))
        .await
        .unwrap();

    let user = db::users::user_for_session(db, "tok")
        .await
        .unwrap()
        .expect("session");
    assert_eq!(
        user.totp_secret.as_deref(),
        Some("JBSWY3DPEHPK3PXP"),
        "the session query must select totp_secret, or 2FA enrolment breaks"
    );
    assert!(user.totp_confirmed_at.is_none());

    db::users::confirm_totp(db, id).await.unwrap();
    let user = db::users::user_for_session(db, "tok")
        .await
        .unwrap()
        .unwrap();
    assert!(user.totp_confirmed_at.is_some());

    db::users::disable_totp(db, id).await.unwrap();
    let user = db::users::user_for_session(db, "tok")
        .await
        .unwrap()
        .unwrap();
    assert!(user.totp_secret.is_none());
    assert!(user.totp_confirmed_at.is_none());
}

#[tokio::test]
async fn expired_sessions_are_ignored_and_purged() {
    let test = test_db().await;
    let db = &test.pool;
    let id = a_user(db, "expiry@example.com", "owner").await;

    db::users::create_session(db, "stale", id, "", "", Duration::seconds(-60))
        .await
        .unwrap();

    assert!(
        db::users::user_for_session(db, "stale")
            .await
            .unwrap()
            .is_none(),
        "an expired session must not authenticate"
    );
    assert_eq!(db::users::purge_expired_sessions(db).await.unwrap(), 1);
}

// ---------------------------------------------------------------------------
// api tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_tokens_are_stored_hashed_and_can_be_revoked() {
    let test = test_db().await;
    let db = &test.pool;
    let user_id = a_user(db, "tokens@example.com", "owner").await;

    let (id, plaintext) = db::tokens::create(db, user_id, "cli").await.unwrap();
    assert!(plaintext.starts_with("wpp_"), "token: {plaintext}");

    let stored: String = sqlx::query_scalar("SELECT token_hash FROM api_tokens WHERE id = ?1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap();
    assert_ne!(stored, plaintext, "the plaintext token must not be stored");
    assert!(!stored.contains("wpp_"));

    let resolved = db::tokens::user_for_token(db, &plaintext)
        .await
        .unwrap()
        .expect("token resolves to its user");
    assert_eq!(resolved.id, user_id);

    assert!(
        db::tokens::user_for_token(db, "wpp_wrong")
            .await
            .unwrap()
            .is_none()
    );

    assert_eq!(db::tokens::list(db, user_id).await.unwrap().len(), 1);
    db::tokens::revoke(db, user_id, id).await.unwrap();
    assert!(db::tokens::list(db, user_id).await.unwrap().is_empty());
    assert!(
        db::tokens::user_for_token(db, &plaintext)
            .await
            .unwrap()
            .is_none(),
        "a revoked token must stop working"
    );
}

// ---------------------------------------------------------------------------
// servers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn servers_round_trip_including_the_certificate_pin() {
    let test = test_db().await;
    let db = &test.pool;

    let id = a_server(db, "node-1").await;
    let row = db::servers::get(db, id).await.unwrap().expect("server");

    assert_eq!(row.server.name, "node-1");
    assert_eq!(row.server.status, ServerStatus::Provisioning);
    assert_eq!(row.agent_fingerprint.as_deref(), Some("sha256:AA:BB"));
    assert_eq!(row.site_count, 0);
    assert_eq!(row.connection().url, "https://10.0.0.5:8443");
    assert_eq!(
        row.connection().fingerprint.as_deref(),
        Some("sha256:AA:BB"),
        "the pin must reach the agent client, or HTTPS agents are unusable"
    );

    db::servers::record_heartbeat(db, id, ServerStatus::Online, Some("9.9.9"), None)
        .await
        .unwrap();
    let row = db::servers::get(db, id).await.unwrap().unwrap();
    assert_eq!(row.server.status, ServerStatus::Online);
    assert_eq!(row.server.agent_version.as_deref(), Some("9.9.9"));
    assert!(row.server.last_seen_at.is_some());

    assert_eq!(db::servers::count(db).await.unwrap(), 1);
    db::servers::delete(db, id).await.unwrap();
    assert_eq!(db::servers::count(db).await.unwrap(), 0);
}

// ---------------------------------------------------------------------------
// sites, domains, backups
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sites_round_trip_and_uids_do_not_repeat() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;

    let first = a_site(db, server_id, "one.example.com").await;
    let second = a_site(db, server_id, "two.example.com").await;

    let one = db::sites::get(db, first).await.unwrap().expect("site");
    let two = db::sites::get(db, second).await.unwrap().expect("site");
    assert_ne!(one.site.uid, two.site.uid, "each site needs its own uid");
    assert_eq!(one.server_name, "node-1");
    assert_eq!(one.site.status, SiteStatus::Provisioning);

    assert!(
        db::sites::domain_exists(db, "one.example.com")
            .await
            .unwrap()
    );
    assert!(
        !db::sites::domain_exists(db, "nope.example.com")
            .await
            .unwrap()
    );
    assert_eq!(db::sites::count(db).await.unwrap(), 2);

    db::sites::set_status(db, first, SiteStatus::Online)
        .await
        .unwrap();
    db::sites::set_php(db, first, PhpVersion::Php82)
        .await
        .unwrap();
    db::sites::set_wp_version(db, first, "6.8").await.unwrap();
    db::sites::set_limits(
        db,
        first,
        ResourceLimits {
            cpu_cores: 2.0,
            memory_mb: 4096,
            php_workers: 24,
        },
    )
    .await
    .unwrap();
    db::sites::set_cache(
        db,
        first,
        CacheSettings {
            fastcgi_cache: false,
            ttl_seconds: 60,
            redis_object_cache: true,
            opcache: false,
            brotli: false,
        },
    )
    .await
    .unwrap();
    let expires = Utc::now() + Duration::days(30);
    db::sites::set_ssl(db, first, true, SslIssuer::LetsEncrypt, Some(expires))
        .await
        .unwrap();

    let one = db::sites::get(db, first).await.unwrap().unwrap();
    assert_eq!(one.site.status, SiteStatus::Online);
    assert_eq!(one.site.php_version, PhpVersion::Php82);
    assert_eq!(one.site.wp_version.as_deref(), Some("6.8"));
    assert_eq!(one.site.limits.memory_mb, 4096);
    assert!(!one.site.cache.fastcgi_cache);
    assert!(one.site.cache.redis_object_cache);
    assert!(one.site.ssl.enabled);
    assert_eq!(
        one.site.ssl.expires_at.map(|d| d.date_naive()),
        Some(expires.date_naive())
    );

    assert_eq!(
        db::sites::count_by_status(db, SiteStatus::Online)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        db::sites::list_for_server(db, server_id)
            .await
            .unwrap()
            .len(),
        2
    );

    db::sites::delete(db, second).await.unwrap();
    assert_eq!(db::sites::count(db).await.unwrap(), 1);
}

#[tokio::test]
async fn domains_track_the_primary_and_aliases() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;
    let site_id = a_site(db, server_id, "primary.example.com").await;

    // `create` registers the primary domain itself.
    let domains = db::sites::domains(db, site_id).await.unwrap();
    assert_eq!(domains.len(), 1);
    assert!(domains[0].primary);

    db::sites::add_domain(db, site_id, "www.primary.example.com", false)
        .await
        .unwrap();
    assert_eq!(db::sites::domains(db, site_id).await.unwrap().len(), 2);

    // The primary must survive an attempt to remove it.
    db::sites::remove_domain(db, site_id, "primary.example.com")
        .await
        .unwrap();
    assert_eq!(db::sites::domains(db, site_id).await.unwrap().len(), 2);

    db::sites::remove_domain(db, site_id, "www.primary.example.com")
        .await
        .unwrap();
    let remaining = db::sites::domains(db, site_id).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].primary);
}

#[tokio::test]
async fn syncing_the_same_snapshot_twice_does_not_duplicate_rows() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;
    let site_id = a_site(db, server_id, "backups.example.com").await;

    let snapshot = db::sites::NewBackup {
        snapshot_id: "abc123".into(),
        scope: BackupScope::Full,
        size_bytes: 100,
        files_bytes: 60,
        db_bytes: 40,
        restic_repo: Some("minio".into()),
    };

    let first = db::sites::upsert_backup(db, site_id, &snapshot)
        .await
        .unwrap();

    // Same snapshot, updated sizes: an upsert, not a second row.
    let grown = db::sites::NewBackup {
        size_bytes: 200,
        files_bytes: 150,
        db_bytes: 50,
        ..snapshot.clone()
    };
    let second = db::sites::upsert_backup(db, site_id, &grown).await.unwrap();
    assert_eq!(
        first, second,
        "upsert must reuse the row for a known snapshot"
    );

    let backups = db::sites::backups(db, site_id, 10).await.unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].size_bytes, 200);
    assert_eq!(backups[0].destination, "minio");

    // A node sync keeps the snapshot's own timestamp.
    let when = Utc::now() - Duration::days(3);
    db::sites::upsert_backup_at(
        db,
        site_id,
        &db::sites::NewBackup {
            snapshot_id: "older".into(),
            ..snapshot.clone()
        },
        when,
    )
    .await
    .unwrap();

    let backups = db::sites::backups(db, site_id, 10).await.unwrap();
    assert_eq!(backups.len(), 2);
    // Newest first.
    assert_eq!(backups[0].snapshot_id, "abc123");
    assert_eq!(backups[1].created_at.date_naive(), when.date_naive());
}

// ---------------------------------------------------------------------------
// jobs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_job_moves_from_queued_through_running_to_finished() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;
    let site_id = a_site(db, server_id, "jobs.example.com").await;

    let job_id = db::jobs::enqueue(
        db,
        JobKind::SiteCreate,
        Some(server_id),
        Some(site_id),
        Some(serde_json::json!({ "request_ssl": true })),
        "tester@example.com",
    )
    .await
    .unwrap();

    assert_eq!(db::jobs::count_active(db).await.unwrap(), 1);
    assert_eq!(
        db::jobs::payload(db, job_id).await.unwrap().unwrap()["request_ssl"],
        serde_json::json!(true)
    );

    let claimed = db::jobs::claim_next(db)
        .await
        .unwrap()
        .expect("claims the job");
    assert_eq!(claimed.job.id, job_id);
    assert_eq!(claimed.job.status, JobStatus::Running);
    assert_eq!(claimed.site_domain.as_deref(), Some("jobs.example.com"));
    assert_eq!(claimed.actor, "tester@example.com");

    // Claiming is exclusive: nothing else is queued now.
    assert!(db::jobs::claim_next(db).await.unwrap().is_none());

    db::jobs::progress(db, job_id, 50, "Halfway").await.unwrap();
    db::jobs::add_step(db, job_id, "Create filesystem", true, 120, None)
        .await
        .unwrap();
    db::jobs::add_step(db, job_id, "Issue TLS", false, 90, Some("dns not ready"))
        .await
        .unwrap();

    let steps = db::jobs::steps(db, job_id).await.unwrap();
    assert_eq!(steps.len(), 2);
    assert!(steps[0].ok);
    assert!(!steps[1].ok);
    assert_eq!(steps[1].detail.as_deref(), Some("dns not ready"));

    db::jobs::finish(db, job_id, JobStatus::Succeeded, "Completed", None)
        .await
        .unwrap();

    let done = db::jobs::get(db, job_id).await.unwrap().unwrap();
    assert_eq!(done.job.status, JobStatus::Succeeded);
    assert_eq!(done.job.progress, 100);
    assert!(done.job.finished_at.is_some());
    assert_eq!(db::jobs::count_active(db).await.unwrap(), 0);
    assert_eq!(
        db::jobs::list_for_site(db, site_id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn interrupted_jobs_are_failed_on_boot_not_left_running() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;
    let site_id = a_site(db, server_id, "orphan.example.com").await;

    db::jobs::enqueue(
        db,
        JobKind::BackupCreate,
        Some(server_id),
        Some(site_id),
        None,
        "x",
    )
    .await
    .unwrap();
    db::jobs::claim_next(db).await.unwrap().expect("claimed");

    assert_eq!(db::jobs::requeue_orphans(db).await.unwrap(), 1);

    let jobs = db::jobs::list(db, 10).await.unwrap();
    assert_eq!(jobs[0].job.status, JobStatus::Failed);
    assert!(jobs[0].job.error.is_some());
    assert_eq!(db::jobs::count_active(db).await.unwrap(), 0);
}

#[tokio::test]
async fn has_active_distinguishes_kinds_so_the_scheduler_cannot_pile_up_backups() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;
    let site_id = a_site(db, server_id, "sched.example.com").await;

    assert!(
        !db::jobs::has_active(db, site_id, JobKind::BackupCreate)
            .await
            .unwrap()
    );

    db::jobs::enqueue(
        db,
        JobKind::BackupCreate,
        Some(server_id),
        Some(site_id),
        None,
        "scheduler",
    )
    .await
    .unwrap();

    assert!(
        db::jobs::has_active(db, site_id, JobKind::BackupCreate)
            .await
            .unwrap()
    );
    assert!(
        !db::jobs::has_active(db, site_id, JobKind::CacheClear)
            .await
            .unwrap(),
        "a queued backup must not block unrelated kinds"
    );
}

// ---------------------------------------------------------------------------
// destinations and schedules
// ---------------------------------------------------------------------------

#[tokio::test]
async fn destination_secrets_are_sealed_and_resolve_to_a_restic_target() {
    let test = test_db().await;
    let db = &test.pool;
    let secrets = SecretBox::from_key([5u8; 32]);

    let created = db::destinations::create(
        db,
        db::destinations::DestinationForm {
            name: "minio".into(),
            provider: "minio".into(),
            bucket: "wp-backups".into(),
            region: "us-east-1".into(),
            endpoint: "http://127.0.0.1:9000".into(),
            access_key_id: "AKIA".into(),
            secret: "s3cret".into(),
            restic_password: "repo-pass".into(),
        },
        &secrets,
    )
    .await
    .unwrap();
    let id = created.id;

    let stored: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT secret_sealed, restic_password_sealed FROM backup_destinations WHERE id = ?1",
    )
    .bind(id)
    .fetch_one(db)
    .await
    .unwrap();

    for sealed in [stored.0.unwrap(), stored.1.unwrap()] {
        assert!(!sealed.contains("s3cret"), "secret stored in the clear");
        assert!(!sealed.contains("repo-pass"), "secret stored in the clear");
    }

    let dest = db::destinations::get(db, id).await.unwrap().unwrap();
    let creds = db::destinations::decrypt_credentials(&dest, &secrets).unwrap();
    assert_eq!(creds.secret, "s3cret");
    assert_eq!(creds.restic_password, "repo-pass");

    let target = db::destinations::restic_target(&dest, &secrets).unwrap();
    assert_eq!(target.repo, "s3:127.0.0.1:9000/wp-backups/wp");

    // A wrong key must not silently produce garbage credentials.
    let other = SecretBox::from_key([6u8; 32]);
    assert!(db::destinations::decrypt_credentials(&dest, &other).is_err());
}

#[tokio::test]
async fn schedules_become_due_and_are_pushed_forward_after_a_run() {
    let test = test_db().await;
    let db = &test.pool;
    let secrets = SecretBox::from_key([5u8; 32]);
    let server_id = a_server(db, "node-1").await;
    let site_id = a_site(db, server_id, "sched2.example.com").await;

    let dest_id = db::destinations::create(
        db,
        db::destinations::DestinationForm {
            name: "d".into(),
            provider: "minio".into(),
            bucket: "b".into(),
            region: String::new(),
            endpoint: "http://127.0.0.1:9000".into(),
            access_key_id: "k".into(),
            secret: "s".into(),
            restic_password: "p".into(),
        },
        &secrets,
    )
    .await
    .unwrap()
    .id;

    let policy = wp_common::models::RetentionPolicy::default();
    db::schedules::upsert(
        db,
        db::schedules::ScheduleForm {
            site_id,
            destination_id: Some(dest_id),
            scope: "full".into(),
            interval_minutes: 1440,
            enabled: true,
            keep_hourly: policy.hourly as i64,
            keep_daily: policy.daily as i64,
            keep_weekly: policy.weekly as i64,
            keep_monthly: policy.monthly as i64,
        },
    )
    .await
    .unwrap();

    // A fresh schedule has no next_run_at and must be treated as due.
    let due = db::schedules::due(db, &Utc::now().to_rfc3339())
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    let schedule = due[0].clone();
    assert_eq!(schedule.site_id, site_id);
    assert_eq!(schedule.interval_minutes, 1440);

    db::schedules::mark_scheduled(
        db,
        schedule.id,
        &Utc::now().to_rfc3339(),
        &(Utc::now() + Duration::minutes(1440)).to_rfc3339(),
    )
    .await
    .unwrap();

    assert!(
        db::schedules::due(db, &Utc::now().to_rfc3339())
            .await
            .unwrap()
            .is_empty(),
        "a scheduled backup must not fire again immediately"
    );

    // The site's destination is what a backup job resolves against.
    let resolved = db::destinations::for_site(db, &secrets, site_id)
        .await
        .unwrap();
    assert_eq!(resolved.expect("destination for site").0.id, dest_id);
}

// ---------------------------------------------------------------------------
// teams
// ---------------------------------------------------------------------------

#[tokio::test]
async fn site_visibility_follows_role_and_grants() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;
    let mine = a_site(db, server_id, "mine.example.com").await;
    let theirs = a_site(db, server_id, "theirs.example.com").await;

    let owner = a_user(db, "owner2@example.com", "owner").await;
    let viewer = a_user(db, "viewer@example.com", "viewer").await;

    assert!(db::teams::has_global_access("owner"));
    assert!(db::teams::has_global_access("admin"));
    assert!(!db::teams::has_global_access("operator"));
    assert!(!db::teams::has_global_access("viewer"));

    assert_eq!(
        db::sites::list_for_user(db, owner, "owner")
            .await
            .unwrap()
            .len(),
        2,
        "an owner sees every site"
    );
    assert!(
        db::sites::list_for_user(db, viewer, "viewer")
            .await
            .unwrap()
            .is_empty(),
        "a viewer with no grants sees nothing"
    );
    assert!(
        db::sites::get_for_user(db, mine, viewer, "viewer")
            .await
            .unwrap()
            .is_none()
    );

    db::teams::grant_site_access(db, mine, viewer, "viewer")
        .await
        .unwrap();

    let visible = db::sites::list_for_user(db, viewer, "viewer")
        .await
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].site.id, mine);
    assert!(
        db::sites::get_for_user(db, mine, viewer, "viewer")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        db::sites::get_for_user(db, theirs, viewer, "viewer")
            .await
            .unwrap()
            .is_none(),
        "a grant is per site, not a blanket unlock"
    );

    db::teams::revoke_site_access(db, mine, viewer)
        .await
        .unwrap();
    assert!(
        db::sites::list_for_user(db, viewer, "viewer")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn invitations_are_single_use() {
    let test = test_db().await;
    let db = &test.pool;

    let inviter = a_user(db, "inviter@example.com", "owner").await;
    let token = wp_panel::auth::random_token();
    db::teams::create_invitation(db, "new@example.com", "operator", &token, inviter, 7)
        .await
        .unwrap();

    let invite = db::teams::get_invitation_by_token(db, &token)
        .await
        .unwrap()
        .expect("invitation is valid");
    assert_eq!(invite.email, "new@example.com");
    assert_eq!(invite.role, "operator");

    db::teams::mark_invitation_used(db, invite.id)
        .await
        .unwrap();
    assert!(
        db::teams::get_invitation_by_token(db, &token)
            .await
            .unwrap()
            .is_none(),
        "an accepted invitation must not be reusable"
    );
}

// ---------------------------------------------------------------------------
// audit, notifications, metrics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_entries_are_listed_newest_first() {
    let test = test_db().await;
    let db = &test.pool;

    db::audit::record(db, "a@example.com", "auth.login", "panel", None, true)
        .await
        .unwrap();
    db::audit::record(
        db,
        "a@example.com",
        "site.create",
        "example.com",
        Some("job 1"),
        false,
    )
    .await
    .unwrap();

    let entries = db::audit::list(db, 10).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].action, "site.create");
    assert!(!entries[0].success);
    assert_eq!(entries[0].detail.as_deref(), Some("job 1"));
}

#[tokio::test]
async fn notifications_open_once_per_rule_and_resolve() {
    let test = test_db().await;
    let db = &test.pool;

    db::notifications::open_if_absent(db, "ssl.expiring", "warning", "example.com", "3 days left")
        .await
        .unwrap();
    // Same rule and target firing again must not stack up.
    db::notifications::open_if_absent(db, "ssl.expiring", "warning", "example.com", "2 days left")
        .await
        .unwrap();

    let open = db::notifications::list_open(db, 10).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(db::notifications::count_open(db).await.unwrap(), 1);

    db::notifications::resolve_rule(db, "ssl.expiring", "example.com")
        .await
        .unwrap();
    assert_eq!(db::notifications::count_open(db).await.unwrap(), 0);
}

#[tokio::test]
async fn metrics_history_is_recorded_and_trimmed() {
    let test = test_db().await;
    let db = &test.pool;
    let server_id = a_server(db, "node-1").await;
    let site_id = a_site(db, server_id, "metrics.example.com").await;

    for _ in 0..3 {
        db::metrics::insert_server(db, server_id, 10.0, 20.0, 30.0, 0.5, 2)
            .await
            .unwrap();
        db::metrics::insert_site(
            db,
            &wp_common::models::SiteMetricSample {
                site_id,
                cpu_percent: 1.5,
                memory_mb: 128,
                php_busy_workers: 2,
                cache_hit_ratio: Some(0.9),
            },
        )
        .await
        .unwrap();
    }

    let server_history = db::metrics::server_history(db, server_id, 24)
        .await
        .unwrap();
    assert_eq!(server_history.len(), 3);
    assert_eq!(server_history[0].cpu, 10.0);

    let site_history = db::metrics::site_history(db, site_id, 24).await.unwrap();
    assert_eq!(site_history.len(), 3);
    assert_eq!(site_history[0].memory_mb, 128);

    // Retention keeps recent rows.
    db::metrics::cleanup(db, 30).await.unwrap();
    assert_eq!(
        db::metrics::server_history(db, server_id, 24)
            .await
            .unwrap()
            .len(),
        3
    );
}
