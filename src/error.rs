use thiserror::Error;

#[derive(Error, Debug)]
pub enum MinerError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Bad credentials")]
    BadCredentials,

    #[error("Invalid cookies / session")]
    WrongCookies,

    #[error("Streamer does not exist: {0}")]
    StreamerNotFound(String),

    #[error("GQL error: {0}")]
    Gql(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, MinerError>;
