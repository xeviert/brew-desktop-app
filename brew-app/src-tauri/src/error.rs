//! The error boundary. `brewcore` returns `thiserror` enums, which are not
//! `Serialize`; commands must return something the frontend can read. Converting
//! here — rather than letting commands return strings — keeps `tauri::Error` out of
//! core and gives the UI a stable `code` to branch on.

#[derive(Debug, serde::Serialize)]
pub struct ApiError {
    /// Stable identifier. Messages change; this must not.
    pub code: String,
    /// For humans, on screen.
    pub message: String,
    /// For the log. May be verbose.
    pub detail: Option<String>,
}

impl From<brewcore::Error> for ApiError {
    fn from(err: brewcore::Error) -> Self {
        Self {
            code: err.code().to_owned(),
            message: err.to_string(),
            detail: Some(format!("{err:?}")),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
