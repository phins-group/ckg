// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::json;

use crate::model::{EdgeKind, EdgeRecord, FileRecord, NodeKind, NodeRecord, SearchHit, Subgraph};

pub struct Storage {
    db_path: PathBuf,
    conn: Connection,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub db_path: String,
    pub quick_check: String,
    pub integrity_ok: bool,
    pub indexed_files: usize,
    pub db_bytes: u64,
    pub wal_bytes: Option<u64>,
    pub shm_bytes: Option<u64>,
    pub maintenance_ran: bool,
    pub optimize_ran: bool,
    pub fts_optimize_ran: bool,
    pub wal_checkpoint_ran: bool,
}

#[derive(Debug, Clone)]
pub struct NewFile<'a> {
    pub path: &'a str,
    pub abs_path: &'a str,
    pub extension: Option<&'a str>,
    pub language: Option<&'a str>,
    pub hash: &'a str,
    pub size: i64,
    pub modified_at: i64,
    pub is_binary: bool,
}

#[derive(Debug, Clone)]
pub struct NewNode<'a> {
    pub repo_id: i64,
    pub file_id: Option<i64>,
    pub kind: NodeKind,
    pub name: &'a str,
    pub qualified_name: &'a str,
    pub path: Option<&'a str>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub summary: Option<&'a str>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct NewChunk<'a> {
    pub repo_id: i64,
    pub file_id: i64,
    pub node_id: Option<i64>,
    pub kind: &'a str,
    pub text: &'a str,
    pub search_text: Option<&'a str>,
    pub start_line: i64,
    pub end_line: i64,
    pub summary: Option<&'a str>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ImportEdgeRecord {
    pub source_node_id: i64,
    pub source_path: String,
    pub import_source: String,
    pub metadata: serde_json::Value,
}

impl Storage {
    pub fn open_for_repo(repo_path: &Path) -> Result<Self> {
        let db_dir = repo_path.join(".ckg");
        fs::create_dir_all(&db_dir)
            .with_context(|| format!("failed to create {}", db_dir.display()))?;
        let gitignore = db_dir.join(".gitignore");
        if !gitignore.exists() {
            fs::write(&gitignore, "*\n")
                .with_context(|| format!("failed to write {}", gitignore.display()))?;
        }
        Self::open_path(db_dir.join("ckg.sqlite"))
    }

