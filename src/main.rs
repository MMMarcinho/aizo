mod analyzer;
mod db;

use anyhow::Result;
use clap::{Parser, Subcommand};
use db::{Db, Preference};
use dirs::home_dir;
use std::io::Read;
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
    /// Analyze a session file (or stdin) with the flash LLM and update preferences
    Analyze {
        /// Path to session text file; reads stdin if omitted
        file: Option<PathBuf>,
    },

    /// Recall preferences matching a keyword, sorted by effective weight  [primary agent use]
    Recall {
        /// Keyword to match against item labels and reasons (case-insensitive)
        query: String,
    },

    /// Show top-N preferences by current effective weight
    Top {
        /// Number of entries to show (default: 10)
        #[arg(default_value = "10")]
        n: usize,
    },

    /// Print the full preference profile as JSON, sorted by effective weight
    Show,

    /// Manually add or update a preference
    Add {
        /// Category: preference|love, aversion|hate, habit, style, taboo
        category: String,
        /// Short item label (e.g. "dark mode", "verbose comments")
        item: String,
        /// One-sentence reason (remaining words joined)
        #[arg(trailing_var_arg = true, num_args = 1..)]
        reason: Vec<String>,
    },

    /// Remove a preference by category and item label
    Remove {
        /// Category: preference|love, aversion|hate, habit, style, taboo
        category: String,
        /// Item label to remove (case-insensitive, remaining words joined)
        #[arg(trailing_var_arg = true, num_args = 1..)]
        item: Vec<String>,
    },

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
    SetHalfLife {
        days: f64,
    },
    /// Set minimum decay floor (0.0–1.0; prevents score from reaching zero)
    SetFloor {
        floor: f64,
    },
}

fn default_db_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aizo")
        .join("preferences.db")
}

/// Map user-facing category names (including aliases) to canonical name + default base_score.
fn resolve_category(s: &str) -> Result<(&'static str, f64)> {
    match s {
        "preference" | "love" => Ok(("preference", 9.0)),
        "aversion" | "hate" => Ok(("aversion", 1.0)),
        "habit" => Ok(("habit", 5.0)),
        "style" => Ok(("style", 8.0)),
        "taboo" => Ok(("taboo", 0.5)),
        _ => anyhow::bail!(
            "unknown category '{s}'. Use: preference (love), aversion (hate), habit, style, taboo"
        ),
    }
}

fn print_entries(entries: &[Preference]) {
    println!("{}", serde_json::to_string_pretty(entries).unwrap());
}

fn print_analyze_summary(entries: &[analyzer::ExtractedEntry]) {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in entries {
        *counts.entry(e.category.as_str()).or_insert(0) += 1;
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(k, v)| format!("{v} {k}(s)"))
        .collect();
    parts.sort();
    println!("✓  Extracted {} entries: {}", entries.len(), parts.join(", "));

    let mut preferences: Vec<_> = entries.iter().filter(|e| e.base_score >= 7.0).collect();
    let mut aversions: Vec<_> = entries.iter().filter(|e| e.base_score <= 3.0).collect();
    preferences.sort_by(|a, b| b.base_score.partial_cmp(&a.base_score).unwrap());
    aversions.sort_by(|a, b| a.base_score.partial_cmp(&b.base_score).unwrap());

    if !preferences.is_empty() {
        println!("\nPreferences / loves:");
        for e in &preferences {
            println!("  + [{:4.1}] [{}] {}  —  {}", e.base_score, e.category, e.item, e.reason);
        }
    }
    if !aversions.is_empty() {
        println!("\nAversions / hates:");
        for e in &aversions {
            println!("  - [{:4.1}] [{}] {}  —  {}", e.base_score, e.category, e.item, e.reason);
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db_path);
    let db = Db::open(&db_path)?;

    match cli.command {
        Command::Analyze { file } => {
            let text = match file {
                Some(p) => std::fs::read_to_string(&p)?,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            if text.trim().is_empty() {
                anyhow::bail!("no session text provided");
            }

            let api_key = analyzer::api_key_from_env()?;
            eprintln!("Analyzing with flash LLM (claude-haiku-4-5)…");
            let result = analyzer::analyze(&text, &api_key)?;

            for e in &result.entries {
                db.upsert(&e.category, &e.item, &e.reason, e.base_score, "analysis")?;
            }
            db.log_session(result.entries.len())?;

            print_analyze_summary(&result.entries);
        }

        Command::Recall { query } => {
            let prefs = db.recall(&query)?;
            if prefs.is_empty() {
                println!("No preferences matched \"{}\".", query);
            } else {
                print_entries(&prefs);
            }
        }

        Command::Top { n } => {
            let prefs = db.top(n)?;
            if prefs.is_empty() {
                println!("No preferences recorded yet.");
            } else {
                print_entries(&prefs);
            }
        }

        Command::Show => {
            let prefs = db.all()?;
            if prefs.is_empty() {
                println!("No preferences recorded yet.");
            } else {
                print_entries(&prefs);
            }
        }

        Command::Add { category, item, reason } => {
            let (cat, default_score) = resolve_category(&category)?;
            if reason.is_empty() {
                anyhow::bail!("usage: aizo add <category> <item> <reason…>");
            }
            let reason_str = reason.join(" ");
            db.upsert(cat, &item, &reason_str, default_score, "manual")?;
            println!("Added [{cat}]: \"{item}\" (base_score {default_score:.1})");
        }

        Command::Remove { category, item } => {
            let (cat, _) = resolve_category(&category)?;
            let item_str = item.join(" ");
            if item_str.is_empty() {
                anyhow::bail!("usage: aizo remove <category> <item…>");
            }
            let removed = db.remove(cat, &item_str)?;
            if removed {
                println!("Removed [{cat}]: \"{item_str}\"");
            } else {
                println!("Not found: [{cat}] \"{item_str}\"");
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
            println!("Total       : {}", stats.total());
            println!("  preference: {}", stats.preferences);
            println!("  aversion  : {}", stats.aversions);
            println!("  habit     : {}", stats.habits);
            println!("  style     : {}", stats.styles);
            println!("  taboo     : {}", stats.taboos);
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
