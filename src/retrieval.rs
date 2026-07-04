// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use globset::Glob;
use regex::RegexBuilder;

use crate::{
    model::{EdgeRecord, NodeRecord, SearchHit, Subgraph, TaskContextResponse},
    storage::{EdgeDirection, Storage},
};

pub struct RetrievalEngine {
    storage: Storage,
}

impl RetrievalEngine {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.storage
            .search(query, limit)
            .and_then(|hits| self.hydrate_hits(hits))
    }

    pub fn neighborhood(&self, node_id: i64, hops: usize) -> Result<Subgraph> {
        self.storage.neighborhood(node_id, hops)
    }

    pub fn task_context(
        &self,
        task: &str,
        max_tokens: usize,
        hops: usize,
    ) -> Result<TaskContextResponse> {
        self.task_context_for_repo(None, task, max_tokens, hops, true)
    }

    pub fn task_context_for_repo(
        &self,
        repo_path: Option<&Path>,
        task: &str,
        max_tokens: usize,
        hops: usize,
        include_git_dirty: bool,
    ) -> Result<TaskContextResponse> {
        let hits = self.search(task, 30)?;
        let mut file_ids = Vec::new();
        let mut seen_file_ids = HashSet::new();
        let mut relevant_files = Vec::new();
        let mut relevant_symbols = Vec::new();

        if include_git_dirty {
            if let Some(repo_path) = repo_path {
                for path in git_dirty_paths(repo_path)? {
                    if let Some(file) = self.storage.get_file_by_path_any_repo(&path)? {
                        if seen_file_ids.insert(file.id) {
                            file_ids.push(file.id);
                            relevant_files.push(SearchHit {
                                kind: "dirty_file".to_string(),
                                ref_id: file.id,
                                file_id: Some(file.id),
                                node_id: None,
                                path: Some(file.path.clone()),
                                name: Some(file.path),
                                snippet: Some("git dirty/uncommitted file".to_string()),
                                score: 120.0,
                            });
                        }
                    }
                }
            }
        }

        for hit in &hits {
            if let Some(file_id) = hit.file_id {
                if seen_file_ids.insert(file_id) {
                    file_ids.push(file_id);
                    relevant_files.push(hit.clone());
                }
            }
            if hit.node_id.is_some() && hit.name.as_deref() != Some("Source file") {
                relevant_symbols.push(hit.clone());
            }
        }
        relevant_files.truncate(8);
        relevant_symbols.truncate(12);
        file_ids.truncate(8);

        let subgraph = self.subgraph_for_hits(&hits, hops)?;
        let suggested_tests = self.suggest_tests(task)?;
        let context_pack_tokens = max_tokens.saturating_div(2).max(400);
        let context_pack = self.context_pack(&file_ids, context_pack_tokens)?;

        let response = TaskContextResponse {
            query: task.to_string(),
            relevant_files,
            relevant_symbols,
            subgraph,
            suggested_tests,
            context_pack,
        };
        Ok(budget_task_context_response(response, max_tokens))
    }

    pub fn file_content(&self, path: &str) -> Result<Option<serde_json::Value>> {
        self.file_content_range(path, None, None, false)
    }

    pub fn file_content_range(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
        line_numbers: bool,
    ) -> Result<Option<serde_json::Value>> {
        let Some(file) = self.storage.get_file_by_path_any_repo(path)? else {
            return Ok(None);
        };
        let content = fs::read_to_string(&file.abs_path).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        let start = offset.unwrap_or(1).max(1);
        let line_limit = limit.unwrap_or(lines.len()).max(1);
        let end = start
            .saturating_add(line_limit)
            .saturating_sub(1)
            .min(lines.len());
        let selected = if start <= lines.len() {
            &lines[(start - 1)..end]
        } else {
            &[]
        };
        let ranged_content = if line_numbers {
            selected
                .iter()
                .enumerate()
                .map(|(idx, line)| format!("{:>6}\t{}", start + idx, line))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            selected.join("\n")
        };
        Ok(Some(serde_json::json!({
            "path": file.path,
            "language": file.language,
            "size": file.size,
            "hash": file.hash,
            "start_line": start,
            "end_line": end,
            "total_lines": lines.len(),
            "content": ranged_content,
        })))
    }

    pub fn file_content_range_with_fallback(
        &self,
        repo_path: &Path,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
        line_numbers: bool,
    ) -> Result<Option<serde_json::Value>> {
        if let Some(indexed) = self.file_content_range(path, offset, limit, line_numbers)? {
            return Ok(Some(indexed));
        }
        let Some(abs_path) = safe_repo_path(repo_path, path)? else {
            return Ok(None);
        };
        if !abs_path.is_file() {
            return Ok(None);
        }
        let content = fs::read_to_string(&abs_path)?;
        let lines: Vec<&str> = content.lines().collect();
        let start = offset.unwrap_or(1).max(1);
        let line_limit = limit.unwrap_or(lines.len()).max(1);
        let end = start
            .saturating_add(line_limit)
            .saturating_sub(1)
            .min(lines.len());
        let selected = if start <= lines.len() {
            &lines[(start - 1)..end]
        } else {
            &[]
        };
        let ranged_content = if line_numbers {
            selected
                .iter()
                .enumerate()
                .map(|(idx, line)| format!("{:>6}\t{}", start + idx, line))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            selected.join("\n")
        };
        Ok(Some(serde_json::json!({
            "path": path.replace('\\', "/"),
            "language": language_from_path(path),
            "indexed": false,
            "start_line": start,
            "end_line": end,
            "total_lines": lines.len(),
            "content": ranged_content,
        })))
    }

    pub fn glob(&self, repo_id: i64, pattern: &str, limit: usize) -> Result<serde_json::Value> {
        let matcher = Glob::new(pattern)?.compile_matcher();
        let files = self.storage.list_files(repo_id)?;
        let matches = files
            .into_iter()
            .filter(|file| matcher.is_match(&file.path))
            .take(limit)
            .map(|file| {
                serde_json::json!({
                    "id": file.id,
                    "path": file.path,
                    "language": file.language,
                    "size": file.size,
                    "hash": file.hash
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({ "pattern": pattern, "files": matches }))
    }

    pub fn grep(
        &self,
        repo_id: i64,
        query: &str,
        path_glob: Option<&str>,
        case_sensitive: bool,
        regex: bool,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let path_matcher = path_glob
            .map(|pattern| Glob::new(pattern).map(|glob| glob.compile_matcher()))
            .transpose()?;
        let regex_matcher = if regex {
            Some(
                RegexBuilder::new(query)
                    .case_insensitive(!case_sensitive)
                    .build()?,
            )
        } else {
            None
        };
        let needle = (!regex).then(|| {
            if case_sensitive {
                query.to_string()
            } else {
                query.to_ascii_lowercase()
            }
        });
        let mut matches = Vec::new();
        for file in self.storage.list_files(repo_id)? {
            if matches.len() >= limit {
                break;
            }
            if let Some(matcher) = &path_matcher {
                if !matcher.is_match(&file.path) {
                    continue;
                }
            }
            let Ok(content) = fs::read_to_string(&file.abs_path) else {
                continue;
            };
            for (idx, line) in content.lines().enumerate() {
                let matched = if let Some(regex) = &regex_matcher {
                    regex.is_match(line)
                } else {
                    let haystack = if case_sensitive {
                        line.to_string()
                    } else {
                        line.to_ascii_lowercase()
                    };
                    haystack.contains(needle.as_deref().unwrap_or_default())
                };
                if matched {
                    matches.push(serde_json::json!({
                        "path": file.path,
                        "line": idx + 1,
                        "text": line
                    }));
                    if matches.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(serde_json::json!({
            "query": query,
            "path_glob": path_glob,
            "case_sensitive": case_sensitive,
            "regex": regex,
            "matches": matches
        }))
    }

    pub fn workspace_symbols(
        &self,
        repo_id: i64,
        query: &str,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let nodes = self.storage.nodes_matching(
            repo_id,
            query,
            &["Function", "Method", "Class", "Type", "Test", "Endpoint"],
            limit,
        )?;
        Ok(serde_json::json!({ "symbols": nodes }))
    }

    pub fn document_symbols(&self, repo_id: i64, path: &str) -> Result<serde_json::Value> {
        let mut symbols = self.storage.symbols_by_file_path(repo_id, path)?;
        symbols.extend(self.storage.endpoints_by_file_path(repo_id, path)?);
        Ok(serde_json::json!({ "path": path, "symbols": symbols }))
    }

    pub fn definition(&self, repo_id: i64, query: &str, limit: usize) -> Result<serde_json::Value> {
        let definitions = self.storage.nodes_matching(
            repo_id,
            query,
            &["Function", "Method", "Class", "Type", "Test", "Endpoint"],
            limit,
        )?;
        Ok(serde_json::json!({ "query": query, "definitions": definitions }))
    }

    pub fn definition_at(
        &self,
        repo_id: i64,
        path: &str,
        line: i64,
        character: Option<i64>,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let Some(node) = self.storage.node_at_position(repo_id, path, line)? else {
            return Ok(serde_json::json!({
                "path": path,
                "line": line,
                "character": character,
                "definitions": []
            }));
        };
        let definitions = vec![node.clone()]
            .into_iter()
            .take(limit)
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "path": path,
            "line": line,
            "character": character,
            "symbol": node,
            "definitions": definitions
        }))
    }

    pub fn references(
        &self,
        repo_id: i64,
        node_id: i64,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let edges = self.storage.edges_for_node(
            repo_id,
            node_id,
            &["CALLS", "REFERENCES", "TESTS", "IMPORTS"],
            EdgeDirection::Incoming,
            limit,
        )?;
        let graph = self.subgraph_from_edges(edges)?;
        Ok(serde_json::json!({ "node_id": node_id, "subgraph": graph }))
    }

    pub fn references_at(
        &self,
        repo_id: i64,
        path: &str,
        line: i64,
        character: Option<i64>,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let Some(node) = self.storage.node_at_position(repo_id, path, line)? else {
            return Ok(serde_json::json!({
                "path": path,
                "line": line,
                "character": character,
                "subgraph": { "nodes": [], "edges": [] }
            }));
        };
        let references = self.references(repo_id, node.id, limit)?;
        Ok(serde_json::json!({
            "path": path,
            "line": line,
            "character": character,
            "symbol": node,
            "references": references
        }))
    }

    pub fn call_hierarchy(
        &self,
        repo_id: i64,
        node_id: i64,
        direction: &str,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let direction = match direction {
            "incoming" => EdgeDirection::Incoming,
            "outgoing" => EdgeDirection::Outgoing,
            _ => EdgeDirection::Both,
        };
        let edges = self
            .storage
            .edges_for_node(repo_id, node_id, &["CALLS"], direction, limit)?;
        let graph = self.subgraph_from_edges(edges)?;
        Ok(
            serde_json::json!({ "node_id": node_id, "direction": format!("{:?}", direction), "subgraph": graph }),
        )
    }

    pub fn call_hierarchy_at(
        &self,
        repo_id: i64,
        path: &str,
        line: i64,
        character: Option<i64>,
        direction: &str,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let Some(node) = self.storage.node_at_position(repo_id, path, line)? else {
            return Ok(serde_json::json!({
                "path": path,
                "line": line,
                "character": character,
                "direction": direction,
                "subgraph": { "nodes": [], "edges": [] }
            }));
        };
        let hierarchy = self.call_hierarchy(repo_id, node.id, direction, limit)?;
        Ok(serde_json::json!({
            "path": path,
            "line": line,
            "character": character,
            "symbol": node,
            "call_hierarchy": hierarchy
        }))
    }

    pub fn imports(&self, repo_id: i64, node_id: i64, limit: usize) -> Result<serde_json::Value> {
        let edges = self.storage.edges_for_node(
            repo_id,
            node_id,
            &["IMPORTS"],
            EdgeDirection::Outgoing,
            limit,
        )?;
        let graph = self.subgraph_from_edges(edges)?;
        Ok(serde_json::json!({ "node_id": node_id, "subgraph": graph }))
    }

    pub fn dependents(
        &self,
        repo_id: i64,
        node_id: i64,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let edges = self.storage.edges_for_node(
            repo_id,
            node_id,
            &["IMPORTS"],
            EdgeDirection::Incoming,
            limit,
        )?;
        let graph = self.subgraph_from_edges(edges)?;
        Ok(serde_json::json!({ "node_id": node_id, "subgraph": graph }))
    }

    pub fn suggested_tests_detailed(
        &self,
        repo_path: &Path,
        task: &str,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let command = detect_test_command(repo_path);
        let tests = self
            .suggest_tests(task)?
            .into_iter()
            .take(limit)
            .map(|hit| {
                serde_json::json!({
                    "path": hit.path,
                    "name": hit.name,
                    "node_id": hit.node_id,
                    "score": hit.score,
                    "reason": "matched test/spec path or symbol near task query",
                    "command": command
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({ "task": task, "suggested_tests": tests }))
    }

    fn hydrate_hits(&self, mut hits: Vec<SearchHit>) -> Result<Vec<SearchHit>> {
        for hit in &mut hits {
            if hit.path.as_deref().unwrap_or_default().is_empty() {
                if let Some(file_id) = hit.file_id {
                    if let Some(file) = self.storage.get_file(file_id)? {
                        hit.path = Some(file.path);
                    }
                }
            }
        }
        Ok(hits)
    }

    fn subgraph_for_hits(&self, hits: &[SearchHit], hops: usize) -> Result<Subgraph> {
        let mut node_ids = Vec::new();
        let mut seen = HashSet::new();
        for hit in hits.iter().take(12) {
            let node_id = match (hit.node_id, hit.file_id) {
                (Some(node_id), _) => Some(node_id),
                (None, Some(file_id)) => self.storage.file_node_id(file_id)?,
                _ => None,
            };
            if let Some(node_id) = node_id {
                if seen.insert(node_id) {
                    node_ids.push(node_id);
                }
            }
        }

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seen_nodes = HashSet::new();
        let mut seen_edges = HashSet::new();
        for node_id in node_ids {
            let graph = self.storage.neighborhood(node_id, hops)?;
            for node in graph.nodes {
                if seen_nodes.insert(node.id) {
                    nodes.push(node);
                }
            }
            for edge in graph.edges {
                if seen_edges.insert(edge.id) {
                    edges.push(edge);
                }
            }
        }
        Ok(Subgraph { nodes, edges })
    }

    fn subgraph_from_edges(&self, edges: Vec<crate::model::EdgeRecord>) -> Result<Subgraph> {
        let mut nodes = Vec::new();
        let mut seen_nodes = HashSet::new();
        for edge in &edges {
            for node_id in [edge.source_id, edge.target_id] {
                if seen_nodes.insert(node_id) {
                    if let Some(node) = self.storage.get_node(node_id)? {
                        nodes.push(node);
                    }
                }
            }
        }
        Ok(Subgraph { nodes, edges })
    }

    fn suggest_tests(&self, task: &str) -> Result<Vec<SearchHit>> {
        let mut hits = self.search(&format!("test {}", task), 20)?;
        hits.retain(|hit| {
            let haystack = format!(
                "{} {}",
                hit.path.as_deref().unwrap_or_default(),
                hit.name.as_deref().unwrap_or_default()
            )
            .to_ascii_lowercase();
            haystack.contains("test") || haystack.contains("spec")
        });
        hits.truncate(8);
        Ok(hits)
    }

    fn context_pack(&self, file_ids: &[i64], max_tokens: usize) -> Result<String> {
        let char_budget = max_tokens.saturating_mul(4).max(2000);
        let mut out = String::new();
        for file_id in file_ids {
            let Some(file) = self.storage.get_file(*file_id)? else {
                continue;
            };
            let header = format!(
                "\n\n## File: {}\nLanguage: {}\nHash: {}\n",
                file.path,
                file.language.as_deref().unwrap_or("unknown"),
                file.hash
            );
            if !push_budgeted(&mut out, &header, char_budget) {
                break;
            }

            let symbols = self.storage.top_symbols_for_file(*file_id, 20)?;
            if !symbols.is_empty() {
                let mut symbol_text = String::from("Symbols:\n");
                for symbol in symbols {
                    symbol_text.push_str(&format!(
                        "- {} {}:{}-{} :: {}\n",
                        symbol.kind,
                        symbol.path.unwrap_or_default(),
                        symbol.start_line.unwrap_or_default(),
                        symbol.end_line.unwrap_or_default(),
                        symbol.summary.unwrap_or(symbol.name)
                    ));
                }
                if !push_budgeted(&mut out, &symbol_text, char_budget) {
                    break;
                }
            }

            for (start, end, text) in self.storage.read_chunks_for_file(*file_id, 3)? {
                let chunk = format!(
                    "\nSnippet {}:{}-{}\n```\n{}\n```\n",
                    file.path, start, end, text
                );
                if !push_budgeted(&mut out, &chunk, char_budget) {
                    return Ok(out);
                }
            }
        }
        Ok(out.trim().to_string())
    }
}

fn push_budgeted(out: &mut String, next: &str, char_budget: usize) -> bool {
    if out.len() + next.len() > char_budget {
        return false;
    }
    out.push_str(next);
    true
}

fn budget_task_context_response(
    mut response: TaskContextResponse,
    max_tokens: usize,
) -> TaskContextResponse {
    let token_budget = max_tokens.max(200);
    let char_budget = token_budget.saturating_mul(4);
    let pack_budget = char_budget.saturating_div(2).max(800);

    truncate_string(&mut response.context_pack, pack_budget);

    let file_limit = scaled_limit(token_budget, 250, 2, 8);
    let symbol_limit = scaled_limit(token_budget, 200, 3, 12);
    let test_limit = scaled_limit(token_budget, 500, 1, 8);
    let node_limit = scaled_limit(token_budget, 120, 6, 80);
    let edge_limit = scaled_limit(token_budget, 160, 4, 80);

    response.relevant_files.truncate(file_limit);
    response.relevant_symbols.truncate(symbol_limit);
    response.suggested_tests.truncate(test_limit);
    trim_hits(&mut response.relevant_files);
    trim_hits(&mut response.relevant_symbols);
    trim_hits(&mut response.suggested_tests);
    compact_subgraph(&mut response.subgraph, node_limit, edge_limit);

    let target = char_budget.max(2_500);
    for _ in 0..10 {
        let size = serde_json::to_string(&response)
            .map(|text| text.len())
            .unwrap_or(usize::MAX);
        if size <= target {
            break;
        }

        if response.subgraph.edges.len() > 2 {
            let next = response.subgraph.edges.len().div_ceil(2);
            let nodes_len = response.subgraph.nodes.len();
            compact_subgraph(&mut response.subgraph, nodes_len, next);
            continue;
        }
        if response.subgraph.nodes.len() > 4 {
            let next = response.subgraph.nodes.len().div_ceil(2);
            let edges_len = response.subgraph.edges.len();
            compact_subgraph(&mut response.subgraph, next, edges_len);
            continue;
        }
        if response.relevant_symbols.len() > 2 {
            response
                .relevant_symbols
                .truncate(response.relevant_symbols.len().div_ceil(2));
            continue;
        }
        if response.relevant_files.len() > 1 {
            response
                .relevant_files
                .truncate(response.relevant_files.len().div_ceil(2));
            continue;
        }
        if response.suggested_tests.len() > 1 {
            response
                .suggested_tests
                .truncate(response.suggested_tests.len().div_ceil(2));
            continue;
        }

        let current = response.context_pack.len();
        if current <= 400 {
            break;
        }
        truncate_string(&mut response.context_pack, current.saturating_mul(3) / 4);
    }

    response
}

fn scaled_limit(tokens: usize, tokens_per_item: usize, min: usize, max: usize) -> usize {
    let scaled = tokens.saturating_div(tokens_per_item).max(min);
    scaled.min(max)
}

fn trim_hits(hits: &mut [SearchHit]) {
    for hit in hits {
        trim_opt_string(&mut hit.path, 180);
        trim_opt_string(&mut hit.name, 120);
        trim_opt_string(&mut hit.snippet, 240);
    }
}

fn compact_subgraph(subgraph: &mut Subgraph, node_limit: usize, edge_limit: usize) {
    subgraph.edges.truncate(edge_limit);
    for edge in &mut subgraph.edges {
        compact_edge(edge);
    }

    let mut referenced = HashSet::new();
    for edge in &subgraph.edges {
        referenced.insert(edge.source_id);
        referenced.insert(edge.target_id);
    }

    let mut kept = Vec::new();
    let mut seen = HashSet::new();
    for node in subgraph.nodes.drain(..) {
        if !referenced.is_empty() && !referenced.contains(&node.id) {
            continue;
        }
        if seen.insert(node.id) {
            kept.push(compact_node(node));
        }
        if kept.len() >= node_limit {
            break;
        }
    }

    subgraph.nodes = kept;
}

fn compact_node(mut node: NodeRecord) -> NodeRecord {
    trim_string(&mut node.name, 120);
    trim_string(&mut node.qualified_name, 180);
    trim_opt_string(&mut node.path, 180);
    trim_opt_string(&mut node.summary, 220);
    node.metadata = serde_json::json!({});
    node
}

fn compact_edge(edge: &mut EdgeRecord) {
    edge.metadata = compact_edge_metadata(&edge.metadata);
}

fn compact_edge_metadata(metadata: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for key in ["source", "imported", "callee", "handler", "route", "method"] {
        if let Some(value) = metadata.get(key) {
            out.insert(key.to_string(), compact_json_value(value, 160));
        }
    }
    serde_json::Value::Object(out)
}

fn compact_json_value(value: &serde_json::Value, max_chars: usize) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            let mut text = text.clone();
            truncate_string(&mut text, max_chars);
            serde_json::Value::String(text)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(8)
                .map(|value| compact_json_value(value, max_chars))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn trim_opt_string(value: &mut Option<String>, max_chars: usize) {
    if let Some(text) = value {
        truncate_string(text, max_chars);
    }
}

fn trim_string(value: &mut String, max_chars: usize) {
    truncate_string(value, max_chars);
}

fn truncate_string(value: &mut String, max_chars: usize) {
    if value.chars().count() <= max_chars {
        return;
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    *value = truncated;
}

fn git_dirty_paths(repo_path: &Path) -> Result<Vec<String>> {
    if !repo_path.join(".git").exists() {
        return Ok(Vec::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("status")
        .arg("--porcelain=v1")
        .arg("-z")
        .output();
    let Ok(output) = output else {
        return Ok(Vec::new());
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let status = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let code = &entry[..2];
        let path = entry[3..].to_string();
        if code.starts_with('R') || code.starts_with('C') {
            let _old = entries.next();
        }
        if !code.contains('D') {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn detect_test_command(repo_path: &Path) -> Option<String> {
    if repo_path.join("pnpm-lock.yaml").exists() {
        return Some("pnpm test".to_string());
    }
    if repo_path.join("yarn.lock").exists() {
        return Some("yarn test".to_string());
    }
    if repo_path.join("package.json").exists() {
        return Some("npm test".to_string());
    }
    if repo_path.join("Cargo.toml").exists() {
        return Some("cargo test".to_string());
    }
    None
}

fn safe_repo_path(repo_path: &Path, rel_path: &str) -> Result<Option<PathBuf>> {
    let root = repo_path.canonicalize()?;
    let clean = rel_path.replace('\\', "/");
    if clean.starts_with('/') || clean.split('/').any(|part| part == "..") {
        return Ok(None);
    }
    let candidate = root.join(clean);
    let canonical = match candidate.canonicalize() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if canonical.starts_with(&root) {
        Ok(Some(canonical))
    } else {
        Ok(None)
    }
}

fn language_from_path(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension().and_then(|ext| ext.to_str())?;
    match ext {
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "rs" => Some("rust"),
        "md" | "mdx" => Some("markdown"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}
