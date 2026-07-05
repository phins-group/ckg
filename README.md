# ckg

Stop making AI coding agents read the whole repo.

[![Release](https://img.shields.io/github/v/release/phins-group/ckg)](https://github.com/phins-group/ckg/releases)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)

Maintained by PHINs Group.

`ckg` is a local-first Code Knowledge Graph for AI coding agents. It indexes
your repo into SQLite and returns the files, symbols, snippets, tests, and graph
signals relevant to a task through CLI, HTTP, or MCP stdio.

No database server. No cloud index. No open port required for MCP.

```bash
cargo install ckg
ckg index /path/to/repo
ckg task-context /path/to/repo "Fix bug: user cannot upload avatar" --max-tokens 800 --json
ckg mcp /path/to/repo --compact
```

Measured with the included benchmark script on a 10k TypeScript fixture:
cold index 7810 ms, no-op incremental 110 ms, SQLite DB 67.77 MB.

> Status: early alpha. The graph is useful today, but many analyses are still
> best-effort and not type-aware.

## Why It Exists

Most coding agents can grep, but they still need to know which files define a
symbol, which routes call a handler, which tests are nearby, and what small
snippets are worth reading. `ckg` gives the agent that context locally.

- Single-file SQLite storage in `.ckg/ckg.sqlite`.
- Incremental indexing by file hash, respecting `.gitignore`.
- Tree-sitter parsing for JavaScript, TypeScript, and Rust.
- FTS search over paths, symbols, summaries, and source chunks.
- AST/symbol, dependency, call, route/product-flow, test, and summary signals.
- MCP compact mode with bounded output for token-conscious agents.

## Install

```bash
cargo install ckg
```

Prebuilt binaries are available from
[GitHub Releases](https://github.com/phins-group/ckg/releases).

Set `VERSION` to the release you want, for example `v0.1.5`.

macOS Apple Silicon:

```bash
VERSION=v0.1.5
curl -L "https://github.com/phins-group/ckg/releases/download/${VERSION}/ckg-${VERSION}-aarch64-apple-darwin.tar.gz" -o ckg.tar.gz
tar -xzf ckg.tar.gz
sudo install "ckg-${VERSION}-aarch64-apple-darwin/ckg" /usr/local/bin/ckg
ckg --help
```

Linux x86_64:

```bash
VERSION=v0.1.5
curl -L "https://github.com/phins-group/ckg/releases/download/${VERSION}/ckg-${VERSION}-x86_64-unknown-linux-gnu.tar.gz" -o ckg.tar.gz
tar -xzf ckg.tar.gz
sudo install "ckg-${VERSION}-x86_64-unknown-linux-gnu/ckg" /usr/local/bin/ckg
ckg --help
```

Windows x86_64 PowerShell:

```powershell
$Version = "v0.1.5"
$Zip = "ckg-$Version-x86_64-pc-windows-msvc.zip"
Invoke-WebRequest "https://github.com/phins-group/ckg/releases/download/$Version/$Zip" -OutFile $Zip
Expand-Archive $Zip -DestinationPath .
.\ckg-$Version-x86_64-pc-windows-msvc\ckg.exe --help
```

Add the extracted directory to `PATH`, or move `ckg.exe` to a directory already
on `PATH`.

Build from source:

```bash
git clone https://github.com/phins-group/ckg.git
cd ckg
cargo build --release
```

## 60-Second Demo

```bash
# Build a local graph.
ckg index /path/to/repo

# Search indexed files and symbols.
ckg search "upload avatar" --repo-path /path/to/repo --limit 10

# Ask for compact task context.
ckg task-context /path/to/repo "Fix bug: user cannot upload avatar" \
  --max-tokens 800 \
  --json

# Start MCP stdio mode for local agents.
ckg mcp /path/to/repo --compact
```

Typical compact `task-context` output includes relevant files, symbols, read
hints, suggested tests, graph counts, and a short context pack under the token
budget.

## Commands

```bash
ckg init /path/to/repo
ckg index /path/to/repo
ckg index /path/to/repo --full
ckg search "AvatarService" --repo-path /path/to/repo --limit 20
ckg search "AvatarService" --repo-path /path/to/repo --json
ckg doctor /path/to/repo
ckg doctor /path/to/repo --maintenance --json
ckg task-context /path/to/repo "Fix avatar upload" --max-tokens 800 --json
ckg mcp /path/to/repo --compact
ckg serve /path/to/repo --port 8765
```

## MCP Usage

Recommended MCP config:

```json
{
  "mcp": {
    "ckg": {
      "type": "local",
      "command": ["ckg", "mcp", "/path/to/repo", "--compact"],
      "enabled": true,
      "timeout": 30000
    }
  }
}
```

Use `--compact` for agent-facing configs. It exposes short alias tools only.
Some MCP clients prefix tool names with the server name. If the server is named
`ckg`, the alias `task_context` may appear to the model as `ckg_task_context`.
In compact mode, retrieval tools default to `auto_index: false`, graph tools
default to brief summaries, `read` defaults to 120 lines, and responses are
bounded by a 12 KB server-side budget unless `max_bytes` is provided.

Main MCP tools:

| Tool | Purpose |
|---|---|
| `status` | Check whether the local index is stale without updating it. |
| `index` | Update the local graph/index. |
| `task_context` | Return compact task-focused context. |
| `search` | Search paths, symbols, summaries, and chunks. |
| `read` | Read a repo-relative file, with safe fallback for new files. |
| `grep` | Regex or substring grep over indexed files. |
| `glob` | Match indexed file paths by glob. |
| `workspace_symbols` | Search indexed symbols. |
| `document_symbols` | List symbols in one indexed file. |
| `definition` | Find definitions by query. |
| `definition_at` | Best-effort definition lookup by file line. |
| `references` | References around a known node id. |
| `references_at` | Best-effort references by file line. |
| `call_hierarchy` | Incoming/outgoing calls around a known node id. |
| `call_hierarchy_at` | Best-effort call hierarchy by file line. |
| `imports` | Outgoing imports for a node. |
| `dependents` | Incoming import dependents for a node. |
| `suggested_tests` | Suggest likely tests for a task. |
| `ast_graph` | Return AST/symbol graph edges. |
| `dependency_graph` | Return import/dependency graph edges. |
| `call_graph` | Return call graph edges. |
| `product_flow_graph` | Return route/product-flow graph edges. |
| `test_graph` | Return test graph edges. |
| `semantic_summaries` | Return indexed summaries. |

### Recommended Agent Flow

For an AI coding agent:

1. Call `status`.
2. If `needs_index` is `true`, call `index`, or call the next retrieval tool
   with `auto_index: true`.
3. Call `task_context`:

```json
{
  "task": "Fix MCP integration",
  "max_tokens": 800,
  "response_mode": "brief",
  "auto_index": false
}
```

4. Use `read`, `grep`, `search`, or `definition_at` for follow-up details.
5. Use graph tools only when the agent needs raw graph data.

`task_context` defaults to `response_mode: "brief"` for MCP. It returns:

```json
{
  "query": "Fix MCP integration",
  "context": "## File: ...",
  "files": ["src/avatar.ts"],
  "symbols": [{ "name": "uploadAvatar", "path": "src/avatar.ts", "line": 42, "node_id": 123 }],
  "read_hints": [{ "path": "src/avatar.ts", "offset": 22, "limit": 80 }],
  "tests": ["src/avatar.test.ts"],
  "graph": {
    "nodes": 3,
    "edges": 2,
    "sample_edges": ["route -REFERENCES-> uploadAvatar"]
  }
}
```

Use `response_mode: "normal"` only when raw `relevant_files`,
`relevant_symbols`, `subgraph`, and `suggested_tests` fields are needed.

See [MCP.md](MCP.md) for detailed tool schemas and examples.

## HTTP API

Start the server:

```bash
ckg serve /path/to/repo --port 8765
```

Health:

```bash
curl http://127.0.0.1:8765/health
```

Index:

```bash
curl -X POST http://127.0.0.1:8765/index
```

Force full index:

```bash
curl -X POST http://127.0.0.1:8765/index \
  -H 'content-type: application/json' \
  -d '{"full":true}'
```

Search:

```bash
curl -X POST http://127.0.0.1:8765/search \
  -H 'content-type: application/json' \
  -d '{"query":"AvatarService","limit":10}'
```

Task context:

```bash
curl -X POST http://127.0.0.1:8765/task-context \
  -H 'content-type: application/json' \
  -d '{"task":"Fix avatar upload","max_tokens":12000,"hops":2}'
```

Node neighborhood:

```bash
curl 'http://127.0.0.1:8765/nodes/1/neighborhood?hops=2'
```

File content:

```bash
curl http://127.0.0.1:8765/files/src/avatar.ts
```

## Indexing Behavior

On first index, or with `--full`, `ckg` scans the whole repository and stores
file metadata, hashes, chunks, nodes, and edges.

On later runs, `ckg` tries to avoid full re-indexing:

- It uses `.ckg/git` as an internal snapshot to detect changed, new, and
  deleted files.
- It does not modify the repository's real `.git`.
- If Git is unavailable, it scans file metadata and hashes only likely changed
  files.
- Unchanged files are skipped.
- Deleted files are removed from the SQLite index.

Retrieval tools in normal MCP mode default to `auto_index: true`. In compact
mode they default to `auto_index: false`; call `status` first, then `index` only
when needed.

## Benchmark

The repository includes a reproducible benchmark script:

```bash
scripts/benchmark.sh
```

Set `FILES=10000` to generate a larger fixture.

It builds the release binary, generates a temporary TypeScript fixture, runs
indexing/retrieval commands, and prints a Markdown report.

Current 10k-file sample on the measured machine:

- Cold index: 7810 ms.
- No-op incremental index: 110 ms.
- One-file incremental index: 904 ms.
- MCP status check: 115 ms.
- SQLite database size: 67.77 MB.

Sample result measured with `scripts/benchmark.sh`:

Environment:

- Machine: Darwin x86_64 25.5.0
- Rust: rustc 1.93.0 (254b59607 2026-01-19)
- ckg binary: `target/release/ckg`

Fixture:

- Generated TypeScript feature files: 1000 and 10000
- Total source/config files: 1093 and 10903
- Route files: 50 and 500
- Test files: 40 and 400

| Fixture | Cold index | No-op incremental | 1-file incremental | Status check | Search JSON | task_context 800 | SQLite size |
|---|---:|---:|---:|---:|---:|---:|---:|
| 1000 TS files | 1372 ms | 166 ms | 206 ms | 96 ms | 34 ms / 2666 bytes | 88 ms / 3908 bytes | 7.65 MB |
| 10000 TS files | 7810 ms | 110 ms | 904 ms | 115 ms | 36 ms / 1557 bytes | 562 ms / 3944 bytes | 67.77 MB |

Notes:

- Cold index runs `ckg index --full`.
- No-op incremental runs `ckg index` immediately after cold index.
- 1-file incremental appends one line to one TypeScript file, then runs
  `ckg index`.
- Status check calls MCP `status` through stdio and does not update the index.
- Results are machine-dependent and intended as a reproducible sample, not a
  universal performance guarantee.

## Storage

By default, `ckg` writes:

```text
/path/to/repo/.ckg/ckg.sqlite
/path/to/repo/.ckg/git
```

`.ckg/.gitignore` contains `*`, so CKG metadata does not dirty user Git repos.

Main SQLite tables:

- `repos`
- `files`
- `file_hashes`
- `nodes`
- `edges`
- `chunks`
- `search_fts` when FTS5 is available

Chunk rows store compact previews and line ranges. Full snippets are read from
the local filesystem when context is packed. FTS still indexes file chunks so
search can match source text without storing full source twice in ordinary chunk
rows.

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

## Project Layout

```text
src/main.rs       CLI entrypoint
src/model.rs      API and graph model structs
src/storage.rs    SQLite schema, migrations, search, graph reads/writes
src/scanner.rs    .gitignore-aware scanner and hashing
src/parser.rs     Tree-sitter parsing and source fact extraction
src/indexer.rs    Incremental indexing and graph construction
src/retrieval.rs  Search, task context, context packing
src/mcp.rs        MCP stdio server and tool definitions
src/server.rs     Axum localhost HTTP API
```

## Limitations

`ckg` currently provides best-effort static analysis:

- JS/TS import resolution is local and partial.
- Default exports, re-exports, package exports, and Rust module resolution are
  not complete.
- Call graph extraction is not type-aware.
- Dynamic calls and class instance method dispatch are not reliably resolved.
- Product-flow detection currently targets simple route call shapes.
- Semantic summaries are based on leading comments and signatures, not LLM
  summaries.
- The Rust crate is currently packaged primarily as a binary. A stable public
  library API is planned but not finalized.

## Development

Build:

```bash
cargo build
```

Run tests:

```bash
cargo test
```

Format:

```bash
cargo fmt
```

Check:

```bash
cargo check
```

## Roadmap

- Better TypeScript import/export and symbol resolution.
- Rust module and crate dependency resolution.
- Framework-specific route detectors for Next.js, Express, NestJS, Hono, and
  Fastify.
- Type-aware call resolution.
- Coverage import for richer test graphs.
- Optional vector search with `sqlite-vec`.
- Cached LLM semantic summaries.
- Stable Rust library API.

## License

Licensed under either of:

- MIT License
- Apache License, Version 2.0

at your option.

SPDX-License-Identifier: MIT OR Apache-2.0