    pub fn open_path(db_path: PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open {}", db_path.display()))?;
        conn.set_prepared_statement_cache_capacity(256);
        let storage = Self { db_path, conn };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn begin_write(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    pub fn commit_write(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn rollback_write(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    pub fn is_in_write(&self) -> bool {
        !self.conn.is_autocommit()
    }

    pub fn doctor_report(&self, repo_id: i64, maintenance: bool) -> Result<DoctorReport> {
        let mut optimize_ran = false;
        let mut fts_optimize_ran = false;
        let mut wal_checkpoint_ran = false;

        if maintenance {
            self.optimize()?;
            optimize_ran = true;
            fts_optimize_ran = self.optimize_fts()?;
            self.wal_checkpoint_truncate()?;
            wal_checkpoint_ran = true;
        }

        let quick_check = self.quick_check()?;
        Ok(DoctorReport {
            db_path: self.db_path.display().to_string(),
            integrity_ok: quick_check == "ok",
            quick_check,
            indexed_files: self.list_file_paths(repo_id)?.len(),
            db_bytes: file_size(&self.db_path).unwrap_or(0),
            wal_bytes: file_size(self.db_path.with_extension("sqlite-wal")),
            shm_bytes: file_size(self.db_path.with_extension("sqlite-shm")),
            maintenance_ran: maintenance,
            optimize_ran,
            fts_optimize_ran,
            wal_checkpoint_ran,
        })
    }

    pub fn quick_check(&self) -> Result<String> {
        self.conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn optimize(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA optimize;")?;
        Ok(())
    }

    pub fn optimize_fts(&self) -> Result<bool> {
        if !self.fts_available()? {
            return Ok(false);
        }
        self.conn
            .execute("INSERT INTO search_fts(search_fts) VALUES('optimize')", [])?;
        Ok(true)
    }

    pub fn wal_checkpoint_truncate(&self) -> Result<()> {
        let _: (i64, i64, i64) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        Ok(())
    }

    pub fn init_repo(&self, repo_path: &Path) -> Result<i64> {
        let abs = canonical_or_absolute(repo_path)?;
        let name = abs
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repository");
        let abs_string = abs.to_string_lossy().to_string();
        let now = now_secs();
        self.conn.execute(
            "INSERT INTO repos(path, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(path) DO UPDATE SET name=excluded.name, updated_at=excluded.updated_at",
            params![abs_string, name, now],
        )?;
        let repo_id: i64 = self.conn.query_row(
            "SELECT id FROM repos WHERE path = ?1",
            params![abs_string],
            |row| row.get(0),
        )?;
        let repo_node = NewNode {
            repo_id,
            file_id: None,
            kind: NodeKind::Repository,
            name,
            qualified_name: &abs_string,
            path: Some(""),
            start_line: None,
            end_line: None,
            summary: Some("Repository root"),
            metadata: json!({}),
        };
        self.upsert_node(&repo_node)?;
        Ok(repo_id)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;

            CREATE TABLE IF NOT EXISTS repos (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                abs_path TEXT NOT NULL,
                extension TEXT,
                language TEXT,
                hash TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified_at INTEGER NOT NULL,
                is_binary INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                UNIQUE(repo_id, path)
            );

            CREATE TABLE IF NOT EXISTS file_hashes (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                hash TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                path TEXT,
                start_line INTEGER,
                end_line INTEGER,
                summary TEXT,
                metadata TEXT NOT NULL DEFAULT '{}'
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_identity
            ON nodes(repo_id, kind, qualified_name, COALESCE(path, ''), COALESCE(start_line, -1));

            CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes(file_id);
            CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);

            CREATE TABLE IF NOT EXISTS edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                kind TEXT NOT NULL,
                metadata TEXT NOT NULL DEFAULT '{}',
                UNIQUE(repo_id, source_id, target_id, kind)
            );

            CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
            CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);

            CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                node_id INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
                kind TEXT NOT NULL,
                text TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                summary TEXT,
                metadata TEXT NOT NULL DEFAULT '{}'
            );

            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);
            CREATE INDEX IF NOT EXISTS idx_chunks_node ON chunks(node_id);
            "#,
        )?;

        if !self.fts_available()? {
            let _ = self.conn.execute_batch(
                r#"
                CREATE VIRTUAL TABLE search_fts USING fts5(
                    kind,
                    ref_id UNINDEXED,
                    file_id UNINDEXED,
                    node_id UNINDEXED,
                    path,
                    name,
                    text,
                    tokenize = 'unicode61'
                );
                "#,
            );
        }

        Ok(())
    }

