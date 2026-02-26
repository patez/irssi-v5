mod auth;
mod config;
mod session;
mod soju;
mod store;

use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{FromRequest, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TungMsg;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::signal;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use auth::{User, Validator};
use config::Config;
use session::Manager as SessionManager;
use soju::Manager as SojuManager;
use store::Store;

use tokio_tungstenite::tungstenite::client::IntoClientRequest;

// ── App state ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
    validator: Option<Arc<Validator>>,
    store: Store,
    sessions: Arc<SessionManager>,
    soju: Arc<SojuManager>,
}

impl AppState {
    async fn authenticate(&self, headers: &HeaderMap) -> Result<User, AppError> {
        if self.cfg.dev_mode {
            let username = self.cfg.dev_user.clone();
            let is_admin = self.cfg.admin_users.contains(&username);
            return Ok(User {
                username: username.clone(),
                email: format!("{}@dev", username),
                is_admin,
            });
        }

        let token = headers
            .get("cf-access-jwt-assertion")
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized(
                "Missing CF-Access-Jwt-Assertion header — access via Cloudflare Access".into(),
            ))?;

        let user: User = self.validator
            .as_ref()
            .expect("validator must exist when not in dev mode")
            .validate(token)
            .await
            .map_err(|e| -> AppError {
                warn!("JWT validation failed: {}", e);
                AppError::Unauthorized(format!("Invalid Cloudflare Access token: {}", e))
            })?;
        Ok(user)
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

enum AppError {
    Unauthorized(String),
    Forbidden,
    Internal(anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": msg})),
            )
                .into_response(),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Admin access required"})),
            )
                .into_response(),
            AppError::Internal(e) => {
                error!("Internal error: {:#}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "Internal server error"})),
                )
                    .into_response()
            }
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn handle_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    let _ = state.store.touch(&user.username, user.is_admin).await;

    Ok(Json(json!({
        "username": user.username,
        "email":    user.email,
        "isAdmin":  user.is_admin,
    })))
}

/// Provision the user's session (soju + ttyd). Called by the frontend
/// before loading the terminal iframe. Returns 200 when ready.
/// Route: GET /api/terminal
async fn handle_provision(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    let _ = state.store.touch(&user.username, user.is_admin).await;

    let user_dir = if state.cfg.dev_mode {
        let dir = state.cfg.sessions_dir.join(&user.username);
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    } else {
        state
            .soju
            .ensure_user(&user.username)
            .await
            .map_err(|e| {
                error!("soju.ensure_user({}): {:#}", user.username, e);
                AppError::Internal(e)
            })?;
        state.soju.user_dir(&user.username)
    };

    state
        .sessions
        .get_or_create(&user.username, &user_dir)
        .await
        .map_err(|e| {
            error!("session.get_or_create({}): {:#}", user.username, e);
            AppError::Internal(e)
        })?;

    Ok(Json(json!({"ok": true})))
}

// ── Router changes ────────────────────────────────────────────────────────────
// Replace the three /terminal routes with explicit WS + HTTP routes:
//
//   .route("/terminal/ws",    get(handle_terminal_ws))
//   .route("/terminal/",      get(handle_terminal_http))
//   .route("/terminal/*path", get(handle_terminal_http))
//
// ttyd's JS client connects to /terminal/ws — so the iframe src stays /terminal/
// but the WS endpoint is explicit and axum can negotiate the upgrade cleanly.

// ── WebSocket handler ─────────────────────────────────────────────────────────

/// Dedicated WS upgrade handler — axum extracts `WebSocketUpgrade` before
/// your code runs, so the HTTP→WS handshake is already done when we call
/// connect_async to ttyd. No more race between upgrade negotiation and the
/// upstream connect.
async fn handle_terminal_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Result<Response, AppError> {
    let user = state.authenticate(&headers).await?;

    let port = state
        .sessions
        .get_or_create(
            &user.username,
            &state.cfg.sessions_dir.join(&user.username),
        )
        .await
        .map_err(|e| {
            error!("session.get_or_create({}): {:#}", user.username, e);
            AppError::Internal(e)
        })?;

    // Build upstream WS request to ttyd
    let ws_url = format!("ws://127.0.0.1:{}/ws", port);
    info!("WS proxy for {}: → {}", user.username, ws_url);

    let origin = format!("http://127.0.0.1:{}", port);

    let mut tung_req = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("bad ws url: {}", e)))?;

    tung_req.headers_mut().insert(
        "origin",
        origin
            .parse()
            .map_err(|e| AppError::Internal(anyhow::anyhow!("bad origin: {}", e)))?,
    );
    tung_req.headers_mut().insert(
        "sec-websocket-protocol",
        "tty".parse().unwrap(),
    );
    tung_req.headers_mut().remove("cookie");

    let (upstream, _) = tokio_tungstenite::connect_async(tung_req)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("ws connect ttyd: {}", e)))?;

    Ok(ws
        .protocols(["tty"])
        .on_upgrade(move |client| splice_ws(client, upstream))
        .into_response())
}

