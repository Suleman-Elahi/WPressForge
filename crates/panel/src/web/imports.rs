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
    #[serde(default)]
    pub ssh_pass: String,
    #[serde(default)]
    pub ssh_key: String,
    pub remote_path: String,
}

#[derive(Template)]
#[template(path = "imports/inspect.html")]
struct InspectTemplate {
    chrome: Chrome,
    form: InspectForm,
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

    Ok(render(InspectTemplate {
        chrome: Chrome::new(&state, &user, &session, "sites", "Import Inspection", None).await,
        form,
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
    #[serde(default)]
    pub ssh_pass: String,
    #[serde(default)]
    pub ssh_key: String,
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

    let auth = if form.ssh_auth == "password" {
        SshAuth::Password(form.ssh_pass.clone())
    } else {
        SshAuth::PrivateKey(form.ssh_key.clone())
    };

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

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::ImportRun,
        Some(form.server_id),
        Some(site_id),
        Some(json!({
            "source": source,
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
