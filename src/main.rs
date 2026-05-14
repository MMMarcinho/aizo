mod analyzer;
mod db;
mod scoring;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use db::{Db, Preference};
use dirs::home_dir;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// aizo (爱憎) — human-like preference memory for AI agents.
///
/// Extracts, quantifies, decays, and recalls user preferences from session
/// history using a flash LLM + SQLite. Effective weight = base_score × decay.
#[derive(Parser)]
#[command(name = "aizo", version, about, long_about = None)]
struct Cli {
    /// Path to the SQLite database [env: AIZO_DB_PATH]
    #[arg(long, env = "AIZO_DB_PATH", global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze a session file (or stdin) with the configured LLM and update preferences
    ///
    /// Requires LLM configuration via env vars (AIZO_MODEL, AIZO_API_KEY, etc.).
    /// For LLM-agnostic analysis use: aizo extract [file] | <llm> | aizo import
    Analyze {
        /// Path to session text/JSON file; reads stdin if omitted
        file: Option<PathBuf>,
    },

    /// Print the extraction prompt to stdout — pipe to any LLM, then pipe output to 'aizo import'
    ///
    /// Example: aizo extract chat.txt | ollama run qwen2.5:3b | aizo import
    Extract {
        /// Path to session text/JSON file; reads stdin if omitted
        file: Option<PathBuf>,
    },

    /// Read a preference JSON response from stdin and upsert entries
    ///
    /// Accepts the {"entries":[...]} format produced by any LLM given the 'aizo extract' prompt.
    Import,

