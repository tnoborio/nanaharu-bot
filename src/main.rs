use anyhow::Context;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use base64::{engine::general_purpose, Engine as _};
use cloud_storage::Client as GcsClient;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::{collections::HashMap, env, io::Write, net::SocketAddr};
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    channel_secret: String,
    channel_access_token: String,
    gcs_bucket: String,
    admin_user_ids: Vec<String>,
    presets: HashMap<String, String>,
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
    let gcs_bucket = env::var("GCS_BUCKET").context("GCS_BUCKET must be set in the environment")?;

    let admin_user_ids = env::var("ADMIN_USER_IDS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>();
    if admin_user_ids.is_empty() {
        info!("ADMIN_USER_IDS is empty; image uploads will be rejected");
    }

    let presets = load_presets();

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
        gcs_bucket,
        admin_user_ids,
        presets,
    };

    let app = Router::new()
        .route("/webhook", post(handle_webhook))
        .route("/", get(|| async { "ok" }))
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
        if let (Some(reply_token), Some(message)) = (event.reply_token.clone(), event.message.clone()) {
            match message.r#type.as_str() {
                "text" => {
                    if let Some(text) = message.text.clone() {
                        handle_text_message(state, &reply_token, text).await?;
                    }
                }
                "image" => {
                    handle_image_message(state, &reply_token, &event, message).await?;
                }
                _ => {}
            }
        }
    } else if event.r#type == "postback" {
        if let (Some(reply_token), Some(postback)) = (event.reply_token.clone(), event.postback.clone()) {
            handle_postback(state, &reply_token, postback).await?;
        }
    }

    Ok(())
}

fn load_presets() -> HashMap<String, String> {
    // 固定メッセージ -> GCS オブジェクトパス
    let pairs = [
        ("menu1", "images/menu1.jpg"),
        ("menu2", "images/menu2.jpg"),
        ("menu3", "images/menu3.jpg"),
        ("menu4", "images/menu4.jpg"),
    ];
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

async fn handle_text_message(
    state: &AppState,
    reply_token: &str,
    text: String,
) -> anyhow::Result<()> {
    let trimmed = text.trim();
    if let Some(object) = state.presets.get(trimmed) {
        let url = public_url(&state.gcs_bucket, object);
        send_image_reply(
            &state.client,
            &state.channel_access_token,
            reply_token,
            &url,
        )
        .await?;
    } else {
        // fallback echo
        send_text_reply(
            &state.client,
            &state.channel_access_token,
            reply_token,
            trimmed,
        )
        .await?;
    }
    Ok(())
}

async fn handle_image_message(
    state: &AppState,
    reply_token: &str,
    event: &LineEvent,
    message: LineMessage,
) -> anyhow::Result<()> {
    println!("handling image message: {:?}", message);

    let user_id = event
        .source
        .as_ref()
        .and_then(|s| s.user_id.as_ref())
        .map(|s| s.as_str());
    if !is_admin(user_id, &state.admin_user_ids) {
        send_text_reply(
            &state.client,
            &state.channel_access_token,
            reply_token,
            "この操作は管理者のみ可能です。",
        )
        .await?;
        return Ok(());
    }
    println!("user is admin: {:?}", user_id);

    // Download image content from LINE
    let content = fetch_line_content(&state.client, &state.channel_access_token, &message.id).await?;

    // Save to GCS as temporary object
    let pending_id = Uuid::new_v4().to_string();
    let tmp_object = format!("uploads/{}.jpg", pending_id);
    upload_to_gcs(&state.gcs_bucket, &tmp_object, content).await?;

    // Ask which preset to bind
    send_mapping_prompt(
        &state.client,
        &state.channel_access_token,
        reply_token,
        &pending_id,
        &state.presets,
    )
    .await?;

    Ok(())
}

async fn handle_postback(
    state: &AppState,
    reply_token: &str,
    postback: LinePostback,
) -> anyhow::Result<()> {
    let data = postback.data.unwrap_or_default();
    let params = url::form_urlencoded::parse(data.as_bytes())
        .into_owned()
        .collect::<HashMap<String, String>>();
    let pending_id = match params.get("pending") {
        Some(v) => v,
        None => return Ok(()),
    };
    let target_key = match params.get("target") {
        Some(v) => v,
        None => return Ok(()),
    };

    let tmp_object = format!("uploads/{}.jpg", pending_id);
    let Some(target_object) = state.presets.get(target_key) else {
        send_text_reply(
            &state.client,
            &state.channel_access_token,
            reply_token,
            "指定されたメッセージが見つかりません。",
        )
        .await?;
        return Ok(());
    };

    // Copy temporary object to target
    copy_gcs_object(&state.gcs_bucket, &tmp_object, target_object).await?;

    let url = public_url(&state.gcs_bucket, target_object);
    send_text_reply(
        &state.client,
        &state.channel_access_token,
        reply_token,
        &format!("画像を更新しました: {}", target_key),
    )
    .await?;
    send_image_reply(
        &state.client,
        &state.channel_access_token,
        reply_token,
        &url,
    )
    .await?;

    Ok(())
}

fn is_admin(user_id: Option<&str>, admins: &[String]) -> bool {
    match user_id {
        Some(uid) => admins.iter().any(|a| a == uid),
        None => false,
    }
}

fn public_url(bucket: &str, object: &str) -> String {
    format!("https://storage.googleapis.com/{}/{}", bucket, object)
}

async fn fetch_line_content(
    client: &reqwest::Client,
    channel_access_token: &str,
    message_id: &str,
) -> anyhow::Result<Vec<u8>> {
    let url = format!(
        "https://api-data.line.me/v2/bot/message/{}/content",
        message_id
    );
    let resp = client
        .get(url)
        .bearer_auth(channel_access_token)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        anyhow::bail!("failed to fetch content from LINE: status={}", status);
    }
    Ok(bytes.to_vec())
}

