use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::scoring::{self, DecayConfig};

pub const TOUCH_COOLDOWN_HOURS: i64 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TouchStatus {
    Touched,
    Cooldown,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    pub id: i64,
    pub item: Option<String>,
    pub status: TouchStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub id: i64,
    pub item: String,
    pub reason: String,
    pub keywords: Vec<String>, // synonym/related-term tags for richer recall
    pub scenarios: Vec<String>, // task scenarios this entry applies to
    pub base_score: f64,       // 0–10: 0 = hard limit, 10 = strong love
    pub source: String,        // "analysis" | "manual"
    pub added_at: String,
    pub last_seen: String,   // reset on every reinforcement; drives decay clock
    pub touch_count: i64,    // number of times this entry has been touched
    pub token_count: i64,    // estimated token length, only maintained when enabled
    pub score_exponent: f64, // α = (10 − s) / 10
    pub decay_coefficient: f64, // d(t) ∈ [floor, 1.0]
    pub effective_weight: f64, // w = s · d(t)^α
}

pub struct Db {
    conn: Connection,
}

fn parse_keywords(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub(crate) fn estimate_token_count(item: &str, reason: &str, keywords: &[String]) -> i64 {
    let mut tokens = 0i64;
    let mut ascii_run = 0usize;

    let flush_ascii = |run: &mut usize, tokens: &mut i64| {
        if *run > 0 {
            *tokens += (*run).div_ceil(4) as i64;
            *run = 0;
        }
    };

    for ch in item
        .chars()
        .chain(std::iter::once(' '))
        .chain(reason.chars())
        .chain(std::iter::once(' '))
        .chain(keywords.join(" ").chars())
    {
        if ch.is_ascii_alphanumeric() {
            ascii_run += 1;
        } else {
            flush_ascii(&mut ascii_run, &mut tokens);
            if is_cjk(ch) || (!ch.is_ascii() && !ch.is_whitespace()) {
                tokens += 1;
            }
        }
    }
    flush_ascii(&mut ascii_run, &mut tokens);
    tokens
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xf900..=0xfaff
            | 0x20000..=0x2a6df
            | 0x2a700..=0x2b73f
            | 0x2b740..=0x2b81f
            | 0x2b820..=0x2ceaf
            | 0x30000..=0x3134f
    )
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
        self.conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS schema_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
        ",
        )?;

        let mut version = self.schema_version();

        if version < 2 {
            self.conn.execute_batch(
                "
                DROP TABLE IF EXISTS preferences;
                DROP TABLE IF EXISTS sessions;
                DROP TABLE IF EXISTS decay_config;
            ",
            )?;
        }

        // Ensure supporting tables always exist
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS decay_config (
                id              INTEGER PRIMARY KEY CHECK(id = 1),
                half_life_days  REAL    NOT NULL DEFAULT 30.0,
                floor           REAL    NOT NULL DEFAULT 0.1
            );
            INSERT OR IGNORE INTO decay_config (id, half_life_days, floor)
                VALUES (1, 30.0, 0.1);
        ",
        )?;

        if version < 5 {
            // Rebuild preferences table: drop category, unique on LOWER(item) only.
            // For any duplicate items (same item under different old categories),
            // keep the row whose base_score is furthest from neutral (5.0).
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS preferences_v5 (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    item        TEXT    NOT NULL,
                    reason      TEXT    NOT NULL,
                    keywords    TEXT    NOT NULL DEFAULT '',
                    base_score  REAL    NOT NULL DEFAULT 5.0,
                    source      TEXT    NOT NULL DEFAULT 'manual',
                    added_at    TEXT    NOT NULL,
                    last_seen   TEXT    NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_pref_item_v5
                    ON preferences_v5(LOWER(item));
            ",
            )?;

            // Copy from old table if it exists (versions 2–4 had category column).
            let old_exists: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='preferences'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                > 0;

            if old_exists {
                // Order by extremity so INSERT OR IGNORE keeps the most salient entry
                // when the same item appeared under multiple old categories.
                self.conn.execute_batch(
                    "
                    INSERT OR IGNORE INTO preferences_v5
                        (item, reason, keywords, base_score, source, added_at, last_seen)
                    SELECT item,
                           reason,
                           COALESCE(keywords, ''),
                           base_score,
                           source,
                           added_at,
                           last_seen
                    FROM preferences
                    ORDER BY ABS(base_score - 5.0) DESC, last_seen DESC;

                    DROP TABLE preferences;
                ",
                )?;
            }

            self.conn.execute_batch(
                "
                ALTER TABLE preferences_v5 RENAME TO preferences;
            ",
            )?;
        } else {
            // Fresh database at v5+: create the table directly
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS preferences (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    item        TEXT    NOT NULL,
                    reason      TEXT    NOT NULL,
                    keywords    TEXT    NOT NULL DEFAULT '',
                    base_score  REAL    NOT NULL DEFAULT 5.0,
                    source      TEXT    NOT NULL DEFAULT 'manual',
                    added_at    TEXT    NOT NULL,
                    last_seen   TEXT    NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_pref_item
                    ON preferences(LOWER(item));
            ",
            )?;
        }

        if version < 5 {
            self.set_version(5)?;
            version = 5;
        }

        if version < 6 {
            self.conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS preference_scenarios (
                    preference_id INTEGER NOT NULL,
                    scenario TEXT NOT NULL,
                    PRIMARY KEY (preference_id, scenario),
                    FOREIGN KEY (preference_id) REFERENCES preferences(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_preference_scenarios_scenario
                    ON preference_scenarios(scenario);
            ",
            )?;
        }

        if version < 6 {
            self.set_version(6)?;
            version = 6;
        }

        if version < 7 {
            self.conn.execute_batch(
                "
                ALTER TABLE preferences ADD COLUMN touch_count INTEGER NOT NULL DEFAULT 0;
            ",
            )?;
        }

        if version < 7 {
            self.set_version(7)?;
            version = 7;
        }

        if version < 8 {
            if !self.column_exists("preferences", "token_count")? {
                self.conn.execute_batch(
                    "
                    ALTER TABLE preferences ADD COLUMN token_count INTEGER NOT NULL DEFAULT 0;
                ",
                )?;
            }
            if !self.column_exists("decay_config", "token_counting_enabled")? {
                self.conn.execute_batch(
                    "
                    ALTER TABLE decay_config ADD COLUMN token_counting_enabled INTEGER NOT NULL DEFAULT 0;
                ",
                )?;
            }
            self.set_version(8)?;
        }
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(cols.iter().any(|name| name == column))
    }

    // ── Scenario backfill ──────────────────────────────────────────────────────

    /// Backfill scenario associations from existing keyword data.
    /// Idempotent: uses INSERT OR IGNORE so repeated calls are safe.
    pub fn backfill_scenarios(
        &self,
        scenario_config: &HashMap<String, (String, Vec<String>)>,
    ) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT id, keywords FROM preferences")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let mut insert = self.conn.prepare(
            "INSERT OR IGNORE INTO preference_scenarios (preference_id, scenario) VALUES (?1, ?2)",
        )?;

        let mut total = 0usize;
        for (id, keywords_raw) in &rows {
            let kws: Vec<&str> = keywords_raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if kws.is_empty() {
                continue;
            }

            for (scenario_name, (_, scenario_kws)) in scenario_config {
                let hits = kws
                    .iter()
                    .filter(|kw| scenario_kws.iter().any(|sk| sk == **kw))
                    .count();
                // High-confidence: at least 2 keyword matches, or the scenario name itself appears as a keyword
                let direct = kws.contains(&scenario_name.as_str());
                if hits >= 2 || direct {
                    let n = insert.execute(params![id, scenario_name])?;
                    total += n;
                }
            }
        }
        Ok(total)
    }

    pub fn get_decay_config(&self) -> Result<DecayConfig> {
        self.conn
            .query_row(
                "SELECT half_life_days, floor, token_counting_enabled FROM decay_config WHERE id = 1",
                [],
                |row| {
                    Ok(DecayConfig {
                        half_life_days: row.get(0)?,
                        floor: row.get(1)?,
                        token_counting_enabled: row.get::<_, i64>(2)? != 0,
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

    pub fn set_token_counting_enabled(&self, enabled: bool) -> Result<usize> {
        self.conn.execute(
            "UPDATE decay_config SET token_counting_enabled = ?1 WHERE id = 1",
            params![if enabled { 1 } else { 0 }],
        )?;
        if enabled {
            self.backfill_token_counts()
        } else {
            Ok(self
                .conn
                .execute("UPDATE preferences SET token_count = 0", [])?)
        }
    }

    pub fn backfill_token_counts(&self) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, item, reason, keywords FROM preferences")?;
        let rows: Vec<(i64, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let mut updated = 0usize;
        for (id, item, reason, keywords_raw) in rows {
            let keywords = parse_keywords(&keywords_raw);
            let token_count = estimate_token_count(&item, &reason, &keywords);
            updated += self.conn.execute(
                "UPDATE preferences SET token_count = ?1 WHERE id = ?2",
                params![token_count, id],
            )?;
        }
        Ok(updated)
    }

    // ── Row hydration ────────────────────────────────────────────────────────

    // SELECT column order: id(0) item(1) reason(2) keywords(3) base_score(4)
    //                      source(5) added_at(6) last_seen(7) touch_count(8) token_count(9)
    fn hydrate(row: &rusqlite::Row, cfg: &DecayConfig) -> rusqlite::Result<Preference> {
        let last_seen: String = row.get(7)?;
        let keywords_raw: String = row.get(3)?;
        let base_score: f64 = row.get(4)?;

        let s = scoring::compute(base_score, &last_seen, cfg);

        Ok(Preference {
            id: row.get(0)?,
            item: row.get(1)?,
            reason: row.get(2)?,
            keywords: keywords_raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            scenarios: Vec::new(), // filled by load_scenarios_for
            base_score,
            source: row.get(5)?,
            added_at: row.get(6)?,
            last_seen,
            touch_count: row.get(8)?,
            token_count: row.get(9)?,
            score_exponent: s.score_exponent,
            decay_coefficient: s.decay_coefficient,
            effective_weight: s.effective_weight,
        })
    }

    /// Batch-load scenarios for a set of preferences, populating their `.scenarios` fields.
    fn load_scenarios_for(&self, prefs: &mut [Preference]) -> Result<()> {
        if prefs.is_empty() {
            return Ok(());
        }
        let placeholders: Vec<String> = prefs
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT preference_id, scenario FROM preference_scenarios WHERE preference_id IN ({}) ORDER BY preference_id, scenario",
            placeholders.join(", ")
        );
        let ids: Vec<rusqlite::types::Value> = prefs
            .iter()
            .map(|p| rusqlite::types::Value::Integer(p.id))
            .collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let pairs: Vec<(i64, String)> = stmt
            .query_map(rusqlite::params_from_iter(ids), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;

        // Group by preference_id
        let mut map: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
        for (pid, scenario) in pairs {
            map.entry(pid).or_default().push(scenario);
        }

        for pref in prefs.iter_mut() {
            pref.scenarios = map.remove(&pref.id).unwrap_or_default();
        }
        Ok(())
    }

    // ── Writes ────────────────────────────────────────────────────────────────

    /// Upsert with score smoothing (new = old×0.4 + incoming×0.6) and last_seen refresh.
    pub fn upsert(
        &self,
        item: &str,
        reason: &str,
        keywords: &[String],
        scenarios: &[String],
        base_score: f64,
        source: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let kw = keywords.join(", ");
        let cfg = self.get_decay_config()?;
        let token_count = if cfg.token_counting_enabled {
            estimate_token_count(item, reason, keywords)
        } else {
            0
        };
        self.conn.execute(
            "INSERT INTO preferences
                (item, reason, keywords, base_score, source, added_at, last_seen, token_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)
             ON CONFLICT(LOWER(item)) DO UPDATE SET
                reason     = excluded.reason,
                keywords   = excluded.keywords,
                base_score = base_score * 0.4 + excluded.base_score * 0.6,
                source     = excluded.source,
                last_seen  = excluded.last_seen,
                token_count = excluded.token_count",
            params![item, reason, kw, base_score, source, now, token_count],
        )?;

        // Get the row id (new or existing) to write scenario associations
        let pref_id: i64 = self.conn.query_row(
            "SELECT id FROM preferences WHERE LOWER(item) = LOWER(?1)",
            params![item],
            |r| r.get(0),
        )?;

        // Replace scenarios: delete old, insert new
        self.conn.execute(
            "DELETE FROM preference_scenarios WHERE preference_id = ?1",
            params![pref_id],
        )?;
        let mut stmt = self.conn.prepare(
            "INSERT OR IGNORE INTO preference_scenarios (preference_id, scenario) VALUES (?1, ?2)",
        )?;
        for s in scenarios {
            stmt.execute(params![pref_id, s])?;
        }

        Ok(())
    }

    fn touch_allowed(last_seen: &str, now: DateTime<Utc>) -> bool {
        let Ok(last) = DateTime::parse_from_rfc3339(last_seen) else {
            return true;
        };
        now.signed_duration_since(last.with_timezone(&Utc)) >= Duration::hours(TOUCH_COOLDOWN_HOURS)
    }

    fn touch_by_id(&self, id: i64, now: DateTime<Utc>) -> Result<ApplyResult> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT item, last_seen FROM preferences WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        let Some((item, last_seen)) = row else {
            return Ok(ApplyResult {
                id,
                item: None,
                status: TouchStatus::NotFound,
            });
        };

        if !Self::touch_allowed(&last_seen, now) {
            return Ok(ApplyResult {
                id,
                item: Some(item),
                status: TouchStatus::Cooldown,
            });
        }

        self.conn.execute(
            "UPDATE preferences SET last_seen = ?1, touch_count = touch_count + 1 WHERE id = ?2",
            params![now.to_rfc3339(), id],
        )?;
        Ok(ApplyResult {
            id,
            item: Some(item),
            status: TouchStatus::Touched,
        })
    }

    /// Mark recalled entries as actually used, respecting the touch cooldown.
    pub fn apply_ids(&self, ids: &[i64]) -> Result<Vec<ApplyResult>> {
        let now = Utc::now();
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();
        for id in ids {
            if seen.insert(*id) {
                results.push(self.touch_by_id(*id, now)?);
            }
        }
        Ok(results)
    }

    /// Reset the decay clock for one entry without changing its score or reason.
    /// Returns whether the entry was updated, cooled down, or missing.
    pub fn touch(&self, item: &str) -> Result<TouchStatus> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, last_seen FROM preferences WHERE LOWER(item) = LOWER(?1)",
                params![item],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        let Some((id, last_seen)) = row else {
            return Ok(TouchStatus::NotFound);
        };

        let now = Utc::now();
        if !Self::touch_allowed(&last_seen, now) {
            return Ok(TouchStatus::Cooldown);
        }

        self.conn.execute(
            "UPDATE preferences SET last_seen = ?1, touch_count = touch_count + 1 WHERE id = ?2",
            params![now.to_rfc3339(), id],
        )?;
        Ok(TouchStatus::Touched)
    }

    pub fn remove(&self, item: &str) -> Result<bool> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM preferences WHERE LOWER(item) = LOWER(?1)",
                params![item],
                |r| r.get(0),
            )
            .ok();

        let Some(id) = id else {
            return Ok(false);
        };

        self.remove_id(id).map(|removed| removed.is_some())
    }

    pub fn remove_id(&self, id: i64) -> Result<Option<String>> {
        let item: Option<String> = self
            .conn
            .query_row(
                "SELECT item FROM preferences WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .ok();

        let Some(item) = item else {
            return Ok(None);
        };

        self.conn.execute(
            "DELETE FROM preference_scenarios WHERE preference_id = ?1",
            params![id],
        )?;
        self.conn
            .execute("DELETE FROM preferences WHERE id = ?1", params![id])?;
        Ok(Some(item))
    }

    /// Return all scenarios with stats.
    pub fn all_scenarios(
        &self,
        config: &std::collections::HashMap<String, (String, Vec<String>)>,
    ) -> Result<Vec<(String, ScenarioStats)>> {
        let mut stmt = self.conn.prepare(
            "SELECT scenario, COUNT(*) FROM preference_scenarios GROUP BY scenario ORDER BY scenario",
        )?;
        let counts: std::collections::HashMap<String, usize> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let mut result: Vec<(String, ScenarioStats)> = config
            .iter()
            .map(|(name, (desc, kws))| {
                let entries = counts.get(name).copied().unwrap_or(0);
                (
                    name.clone(),
                    ScenarioStats {
                        entries,
                        keywords: kws.len(),
                        description: desc.clone(),
                    },
                )
            })
            .collect();

        // Add any scenarios in DB but not in config
        for (name, entries) in &counts {
            if !config.contains_key(name) {
                result.push((
                    name.clone(),
                    ScenarioStats {
                        entries: *entries,
                        keywords: 0,
                        description: String::new(),
                    },
                ));
            }
        }

        result.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(result)
    }

    /// Return all keywords with their entry counts.
    pub fn all_keywords(&self) -> Result<Vec<(String, usize)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT keywords FROM preferences WHERE keywords != ''")?;
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for row in rows {
            for kw in row.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                *counts.entry(kw.to_string()).or_insert(0) += 1;
            }
        }
        Ok(counts.into_iter().collect())
    }

    pub fn clear(&self) -> Result<()> {
        self.conn.execute_batch("DELETE FROM preferences;")?;
        Ok(())
    }

    /// Update fields on an existing preference entry. Only Some/Non-empty fields are changed.
    /// Returns true if the entry was found.
    pub fn update(
        &self,
        item: &str,
        new_item: Option<&str>,
        reason: Option<&str>,
        score: Option<f64>,
        keywords: Option<&[String]>,
        scenarios: Option<&[String]>,
    ) -> Result<bool> {
        let current: Option<(i64, String, String, String)> = self
            .conn
            .query_row(
                "SELECT id, item, reason, keywords FROM preferences WHERE LOWER(item) = LOWER(?1)",
                params![item],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();

        let Some((pref_id, current_item, current_reason, current_keywords)) = current else {
            return Ok(false);
        };

        if let Some(ni) = new_item {
            self.conn.execute(
                "UPDATE preferences SET item = ?1 WHERE id = ?2",
                params![ni, pref_id],
            )?;
        }

        if let Some(r) = reason {
            self.conn.execute(
                "UPDATE preferences SET reason = ?1 WHERE id = ?2",
                params![r, pref_id],
            )?;
        }

        if let Some(s) = score {
            if !(0.0..=10.0).contains(&s) {
                anyhow::bail!("score must be between 0.0 and 10.0, got {s}");
            }
            self.conn.execute(
                "UPDATE preferences SET base_score = ?1 WHERE id = ?2",
                params![s, pref_id],
            )?;
        }

        if let Some(kws) = keywords {
            let kw = kws
                .iter()
                .map(|k| k.to_lowercase())
                .collect::<Vec<_>>()
                .join(", ");
            self.conn.execute(
                "UPDATE preferences SET keywords = ?1 WHERE id = ?2",
                params![kw, pref_id],
            )?;
        }

        if new_item.is_some() || reason.is_some() || keywords.is_some() {
            let next_item = new_item.unwrap_or(&current_item);
            let next_reason = reason.unwrap_or(&current_reason);
            let next_keywords = keywords
                .map(|kws| kws.to_vec())
                .unwrap_or_else(|| parse_keywords(&current_keywords));
            let cfg = self.get_decay_config()?;
            let token_count = if cfg.token_counting_enabled {
                estimate_token_count(next_item, next_reason, &next_keywords)
            } else {
                0
            };
            self.conn.execute(
                "UPDATE preferences SET token_count = ?1 WHERE id = ?2",
                params![token_count, pref_id],
            )?;
        }

        if let Some(scens) = scenarios {
            self.conn.execute(
                "DELETE FROM preference_scenarios WHERE preference_id = ?1",
                params![pref_id],
            )?;
            let mut stmt = self.conn.prepare(
                "INSERT OR IGNORE INTO preference_scenarios (preference_id, scenario) VALUES (?1, ?2)",
            )?;
            for s in scens {
                stmt.execute(params![pref_id, s])?;
            }
        }

        Ok(true)
    }

    const SELECT: &'static str =
        "SELECT id, item, reason, keywords, base_score, source, added_at, last_seen, touch_count, token_count
         FROM preferences";

    fn sorted(mut rows: Vec<Preference>) -> Vec<Preference> {
        rows.sort_by(|a, b| b.effective_weight.partial_cmp(&a.effective_weight).unwrap());
        rows
    }

    pub fn all(&self) -> Result<Vec<Preference>> {
        let cfg = self.get_decay_config()?;
        let mut stmt = self.conn.prepare(Self::SELECT)?;
        let mut rows: Vec<Preference> = stmt
            .query_map([], |row| Self::hydrate(row, &cfg))?
            .collect::<rusqlite::Result<_>>()?;
        self.load_scenarios_for(&mut rows)?;
        Ok(Self::sorted(rows))
    }

    pub fn top(&self, n: usize, score_ranges: &[(f64, f64)]) -> Result<Vec<Preference>> {
        let mut rows = self.recall(&[], score_ranges, None, None, false)?;
        rows.truncate(n);
        Ok(rows)
    }

    /// Flexible recall: keyword search OR'd across queries, score filter OR'd across ranges.
    ///
    /// - `queries` empty  → no keyword filter (matches all entries)
    /// - `score_ranges` empty → no score filter
    /// - `scenario` → JOIN preference_scenarios to filter by scenario tag
    /// - `limit` applied after sorting by effective_weight
    pub fn recall(
        &self,
        queries: &[&str],
        score_ranges: &[(f64, f64)],
        scenario: Option<&str>,
        limit: Option<usize>,
        touch: bool,
    ) -> Result<Vec<Preference>> {
        let cfg = self.get_decay_config()?;

        let mut where_clauses: Vec<String> = Vec::new();
        let mut bind_vals: Vec<rusqlite::types::Value> = Vec::new();
        let mut joins: Vec<String> = Vec::new();
        let mut p = 1usize; // 1-based SQL param index

        // Scenario filter: JOIN preference_scenarios
        if let Some(s) = scenario {
            joins.push("JOIN preference_scenarios ps ON preferences.id = ps.preference_id".into());
            bind_vals.push(rusqlite::types::Value::Text(s.to_string()));
            where_clauses.push(format!("ps.scenario = ?{}", p));
            p += 1;
        }

        // Keyword filter: each query becomes one OR clause across all text fields
        if !queries.is_empty() {
            let kw_parts: Vec<String> = queries.iter().map(|q| {
                let pattern = format!("%{}%", q.to_lowercase());
                bind_vals.push(rusqlite::types::Value::Text(pattern));
                let i = p; p += 1;
                format!("(LOWER(item) LIKE ?{i} OR LOWER(reason) LIKE ?{i} OR LOWER(keywords) LIKE ?{i})")
            }).collect();
            where_clauses.push(format!("({})", kw_parts.join(" OR ")));
        }

        // Score range filter: multiple ranges are OR'd together
        if !score_ranges.is_empty() {
            let range_parts: Vec<String> = score_ranges
                .iter()
                .map(|(min, max)| {
                    let i = p;
                    p += 2;
                    bind_vals.push(rusqlite::types::Value::Real(*min));
                    bind_vals.push(rusqlite::types::Value::Real(*max));
                    format!("(base_score >= ?{i} AND base_score <= ?{})", i + 1)
                })
                .collect();
            where_clauses.push(format!("({})", range_parts.join(" OR ")));
        }

        let sql = if where_clauses.is_empty() && joins.is_empty() {
            Self::SELECT.to_string()
        } else if joins.is_empty() {
            format!("{} WHERE {}", Self::SELECT, where_clauses.join(" AND "))
        } else {
            let joins_str = joins.join(" ");
            let where_str = if where_clauses.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", where_clauses.join(" AND "))
            };
            format!("{} {} {}", Self::SELECT, joins_str, where_str)
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows: Vec<Preference> = stmt
            .query_map(rusqlite::params_from_iter(bind_vals), |row| {
                Self::hydrate(row, &cfg)
            })?
            .collect::<rusqlite::Result<_>>()?;

        self.load_scenarios_for(&mut rows)?;

        let mut rows = Self::sorted(rows);
        if let Some(n) = limit {
            rows.truncate(n);
        }
        if touch {
            let ids: Vec<i64> = rows.iter().map(|p| p.id).collect();
            self.apply_ids(&ids)?;
        }
        Ok(rows)
    }

    pub fn stats(&self) -> Result<Stats> {
        let count =
            |sql: &str| -> rusqlite::Result<usize> { self.conn.query_row(sql, [], |r| r.get(0)) };
        Ok(Stats {
            high: count("SELECT COUNT(*) FROM preferences WHERE base_score >= 7.0")?,
            mid: count(
                "SELECT COUNT(*) FROM preferences WHERE base_score > 3.0 AND base_score < 7.0",
            )?,
            low: count("SELECT COUNT(*) FROM preferences WHERE base_score <= 3.0")?,
        })
    }
}

