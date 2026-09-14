use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use wp_panel::agent::AgentClient;
use wp_panel::config::Config;
use wp_panel::csrf::CsrfKey;
use wp_panel::router;
use wp_panel::secrets::SecretBox;
use wp_panel::state::AppState;

async fn test_app() -> axum::Router {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .expect("memory db");

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrations");

    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: "test.db".into(),
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

    let agent = AgentClient::new().expect("agent client");
    let csrf = CsrfKey([0u8; 32]);
    let secrets = SecretBox::from_key([1u8; 32]);
    let state = AppState::new(pool, config.clone(), agent, csrf, secrets);

    router(state, &config)
}

#[tokio::test]
async fn healthz_returns_ok() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn unauthenticated_redirects_to_login() {
    let app = test_app().await;

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

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
async fn login_page_renders_ok() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_without_auth_returns_401() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/servers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
