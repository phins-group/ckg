// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;

use crate::{
    model::{EdgeKind, NodeKind, NodeRecord},
    parser::{parse_file, symbol_metadata},
    scanner::{
        hash_entry, scan_path, scan_repo, scan_repo_entries, should_skip_rel_path, ScannedFile,
    },
    storage::{NewChunk, NewFile, NewNode, Storage},
};

pub struct Indexer {
    storage: Storage,
}

#[derive(Debug, Clone)]
pub struct IndexReport {
    pub repo_id: i64,
    pub scanned: usize,
    pub indexed: usize,
    pub skipped_unchanged: usize,
    pub deleted: usize,
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatusReport {
    pub repo_id: i64,
    pub db_path: String,
    pub indexed_files: usize,
    pub scan_mode: String,
    pub scanned: usize,
    pub needs_index: bool,
    pub changed_files: Vec<String>,
    pub new_files: Vec<String>,
    pub modified_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IndexOptions {
    pub full: bool,
}

#[derive(Debug, Default)]
struct GitDelta {
    changed: HashSet<String>,
    deleted: HashSet<String>,
    internal: bool,
}

#[derive(Debug, Clone)]
struct IndexedSymbol {
    id: i64,
    kind: NodeKind,
    start_line: i64,
    end_line: i64,
}

#[derive(Debug, Clone, Default)]
struct PathAliases {
    base_url: Option<String>,
    paths: Vec<(String, Vec<String>)>,
}

impl Indexer {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    pub fn index_repo(&self, repo_path: &Path) -> Result<IndexReport> {
        self.index_repo_with_options(repo_path, IndexOptions::default())
    }

    pub fn status_repo(&self, repo_path: &Path) -> Result<IndexStatusReport> {
        let repo_root = repo_path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", repo_path.display()))?;
        let repo_id = self.storage.init_repo(&repo_root)?;
        let indexed_files = self.storage.list_file_paths(repo_id)?.len();

        if indexed_files == 0 {
            return Ok(IndexStatusReport {
                repo_id,
                db_path: self.storage.db_path().display().to_string(),
                indexed_files,
                scan_mode: "empty_index".to_string(),
                scanned: 0,
                needs_index: true,
                changed_files: Vec::new(),
                new_files: Vec::new(),
                modified_files: Vec::new(),
                deleted_files: Vec::new(),
            });
        }

        if let Some(delta) = git_delta(&repo_root)? {
            let mut changed_files = sorted_strings(delta.changed);
            let deleted_files = sorted_strings(delta.deleted);
            changed_files.retain(|path| !deleted_files.contains(path));
            let mut new_files = Vec::new();
            let mut modified_files = Vec::new();
            for path in &changed_files {
                if self.storage.find_file_by_path(repo_id, path)?.is_some() {
                    modified_files.push(path.clone());
                } else {
                    new_files.push(path.clone());
                }
            }

            return Ok(IndexStatusReport {
                repo_id,
                db_path: self.storage.db_path().display().to_string(),
                indexed_files,
                scan_mode: if delta.internal {
                    "internal_git_delta".to_string()
                } else {
                    "git_status".to_string()
                },
                scanned: changed_files.len() + deleted_files.len(),
                needs_index: !changed_files.is_empty() || !deleted_files.is_empty(),
                changed_files,
                new_files,
                modified_files,
                deleted_files,
            });
        }

        let entries = scan_repo_entries(&repo_root)?;
        let mut seen_paths = HashSet::new();
        let mut new_files = Vec::new();
        let mut modified_files = Vec::new();

        for entry in entries {
            seen_paths.insert(entry.rel_path.clone());
            let Some(existing) = self.storage.find_file_by_path(repo_id, &entry.rel_path)? else {
                new_files.push(entry.rel_path);
                continue;
            };
            if existing.size == entry.size && existing.modified_at == entry.modified_at {
                continue;
            }
            let scanned = hash_entry(entry)?;
            if existing.hash != scanned.hash {
                modified_files.push(scanned.rel_path);
            }
        }

        let mut deleted_files = self
            .storage
            .list_file_paths(repo_id)?
            .into_iter()
            .filter_map(|(_, path)| (!seen_paths.contains(&path)).then_some(path))
            .collect::<Vec<_>>();
        new_files.sort();
        modified_files.sort();
        deleted_files.sort();
        let changed_files = new_files
            .iter()
            .chain(modified_files.iter())
            .cloned()
            .collect::<Vec<_>>();

        Ok(IndexStatusReport {
            repo_id,
            db_path: self.storage.db_path().display().to_string(),
            indexed_files,
            scan_mode: "scan_metadata_hash_candidates".to_string(),
            scanned: seen_paths.len(),
            needs_index: !changed_files.is_empty() || !deleted_files.is_empty(),
            changed_files,
            new_files,
            modified_files,
            deleted_files,
        })
    }

    pub fn index_repo_with_options(
        &self,
        repo_path: &Path,
        options: IndexOptions,
    ) -> Result<IndexReport> {
        let repo_root = repo_path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", repo_path.display()))?;
        let repo_id = self.storage.init_repo(&repo_root)?;
        let repo_node_id = self.ensure_repo_node(repo_id, &repo_root)?;
        let mut indexed = 0;
        let mut skipped_unchanged = 0;
        let mut deleted = 0;
        let mut dir_node_cache = HashMap::new();
        let mut import_node_cache = HashMap::new();
        let existing_files = self.storage.list_file_paths(repo_id)?;
        let internal_git_existed = internal_git_dir(&repo_root).join("config").exists();
        let internal_git_ready = ensure_internal_git(&repo_root).unwrap_or(false);

        let scanned = if options.full
            || existing_files.is_empty()
            || (internal_git_ready && !internal_git_existed)
        {
            let files = scan_repo(&repo_root)?;
            let mut seen_paths = HashSet::new();
            self.storage.begin_write()?;
            let result = (|| -> Result<usize> {
                for scanned in &files {
                    seen_paths.insert(scanned.rel_path.clone());
                    if self.index_scanned_file(
                        repo_id,
                        repo_node_id,
                        scanned,
                        &mut dir_node_cache,
                        &mut import_node_cache,
                    )? {
                        indexed += 1;
                    } else {
                        skipped_unchanged += 1;
                    }
                }
                self.storage.remove_missing_files(repo_id, &seen_paths)
            })();
            match result {
                Ok(count) => {
                    deleted = count;
                    self.storage.commit_write()?;
                }
                Err(error) => {
                    let _ = self.storage.rollback_write();
                    return Err(error);
                }
            }
            if internal_git_ready {
                sync_internal_git_full(&repo_root, &seen_paths)?;
            }
            files.len()
        } else if let Some(delta) = git_delta(&repo_root)? {
            let mut indexed_or_seen_paths = HashSet::new();
            let mut scanned_files = Vec::new();

            for path in &delta.changed {
                if delta.deleted.contains(path) {
                    continue;
                }
                let Some(scanned) = scan_path(&repo_root, path)? else {
                    if delta.internal
                        && !should_skip_rel_path(path)
                        && repo_root.join(path).is_file()
                    {
                        indexed_or_seen_paths.insert(path.clone());
                    }
                    continue;
                };
                indexed_or_seen_paths.insert(path.clone());
                scanned_files.push(scanned);
            }

            self.storage.begin_write()?;
            let result = (|| -> Result<()> {
                for path in &delta.deleted {
                    if self.storage.delete_file_by_path(repo_id, path)? {
                        deleted += 1;
                    }
                }

                for scanned in &scanned_files {
                    if self.index_scanned_file(
                        repo_id,
                        repo_node_id,
                        scanned,
                        &mut dir_node_cache,
                        &mut import_node_cache,
                    )? {
                        indexed += 1;
                    } else {
                        skipped_unchanged += 1;
                    }
                }
                Ok(())
            })();
            match result {
                Ok(()) => self.storage.commit_write()?,
                Err(error) => {
                    let _ = self.storage.rollback_write();
                    return Err(error);
                }
            }
            if delta.internal {
                sync_internal_git_delta(&repo_root, &indexed_or_seen_paths, &delta.deleted)?;
            }
            delta.changed.len() + delta.deleted.len()
        } else {
            let entries = scan_repo_entries(&repo_root)?;
            let mut seen_paths = HashSet::new();
            self.storage.begin_write()?;
            let result = (|| -> Result<usize> {
                for entry in entries {
                    seen_paths.insert(entry.rel_path.clone());
                    if let Some(existing) =
                        self.storage.find_file_by_path(repo_id, &entry.rel_path)?
                    {
                        if existing.size == entry.size && existing.modified_at == entry.modified_at
                        {
                            skipped_unchanged += 1;
                            continue;
                        }
                    }
                    let scanned = hash_entry(entry)?;
                    if self.index_scanned_file(
                        repo_id,
                        repo_node_id,
                        &scanned,
                        &mut dir_node_cache,
                        &mut import_node_cache,
                    )? {
                        indexed += 1;
                    } else {
                        skipped_unchanged += 1;
                    }
                }
                self.storage.remove_missing_files(repo_id, &seen_paths)
            })();
            match result {
                Ok(count) => {
                    deleted = count;
                    self.storage.commit_write()?;
                }
                Err(error) => {
                    let _ = self.storage.rollback_write();
                    return Err(error);
                }
            }
            if internal_git_ready {
                sync_internal_git_full(&repo_root, &seen_paths)?;
            }
            seen_paths.len()
        };

        if indexed > 0 || deleted > 0 {
            let aliases = PathAliases::load(&repo_root);
            self.resolve_local_imports(repo_id, &aliases)?;
        }

        Ok(IndexReport {
            repo_id,
            scanned,
            indexed,
            skipped_unchanged,
            deleted,
            db_path: self.storage.db_path().to_path_buf(),
        })
    }

    fn index_scanned_file(
        &self,
        repo_id: i64,
        repo_node_id: i64,
        scanned: &ScannedFile,
        dir_node_cache: &mut HashMap<String, i64>,
        import_node_cache: &mut HashMap<String, i64>,
    ) -> Result<bool> {
        let existing = self.storage.find_file_by_path(repo_id, &scanned.rel_path)?;
        if let Some(existing) = &existing {
            if existing.hash == scanned.hash {
                return Ok(false);
            }
        }

        let existing_file_id = existing.map(|file| file.id);

        let source = fs::read_to_string(&scanned.abs_path)
            .with_context(|| format!("failed to read {}", scanned.abs_path.display()))?;
        let parsed = parse_file(scanned.language.as_deref(), &scanned.rel_path, &source)?;

        let owns_write = !self.storage.is_in_write();
        if owns_write {
            self.storage.begin_write()?;
        }
        let result = (|| -> Result<()> {
            if let Some(file_id) = existing_file_id {
                self.storage.clear_file_index(file_id)?;
            }

            let new_file = NewFile {
                path: &scanned.rel_path,
                abs_path: &scanned.abs_path.to_string_lossy(),
                extension: scanned.extension.as_deref(),
                language: scanned.language.as_deref(),
                hash: &scanned.hash,
                size: scanned.size,
                modified_at: scanned.modified_at,
                is_binary: false,
            };
            let file_id = self.storage.upsert_file(repo_id, &new_file)?;

            let parent_node_id = self.ensure_directory_chain(
                repo_id,
                repo_node_id,
                &scanned.rel_path,
                dir_node_cache,
            )?;
            let file_node_id = self.ensure_file_node(repo_id, file_id, &scanned.rel_path)?;
            self.storage.add_edge(
                repo_id,
                parent_node_id,
                file_node_id,
                EdgeKind::Contains,
                json!({}),
            )?;

            let mut symbols_by_name: HashMap<String, IndexedSymbol> = HashMap::new();
            let mut indexed_symbols = Vec::new();

            for symbol in &parsed.symbols {
                let symbol_calls: Vec<String> = parsed
                    .calls
                    .iter()
                    .filter(|call| symbol.start_line <= call.line && call.line <= symbol.end_line)
                    .flat_map(|call| {
                        let mut calls = vec![call.name.clone()];
                        if let Some(qualifier) = &call.qualifier {
                            calls.push(format!("{}.{}", qualifier, call.name));
                        }
                        calls
                    })
                    .collect();
                let node_id = self.storage.upsert_node(&NewNode {
                    repo_id,
                    file_id: Some(file_id),
                    kind: symbol.kind,
                    name: &symbol.name,
                    qualified_name: &symbol.qualified_name,
                    path: Some(&scanned.rel_path),
                    start_line: Some(symbol.start_line),
                    end_line: Some(symbol.end_line),
                    summary: Some(symbol.doc_summary.as_deref().unwrap_or(&symbol.signature)),
                    metadata: symbol_metadata(
                        &symbol.signature,
                        &symbol_calls,
                        symbol.doc_summary.as_deref(),
                    ),
                })?;
                self.storage.add_edge(
                    repo_id,
                    file_node_id,
                    node_id,
                    EdgeKind::Defines,
                    json!({}),
                )?;
                self.storage.insert_chunk(&NewChunk {
                    repo_id,
                    file_id,
                    node_id: Some(node_id),
                    kind: symbol.kind.as_str(),
                    text: &symbol.snippet,
                    search_text: None,
                    start_line: symbol.start_line,
                    end_line: symbol.end_line,
                    summary: Some(symbol.doc_summary.as_deref().unwrap_or(&symbol.signature)),
                    metadata: json!({ "symbol": symbol.name }),
                })?;
                let indexed_symbol = IndexedSymbol {
                    id: node_id,
                    kind: symbol.kind,
                    start_line: symbol.start_line,
                    end_line: symbol.end_line,
                };
                symbols_by_name
                    .entry(symbol.name.clone())
                    .or_insert_with(|| indexed_symbol.clone());
                indexed_symbols.push(indexed_symbol);
            }

            for call in &parsed.calls {
                let Some(caller) = indexed_symbols
                    .iter()
                    .filter(|symbol| symbol.start_line <= call.line && call.line <= symbol.end_line)
                    .min_by_key(|symbol| symbol.end_line - symbol.start_line)
                else {
                    continue;
                };
                let Some(target) = symbols_by_name.get(&call.name) else {
                    continue;
                };
                if caller.id == target.id {
                    continue;
                }
                let callee = if let Some(qualifier) = &call.qualifier {
                    format!("{}.{}", qualifier, call.name)
                } else {
                    call.name.clone()
                };
                self.storage.add_edge(
                    repo_id,
                    caller.id,
                    target.id,
                    EdgeKind::Calls,
                    json!({ "line": call.line, "callee": callee }),
                )?;
                if caller.kind == NodeKind::Test {
                    self.storage.add_edge(
                        repo_id,
                        caller.id,
                        target.id,
                        EdgeKind::Tests,
                        json!({ "line": call.line, "callee": callee }),
                    )?;
                }
            }

            for route in &parsed.routes {
                let route_name = format!("{} {}", route.method, route.path);
                let endpoint_id = self.storage.upsert_node(&NewNode {
                    repo_id,
                    file_id: Some(file_id),
                    kind: NodeKind::Endpoint,
                    name: &route_name,
                    qualified_name: &format!("{}::route::{}", scanned.rel_path, route_name),
                    path: Some(&scanned.rel_path),
                    start_line: Some(route.line),
                    end_line: Some(route.line),
                    summary: Some(&route_name),
                    metadata: json!({
                        "method": route.method,
                        "path": route.path,
                        "handler": route.handler,
                        "product_flow": true
                    }),
                })?;
                self.storage.add_edge(
                    repo_id,
                    file_node_id,
                    endpoint_id,
                    EdgeKind::Defines,
                    json!({ "route": route_name }),
                )?;
                if let Some(handler) = &route.handler {
                    if let Some(target) = symbols_by_name.get(handler) {
                        self.storage.add_edge(
                            repo_id,
                            endpoint_id,
                            target.id,
                            EdgeKind::References,
                            json!({ "handler": handler }),
                        )?;
                    }
                }
            }

            for import in &parsed.imports {
                let import_node_id = if let Some(import_node_id) =
                    import_node_cache.get(&import.source)
                {
                    *import_node_id
                } else {
                    let import_node_id = self.storage.upsert_node(&NewNode {
                        repo_id,
                        file_id: None,
                        kind: NodeKind::Symbol,
                        name: &import.source,
                        qualified_name: &format!("import:{}", import.source),
                        path: None,
                        start_line: None,
                        end_line: None,
                        summary: Some("Imported dependency"),
                        metadata: json!({ "external": true, "line": import.line, "bindings": import.bindings }),
                    })?;
                    import_node_cache.insert(import.source.clone(), import_node_id);
                    import_node_id
                };
                self.storage.add_edge(
                    repo_id,
                    file_node_id,
                    import_node_id,
                    EdgeKind::Imports,
                    json!({ "line": import.line, "bindings": import.bindings }),
                )?;
            }

            for chunk in chunk_source(&source, 80, 6000) {
                self.storage.insert_chunk(&NewChunk {
                    repo_id,
                    file_id,
                    node_id: Some(file_node_id),
                    kind: "file",
                    text: &chunk.text,
                    search_text: Some(&chunk.text),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    summary: Some(&format!(
                        "{}:{}-{}",
                        scanned.rel_path, chunk.start_line, chunk.end_line
                    )),
                    metadata: json!({}),
                })?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                if owns_write {
                    self.storage.commit_write()?;
                }
            }
            Err(error) => {
                if owns_write {
                    let _ = self.storage.rollback_write();
                }
                return Err(error);
            }
        }

        Ok(true)
    }

    fn resolve_local_imports(&self, repo_id: i64, aliases: &PathAliases) -> Result<()> {
        self.storage.begin_write()?;
        let result = (|| -> Result<()> {
            self.storage.clear_derived_resolution_edges(repo_id)?;
            let import_edges = self.storage.import_symbol_edges(repo_id)?;
            let mut file_node_cache = HashMap::new();
            let mut symbols_cache = HashMap::new();
            let mut endpoints_cache = HashMap::new();

            for import_edge in import_edges {
                for candidate in import_candidates(
                    &import_edge.source_path,
                    &import_edge.import_source,
                    aliases,
                ) {
                    let target_node_id = if let Some(cached) = file_node_cache.get(&candidate) {
                        *cached
                    } else {
                        let found = self.storage.file_node_id_by_path(repo_id, &candidate)?;
                        file_node_cache.insert(candidate.clone(), found);
                        found
                    };
                    if let Some(target_node_id) = target_node_id {
                        if import_edge.source_node_id != target_node_id {
                            self.storage.add_edge(
                                repo_id,
                                import_edge.source_node_id,
                                target_node_id,
                                EdgeKind::Imports,
                                json!({ "source": import_edge.import_source, "resolved": candidate }),
                            )?;
                        }
                        self.resolve_cross_file_calls_for_import(
                            repo_id,
                            &import_edge.source_path,
                            &candidate,
                            &import_edge.metadata,
                            &mut symbols_cache,
                            &mut endpoints_cache,
                        )?;
                        break;
                    }
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => self.storage.commit_write()?,
            Err(error) => {
                let _ = self.storage.rollback_write();
                return Err(error);
            }
        }
        Ok(())
    }

    fn resolve_cross_file_calls_for_import(
        &self,
        repo_id: i64,
        source_path: &str,
        target_path: &str,
        import_metadata: &serde_json::Value,
        symbols_cache: &mut HashMap<String, Vec<NodeRecord>>,
        endpoints_cache: &mut HashMap<String, Vec<NodeRecord>>,
    ) -> Result<()> {
        let bindings = import_bindings(import_metadata);
        if bindings.is_empty() {
            return Ok(());
        }
        let source_symbols = symbols_for_path(&self.storage, repo_id, source_path, symbols_cache)?;
        let target_symbols = symbols_for_path(&self.storage, repo_id, target_path, symbols_cache)?;
        let target_by_name: HashMap<String, i64> = target_symbols
            .into_iter()
            .map(|symbol| (symbol.name, symbol.id))
            .collect();
        let endpoints = endpoints_for_path(&self.storage, repo_id, source_path, endpoints_cache)?;

        for source_symbol in source_symbols {
            let calls = metadata_calls(&source_symbol.metadata);
            if calls.is_empty() {
                continue;
            }
            for (local, imported) in &bindings {
                let target_name = if imported == "*" {
                    calls.iter().find_map(|call| {
                        call.strip_prefix(&format!("{}.", local))
                            .map(str::to_string)
                    })
                } else if calls.iter().any(|call| call == local) {
                    Some(imported.clone())
                } else {
                    None
                };
                let Some(target_name) = target_name else {
                    continue;
                };
                let Some(target_id) = target_by_name.get(&target_name) else {
                    continue;
                };
                if source_symbol.id == *target_id {
                    continue;
                }
                self.storage.add_edge(
                    repo_id,
                    source_symbol.id,
                    *target_id,
                    EdgeKind::Calls,
                    json!({ "callee": local, "resolved_import": target_name, "target_path": target_path }),
                )?;
                if source_symbol.kind == NodeKind::Test.as_str() {
                    self.storage.add_edge(
                        repo_id,
                        source_symbol.id,
                        *target_id,
                        EdgeKind::Tests,
                        json!({ "callee": local, "resolved_import": target_name, "target_path": target_path }),
                    )?;
                }
            }
        }

        for endpoint in endpoints {
            let Some(handler) = endpoint
                .metadata
                .get("handler")
                .and_then(|handler| handler.as_str())
            else {
                continue;
            };
            for (local, imported) in &bindings {
                if handler != local {
                    continue;
                }
                let Some(target_id) = target_by_name.get(imported) else {
                    continue;
                };
                self.storage.add_edge(
                    repo_id,
                    endpoint.id,
                    *target_id,
                    EdgeKind::References,
                    json!({ "handler": handler, "resolved_import": imported, "target_path": target_path }),
                )?;
            }
        }
        Ok(())
    }

    fn ensure_repo_node(&self, repo_id: i64, repo_root: &Path) -> Result<i64> {
        let repo_name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repository");
        let repo_qualified = repo_root.to_string_lossy();
        self.storage.upsert_node(&NewNode {
            repo_id,
            file_id: None,
            kind: NodeKind::Repository,
            name: repo_name,
            qualified_name: &repo_qualified,
            path: Some(""),
            start_line: None,
            end_line: None,
            summary: Some("Repository root"),
            metadata: json!({}),
        })
    }

    fn ensure_directory_chain(
        &self,
        repo_id: i64,
        repo_node_id: i64,
        rel_file_path: &str,
        dir_node_cache: &mut HashMap<String, i64>,
    ) -> Result<i64> {
        let parent = Path::new(rel_file_path)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let mut current_parent_id = repo_node_id;
        let mut current_path = String::new();

        for component in parent.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let name = name.to_string_lossy();
            if current_path.is_empty() {
                current_path.push_str(&name);
            } else {
                current_path.push('/');
                current_path.push_str(&name);
            }
            if let Some(dir_node_id) = dir_node_cache.get(&current_path) {
                current_parent_id = *dir_node_id;
                continue;
            }
            let dir_node_id = self.storage.upsert_node(&NewNode {
                repo_id,
                file_id: None,
                kind: NodeKind::Directory,
                name: &name,
                qualified_name: &current_path,
                path: Some(&current_path),
                start_line: None,
                end_line: None,
                summary: Some("Directory"),
                metadata: json!({}),
            })?;
            self.storage.add_edge(
                repo_id,
                current_parent_id,
                dir_node_id,
                EdgeKind::Contains,
                json!({}),
            )?;
            dir_node_cache.insert(current_path.clone(), dir_node_id);
            current_parent_id = dir_node_id;
        }

        Ok(current_parent_id)
    }

    fn ensure_file_node(&self, repo_id: i64, file_id: i64, rel_path: &str) -> Result<i64> {
        let name = Path::new(rel_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(rel_path);
        self.storage.upsert_node(&NewNode {
            repo_id,
            file_id: Some(file_id),
            kind: NodeKind::File,
            name,
            qualified_name: rel_path,
            path: Some(rel_path),
            start_line: None,
            end_line: None,
            summary: Some("Source file"),
            metadata: json!({}),
        })
    }
}

#[derive(Debug)]
struct SourceChunk {
    start_line: i64,
    end_line: i64,
    text: String,
}

fn chunk_source(source: &str, lines_per_chunk: usize, max_chars: usize) -> Vec<SourceChunk> {
    let mut chunks = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    for (idx, group) in lines.chunks(lines_per_chunk).enumerate() {
        let mut text = group.join("\n");
        if text.len() > max_chars {
            text.truncate(max_chars);
        }
        let start_line = (idx * lines_per_chunk + 1) as i64;
        let end_line = start_line + group.len() as i64 - 1;
        chunks.push(SourceChunk {
            start_line,
            end_line,
            text,
        });
    }
    chunks
}

fn import_candidates(source_path: &str, import_source: &str, aliases: &PathAliases) -> Vec<String> {
    let mut bases = Vec::new();
    if import_source.starts_with('.') {
        let source_parent = Path::new(source_path)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        bases.push(normalize_rel_path(&source_parent.join(import_source)));
    } else {
        bases.extend(aliases.resolve(import_source));
    }

    let mut candidates = Vec::new();
    for base in bases {
        push_file_candidates(&mut candidates, &base);
    }
    candidates
}

fn push_file_candidates(candidates: &mut Vec<String>, base: &str) {
    if Path::new(&base).extension().is_some() {
        candidates.push(base.to_string());
        return;
    }

    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "rs"] {
        candidates.push(format!("{}.{}", base, ext));
    }
    for ext in ["ts", "tsx", "js", "jsx", "rs"] {
        candidates.push(format!("{}/index.{}", base, ext));
    }
}

fn normalize_rel_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            _ => {}
        }
    }
    parts.join("/")
}

impl PathAliases {
    fn load(repo_root: &Path) -> Self {
        let tsconfig = repo_root.join("tsconfig.json");
        let Ok(text) = fs::read_to_string(tsconfig) else {
            return Self::default();
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let compiler_options = json
            .get("compilerOptions")
            .and_then(|value| value.as_object());
        let base_url = compiler_options
            .and_then(|options| options.get("baseUrl"))
            .and_then(|value| value.as_str())
            .map(normalize_alias_base);
        let paths = compiler_options
            .and_then(|options| options.get("paths"))
            .and_then(|value| value.as_object())
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|(pattern, targets)| {
                        let targets = targets
                            .as_array()?
                            .iter()
                            .filter_map(|target| target.as_str().map(str::to_string))
                            .collect::<Vec<_>>();
                        if targets.is_empty() {
                            None
                        } else {
                            Some((pattern.clone(), targets))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { base_url, paths }
    }

    fn resolve(&self, import_source: &str) -> Vec<String> {
        let mut bases = Vec::new();
        for (pattern, targets) in &self.paths {
            if let Some(capture) = match_alias_pattern(pattern, import_source) {
                for target in targets {
                    let resolved = apply_alias_target(target, capture.as_deref());
                    bases.push(self.with_base_url(&resolved));
                }
            }
        }
        if bases.is_empty() {
            if let Some(base_url) = &self.base_url {
                bases.push(normalize_rel_path(&Path::new(base_url).join(import_source)));
            }
        }
        bases
    }

    fn with_base_url(&self, target: &str) -> String {
        let target = normalize_alias_base(target);
        if target.starts_with('.') || self.base_url.is_none() {
            normalize_rel_path(Path::new(&target))
        } else {
            normalize_rel_path(&Path::new(self.base_url.as_deref().unwrap_or("")).join(target))
        }
    }
}

fn normalize_alias_base(path: &str) -> String {
    normalize_rel_path(Path::new(path))
}

fn match_alias_pattern(pattern: &str, import_source: &str) -> Option<Option<String>> {
    let Some(star_idx) = pattern.find('*') else {
        return (pattern == import_source).then_some(None);
    };
    let prefix = &pattern[..star_idx];
    let suffix = &pattern[star_idx + 1..];
    if import_source.starts_with(prefix) && import_source.ends_with(suffix) {
        let capture = &import_source[prefix.len()..import_source.len() - suffix.len()];
        Some(Some(capture.to_string()))
    } else {
        None
    }
}

fn apply_alias_target(target: &str, capture: Option<&str>) -> String {
    if let Some(capture) = capture {
        target.replace('*', capture)
    } else {
        target.to_string()
    }
}

fn import_bindings(metadata: &serde_json::Value) -> Vec<(String, String)> {
    metadata
        .get("bindings")
        .and_then(|value| value.as_array())
        .map(|bindings| {
            bindings
                .iter()
                .filter_map(|binding| {
                    let local = binding.get("local")?.as_str()?.to_string();
                    let imported = binding.get("imported")?.as_str()?.to_string();
                    Some((local, imported))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn metadata_calls(metadata: &serde_json::Value) -> Vec<String> {
    metadata
        .get("calls")
        .and_then(|value| value.as_array())
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| call.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn git_delta(repo_root: &Path) -> Result<Option<GitDelta>> {
    if let Some(delta) = internal_git_delta(repo_root)? {
        return Ok(Some(delta));
    }

    if has_real_git(repo_root) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("status")
            .arg("--porcelain=v1")
            .arg("-z")
            .arg("--untracked-files=all")
            .output();

        let output = match output {
            Ok(output) if output.status.success() => output,
            _ => return Ok(None),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        return Ok(Some(parse_git_status_z(&stdout)));
    }
    Ok(None)
}

fn parse_git_status_z(status: &str) -> GitDelta {
    let mut delta = GitDelta {
        internal: false,
        ..GitDelta::default()
    };
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());

    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let status_code = &entry[..2];
        let path = entry[3..].replace('\\', "/");
        let x = status_code.as_bytes()[0] as char;
        let y = status_code.as_bytes()[1] as char;

        if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            if let Some(old_path) = entries.next() {
                delta.deleted.insert(old_path.replace('\\', "/"));
            }
            delta.changed.insert(path);
            continue;
        }

        if x == 'D' || y == 'D' {
            delta.deleted.insert(path);
        } else {
            delta.changed.insert(path);
        }
    }

    delta
}

fn has_real_git(repo_root: &Path) -> bool {
    repo_root.join(".git").exists()
}

fn internal_git_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".ckg").join("git")
}

fn ensure_internal_git(repo_root: &Path) -> Result<bool> {
    let git_dir = internal_git_dir(repo_root);
    if git_dir.join("config").exists() {
        return Ok(true);
    }
    fs::create_dir_all(git_dir.parent().unwrap_or(repo_root))?;
    let output = internal_git_command(repo_root)
        .arg("init")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    Ok(matches!(output, Ok(output) if output.status.success()))
}

fn internal_git_delta(repo_root: &Path) -> Result<Option<GitDelta>> {
    if !internal_git_dir(repo_root).join("config").exists() {
        return Ok(None);
    }

    let diff = internal_git_command(repo_root)
        .arg("diff-files")
        .arg("-z")
        .arg("--name-status")
        .output();
    let diff = match diff {
        Ok(output) if output.status.success() => output,
        _ => return Ok(None),
    };

    let others = internal_git_command(repo_root)
        .arg("ls-files")
        .arg("--others")
        .arg("--exclude-standard")
        .arg("-z")
        .output();
    let others = match others {
        Ok(output) if output.status.success() => output,
        _ => return Ok(None),
    };

    let mut delta = parse_git_name_status_z(&String::from_utf8_lossy(&diff.stdout));
    delta.internal = true;
    for path in String::from_utf8_lossy(&others.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
    {
        let path = path.replace('\\', "/");
        if !should_skip_rel_path(&path) {
            delta.changed.insert(path);
        }
    }
    Ok(Some(delta))
}

fn parse_git_name_status_z(status: &str) -> GitDelta {
    let mut delta = GitDelta::default();
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(code) = entries.next() {
        let Some(path) = entries.next() else {
            break;
        };
        let path = path.replace('\\', "/");
        if should_skip_rel_path(&path) {
            continue;
        }
        if code.starts_with('D') {
            delta.deleted.insert(path);
        } else {
            delta.changed.insert(path);
        }
    }
    delta
}

fn sorted_strings(values: HashSet<String>) -> Vec<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}

fn symbols_for_path(
    storage: &Storage,
    repo_id: i64,
    path: &str,
    cache: &mut HashMap<String, Vec<NodeRecord>>,
) -> Result<Vec<NodeRecord>> {
    if let Some(symbols) = cache.get(path) {
        return Ok(symbols.clone());
    }
    let symbols = storage.symbols_by_file_path(repo_id, path)?;
    cache.insert(path.to_string(), symbols.clone());
    Ok(symbols)
}

fn endpoints_for_path(
    storage: &Storage,
    repo_id: i64,
    path: &str,
    cache: &mut HashMap<String, Vec<NodeRecord>>,
) -> Result<Vec<NodeRecord>> {
    if let Some(endpoints) = cache.get(path) {
        return Ok(endpoints.clone());
    }
    let endpoints = storage.endpoints_by_file_path(repo_id, path)?;
    cache.insert(path.to_string(), endpoints.clone());
    Ok(endpoints)
}

fn sync_internal_git_full(repo_root: &Path, seen_paths: &HashSet<String>) -> Result<()> {
    if !ensure_internal_git(repo_root)? {
        return Ok(());
    }
    let _ = internal_git_command(repo_root)
        .arg("read-tree")
        .arg("--empty")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    update_internal_git_add(repo_root, seen_paths)
}

fn sync_internal_git_delta(
    repo_root: &Path,
    add_paths: &HashSet<String>,
    remove_paths: &HashSet<String>,
) -> Result<()> {
    if !ensure_internal_git(repo_root)? {
        return Ok(());
    }
    update_internal_git_remove(repo_root, remove_paths)?;
    update_internal_git_add(repo_root, add_paths)
}

fn update_internal_git_add(repo_root: &Path, paths: &HashSet<String>) -> Result<()> {
    update_internal_git_index(repo_root, &["update-index", "--add", "--info-only"], paths)
}

fn update_internal_git_remove(repo_root: &Path, paths: &HashSet<String>) -> Result<()> {
    update_internal_git_index(repo_root, &["update-index", "--force-remove"], paths)
}

fn update_internal_git_index(
    repo_root: &Path,
    args: &[&str],
    paths: &HashSet<String>,
) -> Result<()> {
    let paths: Vec<&String> = paths
        .iter()
        .filter(|path| !path.is_empty() && !should_skip_rel_path(path))
        .collect();
    if paths.is_empty() {
        return Ok(());
    }

    for chunk in paths.chunks(512) {
        let mut child = internal_git_command(repo_root)
            .args(args)
            .arg("-z")
            .arg("--stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        {
            let stdin = child.stdin.as_mut().expect("stdin is piped");
            for path in chunk {
                stdin.write_all(path.as_bytes())?;
                stdin.write_all(&[0])?;
            }
        }
        let _ = child.wait()?;
    }
    Ok(())
}

fn internal_git_command(repo_root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(internal_git_dir(repo_root))
        .arg("--work-tree")
        .arg(repo_root);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::RetrievalEngine;
    use crate::storage::Storage;

    #[test]
    fn indexes_incrementally_and_searches_symbols() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("src"))?;
        fs::write(
            dir.path().join("src/avatar.ts"),
            r#"
            export class AvatarService {
              uploadAvatar(file: File) { return file.name; }
            }
            "#,
        )?;
        let storage = Storage::open_for_repo(dir.path())?;
        let report = Indexer::new(storage).index_repo(dir.path())?;
        assert_eq!(report.indexed, 1);

        let storage = Storage::open_for_repo(dir.path())?;
        let hits = storage.search("AvatarService", 10)?;
        assert!(hits
            .iter()
            .any(|hit| hit.name.as_deref() == Some("AvatarService")));

        let report = Indexer::new(storage).index_repo(dir.path())?;
        assert_eq!(report.scanned, 0);
        assert_eq!(report.skipped_unchanged, 0);
        Ok(())
    }

    #[test]
    fn parses_git_status_delta() {
        let status =
            " M src/lib.rs\0?? src/new.ts\0D  src/old.rs\0R  src/new_name.ts\0src/old_name.ts\0";
        let delta = parse_git_status_z(status);
        assert!(delta.changed.contains("src/lib.rs"));
        assert!(delta.changed.contains("src/new.ts"));
        assert!(delta.changed.contains("src/new_name.ts"));
        assert!(delta.deleted.contains("src/old.rs"));
        assert!(delta.deleted.contains("src/old_name.ts"));
    }

    #[test]
    fn real_git_revert_after_dirty_index_updates_database() -> Result<()> {
        let dir = tempfile::tempdir()?;
        if !git(dir.path(), &["init"])? {
            return Ok(());
        }
        fs::create_dir_all(dir.path().join("src"))?;
        let clean_source = "export function alpha() { return 1; }\n";
        fs::write(dir.path().join("src/a.ts"), clean_source)?;
        if !git(dir.path(), &["add", "src/a.ts"])? {
            return Ok(());
        }
        if !git(
            dir.path(),
            &[
                "-c",
                "user.email=ckg@example.com",
                "-c",
                "user.name=ckg",
                "commit",
                "-m",
                "init",
            ],
        )? {
            return Ok(());
        }

        let storage = Storage::open_for_repo(dir.path())?;
        let report = Indexer::new(storage).index_repo(dir.path())?;
        assert_eq!(report.indexed, 1);

        fs::write(
            dir.path().join("src/a.ts"),
            "export function alpha() { return 2; }\nexport function beta() { return alpha(); }\n",
        )?;
        let storage = Storage::open_for_repo(dir.path())?;
        let report = Indexer::new(storage).index_repo(dir.path())?;
        assert_eq!(report.indexed, 1);

        let storage = Storage::open_for_repo(dir.path())?;
        assert!(storage
            .search("beta", 10)?
            .iter()
            .any(|hit| hit.name.as_deref() == Some("beta")));

        fs::write(dir.path().join("src/a.ts"), clean_source)?;
        let storage = Storage::open_for_repo(dir.path())?;
        let report = Indexer::new(storage).index_repo(dir.path())?;
        assert_eq!(report.scanned, 1);
        assert_eq!(report.indexed, 1);

        let storage = Storage::open_for_repo(dir.path())?;
        assert!(storage.search("beta", 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn indexes_local_imports_calls_and_tests() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("src"))?;
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
        )?;
        fs::write(
            dir.path().join("src/upload.ts"),
            "export function upload() { return 'ok'; }\n",
        )?;
        fs::write(
            dir.path().join("src/avatar.ts"),
            "import { upload } from './upload';\nexport function saveAvatar() { return upload(); }\n",
        )?;
        fs::write(
            dir.path().join("src/avatar_ns.ts"),
            "import * as uploads from './upload';\nexport function saveViaNamespace() { return uploads.upload(); }\n",
        )?;
        fs::write(
            dir.path().join("src/avatar_alias.ts"),
            "import { upload } from '@/upload';\nexport function saveViaAlias() { return upload(); }\n",
        )?;
        fs::write(
            dir.path().join("src/local.test.ts"),
            "export function helper() { return 1; }\nexport function shouldCallHelper() { return helper(); }\n",
        )?;
        fs::write(
            dir.path().join("src/avatar.test.ts"),
            "import { saveAvatar } from './avatar';\nexport function shouldSaveAvatar() { return saveAvatar(); }\n",
        )?;
        fs::write(
            dir.path().join("src/handlers.ts"),
            "export function handleAvatar() { return 'ok'; }\nrouter.post('/avatar', handleAvatar);\n",
        )?;
        fs::write(
            dir.path().join("src/routes.ts"),
            "import { handleAvatar } from './handlers';\nrouter.post('/avatar', handleAvatar);\n",
        )?;

        let storage = Storage::open_for_repo(dir.path())?;
        let report = Indexer::new(storage).index_repo(dir.path())?;
        assert_eq!(report.indexed, 9);

        let storage = Storage::open_for_repo(dir.path())?;
        assert!(storage.edge_count_by_kind(report.repo_id, EdgeKind::Imports)? >= 6);
        assert!(storage.edge_count_by_kind(report.repo_id, EdgeKind::Calls)? >= 4);
        assert!(storage.edge_count_by_kind(report.repo_id, EdgeKind::Tests)? >= 2);
        assert!(storage.edge_count_by_kind(report.repo_id, EdgeKind::References)? >= 2);
        Ok(())
    }

    #[test]
    fn retrieval_grep_glob_and_read_range_work_from_index() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("src"))?;
        fs::write(
            dir.path().join("src/avatar.ts"),
            "export function uploadAvatar() {\n  return 'avatar';\n}\n",
        )?;

        let storage = Storage::open_for_repo(dir.path())?;
        let report = Indexer::new(storage).index_repo(dir.path())?;

        let storage = Storage::open_for_repo(dir.path())?;
        let engine = RetrievalEngine::new(storage);
        let glob = engine.glob(report.repo_id, "src/**/*.ts", 10)?;
        assert_eq!(glob["files"].as_array().unwrap().len(), 1);

        let grep = engine.grep(
            report.repo_id,
            "avatar",
            Some("src/**/*.ts"),
            false,
            true,
            10,
        )?;
        assert_eq!(grep["matches"].as_array().unwrap().len(), 2);

        let regex_grep = engine.grep(
            report.repo_id,
            "upload[A-Z][a-z]+",
            Some("src/**/*.ts"),
            true,
            true,
            10,
        )?;
        assert_eq!(regex_grep["matches"].as_array().unwrap().len(), 1);

        let read = engine
            .file_content_range("src/avatar.ts", Some(2), Some(1), true)?
            .unwrap();
        assert!(read["content"]
            .as_str()
            .unwrap()
            .contains("return 'avatar'"));
        assert_eq!(read["start_line"].as_u64(), Some(2));

        fs::write(
            dir.path().join("src/new_file.ts"),
            "export const fresh = 1;\n",
        )?;
        let fallback = engine
            .file_content_range_with_fallback(
                dir.path(),
                "src/new_file.ts",
                Some(1),
                Some(1),
                false,
            )?
            .unwrap();
        assert_eq!(fallback["indexed"].as_bool(), Some(false));
        assert!(fallback["content"].as_str().unwrap().contains("fresh"));

        let definition = engine.definition_at(report.repo_id, "src/avatar.ts", 1, Some(18), 10)?;
        assert_eq!(definition["symbol"]["name"].as_str(), Some("uploadAvatar"));

        let hierarchy =
            engine.call_hierarchy_at(report.repo_id, "src/avatar.ts", 1, Some(18), "both", 10)?;
        assert_eq!(hierarchy["symbol"]["name"].as_str(), Some("uploadAvatar"));
        Ok(())
    }

    #[test]
    fn task_context_respects_small_token_budget() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("src"))?;
        for idx in 0..12 {
            fs::write(
                dir.path().join(format!("src/mod{idx}.ts")),
                format!(
                    "import {{ helper }} from './shared';\n\
                     export function feature{idx}() {{\n\
                     helper('MCP integration {idx}');\n\
                     return 'MCP integration {idx}';\n\
                     }}\n"
                ),
            )?;
        }
        fs::write(
            dir.path().join("src/shared.ts"),
            "export function helper(value: string) {\n  return value.toUpperCase();\n}\n",
        )?;
        fs::write(
            dir.path().join("src/shared.test.ts"),
            "import { helper } from './shared';\ntest('MCP integration helper', () => helper('x'));\n",
        )?;

        let storage = Storage::open_for_repo(dir.path())?;
        Indexer::new(storage).index_repo(dir.path())?;

        let storage = Storage::open_for_repo(dir.path())?;
        let engine = RetrievalEngine::new(storage);
        let context =
            engine.task_context_for_repo(Some(dir.path()), "MCP integration", 800, 2, false)?;
        let serialized = serde_json::to_string_pretty(&context)?;
        assert!(
            serialized.len() < 10_000,
            "task_context response is too large: {} bytes",
            serialized.len()
        );
        assert!(context.context_pack.len() <= 1_700);
        assert!(context.subgraph.edges.len() <= 4);
        Ok(())
    }