async fn upload_to_gcs(bucket: &str, object: &str, data: Vec<u8>) -> anyhow::Result<()> {
    let client = GcsClient::default();
    client
        .object()
        .create(bucket, data, object, "image/jpeg")
        .await?;
    Ok(())
}

async fn copy_gcs_object(bucket: &str, source: &str, dest: &str) -> anyhow::Result<()> {
    let client = GcsClient::default();
    let object = client.object().read(bucket, source).await?;
    client.object().copy(&object, bucket, dest).await?;
    Ok(())
}

async fn send_mapping_prompt(
    client: &reqwest::Client,
    channel_access_token: &str,
    reply_token: &str,
    pending_id: &str,
    presets: &HashMap<String, String>,
) -> anyhow::Result<()> {
    let actions: Vec<serde_json::Value> = presets
        .keys()
        .map(|k| {
            serde_json::json!({
                "type": "postback",
                "label": k,
                "data": format!("pending={}&target={}", pending_id, k),
            })
        })
        .collect();

    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [
            {
                "type": "template",
                "altText": "どのメッセージに紐づけますか？",
                "template": {
                    "type": "buttons",
                    "text": "どのメッセージに紐づけますか？",
                    "actions": actions,
                }
            }
        ]
    });

    let resp = client
        .post("https://api.line.me/v2/bot/message/reply")
        .bearer_auth(channel_access_token)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!(?status, body = %text, "LINE mapping prompt failed");
    }

    Ok(())
}

async fn send_text_reply(
    client: &reqwest::Client,
    channel_access_token: &str,
    reply_token: &str,
    text: &str,
) -> anyhow::Result<()> {
    send_reply(client, channel_access_token, reply_token, text).await
}

async fn send_image_reply(
    client: &reqwest::Client,
    channel_access_token: &str,
    reply_token: &str,
    image_url: &str,
) -> anyhow::Result<()> {
    const LINE_REPLY_URL: &str = "https://api.line.me/v2/bot/message/reply";
    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [
            {
                "type": "image",
                "originalContentUrl": image_url,
                "previewImageUrl": image_url,
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
        error!(?status, body = %text, "LINE image reply failed");
    } else {
        info!("sent image reply to LINE");
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

#[derive(Debug, Deserialize, Clone)]
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
    #[serde(default)]
    postback: Option<LinePostback>,
}

#[derive(Debug, Deserialize, Clone)]
struct LineSource {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(rename = "userId")]
    #[serde(default)]
    user_id: Option<String>,
    #[serde(rename = "roomId")]
    #[serde(default)]
    room_id: Option<String>,
    #[serde(rename = "groupId")]
    #[serde(default)]
    group_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct LineMessage {
    id: String,
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct LinePostback {
    data: Option<String>,
}
