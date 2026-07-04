// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    path::PathBuf,
};

use anyhow::Result;
use serde_json::{json, Value};

use crate::{
    indexer::Indexer, model::TaskContextResponse, retrieval::RetrievalEngine, storage::Storage,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct McpOptions {
    pub compact: bool,
}

pub fn serve_stdio(repo_path: PathBuf, options: McpOptions) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": error.to_string() }
                    }),
                )?;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|method| method.as_str())
            .unwrap_or_default();

        if method == "notifications/initialized" {
            continue;
        }

        let response = match handle_request(&repo_path, &request, options) {
            Ok(Some(result)) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Ok(None) => continue,
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": error.to_string() }
            }),
        };
        write_response(&mut stdout, response)?;
    }
    Ok(())
}

fn handle_request(
    repo_path: &PathBuf,
    request: &Value,
    options: McpOptions,
) -> Result<Option<Value>> {
    let method = request
        .get("method")
        .and_then(|method| method.as_str())
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        "initialize" => Ok(Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": { "name": "ckg", "version": env!("CARGO_PKG_VERSION") }
        }))),
        "tools/list" => Ok(Some(json!({ "tools": tools(options) }))),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let value = call_tool(repo_path, name, args)?;
            let text = if options.compact {
                serde_json::to_string(&value)?
            } else {
                serde_json::to_string_pretty(&value)?
            };
            Ok(Some(json!({
                "content": [{
                    "type": "text",
                    "text": text
                }]
            })))
        }
        "resources/list" => Ok(Some(resources_list(repo_path)?)),
        "resources/templates/list" => Ok(Some(resources_templates_list())),
        "resources/read" => {
            let uri = params
                .get("uri")
                .and_then(|uri| uri.as_str())
                .unwrap_or_default();
            Ok(Some(resources_read(repo_path, uri)?))
        }
        _ => Ok(Some(json!({}))),
    }
}

