use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Handler error type. Anything that bubbles up here is logged and rendered as
/// a small, styled error page (or JSON for API routes).
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Forbidden(String),

    #[error("agent error: {0}")]
    Agent(#[from] wp_common::Error),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("template error: {0}")]
    Template(#[from] askama::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Agent(e) => {
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            Self::Database(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        } else {
            tracing::debug!(error = %self, "request rejected");
        }

        let body = crate::web::render_error(status, &self.to_string());
        (status, body).into_response()
    }
}
