mod db;
mod scoring;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use db::{Db, Preference};
use dirs::home_dir;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// aizo (爱憎) — human-like preference memory for AI agents.
///
/// Extracts, quantifies, decays, and recalls user preferences using SQLite.
/// Effective weight = base_score × decay.
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
        /// Minimum base_score for results (0.0–10.0); overrides band lower bounds
        #[arg(long)]
        min_score: Option<f64>,
        /// Do not refresh last_seen for matched entries
        #[arg(long)]
        no_touch: bool,
        /// Output format: human (default), json, context
        #[arg(long, default_value = "human")]
        format: String,
        /// Shorthand for --format json
        #[arg(long)]
        json: bool,
    },

    /// Show top-N preferences by current effective weight (read-only, no touch)
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
        /// Scenario — pulls entries tagged with this scenario + expanded keywords
        #[arg(long)]
        scenario: Option<String>,
        /// Output format: human (default), json, context
        #[arg(long, default_value = "human")]
        format: String,
        /// Shorthand for --format json
        #[arg(long)]
        json: bool,
    },

    /// Print the full preference profile sorted by effective weight (read-only, no touch)
    Show {
        /// Score-range filter by band name (repeatable or comma-separated): preference, style, habit, aversion, taboo
        #[arg(long = "score-band", value_delimiter = ',')]
        score_bands: Vec<String>,
        /// Deprecated alias for --score-band
        #[arg(long = "type", short = 't', value_delimiter = ',')]
        types: Vec<String>,
        /// Output format: human (default), json, context
        #[arg(long, default_value = "human")]
        format: String,
        /// Shorthand for --format json
        #[arg(long)]
        json: bool,
    },

    /// Export preference profile to portable JSON
    Export {
        /// Output file path; prints to stdout if omitted
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,
    },

    /// Manually add or update a preference
    Add {
        /// Short item label (e.g. "dark mode", "verbose comments")
        item: String,
        /// One-sentence reason why this entry matters
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

    /// Update fields on an existing preference entry — only specified fields are changed
    Update {
        /// Item label to match (case-insensitive)
        item: String,
        /// New item label
        #[arg(long)]
        new_item: Option<String>,
        /// New reason text
        #[arg(long)]
        reason: Option<String>,
        /// New base_score (0.0–10.0)
        #[arg(long, short = 's')]
        score: Option<f64>,
        /// Replace keywords (comma-separated); empty list clears keywords
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        keywords: Option<Vec<String>>,
        /// Replace scenarios (comma-separated); empty list clears scenarios
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        scenarios: Option<Vec<String>>,
    },

    /// Refresh the decay clock of an existing entry without changing its score
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

    /// Wipe the entire preference profile
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

/// Shared scenario-based recall logic used by both `recall` and `top`.
fn scenario_recall(
    db: &Db,
    ranges: &[(f64, f64)],
    scenario: &str,
    limit: Option<usize>,
    touch: bool,
) -> Result<Vec<Preference>> {
    let config = load_scenario_config()?;
    let canonical = resolve_scenario(scenario, &config)
        .map(|c| c.to_string())
        .unwrap_or_else(|| scenario.to_string());

    // 1. Exact scenario matches via JOIN
    let mut exact = db.recall(&[], ranges, Some(&canonical), None, touch)?;

    // 2. Keyword expansion from scenario config
    let mut expanded = if let Some(def) = config.scenarios.get(&canonical) {
        let kw_queries: Vec<&str> = def.keywords.iter().map(String::as_str).collect();
        db.recall(&kw_queries, ranges, None, None, false)?
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
    merged.sort_by(|a, b| b.effective_weight.partial_cmp(&a.effective_weight).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(n) = limit { merged.truncate(n); }
    if touch {
        for p in &merged {
            let _ = db.touch(&p.item);
        }
    }
    Ok(merged)
}

fn resolve_format(format: &str, json: bool) -> &str {
    if json { "json" } else { format }
}

fn print_entries(entries: &[Preference], format: &str) {
    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(entries).unwrap());
        }
        "context" => {
            print_context(entries);
        }
        _ => {
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
    }
}

fn print_context(entries: &[Preference]) {
    let loves:  Vec<&Preference> = entries.iter().filter(|e| e.base_score >= 7.0).collect();
    let habits: Vec<&Preference> = entries.iter().filter(|e| e.base_score >= 4.0 && e.base_score < 7.0).collect();
    let dis:    Vec<&Preference> = entries.iter().filter(|e| e.base_score >= 1.5 && e.base_score < 4.0).collect();
    let taboos: Vec<&Preference> = entries.iter().filter(|e| e.base_score < 1.5).collect();

    println!("[User Preferences]");

    if !loves.is_empty() {
        let items: Vec<String> = loves.iter().map(|e| format!("{} ({:.1})", e.item, e.effective_weight)).collect();
        println!("Loves:       {}", items.join(", "));
    }
    if !habits.is_empty() {
        let items: Vec<String> = habits.iter().map(|e| format!("{} ({:.1})", e.item, e.effective_weight)).collect();
        println!("Habits:      {}", items.join(", "));
    }
    if !dis.is_empty() {
        let items: Vec<String> = dis.iter().map(|e| format!("{} ({:.1})", e.item, e.effective_weight)).collect();
        println!("Dislikes:    {}", items.join(", "));
    }
    if !taboos.is_empty() {
        let items: Vec<String> = taboos.iter().map(|e| e.item.clone()).collect();
        println!("Hard limits: {}", items.join(", "));
    }
}

// ── Env loading ───────────────────────────────────────────────────────────────

fn load_env() -> (bool, bool) {
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

        Command::Recall { query, score_bands, types, limit, scenario, min_score, no_touch, format, json } => {
            let format = resolve_format(&format, json);
            // Merge --score-band and deprecated --type
            let mut bands = score_bands;
            if bands.is_empty() { bands = types; }
            let mut ranges: Vec<(f64, f64)> = bands.iter()
                .map(|t| parse_score_band(t))
                .collect::<Result<_>>()?;

            // Apply --min-score: clamp each band's lower bound, or add an open-ended band
            if let Some(min) = min_score {
                if ranges.is_empty() {
                    ranges.push((min, 10.0));
                } else {
                    ranges = ranges.into_iter()
                        .filter(|(_, max)| *max >= min)
                        .map(|(lo, hi)| (lo.max(min), hi))
                        .collect();
                }
            }

            let prefs = if let Some(s) = scenario.as_deref() {
                scenario_recall(&db, &ranges, s, limit, !no_touch)?
            } else {
                let queries: Vec<&str> = query.as_deref().map(|q| vec![q]).unwrap_or_default();
                db.recall(&queries, &ranges, None, limit, !no_touch)?
            };

            if prefs.is_empty() {
                if format == "json" {
                    println!("[]");
                } else {
                    let scope = if bands.is_empty() && min_score.is_none() { String::new() }
                                else if bands.is_empty() { format!(" [≥{:.1}]", min_score.unwrap()) }
                                else if let Some(m) = min_score { format!(" [≥{:.1}, {}]", m, bands.join(", ")) }
                                else { format!(" [{}]", bands.join(", ")) };
                    let what  = query.as_deref().unwrap_or(scenario.as_deref().unwrap_or("*"));
                    println!("No preferences matched \"{what}\"{scope}.");
                }
            } else {
                print_entries(&prefs, format);
            }
        }

        Command::Top { n, score_bands, types, scenario, format, json } => {
            let format = resolve_format(&format, json);
            let mut bands = score_bands;
            if bands.is_empty() { bands = types; }
            let ranges: Vec<(f64, f64)> = bands.iter()
                .map(|t| parse_score_band(t))
                .collect::<Result<_>>()?;

            let prefs = if let Some(s) = scenario.as_deref() {
                scenario_recall(&db, &ranges, s, None, false)?
            } else {
                db.top(n, &ranges)?
            };

            if prefs.is_empty() {
                if format == "json" { println!("[]"); } else { println!("No preferences recorded yet."); }
            } else {
                let limited: Vec<Preference> = prefs.into_iter().take(n).collect();
                print_entries(&limited, format);
            }
        }

        Command::Show { score_bands, types, format, json } => {
            let format = resolve_format(&format, json);
            let mut bands = score_bands;
            if bands.is_empty() { bands = types; }
            let ranges: Vec<(f64, f64)> = bands.iter()
                .map(|t| parse_score_band(t))
                .collect::<Result<_>>()?;
            let prefs = if ranges.is_empty() {
                db.all()?
            } else {
                db.recall(&[], &ranges, None, None, false)?
            };
            if prefs.is_empty() {
                if format == "json" { println!("[]"); } else { println!("No preferences recorded yet."); }
            } else {
                print_entries(&prefs, format);
            }
        }

        Command::Export { output } => {
            let prefs = db.all()?;
            let entries: Vec<serde_json::Value> = prefs.iter().map(|p| serde_json::json!({
                "item":       p.item,
                "reason":     p.reason,
                "keywords":   p.keywords,
                "scenarios":  p.scenarios,
                "base_score": p.base_score,
            })).collect();
            let payload = serde_json::json!({ "entries": entries });
            let text = serde_json::to_string_pretty(&payload)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, &text)?;
                    println!("Exported {} entries to {}", prefs.len(), path.display());
                }
                None => println!("{text}"),
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

        Command::Update { item, new_item, reason, score, keywords, scenarios } => {
            if new_item.is_none() && reason.is_none() && score.is_none() && keywords.is_none() && scenarios.is_none() {
                anyhow::bail!("specify at least one field to update: --new-item, --reason, --score, --keywords, --scenarios");
            }
            let kws: Option<Vec<String>> = keywords.map(|v| v.into_iter().map(|k| k.to_lowercase()).collect());
            let scens: Option<Vec<String>> = scenarios.map(|v| v.into_iter().map(|s| s.to_lowercase()).collect());

            let found = db.update(
                &item,
                new_item.as_deref(),
                reason.as_deref(),
                score,
                kws.as_deref(),
                scens.as_deref(),
            )?;
            if found {
                let mut parts = Vec::new();
                if let Some(ref ni) = new_item { parts.push(format!("item: {ni}")); }
                if reason.is_some() { parts.push("reason updated".into()); }
                if let Some(s) = score { parts.push(format!("score: {s:.1}")); }
                if let Some(ref k) = kws { parts.push(format!("keywords: {}", k.join(", "))); }
                if let Some(ref s) = scens { parts.push(format!("scenarios: {}", s.join(", "))); }
                if parts.is_empty() {
                    println!("Updated \"{item}\"");
                } else {
                    println!("Updated \"{item}\"  {}", parts.join("  "));
                }
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