    pub fn fts_available(&self) -> Result<bool> {
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='search_fts'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    pub fn find_file_by_path(&self, repo_id: i64, path: &str) -> Result<Option<FileRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, repo_id, path, abs_path, extension, language, hash, size, modified_at, is_binary
             FROM files WHERE repo_id = ?1 AND path = ?2",
        )?;
        stmt.query_row(params![repo_id, path], row_to_file)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_file(&self, file_id: i64) -> Result<Option<FileRecord>> {
        self.conn
            .query_row(
                "SELECT id, repo_id, path, abs_path, extension, language, hash, size, modified_at, is_binary
                 FROM files WHERE id = ?1",
                params![file_id],
                row_to_file,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_file_paths(&self, repo_id: i64) -> Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM files WHERE repo_id = ?1")?;
        let rows = stmt.query_map(params![repo_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn list_files(&self, repo_id: i64) -> Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, path, abs_path, extension, language, hash, size, modified_at, is_binary
             FROM files WHERE repo_id = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map(params![repo_id], row_to_file)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn upsert_file(&self, repo_id: i64, file: &NewFile<'_>) -> Result<i64> {
        let now = now_secs();
        {
            let mut stmt = self.conn.prepare_cached(
                "INSERT INTO files(repo_id, path, abs_path, extension, language, hash, size, modified_at, is_binary, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(repo_id, path) DO UPDATE SET
                    abs_path=excluded.abs_path,
                    extension=excluded.extension,
                    language=excluded.language,
                    hash=excluded.hash,
                    size=excluded.size,
                    modified_at=excluded.modified_at,
                    is_binary=excluded.is_binary,
                    updated_at=excluded.updated_at",
            )?;
            stmt.execute(params![
                repo_id,
                file.path,
                file.abs_path,
                file.extension,
                file.language,
                file.hash,
                file.size,
                file.modified_at,
                i64::from(file.is_binary),
                now
            ])?;
        }
        let file_id: i64 = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT id FROM files WHERE repo_id = ?1 AND path = ?2")?;
            stmt.query_row(params![repo_id, file.path], |row| row.get(0))?
        };
        {
            let mut stmt = self.conn.prepare_cached(
                "INSERT INTO file_hashes(file_id, hash, size, modified_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(file_id) DO UPDATE SET
                    hash=excluded.hash,
                    size=excluded.size,
                    modified_at=excluded.modified_at",
            )?;
            stmt.execute(params![file_id, file.hash, file.size, file.modified_at])?;
        }
        self.upsert_fts(
            "file",
            file_id,
            Some(file_id),
            None,
            file.path,
            file.path,
            file.language.unwrap_or(""),
        )?;
        Ok(file_id)
    }

    pub fn clear_file_index(&self, file_id: i64) -> Result<()> {
        let node_ids = self.node_ids_for_file(file_id)?;
        for node_id in &node_ids {
            self.delete_fts("node", *node_id)?;
        }
        self.delete_fts("file", file_id)?;
        self.delete_fts_by_file("chunk", file_id)?;
        self.conn
            .execute("DELETE FROM chunks WHERE file_id = ?1", params![file_id])?;
        if !node_ids.is_empty() {
            let placeholders = repeat_vars(node_ids.len());
            let mut edge_params = node_ids.clone();
            edge_params.extend(node_ids.iter());
            self.conn.execute(
                &format!(
                    "DELETE FROM edges WHERE source_id IN ({0}) OR target_id IN ({0})",
                    placeholders
                ),
                rusqlite::params_from_iter(edge_params.iter()),
            )?;
        }
        self.conn
            .execute("DELETE FROM nodes WHERE file_id = ?1", params![file_id])?;
        Ok(())
    }

    pub fn delete_file(&self, file_id: i64) -> Result<()> {
        self.clear_file_index(file_id)?;
        self.conn
            .execute("DELETE FROM files WHERE id = ?1", params![file_id])?;
        Ok(())
    }

    pub fn delete_file_by_path(&self, repo_id: i64, path: &str) -> Result<bool> {
        let Some(file) = self.find_file_by_path(repo_id, path)? else {
            return Ok(false);
        };
        self.delete_file(file.id)?;
        Ok(true)
    }

    pub fn remove_missing_files(
        &self,
        repo_id: i64,
        seen_paths: &HashSet<String>,
    ) -> Result<usize> {
        let files = self.list_file_paths(repo_id)?;
        let mut deleted = 0;
        for (file_id, path) in files {
            if !seen_paths.contains(&path) {
                self.delete_file(file_id)?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    pub fn upsert_node(&self, node: &NewNode<'_>) -> Result<i64> {
        let metadata = serde_json::to_string(&node.metadata)?;
        {
            let mut stmt = self.conn.prepare_cached(
                "INSERT INTO nodes(repo_id, file_id, kind, name, qualified_name, path, start_line, end_line, summary, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT DO UPDATE SET
                    file_id=excluded.file_id,
                    name=excluded.name,
                    end_line=excluded.end_line,
                    summary=excluded.summary,
                    metadata=excluded.metadata",
            )?;
            stmt.execute(params![
                node.repo_id,
                node.file_id,
                node.kind.as_str(),
                node.name,
                node.qualified_name,
                node.path,
                node.start_line,
                node.end_line,
                node.summary,
                metadata
            ])?;
        }
        let id = self.find_node_id(
            node.repo_id,
            node.kind.as_str(),
            node.qualified_name,
            node.path.unwrap_or(""),
            node.start_line.unwrap_or(-1),
        )?;
        if matches!(
            node.kind,
            NodeKind::File
                | NodeKind::Function
                | NodeKind::Method
                | NodeKind::Class
                | NodeKind::Type
                | NodeKind::Test
                | NodeKind::Doc
                | NodeKind::Endpoint
        ) {
            self.upsert_fts(
                "node",
                id,
                node.file_id,
                Some(id),
                node.path.unwrap_or(""),
                node.name,
                node.summary.unwrap_or(node.qualified_name),
            )?;
        }
        Ok(id)
    }

    fn find_node_id(
        &self,
        repo_id: i64,
        kind: &str,
        qualified_name: &str,
        path_key: &str,
        start_line_key: i64,
    ) -> Result<i64> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM nodes
             WHERE repo_id = ?1
               AND kind = ?2
               AND qualified_name = ?3
               AND COALESCE(path, '') = ?4
               AND COALESCE(start_line, -1) = ?5",
        )?;
        stmt.query_row(
            params![repo_id, kind, qualified_name, path_key, start_line_key],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn add_edge(
        &self,
        repo_id: i64,
        source_id: i64,
        target_id: i64,
        kind: EdgeKind,
        metadata: serde_json::Value,
    ) -> Result<()> {
        let metadata = serde_json::to_string(&metadata)?;
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO edges(repo_id, source_id, target_id, kind, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(repo_id, source_id, target_id, kind)
             DO UPDATE SET metadata=excluded.metadata",
        )?;
        stmt.execute(params![
            repo_id,
            source_id,
            target_id,
            kind.as_str(),
            metadata
        ])?;
        Ok(())
    }

    pub fn clear_derived_resolution_edges(&self, repo_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM edges
             WHERE repo_id = ?1
               AND (
                    (kind = 'IMPORTS' AND metadata LIKE '%\"resolved\"%')
                 OR (kind IN ('CALLS', 'TESTS', 'REFERENCES') AND metadata LIKE '%\"target_path\"%')
               )",
            params![repo_id],
        )?;
        Ok(())
    }

    pub fn insert_chunk(&self, chunk: &NewChunk<'_>) -> Result<i64> {
        let metadata = serde_json::to_string(&chunk.metadata)?;
        let stored_text = compact_chunk_text(chunk.text);
        {
            let mut stmt = self.conn.prepare_cached(
                "INSERT INTO chunks(repo_id, file_id, node_id, kind, text, start_line, end_line, summary, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            stmt.execute(params![
                chunk.repo_id,
                chunk.file_id,
                chunk.node_id,
                chunk.kind,
                stored_text,
                chunk.start_line,
                chunk.end_line,
                chunk.summary,
                metadata
            ])?;
        }
        let id = self.conn.last_insert_rowid();
        if let Some(search_text) = chunk.search_text {
            self.upsert_fts(
                "chunk",
                id,
                Some(chunk.file_id),
                chunk.node_id,
                "",
                chunk.kind,
                search_text,
            )?;
        }
        Ok(id)
    }

    pub fn get_node(&self, id: i64) -> Result<Option<NodeRecord>> {
        self.conn
            .query_row(
                "SELECT id, repo_id, file_id, kind, name, qualified_name, path, start_line, end_line, summary, metadata
                 FROM nodes WHERE id = ?1",
                params![id],
                row_to_node,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_file_by_path_any_repo(&self, path: &str) -> Result<Option<FileRecord>> {
        self.conn
            .query_row(
                "SELECT id, repo_id, path, abs_path, extension, language, hash, size, modified_at, is_binary
                 FROM files WHERE path = ?1 ORDER BY id LIMIT 1",
                params![path],
                row_to_file,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if self.fts_available()? {
            match self.search_fts(query, limit) {
                Ok(hits) => return Ok(hits),
                Err(_) => return self.search_like(query, limit),
            }
        }
        self.search_like(query, limit)
    }

    fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let fts_query = to_fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT kind, ref_id, file_id, node_id, path, name, snippet(search_fts, 6, '[', ']', ' ... ', 16), bm25(search_fts) AS rank
             FROM search_fts
             WHERE search_fts MATCH ?1
             ORDER BY
                CASE
                    WHEN path = ?2 OR name = ?2 THEN -1000.0
                    WHEN path LIKE ?3 OR name LIKE ?3 THEN -100.0
                    ELSE rank
                END
             LIMIT ?4",
        )?;
        let like = format!("%{}%", query);
        let rows = stmt.query_map(params![fts_query, query, like, limit as i64], |row| {
            let rank: f64 = row.get(7)?;
            Ok(SearchHit {
                kind: row.get(0)?,
                ref_id: row.get(1)?,
                file_id: row.get(2)?,
                node_id: row.get(3)?,
                path: row.get(4)?,
                name: row.get(5)?,
                snippet: row.get(6)?,
                score: -rank,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn search_like(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let like = format!("%{}%", query);
        let mut hits = Vec::new();

        let mut file_stmt = self.conn.prepare(
            "SELECT id, path, language,
                CASE WHEN path = ?1 THEN 100.0 WHEN path LIKE ?2 THEN 50.0 ELSE 1.0 END AS score
             FROM files
             WHERE path LIKE ?2 OR language LIKE ?2
             ORDER BY score DESC, path
             LIMIT ?3",
        )?;
        let file_rows = file_stmt.query_map(params![query, like, limit as i64], |row| {
            let path: String = row.get(1)?;
            Ok(SearchHit {
                kind: "file".to_string(),
                ref_id: row.get(0)?,
                file_id: Some(row.get(0)?),
                node_id: None,
                path: Some(path.clone()),
                name: Some(path),
                snippet: row.get::<_, Option<String>>(2)?,
                score: row.get(3)?,
            })
        })?;
        hits.extend(file_rows.collect::<rusqlite::Result<Vec<_>>>()?);

        let mut node_stmt = self.conn.prepare(
            "SELECT id, file_id, path, name, summary,
                CASE WHEN name = ?1 THEN 100.0 WHEN qualified_name LIKE ?2 THEN 60.0 ELSE 10.0 END AS score
             FROM nodes
             WHERE name LIKE ?2 OR qualified_name LIKE ?2 OR summary LIKE ?2 OR path LIKE ?2
             ORDER BY score DESC, name
             LIMIT ?3",
        )?;
        let node_rows = node_stmt.query_map(params![query, like, limit as i64], |row| {
            Ok(SearchHit {
                kind: "node".to_string(),
                ref_id: row.get(0)?,
                file_id: row.get(1)?,
                node_id: Some(row.get(0)?),
                path: row.get(2)?,
                name: row.get(3)?,
                snippet: row.get(4)?,
                score: row.get(5)?,
            })
        })?;
        hits.extend(node_rows.collect::<rusqlite::Result<Vec<_>>>()?);

        let mut chunk_stmt = self.conn.prepare(
            "SELECT id, file_id, node_id, kind, substr(text, 1, 240), 5.0 AS score
             FROM chunks
             WHERE text LIKE ?1 OR summary LIKE ?1
             LIMIT ?2",
        )?;
        let chunk_rows = chunk_stmt.query_map(params![like, limit as i64], |row| {
            Ok(SearchHit {
                kind: "chunk".to_string(),
                ref_id: row.get(0)?,
                file_id: row.get(1)?,
                node_id: row.get(2)?,
                path: None,
                name: row.get(3)?,
                snippet: row.get(4)?,
                score: row.get(5)?,
            })
        })?;
        hits.extend(chunk_rows.collect::<rusqlite::Result<Vec<_>>>()?);

        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn neighborhood(&self, start_node_id: i64, hops: usize) -> Result<Subgraph> {
        let mut seen_nodes = HashSet::new();
        let mut seen_edges = HashSet::new();
        let mut queue = VecDeque::from([(start_node_id, 0usize)]);
        seen_nodes.insert(start_node_id);

        while let Some((node_id, depth)) = queue.pop_front() {
            if depth >= hops {
                continue;
            }
            for edge in self.edges_touching(node_id)? {
                if seen_edges.insert(edge.id) {
                    for next in [edge.source_id, edge.target_id] {
                        if seen_nodes.insert(next) {
                            queue.push_back((next, depth + 1));
                        }
                    }
                }
            }
        }

        let mut nodes = Vec::new();
        for node_id in seen_nodes {
            if let Some(node) = self.get_node(node_id)? {
                nodes.push(node);
            }
        }
        let mut edges = Vec::new();
        for edge_id in seen_edges {
            if let Some(edge) = self.get_edge(edge_id)? {
                edges.push(edge);
            }
        }
        Ok(Subgraph { nodes, edges })
    }

    pub fn subgraph_by_edge_kinds(
        &self,
        repo_id: i64,
        kinds: &[&str],
        limit: usize,
    ) -> Result<Subgraph> {
        if kinds.is_empty() {
            return Ok(Subgraph {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }
        let kind_list = sql_string_list(kinds);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, repo_id, source_id, target_id, kind, metadata
             FROM edges
             WHERE repo_id = ?1 AND kind IN ({})
             ORDER BY id
             LIMIT ?2",
            kind_list
        ))?;
        let edge_rows = stmt.query_map(params![repo_id, limit as i64], row_to_edge)?;
        let edges = edge_rows.collect::<rusqlite::Result<Vec<_>>>()?;

        let mut node_ids = HashSet::new();
        for edge in &edges {
            node_ids.insert(edge.source_id);
            node_ids.insert(edge.target_id);
        }
        let mut nodes = Vec::new();
        for node_id in node_ids {
            if let Some(node) = self.get_node(node_id)? {
                nodes.push(node);
            }
        }
        Ok(Subgraph { nodes, edges })
    }

    pub fn nodes_by_kinds(
        &self,
        repo_id: i64,
        kinds: &[&str],
        limit: usize,
    ) -> Result<Vec<NodeRecord>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let kind_list = sql_string_list(kinds);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, repo_id, file_id, kind, name, qualified_name, path, start_line, end_line, summary, metadata
             FROM nodes
             WHERE repo_id = ?1 AND kind IN ({})
             ORDER BY COALESCE(path, ''), COALESCE(start_line, -1), id
             LIMIT ?2",
            kind_list
        ))?;
        let rows = stmt.query_map(params![repo_id, limit as i64], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn semantic_summary_nodes(&self, repo_id: i64, limit: usize) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, file_id, kind, name, qualified_name, path, start_line, end_line, summary, metadata
             FROM nodes
             WHERE repo_id = ?1
               AND summary IS NOT NULL
               AND summary != ''
               AND kind IN ('File', 'Function', 'Method', 'Class', 'Type', 'Test', 'Endpoint', 'Doc')
             ORDER BY COALESCE(path, ''), COALESCE(start_line, -1), id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![repo_id, limit as i64], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn file_node_id(&self, file_id: i64) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT id FROM nodes WHERE file_id = ?1 AND kind = 'File' LIMIT 1",
                params![file_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn file_node_id_by_path(&self, repo_id: i64, path: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT n.id
                 FROM nodes n
                 JOIN files f ON f.id = n.file_id
                 WHERE f.repo_id = ?1 AND f.path = ?2 AND n.kind = 'File'
                 LIMIT 1",
                params![repo_id, path],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn import_symbol_edges(&self, repo_id: i64) -> Result<Vec<ImportEdgeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source.id, source.path, target.name, e.metadata
             FROM edges e
             JOIN nodes source ON source.id = e.source_id
             JOIN nodes target ON target.id = e.target_id
             WHERE e.repo_id = ?1
               AND e.kind = 'IMPORTS'
               AND source.kind = 'File'
               AND target.kind = 'Symbol'
               AND target.qualified_name LIKE 'import:%'",
        )?;
        let rows = stmt.query_map(params![repo_id], |row| {
            let metadata: String = row.get(3)?;
            Ok(ImportEdgeRecord {
                source_node_id: row.get(0)?,
                source_path: row.get(1)?,
                import_source: row.get(2)?,
                metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| json!({})),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn symbols_by_file_path(&self, repo_id: i64, path: &str) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.repo_id, n.file_id, n.kind, n.name, n.qualified_name, n.path, n.start_line, n.end_line, n.summary, n.metadata
             FROM nodes n
             JOIN files f ON f.id = n.file_id
             WHERE f.repo_id = ?1
               AND f.path = ?2
               AND n.kind IN ('Function', 'Method', 'Class', 'Type', 'Test')
             ORDER BY n.start_line",
        )?;
        let rows = stmt.query_map(params![repo_id, path], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn node_at_position(
        &self,
        repo_id: i64,
        path: &str,
        line: i64,
    ) -> Result<Option<NodeRecord>> {
        self.conn
            .query_row(
                "SELECT n.id, n.repo_id, n.file_id, n.kind, n.name, n.qualified_name, n.path, n.start_line, n.end_line, n.summary, n.metadata
                 FROM nodes n
                 JOIN files f ON f.id = n.file_id
                 WHERE f.repo_id = ?1
                   AND f.path = ?2
                   AND n.kind IN ('Function', 'Method', 'Class', 'Type', 'Test', 'Endpoint')
                   AND COALESCE(n.start_line, -1) <= ?3
                   AND COALESCE(n.end_line, -1) >= ?3
                 ORDER BY (COALESCE(n.end_line, ?3) - COALESCE(n.start_line, ?3)) ASC, n.id DESC
                 LIMIT 1",
                params![repo_id, path, line],
                row_to_node,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn nodes_matching(
        &self,
        repo_id: i64,
        query: &str,
        kinds: &[&str],
        limit: usize,
    ) -> Result<Vec<NodeRecord>> {
        let like = format!("%{}%", query);
        let kind_clause = if kinds.is_empty() {
            String::new()
        } else {
            format!("AND kind IN ({})", sql_string_list(kinds))
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, repo_id, file_id, kind, name, qualified_name, path, start_line, end_line, summary, metadata
             FROM nodes
             WHERE repo_id = ?1
               {}
               AND (name LIKE ?2 OR qualified_name LIKE ?2 OR path LIKE ?2 OR summary LIKE ?2)
             ORDER BY
                CASE
                    WHEN name = ?3 THEN 0
                    WHEN qualified_name = ?3 THEN 1
                    WHEN name LIKE ?2 THEN 2
                    ELSE 3
                END,
                COALESCE(path, ''),
                COALESCE(start_line, -1)
             LIMIT ?4",
            kind_clause
        ))?;
        let rows = stmt.query_map(params![repo_id, like, query, limit as i64], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn edges_for_node(
        &self,
        repo_id: i64,
        node_id: i64,
        kinds: &[&str],
        direction: EdgeDirection,
        limit: usize,
    ) -> Result<Vec<EdgeRecord>> {
        let kind_clause = if kinds.is_empty() {
            String::new()
        } else {
            format!("AND kind IN ({})", sql_string_list(kinds))
        };
        let direction_clause = match direction {
            EdgeDirection::Incoming => "target_id = ?2",
            EdgeDirection::Outgoing => "source_id = ?2",
            EdgeDirection::Both => "(source_id = ?2 OR target_id = ?2)",
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, repo_id, source_id, target_id, kind, metadata
             FROM edges
             WHERE repo_id = ?1
               AND {}
               {}
             ORDER BY id
             LIMIT ?3",
            direction_clause, kind_clause
        ))?;
        let rows = stmt.query_map(params![repo_id, node_id, limit as i64], row_to_edge)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn endpoints_by_file_path(&self, repo_id: i64, path: &str) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.repo_id, n.file_id, n.kind, n.name, n.qualified_name, n.path, n.start_line, n.end_line, n.summary, n.metadata
             FROM nodes n
             JOIN files f ON f.id = n.file_id
             WHERE f.repo_id = ?1
               AND f.path = ?2
               AND n.kind = 'Endpoint'
             ORDER BY n.start_line",
        )?;
        let rows = stmt.query_map(params![repo_id, path], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub fn edge_count_by_kind(&self, repo_id: i64, kind: EdgeKind) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE repo_id = ?1 AND kind = ?2",
            params![repo_id, kind.as_str()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn top_symbols_for_file(&self, file_id: i64, limit: usize) -> Result<Vec<NodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, file_id, kind, name, qualified_name, path, start_line, end_line, summary, metadata
             FROM nodes
             WHERE file_id = ?1 AND kind IN ('Function', 'Method', 'Class', 'Type', 'Test', 'Endpoint')
             ORDER BY start_line
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit as i64], row_to_node)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn read_chunks_for_file(
        &self,
        file_id: i64,
        limit: usize,
    ) -> Result<Vec<(i64, i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT start_line, end_line, text FROM chunks
             WHERE file_id = ?1 AND kind = 'file'
             ORDER BY start_line LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_id, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn node_ids_for_file(&self, file_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM nodes WHERE file_id = ?1")?;
        let rows = stmt.query_map(params![file_id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn edges_touching(&self, node_id: i64) -> Result<Vec<EdgeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, source_id, target_id, kind, metadata
             FROM edges WHERE source_id = ?1 OR target_id = ?1",
        )?;
        let rows = stmt.query_map(params![node_id], row_to_edge)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn get_edge(&self, id: i64) -> Result<Option<EdgeRecord>> {
        self.conn
            .query_row(
                "SELECT id, repo_id, source_id, target_id, kind, metadata FROM edges WHERE id = ?1",
                params![id],
                row_to_edge,
            )
            .optional()
            .map_err(Into::into)
    }

    fn upsert_fts(
        &self,
        kind: &str,
        ref_id: i64,
        file_id: Option<i64>,
        node_id: Option<i64>,
        path: &str,
        name: &str,
        text: &str,
    ) -> Result<()> {
        if !self.fts_available()? {
            return Ok(());
        }
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO search_fts(kind, ref_id, file_id, node_id, path, name, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        stmt.execute(params![kind, ref_id, file_id, node_id, path, name, text])?;
        Ok(())
    }

    fn delete_fts(&self, kind: &str, ref_id: i64) -> Result<()> {
        if self.fts_available()? {
            let mut stmt = self
                .conn
                .prepare_cached("DELETE FROM search_fts WHERE kind = ?1 AND ref_id = ?2")?;
            stmt.execute(params![kind, ref_id])?;
        }
        Ok(())
    }

    fn delete_fts_by_file(&self, kind: &str, file_id: i64) -> Result<()> {
        if self.fts_available()? {
            let mut stmt = self
                .conn
                .prepare_cached("DELETE FROM search_fts WHERE kind = ?1 AND file_id = ?2")?;
            stmt.execute(params![kind, file_id])?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EdgeDirection {
    Incoming,
    Outgoing,
    Both,
}

fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        path: row.get(2)?,
        abs_path: row.get(3)?,
        extension: row.get(4)?,
        language: row.get(5)?,
        hash: row.get(6)?,
        size: row.get(7)?,
        modified_at: row.get(8)?,
        is_binary: row.get::<_, i64>(9)? != 0,
    })
}

fn row_to_node(row: &Row<'_>) -> rusqlite::Result<NodeRecord> {
    let metadata: String = row.get(10)?;
    Ok(NodeRecord {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        file_id: row.get(2)?,
        kind: row.get(3)?,
        name: row.get(4)?,
        qualified_name: row.get(5)?,
        path: row.get(6)?,
        start_line: row.get(7)?,
        end_line: row.get(8)?,
        summary: row.get(9)?,
        metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| json!({})),
    })
}

fn row_to_edge(row: &Row<'_>) -> rusqlite::Result<EdgeRecord> {
    let metadata: String = row.get(5)?;
    Ok(EdgeRecord {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        source_id: row.get(2)?,
        target_id: row.get(3)?,
        kind: row.get(4)?,
        metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| json!({})),
    })
}

fn repeat_vars(count: usize) -> String {
    std::iter::repeat("?")
        .take(count)
        .collect::<Vec<_>>()
        .join(",")
}

fn sql_string_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",")
}

fn to_fts_query(query: &str) -> String {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .filter(|part| !part.is_empty())
        .map(|part| format!("{}*", part.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf> {
    if let Ok(path) = path.canonicalize() {
        return Ok(path);
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn file_size(path: impl AsRef<Path>) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn compact_chunk_text(text: &str) -> String {
    const MAX_BYTES: usize = 1200;
    const TRUNCATED_SUFFIX: &str =
        "\n... [truncated; source is read from filesystem by line range]";
    if text.len() <= MAX_BYTES + TRUNCATED_SUFFIX.len() {
        return text.to_string();
    }

    let mut end = MAX_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = text[..end].trim_end().to_string();
    preview.push_str(TRUNCATED_SUFFIX);
    preview
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_schema() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage::open_path(dir.path().join("ckg.sqlite"))?;
        assert!(storage.fts_available()?);
        Ok(())
    }

    #[test]
    fn like_search_finds_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage::open_path(dir.path().join("ckg.sqlite"))?;
        let repo_id = storage.init_repo(dir.path())?;
        storage.upsert_file(
            repo_id,
            &NewFile {
                path: "src/avatar.ts",
                abs_path: &dir.path().join("src/avatar.ts").to_string_lossy(),
                extension: Some("ts"),
                language: Some("typescript"),
                hash: "abc",
                size: 10,
                modified_at: 1,
                is_binary: false,
            },
        )?;
        let hits = storage.search("avatar", 5)?;
        assert!(!hits.is_empty());
        Ok(())
    }

    #[test]
    fn doctor_report_runs_quick_check_and_maintenance() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage::open_path(dir.path().join("ckg.sqlite"))?;
        let repo_id = storage.init_repo(dir.path())?;
        let report = storage.doctor_report(repo_id, true)?;
        assert!(report.integrity_ok);
        assert_eq!(report.quick_check, "ok");
        assert!(report.maintenance_ran);
        assert!(report.optimize_ran);
        assert!(report.wal_checkpoint_ran);
        Ok(())
    }
}