    /// Recall preferences matching a keyword, sorted by effective weight  [primary agent use]
    ///
    /// --scenario pulls entries explicitly tagged with a scenario; results are
    /// also expanded with the scenario's configured keywords for broader coverage.
    Recall {
        /// Keyword matched against item, reason, AND synonyms; omit to filter by type/scenario only
        query: Option<String>,
        /// Score-range filter by band name (repeatable or comma-separated): preference(≥7), style(≥6.5), habit(4–7), aversion(1.5–4), taboo(≤1.5)
        #[arg(long = "score-band", value_delimiter = ',')]
        score_bands: Vec<String>,
        /// Deprecated alias for --score-band
        #[arg(long = "type", short = 't', value_delimiter = ',')]
        types: Vec<String>,
        /// Max results to return
        #[arg(long, short = 'l')]
        limit: Option<usize>,
        /// Scenario — pulls entries tagged with this scenario + expanded keywords
        #[arg(long)]
        scenario: Option<String>,
        /// Do not refresh last_seen for matched entries
        #[arg(long)]
        no_touch: bool,
        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Show top-N preferences by current effective weight
    Top {
        /// Number of entries to show (default: 10)
        #[arg(default_value = "10")]
        n: usize,
        /// Score-range filter by band name (repeatable or comma-separated): preference, style, habit, aversion, taboo
        #[arg(long = "score-band", value_delimiter = ',')]
        score_bands: Vec<String>,
        /// Deprecated alias for --score-band
        #[arg(long = "type", short = 't', value_delimiter = ',')]
        types: Vec<String>,
        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Print the full preference profile sorted by effective weight
    Show {
        /// Output raw JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Manually add or update a preference
    Add {
        /// Short item label (e.g. "dark mode", "verbose comments")
        item: String,
        /// One-sentence reason why this entry matters (must be quoted if it contains spaces)
        reason: String,
        /// Base score 0.0–10.0 (defaults to 9.0)
        #[arg(long, short = 's')]
        score: Option<f64>,
        /// Comma-separated synonym keywords for richer recall (e.g. concise,brevity,minimal)
        #[arg(long, value_delimiter = ',')]
        keywords: Vec<String>,
        /// Comma-separated scenario names this entry applies to (e.g. coding,writing)
        #[arg(long, value_delimiter = ',')]
        scenarios: Vec<String>,
    },

    /// Add or replace keywords/scenarios on an existing entry
    Tag {
        /// Item label (case-insensitive)
        item: String,
        /// Keywords to set (space or comma-separated)
        #[arg(long, value_delimiter = ',')]
        keywords: Vec<String>,
        /// Comma-separated scenario names (e.g. coding,writing)
        #[arg(long, value_delimiter = ',')]
        scenarios: Vec<String>,
    },

    /// Refresh the decay clock of an existing entry without changing its score
    ///
    /// Call this when the agent confirms a preference was just demonstrated,
    /// without needing to re-analyze the full session through the LLM.
    Touch {
        /// Item label to refresh (case-insensitive, words joined)
        #[arg(trailing_var_arg = true, num_args = 1..)]
        item: Vec<String>,
    },

    /// Remove a preference by item label
    Remove {
        /// Item label to remove (case-insensitive, words joined)
        #[arg(trailing_var_arg = true, num_args = 1..)]
        item: Vec<String>,
    },

    /// List all keywords stored in the database with entry counts
    Keywords {
        /// Sort order: count (default) or alpha
        #[arg(long, default_value = "count")]
        sort: String,
    },

    /// List all scenarios with entry counts and configured keywords
    Scenarios,

    /// Interactive first-time setup: configure LLM provider and write ~/.aizo/.env
    Init,

    /// Wipe the entire preference profile and session history
    Clear,

    /// Show database stats, path, and decay configuration
    Info,

    /// Get or set decay configuration
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show current decay settings
    Show,
    /// Set decay half-life in days (score halves after this many inactive days)
    SetHalfLife { days: f64 },
    /// Set minimum decay floor (0.0–1.0; prevents effective weight from reaching zero)
    SetFloor { floor: f64 },
}

fn default_db_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aizo")
        .join("preferences.db")
}

/// FNV-1a 64-bit hash — stable, dependency-free, good enough for dedup.
fn content_hash(s: &str) -> String {
    const OFFSET: u64 = 14695981039346656037;
    const PRIME: u64 = 1099511628211;
    let mut h = OFFSET;
    for byte in s.bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(PRIME);
    }
    format!("{h:016x}")
}

/// Load ~/.aizo/taxonomy.txt — one keyword per line, # = comment, blank lines ignored.
fn load_taxonomy() -> Option<Vec<String>> {
    let path = home_dir()?.join(".aizo").join("taxonomy.txt");
    let content = std::fs::read_to_string(path).ok()?;
    let terms: Vec<String> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    if terms.is_empty() { None } else { Some(terms) }
}

/// Flatten a JSON conversation export into plain "Speaker: text" lines.
///
/// Supports:
///   - Array of message objects (OpenAI, generic)
///   - Object with a "messages" or "conversation" key containing such an array
///   - Claude claude.ai export: each message has `content` as an array of typed blocks
///   - JSONL: one message object per line
fn json_to_session_text(raw: &str) -> Option<String> {
    fn extract_content(msg: &serde_json::Value) -> Option<String> {
        if let Some(c) = msg.get("content") {
            return match c {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Array(blocks) => {
                    let text = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if text.is_empty() { None } else { Some(text) }
                }
                _ => None,
            };
        }
        msg.get("text")
            .or_else(|| msg.get("message"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn speaker(msg: &serde_json::Value) -> &str {
        let role = msg
            .get("role")
            .or_else(|| msg.get("sender"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        match role {
            "human" | "user" => "User",
            "assistant" | "ai" | "claude" => "Assistant",
            other => other,
        }
    }

    fn msgs_to_lines(arr: &[serde_json::Value]) -> Vec<String> {
        arr.iter()
            .filter_map(|msg| {
                let content = extract_content(msg)?;
                let content = content.trim();
                if content.is_empty() { return None; }
                Some(format!("{}: {content}", speaker(msg)))
            })
            .collect()
    }

    // Try JSONL first (multiple JSON objects, one per line)
    let jsonl_lines: Vec<serde_json::Value> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if jsonl_lines.len() > 1 {
        let lines = msgs_to_lines(&jsonl_lines);
        if !lines.is_empty() {
            return Some(lines.join("\n\n"));
        }
    }

    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let arr: &[serde_json::Value] = match &v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::Array(a)) =
                obj.get("messages").or_else(|| obj.get("conversation"))
            {
                a
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let lines = msgs_to_lines(arr);
    if lines.is_empty() { None } else { Some(lines.join("\n\n")) }
}

fn read_text(file: Option<PathBuf>) -> Result<String> {
    let (raw, hint_json) = match file {
        Some(ref p) => {
            let is_json = matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("json" | "jsonl")
            );
            (std::fs::read_to_string(p)?, is_json)
        }
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            let trimmed = buf.trim_start();
            let looks_json = trimmed.starts_with('{') || trimmed.starts_with('[');
            (buf, looks_json)
        }
    };

    if raw.trim().is_empty() {
        anyhow::bail!("no session text provided");
    }

    if hint_json {
        match json_to_session_text(&raw) {
            Some(text) => return Ok(text),
            None => eprintln!(
                "Warning: JSON input could not be parsed as a conversation; \
                 sending raw content to the LLM."
            ),
        }
    }

    Ok(raw)
}

fn profile_context(db: &Db) -> Result<Option<String>> {
    let prefs = db.top(20, &[])?;
    Ok(if prefs.is_empty() {
        None
    } else {
        Some(format_profile_context(&prefs))
    })
}

fn format_profile_context(prefs: &[Preference]) -> String {
    prefs
        .iter()
        .map(|p| format!("{} ({:.1}): {}", p.item, p.base_score, p.reason))
        .collect::<Vec<_>>()
        .join("\n")
}

fn score_label(score: f64) -> &'static str {
    if score >= 7.0 { "liked" } else if score <= 3.0 { "disliked" } else { "neutral" }
}

/// Map a score-band name to a (min, max) base_score range (inclusive).
fn parse_score_band(t: &str) -> Result<(f64, f64)> {
    match t.trim() {
        "preference" | "pref" | "like" | "love" => Ok((7.0, 10.0)),
        "style"                                  => Ok((6.5, 10.0)),
        "habit"  | "neutral"                     => Ok((4.0, 7.0)),
        "aversion" | "avers" | "dislike" | "hate"=> Ok((1.5, 4.0)),
        "taboo"  | "limit"  | "hard"             => Ok((0.0, 1.5)),
        other => anyhow::bail!(
            "unknown score band '{other}'. Use: preference, style, habit, aversion, taboo"
        ),
    }
}

// ── Scenario config ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct ScenarioDef {
    #[serde(default)]
    description: String,
    keywords: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ScenarioConfig {
    scenarios: HashMap<String, ScenarioDef>,
}

/// Load ~/.aizo/scenarios.yaml, auto-initializing with built-in defaults if missing.
fn load_scenario_config() -> Result<ScenarioConfig> {
    let aizo_dir = home_dir()
        .context("cannot determine home directory")?
        .join(".aizo");
    let path = aizo_dir.join("scenarios.yaml");

    if !path.exists() {
        let default_yaml = r#"# aizo scenario configuration — edit freely to add/remove scenarios and keywords.
# Each scenario defines a set of keywords used for extended recall.
scenarios:
  coding:
    description: "Coding, debugging, implementation, repo changes"
    keywords:
      - coding
      - codex
      - repo
      - test
      - verification
      - tnpm

  writing:
    description: "Docs, system design, plans, summaries"
    keywords:
      - writing
      - document
      - plan
      - structure
      - concise

  communication:
    description: "Reply tone, language, brevity, user-facing communication"
    keywords:
      - communication
      - chinese
      - concise
      - tone
      - kawaii

  biz-analyze:
    description: "Product docs, PRD, requirements, business and user analysis"
    keywords:
      - biz-analyze
      - product
      - business
      - requirement
      - prd
      - user-journey
      - metric
      - strategy
"#;
        std::fs::create_dir_all(&aizo_dir)?;
        std::fs::write(&path, default_yaml)?;
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let config: ScenarioConfig = serde_yaml::from_str(&content)
        .with_context(|| format!("parsing {}", path.display()))?;

    for name in config.scenarios.keys() {
        if config.scenarios[name].keywords.is_empty() {
            anyhow::bail!("scenario '{name}' has no keywords in {path:?}");
        }
    }

    Ok(config)
}

/// Resolve a scenario name (with aliases) to the canonical name in config.
fn resolve_scenario<'a>(name: &'a str, config: &'a ScenarioConfig) -> Option<&'a str> {
    let name = name.trim();
    // Direct match
    if config.scenarios.contains_key(name) {
        return Some(name);
    }
    // Aliases
    match name {
        "code" | "dev" | "development" if config.scenarios.contains_key("coding") => Some("coding"),
        "docs" | "documentation" if config.scenarios.contains_key("writing") => Some("writing"),
        "chat" | "social" | "meeting" if config.scenarios.contains_key("communication") => Some("communication"),
        "biz" | "business" | "product" if config.scenarios.contains_key("biz-analyze") => Some("biz-analyze"),
        _ => None,
    }
}

fn print_entries(entries: &[Preference], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(entries).unwrap());
        return;
    }

    let high = entries.iter().filter(|e| e.base_score >= 7.0).count();
    let low  = entries.iter().filter(|e| e.base_score <= 3.0).count();
    let mid  = entries.len() - high - low;

    let mut parts = Vec::new();
    if high > 0 { parts.push(format!("{high} liked")); }
    if mid  > 0 { parts.push(format!("{mid} neutral")); }
    if low  > 0 { parts.push(format!("{low} disliked")); }
    println!("{} entries  ({})", entries.len(), parts.join(" · "));
    println!();

    for e in entries {
        if e.scenarios.is_empty() {
            println!("  {:<28}  {:>4.1}   {}", e.item, e.effective_weight, e.reason);
        } else {
            println!("  {:<28}  {:>4.1}   {}  [{}]", e.item, e.effective_weight, e.reason, e.scenarios.join(", "));
        }
    }
}

fn print_analyze_summary(entries: &[analyzer::ExtractedEntry]) {
    let high: Vec<_> = {
        let mut v: Vec<_> = entries.iter().filter(|e| e.base_score >= 7.0).collect();
        v.sort_by(|a, b| b.base_score.partial_cmp(&a.base_score).unwrap());
        v
    };
    let low: Vec<_> = {
        let mut v: Vec<_> = entries.iter().filter(|e| e.base_score <= 3.0).collect();
        v.sort_by(|a, b| a.base_score.partial_cmp(&b.base_score).unwrap());
        v
    };

    println!("✓  Extracted {} entries: {} liked, {} disliked", entries.len(), high.len(), low.len());

    if !high.is_empty() {
        println!("\nLiked:");
        for e in &high {
            println!("  + [{:4.1}] {}  —  {}", e.base_score, e.item, e.reason);
            if !e.keywords.is_empty() {
                println!("           keywords: {}", e.keywords.join(", "));
            }
        }
    }
    if !low.is_empty() {
        println!("\nDisliked / limits:");
        for e in &low {
            println!("  - [{:4.1}] {}  —  {}", e.base_score, e.item, e.reason);
            if !e.keywords.is_empty() {
                println!("           keywords: {}", e.keywords.join(", "));
            }
        }
    }
}

// ── Init wizard helpers ───────────────────────────────────────────────────────

fn prompt(label: &str, default: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let v = line.trim().to_string();
    Ok(if v.is_empty() { default.to_string() } else { v })
}

fn prompt_yn(label: &str, default: bool) -> Result<bool> {
    let v = prompt(label, if default { "y" } else { "n" })?;
    Ok(matches!(v.to_lowercase().as_str(), "y" | "yes"))
}

fn mask_key(val: &str) -> String {
    if val.len() > 8 {
        format!("{}****", &val[..8])
    } else {
        "****".to_string()
    }
}

fn show_env(path: &std::path::Path) {
    if let Ok(contents) = std::fs::read_to_string(path) {
        for line in contents.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if k.to_uppercase().contains("KEY") && !v.is_empty() {
                    println!("  {}={}", k, mask_key(v));
                } else {
                    println!("  {line}");
                }
            } else {
                println!("  {line}");
            }
        }
    }
}