fn call_tool(repo_path: &PathBuf, name: &str, args: Value) -> Result<Value> {
    match canonical_tool_name(name) {
        "ckg_index" => {
            let storage = Storage::open_for_repo(repo_path)?;
            let report = Indexer::new(storage).index_repo(repo_path)?;
            Ok(json!({
                "repo_id": report.repo_id,
                "scanned": report.scanned,
                "indexed": report.indexed,
                "skipped_unchanged": report.skipped_unchanged,
                "deleted": report.deleted,
                "db_path": report.db_path
            }))
        }
        "ckg_status" => {
            let storage = Storage::open_for_repo(repo_path)?;
            let report = Indexer::new(storage).status_repo(repo_path)?;
            Ok(serde_json::to_value(report)?)
        }
        "ckg_search" => {
            maybe_auto_index(repo_path, &args)?;
            let query = required_str(&args, "query")?;
            let limit = args
                .get("limit")
                .and_then(|value| value.as_u64())
                .unwrap_or(20) as usize;
            let storage = Storage::open_for_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            Ok(json!({ "hits": engine.search(query, limit)? }))
        }
        "ckg_task_context" => {
            maybe_auto_index(repo_path, &args)?;
            let task = required_str(&args, "task")?;
            let max_tokens = args
                .get("max_tokens")
                .and_then(|value| value.as_u64())
                .unwrap_or(12_000) as usize;
            let hops = args
                .get("hops")
                .and_then(|value| value.as_u64())
                .unwrap_or(2) as usize;
            let include_git_dirty = args
                .get("include_git_dirty")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let storage = Storage::open_for_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            let context = engine.task_context_for_repo(
                Some(repo_path),
                task,
                max_tokens,
                hops,
                include_git_dirty,
            )?;
            let mode = args
                .get("response_mode")
                .and_then(|value| value.as_str())
                .unwrap_or("brief");
            if mode == "brief" {
                Ok(brief_task_context(context, max_tokens))
            } else {
                Ok(serde_json::to_value(context)?)
            }
        }
        "ckg_neighborhood" => {
            let node_id = args
                .get("node_id")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            let hops = args
                .get("hops")
                .and_then(|value| value.as_u64())
                .unwrap_or(2) as usize;
            let storage = Storage::open_for_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            Ok(serde_json::to_value(engine.neighborhood(node_id, hops)?)?)
        }
        "ckg_file" => {
            maybe_auto_index(repo_path, &args)?;
            let path = required_str(&args, "path")?;
            let offset = args
                .get("offset")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize);
            let limit = args
                .get("limit")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize);
            let line_numbers = args
                .get("line_numbers")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let storage = Storage::open_for_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            Ok(engine
                .file_content_range_with_fallback(repo_path, path, offset, limit, line_numbers)?
                .unwrap_or_else(|| json!({ "error": "file not found" })))
        }
        "ckg_grep" => {
            maybe_auto_index(repo_path, &args)?;
            let query = required_str(&args, "query")?;
            let path_glob = args.get("path_glob").and_then(|value| value.as_str());
            let case_sensitive = args
                .get("case_sensitive")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let regex = args
                .get("regex")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let limit = arg_limit(&args, 100);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.grep(repo_id, query, path_glob, case_sensitive, regex, limit)
        }
        "ckg_glob" => {
            maybe_auto_index(repo_path, &args)?;
            let pattern = args
                .get("pattern")
                .and_then(|value| value.as_str())
                .unwrap_or("*");
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.glob(repo_id, pattern, limit)
        }
        "ckg_workspace_symbols" => {
            maybe_auto_index(repo_path, &args)?;
            let query = args
                .get("query")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let limit = arg_limit(&args, 100);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.workspace_symbols(repo_id, query, limit)
        }
        "ckg_document_symbols" => {
            maybe_auto_index(repo_path, &args)?;
            let path = required_str(&args, "path")?;
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.document_symbols(repo_id, path)
        }
        "ckg_definition" => {
            maybe_auto_index(repo_path, &args)?;
            let query = required_str(&args, "query")?;
            let limit = arg_limit(&args, 20);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.definition(repo_id, query, limit)
        }
        "ckg_definition_at" => {
            maybe_auto_index(repo_path, &args)?;
            let path = required_str(&args, "path")?;
            let line = required_i64(&args, "line")?;
            let character = args.get("character").and_then(|value| value.as_i64());
            let limit = arg_limit(&args, 20);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.definition_at(repo_id, path, line, character, limit)
        }
        "ckg_references" => {
            maybe_auto_index(repo_path, &args)?;
            let node_id = required_i64(&args, "node_id")?;
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.references(repo_id, node_id, limit)
        }
        "ckg_references_at" => {
            maybe_auto_index(repo_path, &args)?;
            let path = required_str(&args, "path")?;
            let line = required_i64(&args, "line")?;
            let character = args.get("character").and_then(|value| value.as_i64());
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.references_at(repo_id, path, line, character, limit)
        }
        "ckg_call_hierarchy" => {
            maybe_auto_index(repo_path, &args)?;
            let node_id = required_i64(&args, "node_id")?;
            let direction = args
                .get("direction")
                .and_then(|value| value.as_str())
                .unwrap_or("both");
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.call_hierarchy(repo_id, node_id, direction, limit)
        }
        "ckg_call_hierarchy_at" => {
            maybe_auto_index(repo_path, &args)?;
            let path = required_str(&args, "path")?;
            let line = required_i64(&args, "line")?;
            let character = args.get("character").and_then(|value| value.as_i64());
            let direction = args
                .get("direction")
                .and_then(|value| value.as_str())
                .unwrap_or("both");
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.call_hierarchy_at(repo_id, path, line, character, direction, limit)
        }
        "ckg_imports" => {
            maybe_auto_index(repo_path, &args)?;
            let node_id = required_i64(&args, "node_id")?;
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.imports(repo_id, node_id, limit)
        }
        "ckg_dependents" => {
            maybe_auto_index(repo_path, &args)?;
            let node_id = required_i64(&args, "node_id")?;
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.dependents(repo_id, node_id, limit)
        }
        "ckg_suggested_tests" => {
            maybe_auto_index(repo_path, &args)?;
            let task = required_str(&args, "task")?;
            let limit = arg_limit(&args, 20);
            let storage = Storage::open_for_repo(repo_path)?;
            let engine = RetrievalEngine::new(storage);
            engine.suggested_tests_detailed(repo_path, task, limit)
        }
        "ckg_ast_graph" => {
            maybe_auto_index(repo_path, &args)?;
            let limit = arg_limit(&args, 500);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let mut graph = storage.subgraph_by_edge_kinds(repo_id, &["DEFINES"], limit)?;
            let mut structural = storage.subgraph_by_edge_kinds(repo_id, &["CONTAINS"], limit)?;
            graph.nodes.append(&mut structural.nodes);
            graph.edges.append(&mut structural.edges);
            Ok(serde_json::to_value(graph)?)
        }
        "ckg_dependency_graph" => {
            maybe_auto_index(repo_path, &args)?;
            let limit = arg_limit(&args, 500);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            Ok(serde_json::to_value(storage.subgraph_by_edge_kinds(
                repo_id,
                &["IMPORTS"],
                limit,
            )?)?)
        }
        "ckg_call_graph" => {
            maybe_auto_index(repo_path, &args)?;
            let limit = arg_limit(&args, 500);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            Ok(serde_json::to_value(storage.subgraph_by_edge_kinds(
                repo_id,
                &["CALLS"],
                limit,
            )?)?)
        }
        "ckg_product_flow_graph" => {
            maybe_auto_index(repo_path, &args)?;
            let limit = arg_limit(&args, 500);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let endpoints = storage.nodes_by_kinds(repo_id, &["Endpoint"], limit)?;
            let references =
                storage.subgraph_by_edge_kinds(repo_id, &["REFERENCES", "CALLS"], limit)?;
            Ok(json!({
                "entrypoints": endpoints,
                "subgraph": references
            }))
        }
        "ckg_test_graph" => {
            maybe_auto_index(repo_path, &args)?;
            let limit = arg_limit(&args, 500);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            let tests = storage.nodes_by_kinds(repo_id, &["Test"], limit)?;
            let graph = storage.subgraph_by_edge_kinds(repo_id, &["TESTS"], limit)?;
            Ok(json!({
                "tests": tests,
                "subgraph": graph
            }))
        }
        "ckg_semantic_summaries" => {
            maybe_auto_index(repo_path, &args)?;
            let limit = arg_limit(&args, 200);
            let storage = Storage::open_for_repo(repo_path)?;
            let repo_id = storage.init_repo(repo_path)?;
            Ok(json!({
                "summaries": storage.semantic_summary_nodes(repo_id, limit)?
            }))
        }
        _ => Ok(json!({ "error": format!("unknown tool: {}", name) })),
    }
}

