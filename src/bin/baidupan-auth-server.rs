use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use baidupan_cli::auth::{DeviceCodeResponse, DirectOAuthClient};
use baidupan_cli::config::{AppCredentials, StoredToken};
use baidupan_cli::{Error, Result};
use serde::Deserialize;

const AUTH_SERVER_BIND_ENV: &str = "BAIDUPAN_AUTH_SERVER_BIND";
const DEFAULT_AUTH_SERVER_BIND: &str = "127.0.0.1:28681";

#[derive(Clone)]
struct AppState {
    oauth: DirectOAuthClient,
    credentials: AppCredentials,
}

#[derive(Debug, Deserialize)]
struct PollTokenRequest {
    device_code: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct RefreshTokenRequest {
    refresh_token: String,
}

type HandlerResult<T> = std::result::Result<Json<T>, (StatusCode, String)>;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .init();

    let credentials = AppCredentials::from_direct_env()?;
    let oauth = DirectOAuthClient::new()?;
    let bind = std::env::var(AUTH_SERVER_BIND_ENV)
        .unwrap_or_else(|_| DEFAULT_AUTH_SERVER_BIND.to_string());
    let address: SocketAddr = bind.parse().map_err(|error| {
        Error::InvalidConfig(format!("invalid {AUTH_SERVER_BIND_ENV}: {error}"))
    })?;

    let state = Arc::new(AppState { oauth, credentials });
    let app = Router::new()
        .route("/api/v1/oauth/device-code", post(request_device_code))
        .route("/api/v1/oauth/poll", post(poll_for_token))
        .route("/api/v1/oauth/refresh", post(refresh_token))
        .with_state(state.clone());

    let masked_key = state
        .credentials
        .masked_app_key()
        .unwrap_or_else(|| "****".to_string());
    println!("baidupan-auth-server listening on http://{address}");
    println!("app key: {masked_key}");

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| Error::Api(format!("failed to bind {address}: {error}")))?;
    axum::serve(listener, app)
        .await
        .map_err(|error| Error::Api(format!("auth server failed: {error}")))
}

async fn request_device_code(
    State(state): State<Arc<AppState>>,
) -> HandlerResult<DeviceCodeResponse> {
    state
        .oauth
        .request_device_code(&state.credentials)
        .await
        .map(Json)
        .map_err(map_handler_error)
}

async fn poll_for_token(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PollTokenRequest>,
) -> HandlerResult<StoredToken> {
    let response = DeviceCodeResponse {
        device_code: request.device_code,
        user_code: String::new(),
        verification_url: String::new(),
        qrcode_url: String::new(),
        expires_in: request.expires_in,
        interval: request.interval,
    };

    state
        .oauth
        .poll_for_token(&state.credentials, &response)
        .await
        .map(Json)
        .map_err(map_handler_error)
}

async fn refresh_token(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RefreshTokenRequest>,
) -> HandlerResult<StoredToken> {
    state
        .oauth
        .refresh_token(&state.credentials, &request.refresh_token)
        .await
        .map(Json)
        .map_err(map_handler_error)
}

fn map_handler_error(error: Error) -> (StatusCode, String) {
    match error {
        Error::MissingEnv(_) | Error::InvalidConfig(_) | Error::InvalidRemotePath(_) => {
            (StatusCode::BAD_REQUEST, error.to_string())
        }
        Error::NotLoggedIn => (StatusCode::UNAUTHORIZED, error.to_string()),
        Error::Api(_) => (StatusCode::BAD_GATEWAY, error.to_string()),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}
