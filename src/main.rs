use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Router,
};
use serde::Deserialize;
use anyhow::Context;
use std::{env, io::Write, net::SocketAddr};
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use base64::{engine::general_purpose, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    channel_secret: String,
    channel_access_token: String,
}

#[tokio::main]
async fn main() {
    println!("line-bot starting up...");
    if let Err(err) = run().await {
        error!(error = ?err, "application exited with error");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();
    writeln!(
        &mut stdout,
        "line-bot starting (version {}, arch {}, pid {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        std::process::id()
    )?;
    stdout.flush()?;

    // Initialize logging (default to info if RUST_LOG not set)
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(env_filter).init();

    let channel_secret = env::var("LINE_CHANNEL_SECRET")
        .context("LINE_CHANNEL_SECRET must be set in the environment")?;
    let channel_access_token = env::var("LINE_CHANNEL_ACCESS_TOKEN")
        .context("LINE_CHANNEL_ACCESS_TOKEN must be set in the environment")?;

    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    writeln!(
        &mut stdout,
        "env OK: PORT={}, RUST_LOG={}",
        port,
        env::var("RUST_LOG").unwrap_or_else(|_| "info(default)".into())
    )?;
    stdout.flush()?;

    let state = AppState {
        client: reqwest::Client::new(),
        channel_secret,
        channel_access_token,
    };

    let app = Router::new()
        .route("/webhook", post(handle_webhook))
        .route("/", axum::routing::get(|| async { "ok" }))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(?addr, "starting server");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("failed to bind TCP listener")?;
    writeln!(&mut stdout, "listener bound on {}", addr)?;
    stdout.flush()?;
    axum::serve(listener, app)
        .await
        .context("axum server failed")?;

    Ok(())
}

async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify signature from LINE
    let signature = match headers.get("x-line-signature") {
        Some(v) => match v.to_str() {
            Ok(s) => s,
            Err(_) => {
                error!("invalid x-line-signature header");
                return StatusCode::UNAUTHORIZED.into_response();
            }
        },
        None => {
            error!("missing x-line-signature header");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    if !verify_signature(&state.channel_secret, &body, signature) {
        error!("signature verification failed");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Parse body
    let payload: LineWebhook = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            error!(error = ?e, "failed to parse webhook body");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    info!("received {} event(s)", payload.events.len());

    for event in payload.events {
        if let Err(e) = handle_event(&state, event).await {
            error!(error = ?e, "error handling event");
        }
    }

    StatusCode::OK.into_response()
}

fn verify_signature(channel_secret: &str, body: &[u8], signature_header: &str) -> bool {
    let mut mac = match HmacSha256::new_from_slice(channel_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let result = mac.finalize();
    let expected_bytes = result.into_bytes();

    let decoded_signature = match general_purpose::STANDARD.decode(signature_header) {
        Ok(b) => b,
        Err(_) => return false,
    };

    // Constant-time comparison would be better; this is sufficient for sample code.
    expected_bytes.as_slice() == decoded_signature.as_slice()
}

async fn handle_event(state: &AppState, event: LineEvent) -> anyhow::Result<()> {
    println!("handling event: {:?}", event);
    if event.r#type == "message" {
        if let (Some(reply_token), Some(message)) = (event.reply_token, event.message) {
            if message.r#type == "text" {
                if let Some(text) = message.text {
                    let reply_text = text;
                    send_reply(
                        &state.client,
                        &state.channel_access_token,
                        &reply_token,
                        &reply_text,
                    )
                    .await?;
                }
            }
        }
    }

    Ok(())
}

async fn send_reply(
    client: &reqwest::Client,
    channel_access_token: &str,
    reply_token: &str,
    text: &str,
) -> anyhow::Result<()> {
    const LINE_REPLY_URL: &str = "https://api.line.me/v2/bot/message/reply";

    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [
            {
                "type": "text",
                "text": text,
            }
        ]
    });

    let resp = client
        .post(LINE_REPLY_URL)
        .bearer_auth(channel_access_token)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!(?status, body = %text, "LINE reply failed");
    } else {
        info!("sent reply to LINE");
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct LineWebhook {
    events: Vec<LineEvent>,
}

#[derive(Debug, Deserialize)]
struct LineEvent {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(rename = "replyToken")]
    #[serde(default)]
    reply_token: Option<String>,
    #[serde(default)]
    source: Option<LineSource>,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    message: Option<LineMessage>,
}

#[derive(Debug, Deserialize)]
struct LineSource {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    room_id: Option<String>,
    #[serde(default)]
    group_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LineMessage {
    id: String,
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    text: Option<String>,
}