fn canonical_tool_name(name: &str) -> &str {
    match name {
        "index" => "ckg_index",
        "status" => "ckg_status",
        "search" => "ckg_search",
        "task_context" => "ckg_task_context",
        "neighborhood" => "ckg_neighborhood",
        "file" | "read" => "ckg_file",
        "grep" => "ckg_grep",
        "glob" => "ckg_glob",
        "workspace_symbols" | "symbols" => "ckg_workspace_symbols",
        "document_symbols" => "ckg_document_symbols",
        "definition" => "ckg_definition",
        "definition_at" => "ckg_definition_at",
        "references" => "ckg_references",
        "references_at" => "ckg_references_at",
        "call_hierarchy" => "ckg_call_hierarchy",
        "call_hierarchy_at" => "ckg_call_hierarchy_at",
        "imports" => "ckg_imports",
        "dependents" => "ckg_dependents",
        "suggested_tests" => "ckg_suggested_tests",
        "ast_graph" => "ckg_ast_graph",
        "dependency_graph" => "ckg_dependency_graph",
        "call_graph" => "ckg_call_graph",
        "product_flow_graph" => "ckg_product_flow_graph",
        "test_graph" => "ckg_test_graph",
        "semantic_summaries" => "ckg_semantic_summaries",
        other => other,
    }
}

