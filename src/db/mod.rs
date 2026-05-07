use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::scoring::{self, DecayConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub id: i64,
    pub category: String,       // preference | aversion | habit | style | taboo
    pub item: String,
    pub reason: String,
    pub keywords: Vec<String>,  // synonym/related-term tags for richer recall
    pub base_score: f64,        // 0–10: 0 = hard aversion/taboo, 10 = strong preference
    pub source: String,         // "analysis" | "manual"
    pub added_at: String,
    pub last_seen: String,      // reset on every reinforcement; drives decay clock
    pub score_exponent: f64,    // α = (10 − s) / 10
    pub decay_coefficient: f64, // d(t) ∈ [floor, 1.0]
    pub effective_weight: f64,  // w = s · d(t)^α
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening SQLite at {}", path.display()))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    // ── Schema migration ─────────────────────────────────────────────────────

    fn schema_version(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT COALESCE((SELECT value FROM schema_meta WHERE key='version'), '1')",
                [],
                |r| r.get::<_, String>(0),
            )
            .map(|s| s.parse().unwrap_or(1))
            .unwrap_or(1)
    }

    fn set_version(&self, v: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('version', ?1)",
            params![v.to_string()],
        )?;
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS schema_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
        ")?;

        let version = self.schema_version();

        if version < 2 {
            self.conn.execute_batch("
                DROP TABLE IF EXISTS preferences;
                DROP TABLE IF EXISTS sessions;
                DROP TABLE IF EXISTS decay_config;
            ")?;
        }

        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS preferences (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                category    TEXT    NOT NULL
                    CHECK(category IN ('preference','aversion','habit','style','taboo')),
                item        TEXT    NOT NULL,
                reason      TEXT    NOT NULL,
                base_score  REAL    NOT NULL DEFAULT 5.0,
                source      TEXT    NOT NULL DEFAULT 'manual',
                added_at    TEXT    NOT NULL,
                last_seen   TEXT    NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_pref_cat_item
                ON preferences(category, LOWER(item));

            CREATE TABLE IF NOT EXISTS decay_config (
                id              INTEGER PRIMARY KEY CHECK(id = 1),
                half_life_days  REAL    NOT NULL DEFAULT 30.0,
                floor           REAL    NOT NULL DEFAULT 0.1
            );
            INSERT OR IGNORE INTO decay_config (id, half_life_days, floor)
                VALUES (1, 30.0, 0.1);

            CREATE TABLE IF NOT EXISTS sessions (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                analyzed_at  TEXT    NOT NULL,
                extracted    INTEGER NOT NULL DEFAULT 0,
                content_hash TEXT    NOT NULL DEFAULT ''
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_hash
                ON sessions(content_hash) WHERE content_hash != '';
        ")?;

        if version < 3 {
            self.conn.execute_batch(
                "ALTER TABLE preferences ADD COLUMN keywords TEXT NOT NULL DEFAULT '';",
            )?;
        }

        if version < 4 {
            self.conn.execute_batch("
                ALTER TABLE sessions ADD COLUMN content_hash TEXT NOT NULL DEFAULT '';
                CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_hash
                    ON sessions(content_hash) WHERE content_hash != '';
            ")?;
        }

        self.set_version(4)?;
        Ok(())
    }

    // ── Decay config ─────────────────────────────────────────────────────────

    pub fn get_decay_config(&self) -> Result<DecayConfig> {
        self.conn
            .query_row(
                "SELECT half_life_days, floor FROM decay_config WHERE id = 1",
                [],
                |row| Ok(DecayConfig { half_life_days: row.get(0)?, floor: row.get(1)? }),
            )
            .map_err(Into::into)
    }

    pub fn set_decay_config(&self, half_life_days: f64, floor: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE decay_config SET half_life_days = ?1, floor = ?2 WHERE id = 1",
            params![half_life_days, floor],
        )?;
        Ok(())
    }

    // ── Row hydration ────────────────────────────────────────────────────────

    // SELECT column order: id(0) category(1) item(2) reason(3)
    //                      keywords(4) base_score(5) source(6) added_at(7) last_seen(8)
    fn hydrate(row: &rusqlite::Row, cfg: &DecayConfig) -> rusqlite::Result<Preference> {
        let last_seen: String = row.get(8)?;
        let keywords_raw: String = row.get(4)?;
        let base_score: f64 = row.get(5)?;

        let s = scoring::compute(base_score, &last_seen, cfg);

        Ok(Preference {
            id:                row.get(0)?,
            category:          row.get(1)?,
            item:              row.get(2)?,
            reason:            row.get(3)?,
            keywords: keywords_raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            base_score,
            source:            row.get(6)?,
            added_at:          row.get(7)?,
            last_seen,
            score_exponent:    s.score_exponent,
            decay_coefficient: s.decay_coefficient,
            effective_weight:  s.effective_weight,
        })
    }

    // ── Writes ────────────────────────────────────────────────────────────────

    /// Upsert with score smoothing (new = old×0.4 + incoming×0.6) and last_seen refresh.
    pub fn upsert(
        &self,
        category: &str,
        item: &str,
        reason: &str,
        keywords: &[String],
        base_score: f64,
        source: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let kw = keywords.join(", ");
        self.conn.execute(
            "INSERT INTO preferences
                (category, item, reason, keywords, base_score, source, added_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(category, LOWER(item)) DO UPDATE SET
                reason     = excluded.reason,
                keywords   = excluded.keywords,
                base_score = base_score * 0.4 + excluded.base_score * 0.6,
                source     = excluded.source,
                last_seen  = excluded.last_seen",
            params![category, item, reason, kw, base_score, source, now],
        )?;
        Ok(())
    }

    /// Bulk-refresh last_seen for a list of row IDs (used internally by recall).
    fn touch_ids(&self, ids: Vec<i64>) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let placeholders = ids.iter().enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE preferences SET last_seen = ?1 WHERE id IN ({})",
            placeholders
        );
        let mut stmt = self.conn.prepare(&sql)?;
        stmt.execute(rusqlite::params_from_iter(
            std::iter::once(rusqlite::types::Value::Text(now))
                .chain(ids.iter().map(|&id| rusqlite::types::Value::Integer(id))),
        ))?;
        Ok(())
    }

    /// Reset the decay clock for one entry without changing its score or reason.
    /// Returns true if the entry was found and updated, false if not found.
    pub fn touch(&self, category: &str, item: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE preferences SET last_seen = ?1
             WHERE category = ?2 AND LOWER(item) = LOWER(?3)",
            params![now, category, item],
        )?;
        Ok(n > 0)
    }

    pub fn remove(&self, category: &str, item: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM preferences WHERE category = ?1 AND LOWER(item) = LOWER(?2)",
            params![category, item],
        )?;
        Ok(n > 0)
    }

    pub fn clear(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM preferences; DELETE FROM sessions;")?;
        Ok(())
    }

    pub fn has_analyzed(&self, hash: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE content_hash = ?1",
            params![hash],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn log_session(&self, extracted: usize, hash: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO sessions (analyzed_at, extracted, content_hash)
             VALUES (?1, ?2, ?3)",
            params![now, extracted as i64, hash],
        )?;
        Ok(())
    }

    // ── Reads ─────────────────────────────────────────────────────────────────

    const SELECT: &'static str =
        "SELECT id, category, item, reason, keywords, base_score, source, added_at, last_seen
         FROM preferences";

    fn sorted(mut rows: Vec<Preference>) -> Vec<Preference> {
        rows.sort_by(|a, b| b.effective_weight.partial_cmp(&a.effective_weight).unwrap());
        rows
    }

    pub fn all(&self, category: Option<&str>) -> Result<Vec<Preference>> {
        let cfg = self.get_decay_config()?;
        let rows: Vec<Preference> = if let Some(cat) = category {
            let sql = format!("{} WHERE category = ?1", Self::SELECT);
            let mut stmt = self.conn.prepare(&sql)?;
            let x = stmt
                .query_map(params![cat], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?;
            x
        } else {
            let mut stmt = self.conn.prepare(Self::SELECT)?;
            let x = stmt
                .query_map([], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?;
            x
        };
        Ok(Self::sorted(rows))
    }

    pub fn top(&self, n: usize, category: Option<&str>) -> Result<Vec<Preference>> {
        let mut rows = self.all(category)?;
        rows.truncate(n);
        Ok(rows)
    }

    pub fn recall(&self, query: &str, category: Option<&str>, touch: bool) -> Result<Vec<Preference>> {
        let cfg = self.get_decay_config()?;
        let pattern = format!("%{}%", query.to_lowercase());
        let filter =
            "LOWER(item) LIKE ?1 OR LOWER(reason) LIKE ?1 OR LOWER(keywords) LIKE ?1";
        let rows: Vec<Preference> = if let Some(cat) = category {
            let sql = format!("{} WHERE ({}) AND category = ?2", Self::SELECT, filter);
            let mut stmt = self.conn.prepare(&sql)?;
            let x = stmt
                .query_map(params![pattern, cat], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?;
            x
        } else {
            let sql = format!("{} WHERE {}", Self::SELECT, filter);
            let mut stmt = self.conn.prepare(&sql)?;
            let x = stmt
                .query_map(params![pattern], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?;
            x
        };
        let rows = Self::sorted(rows);
        if touch {
            self.touch_ids(rows.iter().map(|p| p.id).collect())?;
        }
        Ok(rows)
    }

    pub fn stats(&self) -> Result<Stats> {
        let count = |sql: &str| -> rusqlite::Result<usize> {
            self.conn.query_row(sql, [], |r| r.get(0))
        };
        Ok(Stats {
            preferences: count("SELECT COUNT(*) FROM preferences WHERE category='preference'")?,
            aversions:   count("SELECT COUNT(*) FROM preferences WHERE category='aversion'")?,
            habits:      count("SELECT COUNT(*) FROM preferences WHERE category='habit'")?,
            styles:      count("SELECT COUNT(*) FROM preferences WHERE category='style'")?,
            taboos:      count("SELECT COUNT(*) FROM preferences WHERE category='taboo'")?,
            sessions:    count("SELECT COUNT(*) FROM sessions")?,
        })
    }
}

#[derive(Debug)]
pub struct Stats {
    pub preferences: usize,
    pub aversions:   usize,
    pub habits:      usize,
    pub styles:      usize,
    pub taboos:      usize,
    pub sessions:    usize,
}

impl Stats {
    pub fn total(&self) -> usize {
        self.preferences + self.aversions + self.habits + self.styles + self.taboos
    }
}
