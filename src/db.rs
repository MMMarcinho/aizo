use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub id: i64,
    pub category: String,       // preference | aversion | habit | style | taboo
    pub item: String,
    pub reason: String,
    pub keywords: Vec<String>,  // synonym/related-term tags for richer recall
    pub base_score: f64,        // 0-10: 0=hard aversion/taboo, 10=strong preference
    pub source: String,         // "analysis" | "manual"
    pub added_at: String,
    pub last_seen: String,      // reset each time the entry is reinforced
    pub decay_coefficient: f64, // computed: 0.0–1.0
    pub effective_weight: f64,  // base_score × decay_coefficient
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayConfig {
    pub half_life_days: f64,
    pub floor: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            half_life_days: 30.0,
            floor: 0.1,
        }
    }
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
            // v1 used love/hate + confidence — incompatible, drop and rebuild.
            self.conn.execute_batch("
                DROP TABLE IF EXISTS preferences;
                DROP TABLE IF EXISTS sessions;
                DROP TABLE IF EXISTS decay_config;
            ")?;
        }

        // Create v2 tables if they don't exist yet
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
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                analyzed_at TEXT    NOT NULL,
                extracted   INTEGER NOT NULL DEFAULT 0
            );
        ")?;

        if version < 3 {
            // Add keywords column (comma-separated synonym tags) introduced in v3
            self.conn.execute_batch("
                ALTER TABLE preferences ADD COLUMN keywords TEXT NOT NULL DEFAULT '';
            ")?;
        }

        self.set_version(3)?;
        Ok(())
    }

    // ── Decay config ────────────────────────────────────────────────────────

    pub fn get_decay_config(&self) -> Result<DecayConfig> {
        self.conn
            .query_row(
                "SELECT half_life_days, floor FROM decay_config WHERE id = 1",
                [],
                |row| {
                    Ok(DecayConfig {
                        half_life_days: row.get(0)?,
                        floor: row.get(1)?,
                    })
                },
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

    // ── Decay math ──────────────────────────────────────────────────────────

    fn decay_coefficient(last_seen: &str, cfg: &DecayConfig) -> f64 {
        let Ok(last) = DateTime::parse_from_rfc3339(last_seen) else {
            return 1.0;
        };
        let days = Utc::now()
            .signed_duration_since(last)
            .num_seconds()
            .max(0) as f64
            / 86_400.0;
        let lambda = std::f64::consts::LN_2 / cfg.half_life_days;
        let raw = (-lambda * days).exp();
        cfg.floor + (1.0 - cfg.floor) * raw
    }

    fn hydrate(row: &rusqlite::Row, cfg: &DecayConfig) -> rusqlite::Result<Preference> {
        let last_seen: String = row.get(8)?;
        let keywords_raw: String = row.get(4)?; // SELECT order: id,category,item,reason,keywords,...
        let base_score: f64 = row.get(5)?;
        let decay = Self::decay_coefficient(&last_seen, cfg);
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
            decay_coefficient: decay,
            effective_weight:  base_score * decay,
        })
    }

    // ── Writes ───────────────────────────────────────────────────────────────

    /// Upsert with score smoothing and last_seen refresh.
    /// On conflict: new_score = old×0.4 + incoming×0.6; keywords and last_seen always updated.
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

    pub fn log_session(&self, extracted: usize) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (analyzed_at, extracted) VALUES (?1, ?2)",
            params![now, extracted as i64],
        )?;
        Ok(())
    }

    // ── Reads ────────────────────────────────────────────────────────────────

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
            let x = stmt.query_map(params![cat], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?; x
        } else {
            let mut stmt = self.conn.prepare(Self::SELECT)?;
            let x = stmt.query_map([], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?; x
        };
        Ok(Self::sorted(rows))
    }

    pub fn top(&self, n: usize, category: Option<&str>) -> Result<Vec<Preference>> {
        let mut rows = self.all(category)?;
        rows.truncate(n);
        Ok(rows)
    }

    /// Keyword recall: matches item, reason, OR keywords column; sorted by effective weight.
    pub fn recall(&self, query: &str, category: Option<&str>) -> Result<Vec<Preference>> {
        let cfg = self.get_decay_config()?;
        let pattern = format!("%{}%", query.to_lowercase());
        let base_filter =
            "LOWER(item) LIKE ?1 OR LOWER(reason) LIKE ?1 OR LOWER(keywords) LIKE ?1";
        let rows: Vec<Preference> = if let Some(cat) = category {
            let sql = format!("{} WHERE ({}) AND category = ?2", Self::SELECT, base_filter);
            let mut stmt = self.conn.prepare(&sql)?;
            let x = stmt.query_map(params![pattern, cat], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?; x
        } else {
            let sql = format!("{} WHERE {}", Self::SELECT, base_filter);
            let mut stmt = self.conn.prepare(&sql)?;
            let x = stmt.query_map(params![pattern], |row| Self::hydrate(row, &cfg))?
                .collect::<rusqlite::Result<_>>()?; x
        };
        Ok(Self::sorted(rows))
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