fn tools(options: McpOptions) -> Value {
    let mut tools = json!([
        {
            "name": "index",
            "description": "Alias for ckg_index. Index the configured repository.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "status",
            "description": "Alias for ckg_status. Report whether the configured repository index is stale without updating it.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "search",
            "description": "Alias for ckg_search. Search indexed files, symbols, summaries, and chunks.",
            "inputSchema": search_schema()
        },
        {
            "name": "task_context",
            "description": "Alias for ckg_task_context. Build a task-focused context pack.",
            "inputSchema": task_context_schema()
        },
        {
            "name": "read",
            "description": "Alias for ckg_file. Read a repo-relative file with optional line range. Falls back to safe filesystem read when the file is not indexed yet.",
            "inputSchema": read_schema()
        },
        {
            "name": "grep",
            "description": "Index-aware regex grep over indexed text files with substring fallback.",
            "inputSchema": grep_schema()
        },
        {
            "name": "glob",
            "description": "Index-aware file path glob over indexed files.",
            "inputSchema": glob_schema()
        },
        {
            "name": "workspace_symbols",
            "description": "Best-effort indexed workspace symbol search.",
            "inputSchema": symbol_query_schema()
        },
        {
            "name": "document_symbols",
            "description": "Best-effort indexed document symbols for one repo-relative path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "auto_index": { "type": "boolean", "default": true }
                },
                "required": ["path"]
            }
        },
        {
            "name": "definition",
            "description": "Best-effort indexed symbol definition lookup.",
            "inputSchema": symbol_query_schema()
        },
        {
            "name": "definition_at",
            "description": "Best-effort definition lookup at a file line/character.",
            "inputSchema": position_schema(false)
        },
        {
            "name": "references",
            "description": "Best-effort indexed references around a node id.",
            "inputSchema": node_limit_schema()
        },
        {
            "name": "references_at",
            "description": "Best-effort references at a file line/character.",
            "inputSchema": position_schema(false)
        },
        {
            "name": "call_hierarchy",
            "description": "Best-effort indexed call hierarchy around a node id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer" },
                    "direction": { "type": "string", "enum": ["incoming", "outgoing", "both"], "default": "both" },
                    "limit": { "type": "integer", "default": 200 },
                    "auto_index": { "type": "boolean", "default": true }
                },
                "required": ["node_id"]
            }
        },
        {
            "name": "call_hierarchy_at",
            "description": "Best-effort call hierarchy at a file line/character.",
            "inputSchema": position_schema(true)
        },
        {
            "name": "imports",
            "description": "Best-effort indexed outgoing imports for a file/symbol node id.",
            "inputSchema": node_limit_schema()
        },
        {
            "name": "dependents",
            "description": "Best-effort indexed incoming import dependents for a node id.",
            "inputSchema": node_limit_schema()
        },
        {
            "name": "suggested_tests",
            "description": "Suggest indexed tests and likely test command for a task.",
            "inputSchema": suggested_tests_schema()
        },
        {
            "name": "ast_graph",
            "description": "Alias for ckg_ast_graph.",
            "inputSchema": limit_schema()
        },
        {
            "name": "dependency_graph",
            "description": "Alias for ckg_dependency_graph.",
            "inputSchema": limit_schema()
        },
        {
            "name": "call_graph",
            "description": "Alias for ckg_call_graph.",
            "inputSchema": limit_schema()
        },
        {
            "name": "product_flow_graph",
            "description": "Alias for ckg_product_flow_graph.",
            "inputSchema": limit_schema()
        },
        {
            "name": "test_graph",
            "description": "Alias for ckg_test_graph.",
            "inputSchema": limit_schema()
        },
        {
            "name": "semantic_summaries",
            "description": "Alias for ckg_semantic_summaries.",
            "inputSchema": limit_schema()
        }
    ])
    .as_array()
    .cloned()
    .unwrap_or_default();

    if options.compact {
        return Value::Array(tools);
    }

    tools.extend(
        json!([
        {
            "name": "ckg_index",
            "description": "Index the configured repository into the local CKG SQLite database.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "ckg_status",
            "description": "Report whether the configured repository index is stale without updating it.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "ckg_search",
            "description": "Search indexed files, symbols, summaries, and chunks.",
            "inputSchema": search_schema()
        },
        {
            "name": "ckg_task_context",
            "description": "Build a task-focused context pack with relevant files, symbols, subgraph, and tests.",
            "inputSchema": task_context_schema()
        },
        {
            "name": "ckg_neighborhood",
            "description": "Return a graph neighborhood around a node id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer" },
                    "hops": { "type": "integer", "default": 2 }
                },
                "required": ["node_id"]
            }
        },
        {
            "name": "ckg_file",
            "description": "Return file metadata and current content by repository-relative path. Falls back to safe filesystem read when the file is not indexed yet.",
            "inputSchema": read_schema()
        },
        {
            "name": "ckg_grep",
            "description": "Index-aware regex grep over indexed text files with substring fallback.",
            "inputSchema": grep_schema()
        },
        {
            "name": "ckg_glob",
            "description": "Index-aware file path glob over indexed files.",
            "inputSchema": glob_schema()
        },
        {
            "name": "ckg_workspace_symbols",
            "description": "Best-effort indexed workspace symbol search.",
            "inputSchema": symbol_query_schema()
        },
        {
            "name": "ckg_document_symbols",
            "description": "Best-effort indexed document symbols for one repo-relative path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "auto_index": { "type": "boolean", "default": true }
                },
                "required": ["path"]
            }
        },
        {
            "name": "ckg_definition",
            "description": "Best-effort indexed symbol definition lookup.",
            "inputSchema": symbol_query_schema()
        },
        {
            "name": "ckg_definition_at",
            "description": "Best-effort definition lookup at a file line/character.",
            "inputSchema": position_schema(false)
        },
        {
            "name": "ckg_references",
            "description": "Best-effort indexed references around a node id.",
            "inputSchema": node_limit_schema()
        },
        {
            "name": "ckg_references_at",
            "description": "Best-effort references at a file line/character.",
            "inputSchema": position_schema(false)
        },
        {
            "name": "ckg_call_hierarchy",
            "description": "Best-effort indexed call hierarchy around a node id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer" },
                    "direction": { "type": "string", "enum": ["incoming", "outgoing", "both"], "default": "both" },
                    "limit": { "type": "integer", "default": 200 },
                    "auto_index": { "type": "boolean", "default": true }
                },
                "required": ["node_id"]
            }
        },
        {
            "name": "ckg_call_hierarchy_at",
            "description": "Best-effort call hierarchy at a file line/character.",
            "inputSchema": position_schema(true)
        },
        {
            "name": "ckg_imports",
            "description": "Best-effort indexed outgoing imports for a file/symbol node id.",
            "inputSchema": node_limit_schema()
        },
        {
            "name": "ckg_dependents",
            "description": "Best-effort indexed incoming import dependents for a node id.",
            "inputSchema": node_limit_schema()
        },
        {
            "name": "ckg_suggested_tests",
            "description": "Suggest indexed tests and likely test command for a task.",
            "inputSchema": suggested_tests_schema()
        },
        {
            "name": "ckg_ast_graph",
            "description": "Return symbol-level AST graph: CONTAINS and DEFINES edges with repository, directory, file, and symbol nodes.",
            "inputSchema": limit_schema()
        },
        {
            "name": "ckg_dependency_graph",
            "description": "Return dependency graph built from IMPORTS edges, including import symbols and resolved local file imports.",
            "inputSchema": limit_schema()
        },
        {
            "name": "ckg_call_graph",
            "description": "Return call graph built from CALLS edges, including in-file and resolved cross-file calls.",
            "inputSchema": limit_schema()
        },
        {
            "name": "ckg_product_flow_graph",
            "description": "Return product-flow entrypoint graph with Endpoint nodes and REFERENCES edges to handlers.",
            "inputSchema": limit_schema()
        },
        {
            "name": "ckg_test_graph",
            "description": "Return test graph with Test nodes and TESTS edges to code under test.",
            "inputSchema": limit_schema()
        },
        {
            "name": "ckg_semantic_summaries",
            "description": "Return indexed semantic summaries from doc comments/signatures for files, symbols, tests, and endpoints.",
            "inputSchema": limit_schema()
        }
        ])
        .as_array()
        .cloned()
        .unwrap_or_default(),
    );
    Value::Array(tools)
}

