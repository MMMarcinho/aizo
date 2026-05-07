use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// claude-haiku-4-5: fastest, cheapest — ideal for lightweight extraction
const FLASH_MODEL: &str = "claude-haiku-4-5-20251001";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

const SYSTEM_PROMPT: &str = r#"You are a user preference extraction engine. Analyze the conversation and identify the user's explicit and implicit preferences, aversions, habits, communication styles, and hard limits.

Return ONLY a JSON object with this exact shape:
{
  "entries": [
    {
      "category": "<category>",
      "item": "<short label>",
      "reason": "<one sentence>",
      "keywords": ["<synonym1>", "<synonym2>", ...],
      "base_score": <0.0-10.0>
    }
  ]
}

Category definitions:
- "preference"  — things the user consistently likes, prioritizes, or favors (base_score 7–10)
- "aversion"    — things the user dislikes, avoids, or rejects (base_score 0–3)
- "habit"       — behavioral or workflow patterns, value-neutral (base_score 4–6)
- "style"       — communication, tone, or formatting preferences (base_score 5–10)
- "taboo"       — hard limits, absolute rejections, must-never-do (base_score 0–2)

base_score scale:
  0    = absolute taboo / strong rejection
  1–3  = clear dislike
  4–6  = neutral tendency / weak pattern
  7–9  = clear preference
  10   = strong, consistent, high-priority preference

keywords rules:
- 3–6 synonyms, related terms, or paraphrases that a user might search for to find this entry
- lowercase only
- think about alternative ways someone might describe the same concept
- example for "over-engineered code": ["complexity", "bloat", "abstraction", "yagni", "indirection"]

Rules:
- Only include entries where base_score ≤ 3 OR base_score ≥ 7 — discard weak signals near neutral.
- "item" must be a concise reusable label (≤5 words).
- Return {"entries": []} if no strong signals detected.
- Raw JSON only — no markdown fences, no explanation."#;

#[derive(Debug, Deserialize, Serialize)]
pub struct ExtractedEntry {
    pub category: String,
    pub item: String,
    pub reason: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub base_score: f64,
}

#[derive(Debug, Deserialize)]
pub struct ExtractionResult {
    pub entries: Vec<ExtractedEntry>,
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

    let mut result: ExtractionResult = serde_json::from_str(&raw)
        .with_context(|| format!("flash LLM returned non-JSON: {}", &raw[..raw.len().min(300)]))?;

    const VALID_CATS: &[&str] = &["preference", "aversion", "habit", "style", "taboo"];
    result.entries.retain(|e| {
        VALID_CATS.contains(&e.category.as_str())
            && e.base_score >= 0.0
            && e.base_score <= 10.0
    });

    // Normalize keywords to lowercase
    for e in &mut result.entries {
        e.keywords = e.keywords.iter().map(|k| k.to_lowercase()).collect();
    }

    Ok(result)
}

pub fn api_key_from_env() -> Result<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY not set — required for 'analyze'")
}
