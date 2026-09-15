//! HTTP-level tests against the real router.
//!
//! These cover the three defects the audit found above the repository layer:
//! CSRF verification rejecting valid tokens, RBAC gates missing on the JSON API,
//! and role checks that only existed on the HTML routes.
//!
//! Sessions are created directly in the database so the tests do not depend on
//! login throttling or 2FA state.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Duration;
use std::path::PathBuf;
use tower::ServiceExt;
use wp_common::models::{CacheSettings, DatabaseMode, Environment, PhpVersion, ResourceLimits};
use wp_panel::agent::AgentClient;
use wp_panel::config::Config;
use wp_panel::csrf::CsrfKey;
use wp_panel::secrets::SecretBox;
use wp_panel::state::AppState;
use wp_panel::{db, router};

/// Fixed key so tests can mint valid CSRF tokens for a session.
const CSRF_KEY: [u8; 32] = [0u8; 32];

struct TestApp {
    router: axum::Router,
    pool: db::Db,
    path: PathBuf,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

impl TestApp {
    async fn new() -> Self {
        // A file-backed database: `sqlite::memory:` is per-connection, so a
        // pooled router would see several different empty databases.
        let path = std::env::temp_dir().join(format!("wp-panel-http-{}.db", uuid::Uuid::new_v4()));
        let pool = db::connect(&path).await.expect("open database");

        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            database: path.clone(),
            workers: 1,
            admin_email: "admin@example.com".into(),
            admin_password: None,
            secure_cookies: false,
            demo_data: false,
            static_dir: "static".into(),
            smtp_host: None,
            smtp_port: 587,
            smtp_user: None,
            smtp_password: None,
            smtp_from: None,
        };

        let state = AppState::new(
            pool.clone(),
            config.clone(),
            AgentClient::new().expect("agent client"),
            CsrfKey(CSRF_KEY),
            SecretBox::from_key([1u8; 32]),
        );

        Self {
            router: router(state, &config),
            pool,
            path,
        }
    }

    /// Creates a user with `role` and returns `(session cookie, csrf token)`.
    async fn sign_in(&self, email: &str, role: &str) -> (String, String) {
        let hash = wp_panel::auth::hash_password("password-for-tests").expect("hash");
        let user_id = db::users::create(&self.pool, email, "Tester", &hash, role)
            .await
            .expect("create user");

        let session = wp_panel::auth::random_token();
        db::users::create_session(
            &self.pool,
            &session,
            user_id,
            "test-agent",
            "127.0.0.1",
            Duration::days(1),
        )
        .await
        .expect("create session");

        let csrf = CsrfKey(CSRF_KEY).token(&session);
        (format!("{}={session}", wp_panel::auth::COOKIE_NAME), csrf)
    }

    async fn get(&self, uri: &str, cookie: Option<&str>) -> axum::http::Response<Body> {
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        self.router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .expect("request")
    }

    async fn post(&self, uri: &str, cookie: &str, body: &str) -> axum::http::Response<Body> {
        self.router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .expect("request")
    }

    async fn a_site(&self, domain: &str) -> i64 {
        let server_id = db::servers::create(
            &self.pool,
            db::servers::NewServer {
                name: &format!("node-for-{domain}"),
                agent_url: "http://127.0.0.1:1",
                agent_token: "token-token-token-token-1234",
                hostname: "node",
                ip_address: "127.0.0.1",
                provider: None,
                region: None,
                agent_fingerprint: None,
            },
        )
        .await
        .expect("server");

        db::sites::create(
            &self.pool,
            db::sites::NewSite {
                server_id,
                domain,
                title: None,
                php_version: PhpVersion::Php84,
                database_mode: DatabaseMode::Shared,
                environment: Environment::Production,
                parent_site_id: None,
                limits: ResourceLimits::default(),
                cache: CacheSettings::default(),
                request_ssl: false,
            },
        )
        .await
        .expect("site")
    }
}

async fn body_string(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).into_owned()
}

// ---------------------------------------------------------------------------
// basics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn healthz_returns_ok() {
    let app = TestApp::new().await;
    assert_eq!(app.get("/healthz", None).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn unauthenticated_html_redirects_to_login() {
    let app = TestApp::new().await;
    let response = app.get("/", None).await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap(),
        "/login"
    );
}

#[tokio::test]
async fn login_page_renders() {
    let app = TestApp::new().await;
    let response = app.get("/login", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_string(response).await.contains("name=\"password\""));
}

#[tokio::test]
async fn unknown_paths_render_the_error_page() {
    let app = TestApp::new().await;
    let (cookie, _) = app.sign_in("owner@example.com", "owner").await;
    let response = app.get("/no/such/page", Some(&cookie)).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(body_string(response).await.contains("404"));
}