fn cmd_init(db_path: &PathBuf) -> Result<()> {
    let aizo_dir = home_dir()
        .context("cannot determine home directory")?
        .join(".aizo");
    let env_path = aizo_dir.join(".env");

    println!("aizo — first-time setup");
    println!("────────────────────────");

    if env_path.exists() {
        println!("Config already exists at {}:", env_path.display());
        show_env(&env_path);
        println!();
        if !prompt_yn("Overwrite? [y/N]:", false)? {
            println!("Setup cancelled.");
            return Ok(());
        }
        println!();
    }

    println!("Select LLM provider:");
    println!("  1) Anthropic  (default model: claude-haiku-4-5)");
    println!("  2) OpenAI-compatible  (Ollama, OpenRouter, DeepSeek, vLLM, …)");
    println!();
    let choice = prompt("Choice [1]: ", "1")?;

    let mut lines: Vec<String> = vec!["# aizo configuration — generated by: aizo init".into()];

    let anthropic = choice.trim() != "2";

    if anthropic {
        let key = rpassword::prompt_password("ANTHROPIC_API_KEY: ")?;
        if key.trim().is_empty() {
            anyhow::bail!("ANTHROPIC_API_KEY is required for Anthropic");
        }
        lines.push(format!("ANTHROPIC_API_KEY={}", key.trim()));
    } else {
        let model = prompt("AIZO_MODEL (e.g. qwen2.5:7b): ", "")?;
        if model.is_empty() {
            anyhow::bail!("AIZO_MODEL is required");
        }
        let url = prompt(
            "AIZO_API_URL [http://localhost:11434/v1/chat/completions]: ",
            "http://localhost:11434/v1/chat/completions",
        )?;
        let key = rpassword::prompt_password("AIZO_API_KEY (leave blank for local/unauthenticated): ")?;

        lines.push(format!("AIZO_MODEL={model}"));
        lines.push(format!("AIZO_API_URL={url}"));
        if !key.trim().is_empty() {
            lines.push(format!("AIZO_API_KEY={}", key.trim()));
        }
    }

    println!();
    println!("Auto-generate keywords during analyze?");
    println!("  The LLM will add synonyms to each entry for richer recall.");
    println!("  Disable if you prefer to control keywords manually via 'aizo tag'.");
    let kw = prompt_yn("Enable keywords? [y/N]:", false)?;
    if kw {
        lines.push("AIZO_AUTO_KEYWORDS=true".into());
    }

    std::fs::create_dir_all(&aizo_dir)?;
    std::fs::write(&env_path, lines.join("\n") + "\n")?;
    println!("\nWriting config to {} ... done", env_path.display());
    println!("Database initialized at {}", db_path.display());

    // Reload the env we just wrote so the connection test picks it up
    dotenvy::from_path(&env_path).ok();

    println!();
    if prompt_yn("Verify connection? [Y/n]:", true)? {
        print!("Testing… ");
        io::stdout().flush()?;
        match analyzer::test_connection() {
            Ok(_) => println!("✓  LLM responded successfully. aizo is ready."),
            Err(e) => println!("✗  Connection test failed: {e}\n   Check your settings and run aizo init again."),
        }
    }

    Ok(())
}