// ── HTTP proxy handler ────────────────────────────────────────────────────────

/// Proxies ttyd's static assets (HTML, JS, CSS) — no WS logic here at all.
async fn handle_terminal_http(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: axum::extract::Request,
) -> Result<Response, AppError> {
    let user = state.authenticate(&headers).await?;

    let port = state
        .sessions
        .get_or_create(
            &user.username,
            &state.cfg.sessions_dir.join(&user.username),
        )
        .await
        .map_err(|e| {
            error!("session.get_or_create({}): {:#}", user.username, e);
            AppError::Internal(e)
        })?;

    let path = req.uri().path();
    let stripped = path.strip_prefix("/terminal").unwrap_or(path);
    let stripped = if stripped.is_empty() { "/" } else { stripped };
    // At the top of handle_terminal_http, before the proxy logic:
    if stripped == "/token" {
    return Ok(axum::response::Response::builder()
        .status(200)
        .body(axum::body::Body::empty())
        .unwrap());
    }
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();

    let uri_str = format!("http://127.0.0.1:{}{}{}", port, stripped, query);
    info!("HTTP proxy for {}: {} → {}", user.username, path, uri_str);

    let client = reqwest::Client::new();
    let resp = client
        .get(&uri_str)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("http proxy error: {}", e)))?;

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = axum::response::Response::builder().status(status);
    for (k, v) in resp.headers() {
        builder = builder.header(k, v);
    }

    let body = resp
        .bytes()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("proxy body error: {}", e)))?;

    builder
        .body(axum::body::Body::from(body))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("response build error: {}", e)))
}

// ── Router snippet ────────────────────────────────────────────────────────────
//
// let app = Router::new()
//     .route("/api/me",              get(handle_me))
//     .route("/api/terminal",        get(handle_provision))
//     .route("/terminal/ws",         get(handle_terminal_ws))   // ← explicit WS
//     .route("/terminal/",           get(handle_terminal_http))
//     .route("/terminal/*path",      get(handle_terminal_http))
//     .route("/api/session/clear",   post(handle_clear_session))
//     // ... admin routes ...
//     .fallback_service(ServeDir::new(&cfg.public_dir))
//     .layer(TraceLayer::new_for_http())
//     .with_state(state);
//
// Also remove the old `handle_terminal` and `proxy_ttyd` functions entirely.
// The `splice_ws` function stays unchanged.
//
// NOTE: you also need to add this import at the top:
//   use tokio_tungstenite::tungstenite::client::IntoClientRequest;
/// Bidirectional WebSocket splice: browser ↔ ttyd
async fn splice_ws(
    client: axum::extract::ws::WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    use axum::extract::ws::Message as AxMsg;

    let (mut ctx, mut crx) = client.split();
    let (mut utx, mut urx) = upstream.split();

    let c2u = async {
        while let Some(Ok(msg)) = crx.next().await {
            let m = match msg {
                AxMsg::Text(t)   => TungMsg::Text(t),
                AxMsg::Binary(b) => TungMsg::Binary(b),
                AxMsg::Ping(p)   => TungMsg::Ping(p),
                AxMsg::Pong(p)   => TungMsg::Pong(p),
                AxMsg::Close(_)  => break,
            };
            if utx.send(m).await.is_err() { break; }
        }
    };

    let u2c = async {
        while let Some(Ok(msg)) = urx.next().await {
            let m = match msg {
                TungMsg::Text(t)   => AxMsg::Text(t),
                TungMsg::Binary(b) => AxMsg::Binary(b),
                TungMsg::Ping(p)   => AxMsg::Ping(p),
                TungMsg::Pong(p)   => AxMsg::Pong(p),
                TungMsg::Close(_) | TungMsg::Frame(_) => break,
            };
            if ctx.send(m).await.is_err() { break; }
        }
    };

    tokio::select! { _ = c2u => {}, _ = u2c => {} }
}

async fn handle_clear_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    state.sessions.kill(&user.username);
    if !state.cfg.dev_mode {
        let _ = state.soju.delete_user(&user.username).await;
    }
    Ok(Json(json!({"success": true})))
}

// ── Admin handlers ────────────────────────────────────────────────────────────

async fn handle_admin_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    if !user.is_admin { return Err(AppError::Forbidden); }

    let users = state.store.list_users().await.map_err(AppError::from)?;
    let rows: Vec<Value> = users
        .iter()
        .map(|u| {
            json!({
                "username":       u.username,
                "first_seen":     u.first_seen,
                "last_seen":      u.last_seen,
                "is_admin":       u.is_admin != 0,
                "active_session": state.sessions.is_active(&u.username),
            })
        })
        .collect();

    Ok(Json(json!({"users": rows})))
}