    #[test]
    fn status_reports_new_modified_and_deleted_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("src"))?;
        fs::write(dir.path().join("src/a.ts"), "export const a = 1;\n")?;
        fs::write(dir.path().join("src/delete.ts"), "export const gone = 1;\n")?;

        let storage = Storage::open_for_repo(dir.path())?;
        Indexer::new(storage).index_repo_with_options(dir.path(), IndexOptions { full: true })?;

        let storage = Storage::open_for_repo(dir.path())?;
        let clean = Indexer::new(storage).status_repo(dir.path())?;
        assert!(!clean.needs_index);

        fs::write(dir.path().join("src/a.ts"), "export const a = 2;\n")?;
        fs::write(dir.path().join("src/new.ts"), "export const b = 1;\n")?;
        fs::remove_file(dir.path().join("src/delete.ts"))?;

        let storage = Storage::open_for_repo(dir.path())?;
        let status = Indexer::new(storage).status_repo(dir.path())?;
        assert!(status.needs_index);
        assert!(status.modified_files.contains(&"src/a.ts".to_string()));
        assert!(status.new_files.contains(&"src/new.ts".to_string()));
        assert!(status.deleted_files.contains(&"src/delete.ts".to_string()));
        Ok(())
    }

    #[test]
    fn resolves_tsconfig_path_aliases() {
        let aliases = PathAliases {
            base_url: Some(".".to_string()),
            paths: vec![("@/*".to_string(), vec!["src/*".to_string()])],
        };
        assert_eq!(aliases.resolve("@/upload"), vec!["src/upload"]);
    }

    fn git(repo: &Path, args: &[&str]) -> Result<bool> {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(matches!(status, Ok(status) if status.success()))
    }
}
