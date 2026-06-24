//! OpenAI-compatible mock server for deterministic offline tests.
//!
//! Endpoints:
//! - `POST /v1/chat/completions` — returns a canned assistant message.
//! - `POST /v1/embeddings` — returns zero vectors of a fixed dimension.
//!
//! The canned response is configurable: callers can pass a `CodiMockConfig`
//! and change what the model "says" in tests.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;

/// Configuration for the mock server behaviour.
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// The text the mock assistant always replies with.
    pub assistant_reply: String,
    /// Embedding dimension (number of floats per vector).
    pub embed_dim: usize,
}

impl Default for MockConfig {
    fn default() -> Self {
        MockConfig {
            assistant_reply: "Mock assistant reply.".to_string(),
            embed_dim: 8,
        }
    }
}

type SharedState = Arc<MockConfig>;

#[derive(Deserialize)]
struct ChatRequest {
    messages: Vec<Value>,
    #[allow(dead_code)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct EmbedRequest {
    input: Value,
    #[allow(dead_code)]
    model: Option<String>,
}

#[derive(Serialize)]
struct ChatResponse {
    id: String,
    object: &'static str,
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Serialize)]
struct Choice {
    index: u32,
    message: Message,
    finish_reason: &'static str,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

pub fn router(cfg: MockConfig) -> Router {
    let state = Arc::new(cfg);
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings))
        .with_state(state)
}

async fn chat_completions(
    State(cfg): State<SharedState>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    let prompt_tokens = req
        .messages
        .iter()
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .map(|s| s.split_whitespace().count() as u32)
        .sum::<u32>();
    let completion_tokens = 5u32;

    let resp = ChatResponse {
        id: "mock-id".to_string(),
        object: "chat.completion",
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant",
                content: cfg.assistant_reply.clone(),
            },
            finish_reason: "stop",
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    };

    (StatusCode::OK, Json(json!(resp)))
}

async fn embeddings(
    State(cfg): State<SharedState>,
    Json(req): Json<EmbedRequest>,
) -> impl IntoResponse {
    let inputs: Vec<String> = match req.input {
        Value::Array(arr) => arr
            .into_iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect(),
        Value::String(s) => vec![s],
        _ => vec![],
    };

    let data: Vec<Value> = inputs
        .into_iter()
        .enumerate()
        .map(|(i, _)| {
            json!({
                "object": "embedding",
                "index": i,
                "embedding": vec![0.0f32; cfg.embed_dim]
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": data,
            "model": "mock",
            "usage": { "prompt_tokens": 0, "total_tokens": 0 }
        })),
    )
}

/// Start the server on a random free port and return the bound address.
pub async fn start(cfg: MockConfig) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(cfg);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Ok((addr, handle))
}
