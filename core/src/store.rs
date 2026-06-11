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
    /// Remove the project, its transcript, and its journal. Source media
    /// files on disk are never touched.
    fn delete_project(&self, id: Uuid) -> Result<()>;
    fn list_projects(&self) -> Result<Vec<ProjectSummary>>;
    fn save_transcript(&self, project_id: Uuid, transcript: &Transcript) -> Result<()>;
    fn load_transcript(&self, project_id: Uuid) -> Result<Option<Transcript>>;
    fn save_preferences(&self, prefs: &Preferences) -> Result<()>;
    fn load_preferences(&self) -> Result<Preferences>;
    /// Arbitrary small config values (e.g. the MCP auth token).
    fn get_kv(&self, key: &str) -> Result<Option<String>>;
    fn set_kv(&self, key: &str, value: &str) -> Result<()>;
    /// Per-segment embedding vectors for semantic search (replaces any prior
    /// index for the project).
    fn save_embeddings(
        &self,
        project_id: Uuid,
        model: &str,
        vectors: &[(Uuid, Vec<f32>)],
    ) -> Result<()>;
    /// Returns (model, vectors) or None if the project has no index.
    fn load_embeddings(&self, project_id: Uuid) -> Result<Option<(String, Vec<(Uuid, Vec<f32>)>)>>;
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
                updated_at TEXT NOT NULL, doc TEXT NOT NULL, deleted_at TEXT);
             CREATE TABLE IF NOT EXISTS transcripts (
                project_id TEXT PRIMARY KEY, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS preferences (
                id INTEGER PRIMARY KEY CHECK (id = 1), doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS embeddings (
                project_id TEXT NOT NULL, segment_id TEXT NOT NULL,
                model TEXT NOT NULL, vector BLOB NOT NULL,
                PRIMARY KEY (project_id, segment_id));",
        )?;
        // Migration for installs that predate the trash: the ALTER fails
        // harmlessly once the column exists.
        let _ = conn.execute("ALTER TABLE projects ADD COLUMN deleted_at TEXT", []);
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                updated_at TEXT NOT NULL, doc TEXT NOT NULL, deleted_at TEXT);
             CREATE TABLE IF NOT EXISTS transcripts (
                project_id TEXT PRIMARY KEY, doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS preferences (
                id INTEGER PRIMARY KEY CHECK (id = 1), doc TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS embeddings (
                project_id TEXT NOT NULL, segment_id TEXT NOT NULL,
                model TEXT NOT NULL, vector BLOB NOT NULL,
                PRIMARY KEY (project_id, segment_id));",
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
            "INSERT INTO projects (id, name, updated_at, doc, deleted_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name=?2, updated_at=?3, doc=?4, deleted_at=?5",
            params![
                project.id.to_string(),
                project.name,
                project.updated_at.to_rfc3339(),
                doc,
                project.deleted_at.map(|t| t.to_rfc3339())
            ],
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

    fn delete_project(&self, id: Uuid) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM projects WHERE id = ?1", params![id.to_string()])?;
        conn.execute("DELETE FROM transcripts WHERE project_id = ?1", params![id.to_string()])?;
        conn.execute("DELETE FROM kv WHERE key = ?1", params![format!("journal:{id}")])?;
        Ok(())
    }

    fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, updated_at, deleted_at FROM projects ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (id, name, updated_at, deleted_at) = row?;
            fn parse(t: &str) -> Result<chrono::DateTime<chrono::Utc>> {
                chrono::DateTime::parse_from_rfc3339(t)
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .map_err(|e| CoreError::Storage(e.to_string()))
            }
            out.push(ProjectSummary {
                id: Uuid::parse_str(&id).map_err(|e| CoreError::Storage(e.to_string()))?,
                name,
                updated_at: parse(&updated_at)?,
                deleted_at: deleted_at.as_deref().map(parse).transpose()?,
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

    fn save_embeddings(
        &self,
        project_id: Uuid,
        model: &str,
        vectors: &[(Uuid, Vec<f32>)],
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM embeddings WHERE project_id = ?1", params![project_id.to_string()])?;
        for (seg_id, v) in vectors {
            let blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
            tx.execute(
                "INSERT INTO embeddings (project_id, segment_id, model, vector) VALUES (?1, ?2, ?3, ?4)",
                params![project_id.to_string(), seg_id.to_string(), model, blob],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn load_embeddings(&self, project_id: Uuid) -> Result<Option<(String, Vec<(Uuid, Vec<f32>)>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT segment_id, model, vector FROM embeddings WHERE project_id = ?1")?;
        let rows = stmt.query_map(params![project_id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))
        })?;
        let mut model = String::new();
        let mut out = vec![];
        for row in rows {
            let (seg_id, m, blob) = row?;
            model = m;
            let v: Vec<f32> = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            out.push((
                Uuid::parse_str(&seg_id).map_err(|e| CoreError::Storage(e.to_string()))?,
                v,
            ));
        }
        Ok(if out.is_empty() { None } else { Some((model, out)) })
    }
}
