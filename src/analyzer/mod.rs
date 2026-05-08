use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const ANTHROPIC_DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";
const ANTHROPIC_DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";
const OPENAI_DEFAULT_URL: &str = "https://api.openai.com/v1/chat/completions";

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

// ── Prompt construction ───────────────────────────────────────────────────────

fn build_system(existing_profile: Option<&str>) -> String {
    match existing_profile {
        Some(p) => format!(
            "{}\n\nAlready-stored preferences (reference only — skip re-extracting unless meaningfully changed):\n{}",
            SYSTEM_PROMPT, p
        ),
        None => SYSTEM_PROMPT.to_string(),
    }
}

fn build_user(session_text: &str) -> String {
    format!("Extract user preferences from this session:\n\n{session_text}")
}

/// Complete extraction prompt as a single text block.
///
/// Suitable for piping to any CLI LLM or pasting into a chat UI.
/// The model's response should be piped to `aizo import`.
pub fn extraction_prompt(session_text: &str, existing_profile: Option<&str>) -> String {
    format!("{}\n\n{}", build_system(existing_profile), build_user(session_text))
}

// ── Result parsing ────────────────────────────────────────────────────────────

/// Parse and validate an LLM JSON response into structured entries.
pub fn parse_result(text: &str) -> Result<ExtractionResult> {
    // Strip markdown fences if the model wrapped its output anyway
    let trimmed = text.trim();
    let json_str = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```").trim())
        .unwrap_or(trimmed);

    let mut result: ExtractionResult = serde_json::from_str(json_str)
        .with_context(|| format!("LLM returned non-JSON: {}", &json_str[..json_str.len().min(300)]))?;

    const VALID_CATS: &[&str] = &["preference", "aversion", "habit", "style", "taboo"];
    result.entries.retain(|e| {
        VALID_CATS.contains(&e.category.as_str())
            && e.base_score >= 0.0
            && e.base_score <= 10.0
    });
    for e in &mut result.entries {
        e.keywords = e.keywords.iter().map(|k| k.to_lowercase()).collect();
    }

    Ok(result)
}

// ── LLM call (auto-configured) ────────────────────────────────────────────────

/// Auto-detect API format.
///
/// Rules (in priority order):
/// 1. AIZO_API_FORMAT=anthropic  → Anthropic
/// 2. Only ANTHROPIC_API_KEY set, no AIZO_API_KEY or AIZO_API_URL → Anthropic (backward compat)
/// 3. Everything else            → OpenAI-compatible
fn use_anthropic_format() -> bool {
    if std::env::var("AIZO_API_FORMAT").as_deref() == Ok("anthropic") {
        return true;
    }
    std::env::var("AIZO_API_URL").is_err()
        && std::env::var("AIZO_API_KEY").is_err()
        && std::env::var("ANTHROPIC_API_KEY").is_ok()
}

/// Send a minimal request to verify the LLM is reachable and the key works.
pub fn test_connection() -> Result<()> {
    // A bare greeting produces {"entries":[]} — valid JSON, no side effects.
    analyze("User: hi", None)?;
    Ok(())
}

/// Call the configured LLM to extract preferences from a session transcript.
///
/// Configuration (env vars):
///   AIZO_MODEL      — model name (required for OpenAI format; default for Anthropic: claude-haiku-4-5-20251001)
///   AIZO_API_URL    — endpoint (default: provider-specific)
///   AIZO_API_KEY    — API key (fallback: ANTHROPIC_API_KEY, then OPENAI_API_KEY; empty = unauthenticated)
///   AIZO_API_FORMAT — "anthropic" to force Anthropic wire format; otherwise OpenAI-compatible
///
/// For no-LLM workflows, use `aizo extract | <llm> | aizo import` instead.
pub fn analyze(session_text: &str, existing_profile: Option<&str>) -> Result<ExtractionResult> {
    let anthropic = use_anthropic_format();

    let model = std::env::var("AIZO_MODEL").unwrap_or_else(|_| {
        if anthropic { ANTHROPIC_DEFAULT_MODEL.to_string() } else { String::new() }
    });

    if model.is_empty() {
        anyhow::bail!(
            "AIZO_MODEL is not set.\n\
             \n\
             For any OpenAI-compatible API or local model:\n\
             \texport AIZO_MODEL=qwen2.5:7b\n\
             \texport AIZO_API_URL=http://localhost:11434/v1/chat/completions\n\
             \n\
             For Anthropic:\n\
             \texport ANTHROPIC_API_KEY=sk-ant-...\n\
             \n\
             For manual analysis (no API needed):\n\
             \taizo extract chat.txt | <your-llm> | aizo import"
        );
    }

    let raw = if anthropic {
        call_anthropic(&model, session_text, existing_profile)?
    } else {
        call_openai(&model, session_text, existing_profile)?
    };

    parse_result(&raw)
}

// ── Wire formats ──────────────────────────────────────────────────────────────

fn call_openai(model: &str, session_text: &str, existing_profile: Option<&str>) -> Result<String> {
    let api_url = std::env::var("AIZO_API_URL")
        .unwrap_or_else(|_| OPENAI_DEFAULT_URL.to_string());
    let api_key = std::env::var("AIZO_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .unwrap_or_default();

    #[derive(Serialize)]
    struct Req { model: String, max_tokens: u32, messages: Vec<Msg> }
    #[derive(Serialize)]
    struct Msg { role: &'static str, content: String }
    #[derive(Deserialize)]
    struct Resp { choices: Vec<Choice> }
    #[derive(Deserialize)]
    struct Choice { message: MsgOut }
    #[derive(Deserialize)]
    struct MsgOut { content: String }

    let body = Req {
        model: model.to_string(),
        max_tokens: 1024,
        messages: vec![
            Msg { role: "system", content: build_system(existing_profile) },
            Msg { role: "user",   content: build_user(session_text) },
        ],
    };

    let builder = ureq::post(&api_url).set("content-type", "application/json");
    let builder = if api_key.is_empty() {
        builder
    } else {
        builder.set("Authorization", &format!("Bearer {api_key}"))
    };

    let resp: Resp = builder
        .send_json(serde_json::to_value(&body)?)
        .context("calling LLM API")?
        .into_json()
        .context("parsing LLM response")?;

    resp.choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .context("empty choices in API response")
}

fn call_anthropic(model: &str, session_text: &str, existing_profile: Option<&str>) -> Result<String> {
    let api_url = std::env::var("AIZO_API_URL")
        .unwrap_or_else(|_| ANTHROPIC_DEFAULT_URL.to_string());
    let api_key = std::env::var("AIZO_API_KEY")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .context("no API key found; set AIZO_API_KEY or ANTHROPIC_API_KEY")?;

    #[derive(Serialize)]
    struct Req { model: String, max_tokens: u32, system: String, messages: Vec<Msg> }
    #[derive(Serialize)]
    struct Msg { role: &'static str, content: String }
    #[derive(Deserialize)]
    struct Resp { content: Vec<Block> }
    #[derive(Deserialize)]
    struct Block { #[serde(rename = "type")] kind: String, text: Option<String> }

    let body = Req {
        model: model.to_string(),
        max_tokens: 1024,
        system: build_system(existing_profile),
        messages: vec![Msg { role: "user", content: build_user(session_text) }],
    };

    let resp: Resp = ureq::post(&api_url)
        .set("x-api-key", &api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(serde_json::to_value(&body)?)
        .context("calling Anthropic API")?
        .into_json()
        .context("parsing Anthropic response")?;

    resp.content
        .into_iter()
        .filter(|b| b.kind == "text")
        .filter_map(|b| b.text)
        .collect::<Vec<_>>()
        .join("")
        .pipe_non_empty()
        .context("empty response from Anthropic API")
}

trait PipeNonEmpty {
    fn pipe_non_empty(self) -> Option<String>;
}
impl PipeNonEmpty for String {
    fn pipe_non_empty(self) -> Option<String> {
        if self.is_empty() { None } else { Some(self) }
    }
}
