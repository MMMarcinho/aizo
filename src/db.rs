use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub id: i64,
    pub category: String, // "love" | "hate"
    pub item: String,
    pub reason: String,
    pub confidence: f64,
    pub source: String, // "analysis" | "manual"
    pub added_at: String,
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

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS preferences (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                category    TEXT    NOT NULL CHECK(category IN ('love','hate')),
                item        TEXT    NOT NULL,
                reason      TEXT    NOT NULL,
                confidence  REAL    NOT NULL DEFAULT 1.0,
                source      TEXT    NOT NULL DEFAULT 'manual',
                added_at    TEXT    NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_pref_category_item
                ON preferences(category, LOWER(item));
            CREATE TABLE IF NOT EXISTS sessions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                analyzed_at     TEXT    NOT NULL,
                loves_extracted INTEGER NOT NULL DEFAULT 0,
                hates_extracted INTEGER NOT NULL DEFAULT 0
            );
        ")?;
        Ok(())
    }

    /// Insert or update a preference. Higher confidence wins on conflict.
    pub fn upsert(&self, category: &str, item: &str, reason: &str, confidence: f64, source: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO preferences (category, item, reason, confidence, source, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(category, LOWER(item)) DO UPDATE SET
                reason     = CASE WHEN excluded.confidence >= confidence THEN excluded.reason     ELSE reason     END,
                confidence = CASE WHEN excluded.confidence >= confidence THEN excluded.confidence ELSE confidence END,
                source     = CASE WHEN excluded.confidence >= confidence THEN excluded.source     ELSE source     END,
                added_at   = CASE WHEN excluded.confidence >= confidence THEN excluded.added_at   ELSE added_at   END",
            params![category, item, reason, confidence, source, now],
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
        self.conn.execute_batch("DELETE FROM preferences; DELETE FROM sessions;")?;
        Ok(())
    }

    pub fn all(&self) -> Result<Vec<Preference>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, category, item, reason, confidence, source, added_at
             FROM preferences ORDER BY category, confidence DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Preference {
                id:         row.get(0)?,
                category:   row.get(1)?,
                item:       row.get(2)?,
                reason:     row.get(3)?,
                confidence: row.get(4)?,
                source:     row.get(5)?,
                added_at:   row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Full-text recall: search item and reason with LIKE.
    pub fn recall(&self, query: &str) -> Result<Vec<Preference>> {
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = self.conn.prepare(
            "SELECT id, category, item, reason, confidence, source, added_at
             FROM preferences
             WHERE LOWER(item) LIKE ?1 OR LOWER(reason) LIKE ?1
             ORDER BY confidence DESC"
        )?;
        let rows = stmt.query_map(params![pattern], |row| {
            Ok(Preference {
                id:         row.get(0)?,
                category:   row.get(1)?,
                item:       row.get(2)?,
                reason:     row.get(3)?,
                confidence: row.get(4)?,
                source:     row.get(5)?,
                added_at:   row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn log_session(&self, loves: usize, hates: usize) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (analyzed_at, loves_extracted, hates_extracted) VALUES (?1, ?2, ?3)",
            params![now, loves as i64, hates as i64],
        )?;
        Ok(())
    }

    pub fn stats(&self) -> Result<(usize, usize, usize)> {
        let loves: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM preferences WHERE category='love'", [], |r| r.get(0))?;
        let hates: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM preferences WHERE category='hate'", [], |r| r.get(0))?;
        let sessions: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        Ok((loves, hates, sessions))
    }
}
