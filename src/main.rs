mod config;

use anyhow::anyhow;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::post,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};

use crate::config::Config;

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(long)]
    validate: bool,
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    http_client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct WebhookQuery {
    input: String,
    token: String,
}

#[derive(Debug, Serialize)]
struct WebhookResponse {
    success: bool,
    message: String,
    targets: Option<Vec<String>>,
    sent: Option<Vec<WebhookSendResult>>,
}

#[derive(Debug, Serialize)]
struct WebhookSendResult {
    webhook: String,
    status: u16,
    success: bool,
    error: Option<String>,
}

async fn webhook_handler(
    State(state): State<AppState>,
    Query(query): Query<WebhookQuery>,
    body: String,
) -> (StatusCode, Json<WebhookResponse>) {
    let input = match state.config.inputs.get(&query.input) {
        Some(input) => input,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(WebhookResponse {
                    success: false,
                    message: format!("unknown input '{}'", query.input),
                    targets: None,
                    sent: None,
                }),
            );
        }
    };

    if query.token.trim() != *input.token.as_ref().unwrap() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(WebhookResponse {
                success: false,
                message: "invalid token".to_string(),
                targets: None,
                sent: None,
            }),
        );
    }

    tracing::info!("got request: '{body}'");

    let targets = match state.config.get_target_webhooks(&query.input, &body) {
        Ok(targets) => targets,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WebhookResponse {
                    success: false,
                    message: format!("error processing webhook: {}", e),
                    targets: None,
                    sent: None,
                }),
            );
        }
    };

    if targets.is_empty() {
        return (
            StatusCode::OK,
            Json(WebhookResponse {
                success: true,
                message: "webhook blocked by rule".to_string(),
                targets: Some(vec![]),
                sent: None,
            }),
        );
    }

    let mut send_results = Vec::new();

    for target_name in &targets {
        let webhook = match state.config.webhooks.iter().find(|w| w.0 == target_name) {
            Some(webhook) => webhook,
            None => {
                send_results.push(WebhookSendResult {
                    webhook: target_name.clone(),
                    status: 0,
                    success: false,
                    error: Some("webhook not found in config".to_string()),
                });
                continue;
            }
        };

        let formatted_body = match state.config.format_webhook_body(webhook, &body) {
            Ok(body) => body,
            Err(e) => {
                tracing::error!("failed to format webhook body for '{}': {}", webhook.0, e);
                send_results.push(WebhookSendResult {
                    webhook: target_name.clone(),
                    status: 0,
                    success: false,
                    error: Some("webhook not found in config".to_string()),
                });
                continue;
            }
        };

        match send_webhook(&state.http_client, webhook.1, formatted_body).await {
            Ok(status) => {
                let success = status.is_success();
                send_results.push(WebhookSendResult {
                    webhook: webhook.0.clone(),
                    status: status.as_u16(),
                    success,
                    error: if success {
                        None
                    } else {
                        Some(format!("HTTP {}", status.as_u16()))
                    },
                });
            }
            Err(e) => {
                tracing::error!("failed to send webhook to '{}': {}", webhook.0, e);
                send_results.push(WebhookSendResult {
                    webhook: webhook.0.clone(),
                    status: 0,
                    success: false,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    let all_success = send_results.iter().all(|r| r.success);

    (
        if all_success {
            StatusCode::OK
        } else {
            StatusCode::PARTIAL_CONTENT
        },
        Json(WebhookResponse {
            success: all_success,
            message: "webhooks sent".to_string(),
            targets: Some(targets),
            sent: Some(send_results),
        }),
    )
}

async fn send_webhook(
    client: &reqwest::Client,
    webhook: &config::Webhook,
    body: serde_json::Value,
) -> anyhow::Result<reqwest::StatusCode> {
    let response = client
        .post(webhook.url.as_ref().unwrap())
        .header("Content-Type", "application/json")
        .header("User-Agent", "webhook-router (by 2kybe3)")
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    Ok(response.status())
}

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt::init();

    let config = cli.config.ok_or_else(|| anyhow!("no config set"))?;

    let config = Config::from_file(config, cli.validate)?;

    if cli.validate {
        return Ok(());
    }

    let ip_port = (config.ip.clone(), config.port);

    let config = Arc::new(config);

    let state = AppState {
        config,
        http_client: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/webhook", post(webhook_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(ip_port).await?;
    tracing::info!("listening on {}", listener.local_addr()?);

    axum::serve(listener, app).await?;

    Ok(())
}