#[tokio::test]
async fn a_session_cookie_authenticates_html_routes() {
    let app = TestApp::new().await;
    let (cookie, _) = app.sign_in("owner@example.com", "owner").await;

    for path in ["/", "/sites", "/servers", "/jobs", "/audit", "/settings"] {
        assert_eq!(
            app.get(path, Some(&cookie)).await.status(),
            StatusCode::OK,
            "{path} should render for a signed-in owner"
        );
    }
}

// ---------------------------------------------------------------------------
// CSRF
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_post_without_a_csrf_token_is_rejected() {
    let app = TestApp::new().await;
    let (cookie, _) = app.sign_in("owner@example.com", "owner").await;
    let site = app.a_site("csrf.example.com").await;

    let response = app
        .post(&format!("/sites/{site}/actions/restart"), &cookie, "")
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_post_with_a_valid_csrf_token_is_accepted() {
    let app = TestApp::new().await;
    let (cookie, csrf) = app.sign_in("owner@example.com", "owner").await;
    let site = app.a_site("csrf-ok.example.com").await;

    let response = app
        .post(
            &format!("/sites/{site}/actions/restart"),
            &cookie,
            &format!("csrf_token={csrf}"),
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "a valid token must pass: this regressed once and made the whole UI read-only"
    );
}

#[tokio::test]
async fn a_csrf_token_from_another_session_is_rejected() {
    let app = TestApp::new().await;
    let (cookie, _) = app.sign_in("owner@example.com", "owner").await;
    let (_, other_csrf) = app.sign_in("other@example.com", "owner").await;
    let site = app.a_site("csrf-mix.example.com").await;

    let response = app
        .post(
            &format!("/sites/{site}/actions/restart"),
            &cookie,
            &format!("csrf_token={other_csrf}"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn rendered_forms_carry_a_token_that_the_middleware_accepts() {
    let app = TestApp::new().await;
    let (cookie, _) = app.sign_in("owner@example.com", "owner").await;
    let site = app.a_site("form.example.com").await;

    // Take the token out of the page exactly as a browser would.
    let page = body_string(app.get(&format!("/sites/{site}"), Some(&cookie)).await).await;
    let token = page
        .split("name=\"csrf_token\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a rendered page must contain a CSRF token");

    let response = app
        .post(
            &format!("/sites/{site}/actions/restart"),
            &cookie,
            &format!("csrf_token={token}"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}

// ---------------------------------------------------------------------------
// RBAC on HTML routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_viewer_cannot_mutate_anything() {
    let app = TestApp::new().await;
    let (cookie, csrf) = app.sign_in("viewer@example.com", "viewer").await;
    let site = app.a_site("viewer.example.com").await;
    let body = format!("csrf_token={csrf}");

    for path in [
        format!("/sites/{site}/actions/restart"),
        format!("/sites/{site}/actions/backup"),
        format!("/sites/{site}/cache"),
        format!("/sites/{site}/limits"),
        format!("/sites/{site}/console"),
        format!("/sites/{site}/staging/create"),
    ] {
        let status = app.post(&path, &cookie, &body).await.status();
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{path} must be forbidden for a viewer, got {status}"
        );
    }
}

#[tokio::test]
async fn a_viewer_cannot_reach_team_management() {
    let app = TestApp::new().await;
    let (cookie, _) = app.sign_in("viewer2@example.com", "viewer").await;

    assert_eq!(
        app.get("/settings/users", Some(&cookie)).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn an_operator_may_act_but_only_on_granted_sites() {
    let app = TestApp::new().await;
    let (cookie, csrf) = app.sign_in("operator@example.com", "operator").await;
    let granted = app.a_site("granted.example.com").await;
    let other = app.a_site("other.example.com").await;

    let operator = db::users::by_email(&app.pool, "operator@example.com")
        .await
        .unwrap()
        .unwrap();
    db::teams::grant_site_access(&app.pool, granted, operator.id, "operator")
        .await
        .unwrap();

    assert_eq!(
        app.post(
            &format!("/sites/{granted}/actions/restart"),
            &cookie,
            &format!("csrf_token={csrf}")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );

    assert_eq!(
        app.post(
            &format!("/sites/{other}/actions/restart"),
            &cookie,
            &format!("csrf_token={csrf}")
        )
        .await
        .status(),
        StatusCode::NOT_FOUND,
        "a site the operator was not granted must not even be visible"
    );
}

// ---------------------------------------------------------------------------
// JSON API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_without_auth_returns_401() {
    let app = TestApp::new().await;
    assert_eq!(
        app.get("/api/v1/servers", None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get("/api/v1/sites", None).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn api_accepts_a_bearer_token_and_rejects_a_bogus_one() {
    let app = TestApp::new().await;
    let hash = wp_panel::auth::hash_password("password-for-tests").unwrap();
    let user_id = db::users::create(&app.pool, "api@example.com", "API", &hash, "owner")
        .await
        .unwrap();
    let (_, plaintext) = db::tokens::create(&app.pool, user_id, "cli").await.unwrap();

    let with_token = |token: String| {
        let router = app.router.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/sites")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .expect("request")
                .status()
        }
    };

    assert_eq!(with_token(plaintext).await, StatusCode::OK);
    assert_eq!(
        with_token("wpp_not_a_real_token".to_string()).await,
        StatusCode::UNAUTHORIZED
    );
}

/// The API used to answer every question for every caller.
#[tokio::test]
async fn the_api_is_scoped_to_what_the_caller_may_see() {
    let app = TestApp::new().await;
    let granted = app.a_site("api-granted.example.com").await;
    let _hidden = app.a_site("api-hidden.example.com").await;

    let (owner_cookie, _) = app.sign_in("api-owner@example.com", "owner").await;
    let (viewer_cookie, _) = app.sign_in("api-viewer@example.com", "viewer").await;

    let viewer = db::users::by_email(&app.pool, "api-viewer@example.com")
        .await
        .unwrap()
        .unwrap();

    // Before any grant the viewer sees nothing.
    let body = body_string(app.get("/api/v1/sites", Some(&viewer_cookie)).await).await;
    assert_eq!(
        body.trim(),
        "[]",
        "a viewer without grants must see no sites"
    );

    db::teams::grant_site_access(&app.pool, granted, viewer.id, "viewer")
        .await
        .unwrap();

    let body = body_string(app.get("/api/v1/sites", Some(&viewer_cookie)).await).await;
    assert!(body.contains("api-granted.example.com"));
    assert!(
        !body.contains("api-hidden.example.com"),
        "the API must not leak sites the caller has no grant for: {body}"
    );

    // The owner still sees both.
    let body = body_string(app.get("/api/v1/sites", Some(&owner_cookie)).await).await;
    assert!(body.contains("api-granted.example.com"));
    assert!(body.contains("api-hidden.example.com"));

    // Direct lookups are scoped too.
    assert_eq!(
        app.get(&format!("/api/v1/sites/{granted}"), Some(&viewer_cookie))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.get(&format!("/api/v1/sites/{_hidden}"), Some(&viewer_cookie))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn api_server_listings_are_scoped_as_well() {
    let app = TestApp::new().await;
    let granted = app.a_site("srv-granted.example.com").await;
    let _hidden = app.a_site("srv-hidden.example.com").await;

    let (viewer_cookie, _) = app.sign_in("srv-viewer@example.com", "viewer").await;
    let viewer = db::users::by_email(&app.pool, "srv-viewer@example.com")
        .await
        .unwrap()
        .unwrap();
    db::teams::grant_site_access(&app.pool, granted, viewer.id, "viewer")
        .await
        .unwrap();

    let body = body_string(app.get("/api/v1/servers", Some(&viewer_cookie)).await).await;
    assert!(body.contains("node-for-srv-granted.example.com"));
    assert!(
        !body.contains("node-for-srv-hidden.example.com"),
        "servers hosting only invisible sites must stay hidden: {body}"
    );
}

/// A viewer must be refused before any extractor runs, so the answer is 403 for
/// every shape of request body — not 422 from a form that failed to parse.
#[tokio::test]
async fn a_viewer_is_refused_regardless_of_the_request_body() {
    let app = TestApp::new().await;
    let (cookie, csrf) = app.sign_in("viewer3@example.com", "viewer").await;
    let site = app.a_site("viewer-body.example.com").await;

    for body in [
        format!("csrf_token={csrf}"),
        format!("csrf_token={csrf}&cpu_cores=notanumber"),
        format!("csrf_token={csrf}&cpu_cores=2&memory_mb=2048&php_workers=8"),
    ] {
        let status = app
            .post(&format!("/sites/{site}/limits"), &cookie, &body)
            .await
            .status();
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "expected 403 for a viewer, got {status} with body `{body}`"
        );
    }
}

/// The refusal is audit-logged: a read-only account probing endpoints should be
/// visible to an operator afterwards.
#[tokio::test]
async fn a_refused_mutation_is_recorded_in_the_audit_log() {
    let app = TestApp::new().await;
    let (cookie, csrf) = app.sign_in("viewer4@example.com", "viewer").await;
    let site = app.a_site("viewer-audit.example.com").await;

    app.post(
        &format!("/sites/{site}/actions/restart"),
        &cookie,
        &format!("csrf_token={csrf}"),
    )
    .await;

    let entries = db::audit::list(&app.pool, 10).await.unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.action == "auth.role_reject" && !e.success),
        "the rejected mutation should appear in the audit log: {:?}",
        entries.iter().map(|e| &e.action).collect::<Vec<_>>()
    );
}
