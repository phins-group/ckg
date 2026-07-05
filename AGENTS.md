# Agent Guide

Code Knowledge Graph = AST graph + dependency graph + call graph + product flow graph + test graph + semantic summaries

This repository contains `ckg`, a local-first Code Knowledge Graph service for AI coding agents.

The project is a Rust CLI/service that indexes a codebase into SQLite and exposes retrieval through CLI, HTTP, and MCP stdio.

## Mission

`ckg` helps an AI coding agent retrieve compact, graph-aware context for bugfix and feature work.

It models:

- AST/symbol graph
- dependency graph
- call graph
- product-flow entrypoint graph
- test graph
- semantic summaries

The implementation is currently an MVP. It is intentionally local-first and does not require Neo4j, Postgres, Qdrant, or any database server.

## Architecture

Important files:

- `src/main.rs`: CLI entrypoint.
- `src/mcp.rs`: MCP stdio server and tool definitions.
- `src/server.rs`: HTTP server.
- `src/model.rs`: shared API/data model structs and graph enums.
- `src/storage.rs`: SQLite schema, migrations, graph writes, graph reads, search.
- `src/scanner.rs`: `.gitignore`-aware repo scanning, binary filtering, file metadata, hashing.
- `src/parser.rs`: Tree-sitter parsing and source fact extraction.
- `src/indexer.rs`: incremental indexing pipeline and graph construction.
- `src/retrieval.rs`: search, task context, graph neighborhood, context packing.

Storage:

- SQLite database: `.ckg/ckg.sqlite`
- Internal Git snapshot: `.ckg/git`
- `.ckg/.gitignore` contains `*` so CKG metadata does not dirty the user repo.

## Core Flow

Indexing:

```text
ckg index <repo>
  -> open/create SQLite
  -> init repo row/node
  -> use .ckg/git delta snapshot when available
  -> scan/hash changed files
  -> parse source with Tree-sitter
  -> write file/symbol/chunk nodes
  -> write CONTAINS/DEFINES/IMPORTS/CALLS/TESTS/REFERENCES
  -> resolve local imports/calls/routes
```

Retrieval:

```text
search/task-context/MCP tool
  -> in compact mode, read stale index by default unless auto_index=true
  -> in normal mode, auto-index by default unless auto_index=false
  -> query SQLite FTS5 or LIKE fallback
  -> include git dirty files as strong task-context signals when repo has Git
  -> hydrate files/symbols
  -> expand graph neighborhood if needed
  -> pack concise snippets/summaries
```

## Graph Model

Node kinds:

- `Repository`
- `Directory`
- `File`
- `Symbol`
- `Function`
- `Method`
- `Class`
- `Type`
- `Test`
- `Doc`
- `Endpoint`

Edge kinds:

- `CONTAINS`
- `DEFINES`
- `IMPORTS`
- `CALLS`
- `REFERENCES`
- `TESTS`
- `DOCUMENTS`

## Capability Map

### AST graph

Implemented primarily in:

- `parser::parse_file`
- `parser::parse_tree_sitter`
- `parser::collect_ast_facts`
- `parser::classify_symbol`
- `Indexer::index_scanned_file`

MCP tool:

- `ckg_ast_graph`

Current level:

- symbol-level AST graph
- not full statement/expression/identifier AST

### Dependency graph

Implemented primarily in:

- `parser::extract_imports`
- `parser::extract_js_import`
- `parser::extract_js_import_bindings`
- `PathAliases::load`
- `PathAliases::resolve`
- `Indexer::resolve_local_imports`
- `Storage::import_symbol_edges`
- `Storage::file_node_id_by_path`

MCP tool:

- `ckg_dependency_graph`

Current level:

- relative JS/TS imports
- tsconfig `baseUrl` and `paths`
- named imports
- namespace imports
- local file resolution

Not yet:

- package exports
- re-exports
- Rust crate/module resolution
- default export precision

### Call graph

Implemented primarily in:

- `parser::call_name`
- `parser::simple_callee_parts`
- `Indexer::index_scanned_file`
- `Indexer::resolve_cross_file_calls_for_import`
- `metadata_calls`

MCP tool:

- `ckg_call_graph`

Current level:

- in-file calls
- cross-file calls through named imports
- cross-file calls through namespace imports
- cross-file calls through tsconfig aliases

Not yet:

- type-aware method dispatch
- class instance method resolution
- dynamic call targets

### Product flow graph

Implemented primarily in:

- `parser::extract_routes`
- `parser::extract_route_line`
- `parser::route_handler_name`
- `Indexer::index_scanned_file`
- `Indexer::resolve_cross_file_calls_for_import`
- `Storage::endpoints_by_file_path`

MCP tool:

- `ckg_product_flow_graph`