async fn handle_admin_kick(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    if !user.is_admin { return Err(AppError::Forbidden); }
    state.sessions.kill(&username);
    Ok(Json(json!({"success": true})))
}

async fn handle_admin_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    if !user.is_admin { return Err(AppError::Forbidden); }
    state.sessions.kill(&username);
    let _ = state.soju.delete_user(&username).await;
    Ok(Json(json!({"success": true})))
}

async fn handle_admin_delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    if !user.is_admin { return Err(AppError::Forbidden); }
    if username == user.username {
        return Err(AppError::Internal(anyhow::anyhow!("cannot delete yourself")));
    }
    state.sessions.kill(&username);
    let _ = state.soju.delete_user(&username).await;
    state.store.delete_user(&username).await.map_err(AppError::from)?;
    Ok(Json(json!({"success": true})))
}

#[derive(Deserialize)]
struct SettingsBody {
    #[serde(rename = "maxUsers")]
    max_users: Option<u32>,
}

async fn handle_admin_get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    if !user.is_admin { return Err(AppError::Forbidden); }

    let max_users: u32 = state
        .store
        .get_setting("max_users", "50")
        .await
        .parse()
        .unwrap_or(50);
    let total = state.store.user_count().await.unwrap_or(0);

    Ok(Json(json!({
        "maxUsers":       max_users,
        "activeSessions": state.sessions.active_count(),
        "totalUsers":     total,
    })))
}

async fn handle_admin_post_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SettingsBody>,
) -> Result<Json<Value>, AppError> {
    let user = state.authenticate(&headers).await?;
    if !user.is_admin { return Err(AppError::Forbidden); }

    if let Some(max) = body.max_users {
        if max < 1 || max > 1000 {
            return Err(AppError::Internal(anyhow::anyhow!("maxUsers must be 1–1000")));
        }
        state.store.set_setting("max_users", &max.to_string()).await.map_err(AppError::from)?;
    }

    Ok(Json(json!({"success": true})))
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("irssi_v5=info".parse()?))
        .init();

    let cfg = Arc::new(Config::from_env()?);

    if cfg.dev_mode {
        warn!("DEV MODE — CF JWT validation disabled");
    } else if cfg.cf_aud.is_empty() || cfg.cf_team_domain.is_empty() {
        anyhow::bail!("CF_AUD and CF_TEAM_DOMAIN must be set (or set DEV_MODE=true)");
    }

    let validator = if cfg.dev_mode {
        None
    } else {
        Some(Validator::new(
            &cfg.cf_team_domain,
            &cfg.cf_aud,
            cfg.cf_jwks_cache_ttl,
            cfg.admin_users.clone(),
        ))
    };

    let db_path = cfg.data_dir.join("app.db");
    let store = Store::new(db_path.to_str().unwrap()).await?;

    let sessions = SessionManager::new(cfg.ttyd_base_port);

    let soju = SojuManager::new(
        cfg.soju_socket.clone(),
        cfg.sessions_dir.clone(),
        cfg.soju_addr.clone(),
        cfg.irc_addr.clone(),
        cfg.irc_network_name.clone(),
    );

    let state = AppState {
        cfg: Arc::clone(&cfg),
        validator,
        store,
        sessions,
        soju,
    };

    let app = Router::new()
        // User API
        .route("/terminal/ws",    get(handle_terminal_ws))
        .route("/terminal/token", get(handle_terminal_ws))
        .route("/api/me", get(handle_me))
        .route("/api/terminal", get(handle_provision))
        //.route("/token", get(handle_token))
        .route("/terminal/", get(handle_terminal_http))
        .route("/terminal/*path", get(handle_terminal_http))
        .route("/api/session/clear", post(handle_clear_session))
        // Admin API
        .route("/api/admin/users", get(handle_admin_users))
        .route("/api/admin/users/:username", delete(handle_admin_delete_user))
        .route("/api/admin/users/:username/kick", post(handle_admin_kick))
        .route("/api/admin/users/:username/clear", post(handle_admin_clear))
        .route("/api/admin/settings", get(handle_admin_get_settings).post(handle_admin_post_settings))
        // Static files (frontend)
        .fallback_service(ServeDir::new(&cfg.public_dir))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", cfg.port);
    info!("irssi-v5 listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

// Handler — CF Access lands here after OAuth, just validate + redirect home
async fn handle_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // Validates the JWT (ensures the CF cookie is good), then redirects to /
    let _user = state.authenticate(&headers).await?;
    Ok(axum::response::Redirect::to("/").into_response())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutting down...");
}