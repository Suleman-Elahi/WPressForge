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
    /// SHA-256 fingerprint of the agent's TLS certificate, printed by the agent
    /// at startup. Required for any `https://` agent URL.
    #[serde(default)]
    pub agent_fingerprint: String,
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
    let agent_url = form.agent_url.trim();
    if !agent_url.starts_with("http") {
        return Err(AppError::BadRequest(
            "agent URL must start with http:// or https://".into(),
        ));
    }

    let fingerprint = normalise_fingerprint(form.agent_fingerprint.trim())?;

    // The token authenticates the panel to the agent; the pin authenticates the
    // agent to the panel. Without it an HTTPS agent cannot be verified at all,
    // so refuse at the form instead of storing a server that can never connect.
    if agent_url.starts_with("https:") && fingerprint.is_none() {
        return Err(AppError::BadRequest(
            "pin the agent certificate first: paste the sha256 fingerprint the \
             agent prints at startup"
                .into(),
        ));
    }

    // Plain HTTP is only acceptable on loopback, where there is no network to
    // intercept.
    if agent_url.starts_with("http:") && !is_loopback_url(agent_url) {
        return Err(AppError::BadRequest(
            "plain http:// is only allowed for 127.0.0.1 or [::1]; use https:// \
             with a pinned fingerprint"
                .into(),
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
            agent_fingerprint: fingerprint.as_deref(),
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
            url: agent_url.to_owned(),
            token: form.agent_token.trim().to_owned(),
            fingerprint: fingerprint.clone(),
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

/// Accepts `sha256:AB:CD:...`, `AB:CD:...` or bare hex, and normalises to the
/// `sha256:AA:BB:...` form the verifier expects. Empty input means "no pin".
fn normalise_fingerprint(raw: &str) -> Result<Option<String>, AppError> {
    if raw.is_empty() {
        return Ok(None);
    }

    let hex: String = raw
        .trim()
        .trim_start_matches("sha256:")
        .trim_start_matches("SHA256:")
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();

    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest(
            "fingerprint must be 64 hex characters (a SHA-256 digest)".into(),
        ));
    }

    let grouped: Vec<String> = hex
        .to_uppercase()
        .as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect();

    Ok(Some(format!("sha256:{}", grouped.join(":"))))
}

/// True for URLs whose host is a loopback address.
fn is_loopback_url(url: &str) -> bool {
    let rest = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host = rest.split(['/', '?']).next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") || host.starts_with("127.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_fingerprint_spellings() {
        let hex = "68eeb67fd7d8afc4a5c1884737f26f0322a7c394562ef01596e70145ddc3c2a5";
        let expected = normalise_fingerprint(hex).unwrap().unwrap();

        assert!(expected.starts_with("sha256:68:EE:B6:"));
        assert_eq!(normalise_fingerprint(&expected).unwrap().unwrap(), expected);
        assert_eq!(
            normalise_fingerprint(&format!("SHA256:{hex}"))
                .unwrap()
                .unwrap(),
            expected
        );
        assert_eq!(normalise_fingerprint("").unwrap(), None);
    }

    #[test]
    fn rejects_malformed_fingerprints() {
        assert!(normalise_fingerprint("abc").is_err());
        assert!(normalise_fingerprint(&"z".repeat(64)).is_err());
    }

    #[test]
    fn recognises_loopback_urls() {
        assert!(is_loopback_url("http://127.0.0.1:8443"));
        assert!(is_loopback_url("http://localhost:8443/v1"));
        assert!(is_loopback_url("http://[::1]:8443"));
        assert!(!is_loopback_url("http://10.0.0.9:8443"));
        assert!(!is_loopback_url("http://example.com"));
    }
}