// ── Env loading ───────────────────────────────────────────────────────────────

fn load_env() -> (bool, bool) {
    // Load user config first, then project override (later loads don't overwrite earlier ones).
    // Shell env always wins — dotenvy never overwrites already-set variables.
    let user_env = home_dir()
        .map(|h| dotenvy::from_path(h.join(".aizo").join(".env")).is_ok())
        .unwrap_or(false);
    let project_env = dotenvy::dotenv().is_ok();
    (user_env, project_env)
}

fn main() -> Result<()> {
    let (user_env, project_env) = load_env();
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db_path);
    let db = Db::open(&db_path)?;

    // Backfill scenario associations from existing keywords (idempotent)
    {
        if let Ok(sc_config) = load_scenario_config() {
            let config_map: HashMap<String, (String, Vec<String>)> = sc_config.scenarios.iter()
                .map(|(k, v)| (k.clone(), (v.description.clone(), v.keywords.clone())))
                .collect();
            match db.backfill_scenarios(&config_map) {
                Ok(n) if n > 0 => eprintln!("Backfilled {n} scenario associations from existing keywords."),
                Err(e) => eprintln!("Note: scenario backfill skipped ({e})"),
                _ => {}
            }
        }
    }

    match cli.command {
        Command::Keywords { sort } => {
            let mut kws = db.all_keywords()?;

            if sort == "alpha" {
                kws.sort_by(|a, b| a.0.cmp(&b.0));
            } else {
                kws.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            }

            if kws.is_empty() {
                println!("No keywords stored yet.");
            } else {
                let taxonomy = load_taxonomy();
                match &taxonomy {
                    Some(terms) => {
                        let term_set: std::collections::HashSet<&str> =
                            terms.iter().map(String::as_str).collect();
                        let active_count = kws.iter().filter(|(k, _)| term_set.contains(k.as_str())).count();
                        println!("Taxonomy coverage: {active_count} / {} terms active\n", terms.len());

                        println!("  {:<28}  entries", "keyword");
                        println!("  {}", "─".repeat(40));

                        for term in terms {
                            if let Some((_, n)) = kws.iter().find(|(k, _)| k == term) {
                                println!("  {:<28}  {n}", term);
                            } else {
                                println!("  {:<28}  —", term);
                            }
                        }
                        let extras: Vec<_> = kws.iter().filter(|(k, _)| !term_set.contains(k.as_str())).collect();
                        if !extras.is_empty() {
                            println!("\n  [not in taxonomy]");
                            for (kw, n) in &extras {
                                println!("  {:<28}  {n}", kw);
                            }
                        }
                    }
                    None => {
                        println!("  {:<28}  entries", "keyword");
                        println!("  {}", "─".repeat(40));
                        for (kw, n) in &kws {
                            println!("  {:<28}  {n}", kw);
                        }
                    }
                }
            }
        }

        Command::Scenarios => {
            let config = load_scenario_config()?;
            let config_map: HashMap<String, (String, Vec<String>)> = config.scenarios.iter()
                .map(|(k, v)| (k.clone(), (v.description.clone(), v.keywords.clone())))
                .collect();
            let scenarios = db.all_scenarios(&config_map)?;
            if scenarios.is_empty() {
                println!("No scenarios configured.");
            } else {
                println!("  {:<20}  {:>7}  {:>8}  {}", "scenario", "entries", "keywords", "description");
                println!("  {}", "─".repeat(80));
                for (name, stats) in &scenarios {
                    println!("  {:<20}  {:>7}  {:>8}  {}",
                        name, stats.entries, stats.keywords, stats.description);
                }
            }
        }

        Command::Init => {
            cmd_init(&db_path)?;
        }

        Command::Analyze { file } => {
            let text = read_text(file)?;

            let hash = content_hash(&text);
            if db.has_analyzed(&hash)? {
                eprintln!("Session already analyzed (hash {hash}), skipping.");
                return Ok(());
            }

            let existing_profile = profile_context(&db)?;
            let taxonomy = load_taxonomy();

            eprintln!("Analyzing…");
            let result = analyzer::analyze(&text, existing_profile.as_deref(), taxonomy.as_deref())?;

            for e in &result.entries {
                db.upsert(&e.item, &e.reason, &e.keywords, &[], e.base_score, "analysis")?;
            }
            db.log_session(result.entries.len(), &hash)?;
            print_analyze_summary(&result.entries);
        }

        Command::Extract { file } => {
            let text = read_text(file)?;
            let existing_profile = profile_context(&db)?;
            let taxonomy = load_taxonomy();
            print!("{}", analyzer::extraction_prompt(&text, existing_profile.as_deref(), taxonomy.as_deref()));
        }

        Command::Import => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            let result = analyzer::parse_result(&buf)?;
            for e in &result.entries {
                db.upsert(&e.item, &e.reason, &e.keywords, &[], e.base_score, "analysis")?;
            }
            db.log_session(result.entries.len(), "")?;
            print_analyze_summary(&result.entries);
        }

        Command::Recall { query, score_bands, types, limit, scenario, no_touch, json } => {
            // Merge --score-band and deprecated --type
            let mut bands = score_bands;
            if bands.is_empty() { bands = types; }
            let ranges: Vec<(f64, f64)> = bands.iter()
                .map(|t| parse_score_band(t))
                .collect::<Result<_>>()?;

            let prefs = if let Some(s) = scenario.as_deref() {
                // Scenario-based recall: exact match + keyword expansion
                let config = load_scenario_config()?;
                let canonical = resolve_scenario(s, &config)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| s.to_string());

                // 1. Exact scenario matches via JOIN
                let mut exact = db.recall(&[], &ranges, Some(&canonical), None, !no_touch)?;

                // 2. Keyword expansion from scenario config
                let mut expanded = if let Some(def) = config.scenarios.get(&canonical) {
                    let kw_queries: Vec<&str> = def.keywords.iter().map(String::as_str).collect();
                    db.recall(&kw_queries, &ranges, None, None, false)?
                } else {
                    vec![]
                };

                // 3. Merge and dedupe by id
                let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
                let mut merged = Vec::new();
                for p in exact.drain(..).chain(expanded.drain(..)) {
                    if seen.insert(p.id) {
                        merged.push(p);
                    }
                }
                // Re-sort by effective_weight
                merged.sort_by(|a, b| b.effective_weight.partial_cmp(&a.effective_weight).unwrap());
                if let Some(n) = limit { merged.truncate(n); }
                if !no_touch {
                    for p in &merged {
                        let _ = db.touch(&p.item);
                    }
                }
                merged
            } else {
                // Plain keyword / score-band recall
                let queries: Vec<&str> = query.as_deref().map(|q| vec![q]).unwrap_or_default();
                db.recall(&queries, &ranges, None, limit, !no_touch)?
            };

            if prefs.is_empty() {
                let scope = if bands.is_empty() { String::new() }
                            else { format!(" [{}]", bands.join(", ")) };
                let what  = query.as_deref().unwrap_or(scenario.as_deref().unwrap_or("*"));
                println!("No preferences matched \"{what}\"{scope}.");
            } else {
                print_entries(&prefs, json);
            }
        }

        Command::Top { n, score_bands, types, json } => {
            let mut bands = score_bands;
            if bands.is_empty() { bands = types; }
            let ranges: Vec<(f64, f64)> = bands.iter()
                .map(|t| parse_score_band(t))
                .collect::<Result<_>>()?;
            let prefs = db.top(n, &ranges)?;
            if prefs.is_empty() {
                println!("No preferences recorded yet.");
            } else {
                print_entries(&prefs, json);
            }
        }

        Command::Show { json } => {
            let prefs = db.all()?;
            if prefs.is_empty() {
                println!("No preferences recorded yet.");
            } else {
                print_entries(&prefs, json);
            }
        }

        Command::Add { item, reason, score, keywords, scenarios } => {
            let base_score = match score {
                Some(s) if (0.0..=10.0).contains(&s) => s,
                Some(s) => anyhow::bail!("--score must be between 0.0 and 10.0, got {s}"),
                None => 9.0,
            };
            let kws: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
            let scens: Vec<String> = scenarios.iter().map(|s| s.to_lowercase()).collect();
            db.upsert(&item, &reason, &kws, &scens, base_score, "manual")?;
            let label = score_label(base_score);
            let mut extras = Vec::new();
            if !kws.is_empty() { extras.push(format!("keywords: {}", kws.join(", "))); }
            if !scens.is_empty() { extras.push(format!("scenarios: {}", scens.join(", "))); }
            if extras.is_empty() {
                println!("Added [{label}]: \"{item}\" (score {base_score:.1})");
            } else {
                println!("Added [{label}]: \"{item}\" (score {base_score:.1})  {}", extras.join("  "));
            }
        }

        Command::Tag { item, keywords, scenarios } => {
            if keywords.is_empty() && scenarios.is_empty() {
                anyhow::bail!("usage: aizo tag --keywords <kw> --scenarios <scenario> <item>");
            }
            let kws: Option<Vec<String>> = if keywords.is_empty() {
                None
            } else {
                Some(keywords.iter().map(|k| k.to_lowercase()).collect())
            };
            let scens: Option<Vec<String>> = if scenarios.is_empty() {
                None
            } else {
                Some(scenarios.iter().map(|s| s.to_lowercase()).collect())
            };
            let found = db.tag(&item, kws.as_deref(), scens.as_deref())?;
            if found {
                let mut parts = Vec::new();
                if let Some(ref k) = kws { parts.push(format!("keywords: {}", k.join(", "))); }
                if let Some(ref s) = scens { parts.push(format!("scenarios: {}", s.join(", "))); }
                println!("Tagged \"{item}\"  {}", parts.join("  "));
            } else {
                println!("Not found: \"{item}\"");
            }
        }

        Command::Touch { item } => {
            let item_str = item.join(" ");
            if item_str.is_empty() {
                anyhow::bail!("usage: aizo touch <item…>");
            }
            let found = db.touch(&item_str)?;
            if found {
                println!("Touched \"{item_str}\" — decay clock reset.");
            } else {
                println!("Not found: \"{item_str}\"");
            }
        }

        Command::Remove { item } => {
            let item_str = item.join(" ");
            if item_str.is_empty() {
                anyhow::bail!("usage: aizo remove <item…>");
            }
            let removed = db.remove(&item_str)?;
            if removed {
                println!("Removed \"{item_str}\"");
            } else {
                println!("Not found: \"{item_str}\"");
            }
        }

        Command::Clear => {
            db.clear()?;
            println!("Preference profile cleared.");
        }

        Command::Info => {
            let stats = db.stats()?;
            let cfg = db.get_decay_config()?;
            println!("Database    : {}", db_path.display());
            println!("Config");
            println!("  ~/.aizo/.env : {}", if user_env    { "loaded" } else { "not found" });
            println!("  ./.env       : {}", if project_env { "loaded" } else { "not found" });
            println!("  AIZO_MODEL         : {}", std::env::var("AIZO_MODEL").unwrap_or_else(|_| "(not set)".into()));
            println!("  AIZO_API_URL       : {}", std::env::var("AIZO_API_URL").unwrap_or_else(|_| "(not set)".into()));
            println!("  AIZO_API_FORMAT    : {}", std::env::var("AIZO_API_FORMAT").unwrap_or_else(|_| "(not set)".into()));
            println!("  AIZO_AUTO_KEYWORDS : {}", std::env::var("AIZO_AUTO_KEYWORDS").unwrap_or_else(|_| "false (default)".into()));
            println!("  AIZO_MAX_TOKENS    : {}", std::env::var("AIZO_MAX_TOKENS").unwrap_or_else(|_| "8192 (default)".into()));
            println!("Total       : {}", stats.total());
            println!("  liked     : {} (score ≥ 7)", stats.high);
            println!("  neutral   : {} (score 4–6)", stats.mid);
            println!("  disliked  : {} (score ≤ 3)", stats.low);
            println!("Sessions    : {}", stats.sessions);
            println!("Decay");
            println!("  half-life : {} days", cfg.half_life_days);
            println!("  floor     : {}", cfg.floor);
        }

        Command::Config { action } => match action {
            ConfigCmd::Show => {
                let cfg = db.get_decay_config()?;
                println!("{}", serde_json::to_string_pretty(&cfg)?);
            }
            ConfigCmd::SetHalfLife { days } => {
                if days <= 0.0 {
                    anyhow::bail!("half-life must be > 0");
                }
                let cfg = db.get_decay_config()?;
                db.set_decay_config(days, cfg.floor)?;
                println!("Decay half-life set to {days} days.");
            }
            ConfigCmd::SetFloor { floor } => {
                if !(0.0..1.0).contains(&floor) {
                    anyhow::bail!("floor must be in [0.0, 1.0)");
                }
                let cfg = db.get_decay_config()?;
                db.set_decay_config(cfg.half_life_days, floor)?;
                println!("Decay floor set to {floor}.");
            }
        },
    }

    Ok(())
}