fn resources_list(repo_path: &PathBuf) -> Result<Value> {
    let storage = Storage::open_for_repo(repo_path)?;
    Indexer::new(storage).index_repo(repo_path)?;
    let storage = Storage::open_for_repo(repo_path)?;
    let repo_id = storage.init_repo(repo_path)?;
    let mut resources = vec![
        resource(
            "ckg://repo/summary",
            "CKG repository summary",
            "High-level indexed repository summary",
        ),
        resource(
            "ckg://graphs/ast",
            "CKG AST graph",
            "Symbol-level AST graph",
        ),
        resource(
            "ckg://graphs/dependency",
            "CKG dependency graph",
            "Import/dependency graph",
        ),
        resource("ckg://graphs/call", "CKG call graph", "Call graph"),
        resource(
            "ckg://graphs/product-flow",
            "CKG product flow graph",
            "Endpoint/handler graph",
        ),
        resource("ckg://graphs/test", "CKG test graph", "Test-to-code graph"),
        resource(
            "ckg://summaries/semantic",
            "CKG semantic summaries",
            "Indexed doc/signature summaries",
        ),
    ];
    for file in storage.list_files(repo_id)?.into_iter().take(200) {
        resources.push(resource(
            &format!("ckg://files/{}", file.path),
            &format!("File {}", file.path),
            "Indexed file content",
        ));
    }
    Ok(json!({ "resources": resources }))
}