Current level:

- route entrypoint detection for `router/app.get/post/put/patch/delete(path, handler)`
- `Endpoint` nodes
- `Endpoint REFERENCES handler`
- handler can be local or imported
- MCP product-flow graph also includes `CALLS` edges to expose route -> handler -> callee flow

Not yet:

- typed endpoint -> service -> repository -> database/external API classification
- framework-specific route conventions beyond simple call shape

### Test graph

Implemented primarily in:

- `parser::is_test_symbol`
- `Indexer::index_scanned_file`
- `Indexer::resolve_cross_file_calls_for_import`

MCP tool:

- `ckg_test_graph`

Current level:

- test-like symbols by path/name heuristic
- `TESTS` edges when calls resolve
- `suggested_tests` MCP tool returns likely test command from package manager files

Not yet:

- coverage import
- framework semantics for `describe`, `it`, `test`, fixtures, mocks

### Semantic summaries

Implemented primarily in:

- `parser::leading_doc_summary`
- `parser::symbol_metadata`
- `Indexer::index_scanned_file`
- `RetrievalEngine::context_pack`

MCP tool:

- `ckg_semantic_summaries`

Current level:

- leading comments/doc comments
- signature fallback
- file/source labels
- summaries are stored with indexed nodes and refreshed through file-hash incremental indexing

Not yet:

- LLM summaries
- file/module behavior summaries
- risk/change summaries

## MCP Integration

Use:

```bash
ckg mcp /path/to/repo --compact
```

Main MCP tools:

- Preferred aliases for MCP clients: `index`, `status`, `search`, `task_context`, `read`, `grep`, `glob`.
- Graph tools: `ast_graph`, `dependency_graph`, `call_graph`, `product_flow_graph`, `test_graph`, `semantic_summaries`.
- LSP-like tools: `workspace_symbols`, `document_symbols`, `definition`, `definition_at`, `references`, `references_at`, `call_hierarchy`, `call_hierarchy_at`, `imports`, `dependents`.
- Test tool: `suggested_tests`.
- Legacy names remain available: `ckg_index`, `ckg_status`, `ckg_search`, `ckg_task_context`, `ckg_file`, `ckg_ast_graph`, `ckg_dependency_graph`, `ckg_call_graph`, `ckg_product_flow_graph`, `ckg_test_graph`, `ckg_semantic_summaries`.

Some MCP clients prefix tool names with the server name. If the server is named
`ckg`, the preferred alias `search` may appear to the model as `ckg_search`.
Avoid advertising legacy `ckg_search` to prefixing clients unless `ckg_ckg_search`
is acceptable. Use `--compact` for agent-facing configs so only alias tools are
advertised.

In normal MCP mode retrieval tools auto-index by default. In `--compact`
agent-facing mode, retrieval tools default to `auto_index: false`; call
`status` first, then call `index` only when needed or pass `auto_index: true`
explicitly. `read` falls back to a safe repo-local filesystem read if a newly
created file has not been indexed yet.
`task_context.max_tokens` budgets the entire response, including compact graph
signals. MCP `task_context` defaults to `response_mode: "brief"` and returns
`query`, `context`, `files`, `symbols`, `read_hints`, `tests`, and compact graph
counts/sample edges. Compact mode also defaults `read` to 120 lines, `grep` to
20 matches, graph tools to brief summaries with limit 20, and MCP responses to
about 12 KB unless `max_bytes` is provided. Full raw graphs should be requested
through the dedicated graph tools with `response_mode: "normal"`.

MCP resources:

- `ckg://repo/summary`
- `ckg://files/{path}`
- `ckg://nodes/{id}`
- `ckg://graphs/ast`
- `ckg://graphs/dependency`
- `ckg://graphs/call`
- `ckg://graphs/product-flow`
- `ckg://graphs/test`
- `ckg://summaries/semantic`

See `MCP.md` for tool schemas and examples.

## Development Rules

When changing this project:

- Keep it local-first.
- Do not add a required database server.
- Prefer SQLite-compatible designs.
- Keep indexing incremental.
- Respect `.gitignore`.
- Keep graph construction deterministic and testable.
- Add tests for each new graph edge type or resolver rule.
- Avoid large architectural splits until a module has clear pressure to become a crate.

## Testing

Run:

```bash
cargo build
cargo test
```

Known environment issue:

- `cargo fmt` may fail if `rustfmt` is not installed for the active toolchain.

## Common Follow-Up Work

High-value next steps:

- Type-aware call resolution.
- Rust module/crate dependency resolution.
- Re-export/default export resolution.
- Framework-specific product-flow detection.
- Coverage import for `TESTS`.
- LLM-backed summaries cached by file/symbol hash.
- Graph-aware ranking in `ckg_task_context`.