#[derive(Debug)]
pub struct Stats {
    pub high: usize, // score ≥ 7: strong likes
    pub mid: usize,  // score 4–6: neutral habits
    pub low: usize,  // score ≤ 3: dislikes / limits
}

#[derive(Debug, Serialize)]
pub struct ScenarioStats {
    pub entries: usize,
    pub keywords: usize,
    pub description: String,
}

impl Stats {
    pub fn total(&self) -> usize {
        self.high + self.mid + self.low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> PathBuf {
        let unique = Utc::now()
            .timestamp_nanos_opt()
            .expect("current timestamp should fit in nanoseconds");
        std::env::temp_dir().join(format!("aizo-{name}-{}-{unique}.db", std::process::id()))
    }

    #[test]
    fn update_rejects_out_of_range_score() -> Result<()> {
        let path = temp_db_path("invalid-score");
        let db = Db::open(&path)?;

        db.upsert(
            "concise code",
            "Prefers short implementations",
            &[],
            &[],
            9.0,
            "manual",
        )?;

        let err = db
            .update("concise code", None, None, Some(999.0), None, None)
            .expect_err("invalid score should fail");
        assert!(err.to_string().contains("score must be between"));

        let prefs = db.all()?;
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].base_score, 9.0);

        drop(db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));

        Ok(())
    }

    #[test]
    fn apply_ids_respects_touch_cooldown() -> Result<()> {
        let path = temp_db_path("touch-cooldown");
        let db = Db::open(&path)?;

        db.upsert(
            "concise code",
            "Prefers short implementations",
            &[],
            &[],
            9.0,
            "manual",
        )?;
        let id = db.all()?[0].id;

        let first = db.apply_ids(&[id])?;
        assert_eq!(first[0].status, TouchStatus::Cooldown);
        assert_eq!(db.all()?[0].touch_count, 0);

        let old = (Utc::now() - Duration::hours(TOUCH_COOLDOWN_HOURS + 1)).to_rfc3339();
        db.conn.execute(
            "UPDATE preferences SET last_seen = ?1 WHERE id = ?2",
            params![old, id],
        )?;

        let second = db.apply_ids(&[id])?;
        assert_eq!(second[0].status, TouchStatus::Touched);
        assert_eq!(db.all()?[0].touch_count, 1);

        let third = db.apply_ids(&[id])?;
        assert_eq!(third[0].status, TouchStatus::Cooldown);
        assert_eq!(db.all()?[0].touch_count, 1);

        drop(db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));

        Ok(())
    }

    #[test]
    fn token_counting_is_config_gated_and_backfilled() -> Result<()> {
        let path = temp_db_path("token-counting");
        let db = Db::open(&path)?;

        db.upsert(
            "简洁中文说明",
            "Prefer concise Chinese explanations with concrete dates",
            &["coding".into(), "brevity".into()],
            &[],
            9.0,
            "manual",
        )?;
        assert_eq!(db.all()?[0].token_count, 0);

        let backfilled = db.set_token_counting_enabled(true)?;
        assert_eq!(backfilled, 1);
        let counted = db.all()?[0].token_count;
        assert!(counted > 0);

        db.update(
            "简洁中文说明",
            None,
            Some("Prefer very concise Chinese explanations"),
            None,
            None,
            None,
        )?;
        assert!(db.all()?[0].token_count > 0);

        let cleared = db.set_token_counting_enabled(false)?;
        assert_eq!(cleared, 1);
        assert_eq!(db.all()?[0].token_count, 0);

        drop(db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));

        Ok(())
    }

    #[test]
    fn remove_id_deletes_exact_entry_and_scenarios() -> Result<()> {
        let path = temp_db_path("remove-id");
        let db = Db::open(&path)?;

        db.upsert(
            "concise code",
            "Prefers short implementations",
            &["coding".into()],
            &["coding".into()],
            9.0,
            "manual",
        )?;
        db.upsert("dark mode", "Prefers dark UI", &[], &[], 7.0, "manual")?;

        let concise = db
            .all()?
            .into_iter()
            .find(|p| p.item == "concise code")
            .expect("concise entry exists");
        assert_eq!(db.remove_id(concise.id)?, Some("concise code".into()));
        assert_eq!(db.remove_id(concise.id)?, None);

        let remaining = db.all()?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].item, "dark mode");

        let scenario_links: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM preference_scenarios WHERE preference_id = ?1",
            params![concise.id],
            |r| r.get(0),
        )?;
        assert_eq!(scenario_links, 0);

        drop(db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));

        Ok(())
    }

    #[test]
    fn estimate_token_count_handles_ascii_and_cjk() {
        let keywords = vec!["coding".into(), "中文".into()];
        let count = estimate_token_count("short memory", "偏好简洁输出", &keywords);
        assert!(count >= 8, "expected mixed-language estimate, got {count}");
    }
}