fn resources_templates_list() -> Value {
    json!({
        "resourceTemplates": [
            {
                "uriTemplate": "ckg://files/{path}",
                "name": "CKG indexed file",
                "description": "Read indexed file content by repo-relative path",
                "mimeType": "text/plain"
            },
            {
                "uriTemplate": "ckg://nodes/{id}",
                "name": "CKG graph node",
                "description": "Read graph node JSON by id",
                "mimeType": "application/json"
            }
        ]
    })
}

fn resources_read(repo_path: &PathBuf, uri: &str) -> Result<Value> {
    let storage = Storage::open_for_repo(repo_path)?;
    Indexer::new(storage).index_repo(repo_path)?;
    let storage = Storage::open_for_repo(repo_path)?;
    storage.init_repo(repo_path)?;
    let engine = RetrievalEngine::new(storage);
    let (mime_type, text) = if uri == "ckg://repo/summary" {
        let context =
            engine.task_context_for_repo(Some(repo_path), "repository summary", 4_000, 1, true)?;
        ("application/json", serde_json::to_string_pretty(&context)?)
    } else if let Some(path) = uri.strip_prefix("ckg://files/") {
        let value = engine
            .file_content_range_with_fallback(repo_path, path, None, None, true)?
            .unwrap_or_else(|| json!({ "error": "file not found" }));
        ("application/json", serde_json::to_string_pretty(&value)?)
    } else if let Some(id) = uri.strip_prefix("ckg://nodes/") {
        let node_id = id.parse::<i64>().unwrap_or_default();
        let graph = engine.neighborhood(node_id, 1)?;
        ("application/json", serde_json::to_string_pretty(&graph)?)
    } else if uri == "ckg://graphs/ast" {
        graph_resource(repo_path, &["DEFINES", "CONTAINS"], 1_000)?
    } else if uri == "ckg://graphs/dependency" {
        graph_resource(repo_path, &["IMPORTS"], 1_000)?
    } else if uri == "ckg://graphs/call" {
        graph_resource(repo_path, &["CALLS"], 1_000)?
    } else if uri == "ckg://graphs/product-flow" {
        let storage = Storage::open_for_repo(repo_path)?;
        let repo_id = storage.init_repo(repo_path)?;
        let endpoints = storage.nodes_by_kinds(repo_id, &["Endpoint"], 1_000)?;
        let subgraph = storage.subgraph_by_edge_kinds(repo_id, &["REFERENCES", "CALLS"], 1_000)?;
        let value = json!({ "entrypoints": endpoints, "subgraph": subgraph });
        ("application/json", serde_json::to_string_pretty(&value)?)
    } else if uri == "ckg://graphs/test" {
        graph_resource(repo_path, &["TESTS"], 1_000)?
    } else if uri == "ckg://summaries/semantic" {
        let storage = Storage::open_for_repo(repo_path)?;
        let repo_id = storage.init_repo(repo_path)?;
        let value = json!({ "summaries": storage.semantic_summary_nodes(repo_id, 1_000)? });
        ("application/json", serde_json::to_string_pretty(&value)?)
    } else {
        (
            "application/json",
            serde_json::to_string_pretty(&json!({ "error": "unknown resource" }))?,
        )
    };
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": mime_type,
            "text": text
        }]
    }))
}

