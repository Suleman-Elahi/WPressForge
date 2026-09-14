use super::{Chrome, FlashQuery, redirect_with_flash, render};
use crate::auth::{CurrentSession, CurrentUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use serde::Deserialize;
use wp_common::models::ServerStatus;

#[derive(Template)]
#[template(path = "servers/list.html")]
struct ListTemplate {
    chrome: Chrome,
    servers: Vec<db::servers::ServerRow>,
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let servers = db::servers::list(&state.db).await?;
    Ok(render(ListTemplate {
        chrome: Chrome::new(&state, &user, &session, "servers", "Servers", query.flash).await,
        servers,
    }))
}

#[derive(Template)]
#[template(path = "servers/detail.html")]
struct DetailTemplate {
    chrome: Chrome,
    server: db::servers::ServerRow,
    sites: Vec<db::sites::SiteRow>,
    jobs: Vec<db::jobs::JobRow>,
    cpu_spark: Spark,
    memory_spark: Spark,
    disk_spark: Spark,
}

pub async fn detail(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let server = db::servers::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let sites = db::sites::list_for_server(&state.db, id).await?;
    let jobs = db::jobs::list(&state.db, 6).await?;

    // Fetch 24h metrics history for sparklines.
    let history = db::metrics::server_history(&state.db, id, 24)
        .await
        .unwrap_or_default();
    let cpu_vals: Vec<f32> = history.iter().map(|r| r.cpu as f32).collect();
    let mem_vals: Vec<f32> = history.iter().map(|r| r.memory as f32).collect();
    let disk_vals: Vec<f32> = history.iter().map(|r| r.disk as f32).collect();

    Ok(render(DetailTemplate {
        chrome: Chrome::new(
            &state,
            &user,
            &session,
            "servers",
            server.server.name.clone(),
            query.flash,
        )
        .await,
        server,
        sites,
        jobs,
        cpu_spark: spark_from_values(&cpu_vals),
        memory_spark: spark_from_values(&mem_vals),
        disk_spark: spark_from_values(&disk_vals),
    }))
}

#[derive(Template)]
#[template(path = "servers/new.html")]
struct NewTemplate {
    chrome: Chrome,
    /// Token the new agent must be installed with.
    suggested_token: String,
}

pub async fn new_form(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    Ok(render(NewTemplate {
        chrome: Chrome::new(
            &state,
            &user,
            &session,
            "servers",
            "Attach server",
            query.flash,
        )
        .await,
        suggested_token: crate::auth::random_token(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct NewServerForm {
    pub name: String,
    pub agent_url: String,
    pub agent_token: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub ip_address: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub region: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<NewServerForm>,
) -> AppResult<Response> {
    let name = form.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("server name is required".into()));
    }
    if !form.agent_url.starts_with("http") {
        return Err(AppError::BadRequest(
            "agent URL must start with http:// or https://".into(),
        ));
    }

    let id = db::servers::create(
        &state.db,
        db::servers::NewServer {
            name,
            agent_url: form.agent_url.trim(),
            agent_token: form.agent_token.trim(),
            hostname: form.hostname.trim(),
            ip_address: form.ip_address.trim(),
            provider: Some(form.provider.trim()).filter(|s| !s.is_empty()),
            region: Some(form.region.trim()).filter(|s| !s.is_empty()),
            agent_fingerprint: None,
        },
    )
    .await?;

    db::audit::record(
        &state.db,
        &user.0.email,
        "server.create",
        name,
        Some(&form.agent_url),
        true,
    )
    .await?;

    // Probe the agent immediately so the operator gets feedback now rather than
    // at the next heartbeat.
    let flash = match state
        .agent
        .ping(&crate::agent::ServerConnection {
            url: form.agent_url.trim().to_owned(),
            token: form.agent_token.trim().to_owned(),
            fingerprint: None,
        })
        .await
    {
        Ok(result) => {
            let version = match &result.data {
                wp_common::protocol::OperationData::Pong { agent_version, .. } => {
                    Some(agent_version.clone())
                }
                _ => None,
            };
            db::servers::record_heartbeat(
                &state.db,
                id,
                ServerStatus::Online,
                version.as_deref(),
                None,
            )
            .await?;
            "Server attached and the agent answered.".to_string()
        }
        Err(error) => {
            db::servers::record_heartbeat(&state.db, id, ServerStatus::Offline, None, None).await?;
            format!("Server saved, but the agent did not answer: {error}")
        }
    };

    Ok(redirect_with_flash(&format!("/servers/{id}"), &flash))
}

pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let server = db::servers::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if server.site_count > 0 {
        return Err(AppError::BadRequest(
            "detach or delete the sites on this server first".into(),
        ));
    }

    db::servers::delete(&state.db, id).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "server.delete",
        &server.server.name,
        None,
        true,
    )
    .await?;

    Ok(redirect_with_flash("/servers", "Server detached."))
}

// ---------------------------------------------------------------------------
// HTMX fragment: live metrics tiles
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "servers/metrics.html")]
struct MetricsFragment {
    server: db::servers::ServerRow,
}

pub async fn metrics_fragment(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let server = db::servers::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(super::no_store(render(MetricsFragment { server })))
}

// ---------------------------------------------------------------------------
// SVG sparkline renderer
// ---------------------------------------------------------------------------

pub struct Spark {
    pub points: String,
    pub last: String,
    pub tone: &'static str,
}

impl Spark {
    pub fn from_values(values: &[f32], width: f32, height: f32) -> Self {
        if values.is_empty() {
            return Self {
                points: String::new(),
                last: "0".into(),
                tone: "ok",
            };
        }

        let last_val = *values.last().unwrap();
        let tone = if last_val > 85.0 {
            "bad"
        } else if last_val > 60.0 {
            "warn"
        } else {
            "ok"
        };

        let n = values.len();
        let max = values.iter().copied().fold(f32::MIN, f32::max).max(1.0);
        let min = values.iter().copied().fold(f32::MAX, f32::min).min(0.0);
        let range = (max - min).max(1.0);

        let points: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let x = if n == 1 {
                    width / 2.0
                } else {
                    i as f32 * width / (n - 1) as f32
                };
                let y = height - ((v - min) / range * height);
                format!("{x:.1},{y:.1}")
            })
            .collect();

        Self {
            points: points.join(" "),
            last: format!("{:.0}", last_val),
            tone,
        }
    }
}

pub fn spark_from_values(values: &[f32]) -> Spark {
    Spark::from_values(values, 120.0, 28.0)
}
