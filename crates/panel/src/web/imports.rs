use super::{Chrome, redirect_with_flash, render};
use crate::auth::{CurrentSession, CurrentUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use askama::Template;
use axum::extract::{Form, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use wp_common::models::{
    CacheSettings, DatabaseMode, Environment, JobKind, PhpVersion, ResourceLimits,
};
use wp_common::protocol::{ImportSource, Operation, SshAuth};

#[derive(Template)]
#[template(path = "imports/new.html")]
struct NewTemplate {
    chrome: Chrome,
    servers: Vec<db::servers::ServerRow>,
}

pub async fn new_form(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
) -> AppResult<Response> {
    if user.0.role == "viewer" {
        return Err(AppError::Forbidden(
            "Read-only viewer cannot import sites".into(),
        ));
    }
    let servers = db::servers::list(&state.db).await?;
    Ok(render(NewTemplate {
        chrome: Chrome::new(&state, &user, &session, "sites", "Import site", None).await,
        servers,
    }))
}

#[derive(Deserialize)]
pub struct InspectForm {
    pub server_id: i64,
    pub domain: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    pub ssh_auth: String,
    /// Present only on this first POST; never rendered back to the browser.
    #[serde(default)]
    pub ssh_pass: String,
    #[serde(default)]
    pub ssh_key: String,
    pub remote_path: String,
}

/// The non-secret half of the wizard state, safe to render as hidden inputs.
pub struct InspectContext {
    pub server_id: i64,
    pub domain: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    pub ssh_auth: String,
    pub remote_path: String,
    /// Opaque reference to the SSH secret held in [`crate::credentials`].
    pub credential_handle: String,
}

#[derive(Template)]
#[template(path = "imports/inspect.html")]
struct InspectTemplate {
    chrome: Chrome,
    form: InspectContext,
    wp_version: String,
    php_version: String,
    size_mb: u64,
    db_name: Option<String>,
    db_user: Option<String>,
}

pub async fn inspect(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Form(form): Form<InspectForm>,
) -> AppResult<Response> {
    if user.0.role == "viewer" {
        return Err(AppError::Forbidden(
            "Read-only viewer cannot import sites".into(),
        ));
    }
    let server = db::servers::get(&state.db, form.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let auth = if form.ssh_auth == "password" {
        SshAuth::Password(form.ssh_pass.clone())
    } else {
        SshAuth::PrivateKey(form.ssh_key.clone())
    };

    let source = ImportSource {
        host: form.ssh_host.clone(),
        port: form.ssh_port,
        user: form.ssh_user.clone(),
        auth,
        remote_path: form.remote_path.clone(),
        db: None,
    };

    let result = state
        .agent
        .query(
            &server.connection(),
            Operation::InspectImportSource { source },
        )
        .await;
    let data = match result {
        Ok(wp_common::protocol::OperationData::ImportInspection {
            wp_version,
            php_version,
            size_mb,
            db_name,
            db_user,
        }) => (wp_version, php_version, size_mb, db_name, db_user),
        Ok(_) => {
            return Err(AppError::BadRequest(
                "agent returned wrong data type".into(),
            ));
        }
        Err(e) => return Err(AppError::BadRequest(format!("Inspection failed: {e}"))),
    };

    // Keep the SSH secret in the panel's memory and hand the browser a handle.
    let secret = match form.ssh_auth.as_str() {
        "password" => json!({ "password": form.ssh_pass }),
        _ => json!({ "private_key": form.ssh_key }),
    };
    let credential_handle = state.credentials.put(user.0.id, secret.to_string());

    Ok(render(InspectTemplate {
        chrome: Chrome::new(&state, &user, &session, "sites", "Import Inspection", None).await,
        form: InspectContext {
            server_id: form.server_id,
            domain: form.domain,
            ssh_host: form.ssh_host,
            ssh_port: form.ssh_port,
            ssh_user: form.ssh_user,
            ssh_auth: form.ssh_auth,
            remote_path: form.remote_path,
            credential_handle,
        },
        wp_version: data.0,
        php_version: data.1,
        size_mb: data.2,
        db_name: data.3,
        db_user: data.4,
    }))
}

#[derive(Deserialize)]
pub struct CreateForm {
    pub server_id: i64,
    pub domain: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    pub ssh_auth: String,
    /// Handle issued by [`inspect`]; the secret itself never leaves the server.
    pub credential_handle: String,
    pub remote_path: String,
    pub db_host: String,
    pub db_port: u16,
    pub db_name: String,
    pub db_user: String,
    pub db_pass: String,
    pub php_version: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<CreateForm>,
) -> AppResult<Response> {
    if user.0.role == "viewer" {
        return Err(AppError::Forbidden(
            "Read-only viewer cannot import sites".into(),
        ));
    }
    let domain = form.domain.trim().to_lowercase();
    if db::sites::domain_exists(&state.db, &domain).await? {
        return Err(AppError::BadRequest(format!("{domain} already exists")));
    }

    // Recover the SSH secret from the stash before anything is written: an
    // expired or foreign handle must fail loudly, not silently try a
    // passwordless connection, and must not leave an orphan site row behind.
    let stashed = state
        .credentials
        .take(user.0.id, &form.credential_handle)
        .ok_or_else(|| {
            AppError::BadRequest(
                "This import session expired. Start again from the connection step.".into(),
            )
        })?;
    let stashed: serde_json::Value = serde_json::from_str(&stashed)
        .map_err(|_| AppError::BadRequest("Malformed import session".into()))?;

    let auth = match stashed.get("private_key").and_then(|v| v.as_str()) {
        Some(key) if !key.is_empty() => SshAuth::PrivateKey(key.to_string()),
        _ => SshAuth::Password(
            stashed
                .get("password")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
    };

    let php_version = PhpVersion::parse(&form.php_version).unwrap_or(PhpVersion::Php84);

    let site_id = db::sites::create(
        &state.db,
        db::sites::NewSite {
            server_id: form.server_id,
            domain: &domain,
            title: Some(&domain),
            php_version,
            database_mode: DatabaseMode::Shared,
            environment: Environment::Production,
            parent_site_id: None,
            limits: ResourceLimits::default(),
            cache: CacheSettings::default(),
            request_ssl: true,
        },
    )
    .await?;

    if !db::teams::has_global_access(&user.0.role) {
        let _ = db::teams::grant_site_access(&state.db, site_id, user.0.id, "operator").await;
    }

    let source = ImportSource {
        host: form.ssh_host,
        port: form.ssh_port,
        user: form.ssh_user,
        auth,
        remote_path: form.remote_path,
        db: Some(wp_common::protocol::RemoteDb {
            host: form.db_host,
            port: form.db_port,
            name: form.db_name,
            user: form.db_user,
            password: form.db_pass,
        }),
    };

    // The payload is persisted, so the source (SSH key/password plus the remote
    // database password) is sealed with the panel key rather than written as
    // plaintext into the jobs table.
    let sealed = state
        .secrets
        .seal(&serde_json::to_string(&source).map_err(|e| AppError::Other(e.into()))?)?;

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::ImportRun,
        Some(form.server_id),
        Some(site_id),
        Some(json!({
            "source_sealed": sealed,
            "resync": false,
        })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        "site.import",
        &domain,
        Some(&format!("job {job_id}")),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/jobs/{job_id}"),
        &format!("Importing {domain}"),
    ))
}