fn graph_resource(
    repo_path: &PathBuf,
    kinds: &[&str],
    limit: usize,
) -> Result<(&'static str, String)> {
    let storage = Storage::open_for_repo(repo_path)?;
    let repo_id = storage.init_repo(repo_path)?;
    let graph = storage.subgraph_by_edge_kinds(repo_id, kinds, limit)?;
    Ok(("application/json", serde_json::to_string_pretty(&graph)?))
}

fn resource(uri: &str, name: &str, description: &str) -> Value {
    json!({
        "uri": uri,
        "name": name,
        "description": description,
        "mimeType": "application/json"
    })
}

fn limit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "limit": { "type": "integer", "default": 500 },
            "auto_index": { "type": "boolean", "default": true }
        }
    })
}

fn search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" },
            "limit": { "type": "integer", "default": 20 },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["query"]
    })
}

fn task_context_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task": { "type": "string" },
            "max_tokens": { "type": "integer", "default": 12000 },
            "hops": { "type": "integer", "default": 2 },
            "include_git_dirty": { "type": "boolean", "default": true },
            "response_mode": { "type": "string", "enum": ["brief", "normal"], "default": "brief" },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["task"]
    })
}

fn read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "offset": { "type": "integer", "description": "1-based start line" },
            "limit": { "type": "integer", "description": "Maximum lines to return" },
            "line_numbers": { "type": "boolean", "default": false },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["path"]
    })
}

fn grep_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" },
            "path_glob": { "type": "string" },
            "case_sensitive": { "type": "boolean", "default": false },
            "regex": { "type": "boolean", "default": true },
            "limit": { "type": "integer", "default": 100 },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["query"]
    })
}

fn glob_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "default": "*" },
            "limit": { "type": "integer", "default": 200 },
            "auto_index": { "type": "boolean", "default": true }
        }
    })
}

