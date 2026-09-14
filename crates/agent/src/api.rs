//! The agent's HTTP surface: one authenticated endpoint that accepts typed
//! operations, plus a health probe.

use crate::ops;
use crate::state::AgentState;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use wp_common::Error;
use wp_common::protocol::{OperationEnvelope, OperationResult};

/// Replayed request ids are answered from here instead of being executed twice.
static SEEN: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
const IDEMPOTENCY_WINDOW: Duration = Duration::from_secs(600);

pub fn router(state: AgentState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/operations", post(operations))
        .with_state(state)
}

async fn operations(
    State(state): State<AgentState>,
    headers: header::HeaderMap,
    body: Result<Json<OperationEnvelope>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if !authorised(&headers, &state.config.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(OperationResult::err(Error::Unauthorized)),
        )
            .into_response();
    }

    let Json(envelope) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(OperationResult::err(Error::Invalid(rejection.body_text()))),
            )
                .into_response();
        }
    };

    if envelope.protocol_version != wp_common::PROTOCOL_VERSION {
        return (
            StatusCode::CONFLICT,
            Json(OperationResult::err(Error::Invalid(format!(
                "protocol version {} is not supported (agent speaks {})",
                envelope.protocol_version,
                wp_common::PROTOCOL_VERSION
            )))),
        )
            .into_response();
    }

    if already_handled(&envelope.request_id) {
        tracing::info!(request_id = %envelope.request_id, "duplicate request ignored");
        return Json(OperationResult::ok(Default::default())).into_response();
    }

    // Privileged work happens one at a time.
    let _permit = state.lock.clone().acquire_owned().await;
    let result = ops::dispatch(&state, envelope.operation).await;

    let status = if result.success {
        StatusCode::OK
    } else {
        result
            .error
            .as_ref()
            .and_then(|e| StatusCode::from_u16(e.status_code()).ok())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    };

    (status, Json(result)).into_response()
}

/// Constant-time-ish comparison of the bearer token.
fn authorised(headers: &header::HeaderMap, expected: &str) -> bool {
    let Some(provided) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };

    if provided.len() != expected.len() {
        return false;
    }

    provided
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn already_handled(request_id: &str) -> bool {
    let mut guard = SEEN.lock().expect("idempotency map poisoned");
    let map = guard.get_or_insert_with(HashMap::new);

    map.retain(|_, seen| seen.elapsed() < IDEMPOTENCY_WINDOW);

    if map.contains_key(request_id) {
        true
    } else {
        map.insert(request_id.to_string(), Instant::now());
        false
    }
}
