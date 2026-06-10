//! Persistence: SQLite + local files, no cloud, no account.
//! Projects are stored as JSON documents — the model is the schema; SQLite
//! gives us atomic writes and a cheap library index.

use crate::error::{CoreError, Result};
use crate::model::{Preferences, Project, ProjectSummary, Transcript};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

pub trait Store: Send + Sync {
    fn save_project(&self, project: &Project) -> Result<()>;
    fn load_project(&self, id: Uuid) -> Result<Project>;
    fn list_projects(&self) -> Result<Vec<ProjectSummary>>;
    fn save_transcript(&self, project_id: Uuid, transcript: &Transcript) -> Result<()>;
    fn load_transcript(&self, project_id: Uuid) -> Result<Option<Transcript>>;
    fn save_preferences(&self, prefs: &Preferences) -> Result<()>;
    fn load_preferences(&self) -> Result<Preferences>;
    /// Arbitrary small config values (e.g. the MCP auth token).
    fn get_kv(&self, key: &str) -> Result<Option<String>>;
    fn set_kv(&self, key: &str, value: &str) -> Result<()>;
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                updated_at TEXT NOT NULL, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS transcripts (
                project_id TEXT PRIMARY KEY, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS preferences (
                id INTEGER PRIMARY KEY CHECK (id = 1), doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                updated_at TEXT NOT NULL, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS transcripts (
                project_id TEXT PRIMARY KEY, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS preferences (
                id INTEGER PRIMARY KEY CHECK (id = 1), doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Default on-disk location: `<app data dir>/roughcut/library.db`.
    pub fn default_path() -> PathBuf {
        data_dir().join("library.db")
    }
}

/// App data directory (also holds the MCP endpoint file and downloaded models).
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("roughcut")
}

impl Store for SqliteStore {
    fn save_project(&self, project: &Project) -> Result<()> {
        let doc = serde_json::to_string(project)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, updated_at, doc) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name=?2, updated_at=?3, doc=?4",
            params![project.id.to_string(), project.name, project.updated_at.to_rfc3339(), doc],
        )?;
        Ok(())
    }

    fn load_project(&self, id: Uuid) -> Result<Project> {
        let conn = self.conn.lock().unwrap();
        let doc: String = conn
            .query_row("SELECT doc FROM projects WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .map_err(|_| CoreError::NotFound(format!("project {id}")))?;
        Ok(serde_json::from_str(&doc)?)
    }

    fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, name, updated_at FROM projects ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = vec![];
        for row in rows {
            let (id, name, updated_at) = row?;
            out.push(ProjectSummary {
                id: Uuid::parse_str(&id).map_err(|e| CoreError::Storage(e.to_string()))?,
                name,
                updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)
                    .map_err(|e| CoreError::Storage(e.to_string()))?
                    .with_timezone(&chrono::Utc),
            });
        }
        Ok(out)
    }

    fn save_transcript(&self, project_id: Uuid, transcript: &Transcript) -> Result<()> {
        let doc = serde_json::to_string(transcript)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transcripts (project_id, doc) VALUES (?1, ?2)
             ON CONFLICT(project_id) DO UPDATE SET doc=?2",
            params![project_id.to_string(), doc],
        )?;
        Ok(())
    }

    fn load_transcript(&self, project_id: Uuid) -> Result<Option<Transcript>> {
        let conn = self.conn.lock().unwrap();
        let doc: Option<String> = conn
            .query_row(
                "SELECT doc FROM transcripts WHERE project_id = ?1",
                params![project_id.to_string()],
                |r| r.get(0),
            )
            .ok();
        match doc {
            Some(d) => Ok(Some(serde_json::from_str(&d)?)),
            None => Ok(None),
        }
    }

    fn save_preferences(&self, prefs: &Preferences) -> Result<()> {
        let doc = serde_json::to_string(prefs)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO preferences (id, doc) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET doc=?1",
            params![doc],
        )?;
        Ok(())
    }

    fn load_preferences(&self) -> Result<Preferences> {
        let conn = self.conn.lock().unwrap();
        let doc: Option<String> =
            conn.query_row("SELECT doc FROM preferences WHERE id = 1", [], |r| r.get(0)).ok();
        match doc {
            Some(d) => Ok(serde_json::from_str(&d).unwrap_or_default()),
            None => Ok(Preferences::default()),
        }
    }

    fn get_kv(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0))
            .ok())
    }

    fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=?2",
            params![key, value],
        )?;
        Ok(())
    }
}
