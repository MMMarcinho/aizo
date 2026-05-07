mod analyzer;
mod db;

use anyhow::Result;
use clap::{Parser, Subcommand};
use db::Db;
use dirs::home_dir;
use std::io::Read;
use std::path::PathBuf;

/// aizo (爱憎) — user preference profiler for AI agents.
///
/// Analyzes session transcripts with a flash LLM to build a persistent list of
/// what the user loves and hates. Use `recall` to retrieve relevant preferences
/// during agent planning.
#[derive(Parser)]
#[command(name = "aizo", version, about, long_about = None)]
struct Cli {
    /// Path to the SQLite database (overrides AIZO_DB_PATH and default)
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

    /// Recall preferences matching a keyword query (the primary agent use-case)
    Recall {
        /// Search term to match against item labels and reasons
        query: String,
    },

    /// Print the full preference profile as JSON
    Show,

    /// Manually add a preference
    Add {
        /// love or hate
        category: String,
        /// Short item label (e.g. "dark mode")
        item: String,
        /// One-sentence reason
        #[arg(trailing_var_arg = true, num_args = 1..)]
        reason: Vec<String>,
    },

    /// Remove a preference by label
    Remove {
        /// love or hate
        category: String,
        /// Item label to remove (case-insensitive)
        #[arg(trailing_var_arg = true, num_args = 1..)]
        item: Vec<String>,
    },

    /// Wipe the entire preference profile
    Clear,

    /// Print database path and statistics
    Info,
}

fn default_db_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aizo")
        .join("preferences.db")
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

            for p in &result.loves {
                db.upsert("love", &p.item, &p.reason, p.confidence, "analysis")?;
            }
            for p in &result.hates {
                db.upsert("hate", &p.item, &p.reason, p.confidence, "analysis")?;
            }
            db.log_session(result.loves.len(), result.hates.len())?;

            println!(
                "✓  Extracted {} love(s) and {} hate(s).",
                result.loves.len(),
                result.hates.len()
            );
            if !result.loves.is_empty() {
                println!("\nLoves:");
                for p in &result.loves {
                    println!("  + [{:3.0}%] {}  —  {}", p.confidence * 100.0, p.item, p.reason);
                }
            }
            if !result.hates.is_empty() {
                println!("\nHates:");
                for p in &result.hates {
                    println!("  - [{:3.0}%] {}  —  {}", p.confidence * 100.0, p.item, p.reason);
                }
            }
        }

        Command::Recall { query } => {
            let prefs = db.recall(&query)?;
            if prefs.is_empty() {
                println!("No preferences matched \"{}\".", query);
            } else {
                println!("{}", serde_json::to_string_pretty(&prefs)?);
            }
        }

        Command::Show => {
            let prefs = db.all()?;
            println!("{}", serde_json::to_string_pretty(&prefs)?);
        }

        Command::Add { category, item, reason } => {
            let cat = validate_category(&category)?;
            if reason.is_empty() {
                anyhow::bail!("usage: aizo add love|hate <item> <reason…>");
            }
            let reason_str = reason.join(" ");
            db.upsert(cat, &item, &reason_str, 1.0, "manual")?;
            println!("Added {cat}: \"{item}\"");
        }

        Command::Remove { category, item } => {
            let cat = validate_category(&category)?;
            let item_str = item.join(" ");
            if item_str.is_empty() {
                anyhow::bail!("usage: aizo remove love|hate <item…>");
            }
            let removed = db.remove(cat, &item_str)?;
            if removed {
                println!("Removed \"{item_str}\" from {cat}s.");
            } else {
                println!("\"{}\" not found in {cat}s.", item_str);
            }
        }

        Command::Clear => {
            db.clear()?;
            println!("Preference profile cleared.");
        }

        Command::Info => {
            let (loves, hates, sessions) = db.stats()?;
            println!("Database : {}", db_path.display());
            println!("Loves    : {loves}");
            println!("Hates    : {hates}");
            println!("Sessions : {sessions}");
        }
    }

    Ok(())
}

fn validate_category(s: &str) -> Result<&'static str> {
    match s {
        "love" => Ok("love"),
        "hate" => Ok("hate"),
        _ => anyhow::bail!("category must be 'love' or 'hate', got '{s}'"),
    }
}