fn symbol_query_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" },
            "limit": { "type": "integer", "default": 100 },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["query"]
    })
}

fn node_limit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "node_id": { "type": "integer" },
            "limit": { "type": "integer", "default": 200 },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["node_id"]
    })
}

fn suggested_tests_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task": { "type": "string" },
            "limit": { "type": "integer", "default": 20 },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["task"]
    })
}

fn position_schema(include_direction: bool) -> Value {
    let mut value = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "line": { "type": "integer", "description": "1-based line number" },
            "character": { "type": "integer", "description": "1-based character offset" },
            "limit": { "type": "integer", "default": 200 },
            "auto_index": { "type": "boolean", "default": true }
        },
        "required": ["path", "line"]
    });
    if include_direction {
        value["properties"]["direction"] = json!({ "type": "string", "enum": ["incoming", "outgoing", "both"], "default": "both" });
    }
    value
}

fn maybe_auto_index(repo_path: &PathBuf, args: &Value) -> Result<()> {
    let enabled = args
        .get("auto_index")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    if enabled {
        let storage = Storage::open_for_repo(repo_path)?;
        Indexer::new(storage).index_repo(repo_path)?;
    }
    Ok(())
}

fn brief_task_context(mut context: TaskContextResponse, max_tokens: usize) -> Value {
    let context_limit = max_tokens.saturating_mul(2).clamp(600, 2_400);
    truncate_chars(&mut context.context_pack, context_limit);

    let files = context
        .relevant_files
        .iter()
        .filter_map(|hit| hit.path.clone())
        .take(3)
        .collect::<Vec<_>>();

    let symbols = context
        .relevant_symbols
        .iter()
        .filter_map(|hit| {
            let name = hit.name.as_deref()?;
            Some(json!({
                "name": name,
                "path": hit.path.clone(),
            }))
        })
        .take(4)
        .collect::<Vec<_>>();

    let tests = context
        .suggested_tests
        .iter()
        .filter_map(|hit| hit.path.clone().or_else(|| hit.name.clone()))
        .take(2)
        .collect::<Vec<_>>();

    let node_names = context
        .subgraph
        .nodes
        .iter()
        .map(|node| (node.id, compact_label(&node.name, &node.path)))
        .collect::<HashMap<_, _>>();
    let sample_edges = context
        .subgraph
        .edges
        .iter()
        .take(3)
        .map(|edge| {
            let source = node_names
                .get(&edge.source_id)
                .cloned()
                .unwrap_or_else(|| edge.source_id.to_string());
            let target = node_names
                .get(&edge.target_id)
                .cloned()
                .unwrap_or_else(|| edge.target_id.to_string());
            format!("{} -{}-> {}", source, edge.kind, target)
        })
        .collect::<Vec<_>>();

    json!({
        "query": context.query,
        "context": context.context_pack,
        "files": files,
        "symbols": symbols,
        "tests": tests,
        "graph": {
            "nodes": context.subgraph.nodes.len(),
            "edges": context.subgraph.edges.len(),
            "sample_edges": sample_edges
        }
    })
}

fn compact_label(name: &str, path: &Option<String>) -> String {
    let mut label = match path {
        Some(path) if path != name => format!("{name} ({path})"),
        _ => name.to_string(),
    };
    truncate_chars(&mut label, 120);
    label
}

fn truncate_chars(value: &mut String, max_chars: usize) {
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

fn arg_limit(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(|value| value.as_u64())
        .unwrap_or(default as u64) as usize
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required string argument '{}'", key))
}

fn required_i64(args: &Value, key: &str) -> Result<i64> {
    args.get(key)
        .and_then(|value| value.as_i64())
        .ok_or_else(|| anyhow::anyhow!("missing required integer argument '{}'", key))
}

fn write_response(stdout: &mut io::Stdout, response: Value) -> Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
    stdout.flush()?;
    Ok(())
}
