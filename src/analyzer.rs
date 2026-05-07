use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// claude-haiku-4-5: fastest, cheapest — ideal for lightweight extraction
const FLASH_MODEL: &str = "claude-haiku-4-5-20251001";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const SYSTEM_PROMPT: &str = r#"You are a preference-extraction engine. Given a conversation or session transcript, identify what the user explicitly or implicitly loves and hates — their stylistic preferences, workflow habits, values, pet peeves, and strong opinions.

Return ONLY a JSON object with this exact shape:
{
  "loves": [
    { "item": "<short label>", "reason": "<one sentence why>", "confidence": <0.0-1.0> }
  ],
  "hates": [
    { "item": "<short label>", "reason": "<one sentence why>", "confidence": <0.0-1.0> }
  ]
}

Rules:
- "item" must be a short, reusable label (e.g. "concise code", "verbose comments", "dark mode")
- Only include preferences with confidence >= 0.5
- Return an empty array if no preferences detected for that category
- Do not wrap in markdown fences — raw JSON only"#;

#[derive(Debug, Deserialize, Serialize)]
pub struct ExtractedPref {
    pub item: String,
    pub reason: String,
    pub confidence: f64,
}

#[derive(Debug, Deserialize)]
pub struct ExtractionResult {
    pub loves: Vec<ExtractedPref>,
    pub hates: Vec<ExtractedPref>,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct Request {
    model: &'static str,
    max_tokens: u32,
    system: &'static str,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
}

pub fn analyze(session_text: &str, api_key: &str) -> Result<ExtractionResult> {
    let body = Request {
        model: FLASH_MODEL,
        max_tokens: 1024,
        system: SYSTEM_PROMPT,
        messages: vec![Message {
            role: "user",
            content: format!("Extract user preferences from this session:\n\n{session_text}"),
        }],
    };

    let response = ureq::post(API_URL)
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(serde_json::to_value(&body)?)
        .context("calling Anthropic API")?;

    let api_resp: ApiResponse = response.into_json().context("parsing API response")?;

    let raw = api_resp
        .content
        .into_iter()
        .filter(|b| b.kind == "text")
        .filter_map(|b| b.text)
        .collect::<Vec<_>>()
        .join("");

    serde_json::from_str(&raw)
        .with_context(|| format!("flash LLM returned non-JSON: {}", &raw[..raw.len().min(200)]))
}

pub fn api_key_from_env() -> Result<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY not set — required for 'analyze'")
}
