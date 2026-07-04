# CKG MCP Tools

This project exposes a local MCP stdio server for AI coding agents.

Run it without opening any network port:

```bash
ckg mcp /path/to/repo
```

Example client config:

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

The server speaks JSON-RPC over stdin/stdout. Tool responses are returned as MCP text content containing JSON.

## Recommended Agent Flow

For a bugfix or feature task:

1. Call `task_context` with the task text. Retrieval tools auto-index by default.
2. Optionally call `index` explicitly for a known full refresh.
3. Use specific graph tools when deeper reasoning is needed:
   - `dependency_graph` for imports/module relationships.
   - `call_graph` for caller/callee relationships.
   - `product_flow_graph` for endpoint/handler flow.
   - `test_graph` for related tests.
   - `semantic_summaries` for doc-comment/signature summaries.
4. Use `read` only for targeted file reads.

## Tool Mapping

| CKG concept | MCP tool | Backing graph/data |
|---|---|---|
| AST graph | `ast_graph` | `CONTAINS` + `DEFINES` edges |
| Dependency graph | `dependency_graph` | `IMPORTS` edges |
| Call graph | `call_graph` | `CALLS` edges |
| Product flow graph | `product_flow_graph` | `Endpoint` nodes + `REFERENCES` + `CALLS` edges |
| Test graph | `test_graph` | `Test` nodes + `TESTS` edges |
| Semantic summaries | `semantic_summaries` | node `summary` + metadata |
| Task context pack | `task_context` | search + graph neighborhood + snippets |
| Indexed grep | `grep` | regex or substring search over indexed file content |
| Indexed glob | `glob` | indexed file paths |
| Indexed read | `read` | file content with optional line range |
| LSP-like lookup | `workspace_symbols`, `document_symbols`, `definition`, `references`, `call_hierarchy` | indexed symbols/edges |

## MCP Client Tool Naming

Some MCP clients expose tools to the model with a server-name prefix:

```text
<server_name>_<mcp_tool_name>
```

If the MCP server is configured as `ckg`, the tool call visible to the model is
`ckg_search` for the MCP tool named `search`.

Legacy CKG-prefixed tools are still exposed for compatibility. If an agent calls
the legacy MCP tool `ckg_search` through a client that also prefixes the server
name, the visible tool name may become `ckg_ckg_search`. Prefer the short aliases below.
Use `ckg mcp /path/to/repo --compact` to expose only the short aliases.

CKG tools as prefixed tool calls when the MCP server name is `ckg`:

| Preferred MCP tool | Prefixed tool call | Use for |
|---|---|---|
| `index` | `ckg_index` | Update the local graph/index before retrieval. |
| `status` | `ckg_status` | Check whether the local graph/index is stale without updating it. |
| `search` | `ckg_search` | Search indexed paths, symbols, summaries, and chunks. |
| `task_context` | `ckg_task_context` | Build compact task-focused context for bugfix/feature work. |
| `read` | `ckg_read` | Read indexed file content with optional line range. |
| `grep` | `ckg_grep` | Indexed substring grep with optional path glob. |
| `glob` | `ckg_glob` | Indexed file path glob. |
| `workspace_symbols` | `ckg_workspace_symbols` | Indexed workspace symbol search. |
| `document_symbols` | `ckg_document_symbols` | Indexed symbols for one file. |
| `definition` | `ckg_definition` | Best-effort symbol definition lookup. |
| `definition_at` | `ckg_definition_at` | Best-effort definition lookup by file line/character. |
| `references` | `ckg_references` | Best-effort references around a known `node_id`. |
| `references_at` | `ckg_references_at` | Best-effort references by file line/character. |
| `call_hierarchy` | `ckg_call_hierarchy` | Best-effort incoming/outgoing calls around a known `node_id`. |
| `call_hierarchy_at` | `ckg_call_hierarchy_at` | Best-effort call hierarchy by file line/character. |
| `imports` | `ckg_imports` | Outgoing import edges for a node. |
| `dependents` | `ckg_dependents` | Incoming import dependents for a node. |
| `suggested_tests` | `ckg_suggested_tests` | Test files and likely test command for a task. |
| `neighborhood` | `ckg_neighborhood` | Expand graph context around a known `node_id`. |
| `ast_graph` | `ckg_ast_graph` | Inspect repository/directory/file/symbol structure. |
| `dependency_graph` | `ckg_dependency_graph` | Inspect import/dependency relationships. |
| `call_graph` | `ckg_call_graph` | Inspect caller/callee relationships. |
| `product_flow_graph` | `ckg_product_flow_graph` | Inspect endpoint/handler/product-flow relationships. |
| `test_graph` | `ckg_test_graph` | Inspect test nodes and test-to-code edges. |
| `semantic_summaries` | `ckg_semantic_summaries` | Retrieve doc-comment/signature summaries. |

Common capability mapping:

| Client need | CKG tool/resource |
|---|---|
| Text search | `search`, `grep` |
| File path discovery | `glob` |
| File read | `read` |
| Workspace symbol search | `workspace_symbols`, `search` |
| Document symbols | `document_symbols` |
| Definition lookup | `definition`, `definition_at` |
| References | `references`, `references_at`, `call_graph` |
| Incoming/outgoing calls | `call_hierarchy`, `call_hierarchy_at`, `call_graph` |
| Hover-like summaries | `semantic_summaries` |
| Task context | `task_context` |
| Repo/file/node resources | MCP resources |

Recommended MCP client usage:

1. Call `ckg_status`.
2. If `needs_index` is `true`, call `ckg_index`, or call the next retrieval tool
   with `auto_index: true`.
3. Call `ckg_task_context` with the user task.
4. Use `ckg_search`, `ckg_grep`, or `ckg_glob` for follow-up discovery.
5. Use graph-specific tools when needed: `ckg_call_graph`, `ckg_dependency_graph`,
   `ckg_product_flow_graph`, `ckg_test_graph`.
6. Use `ckg_read` for targeted line-range file reads after graph/search narrows the path.
7. Use MCP resources for attachable context when useful:
   `list_mcp_resources`, `list_mcp_resource_templates`, `read_mcp_resource`.

## Tools

### `ckg_index`

Indexes the configured repository.

Input:

```json
{}
```

Output:

```json
{
  "repo_id": 1,
  "scanned": 3,
  "indexed": 1,
  "skipped_unchanged": 0,
  "deleted": 0,
  "db_path": "/path/to/repo/.ckg/ckg.sqlite"
}
```

Notes:
- Uses `.ckg/git` internal snapshot for delta detection.
- Creates/updates `.ckg/ckg.sqlite`.
- Does not open a port.

### `ckg_search`

Searches indexed files, symbols, summaries, and chunks.

Input:

```json
{
  "query": "avatar upload",
  "limit": 20
}
```

Output:

```json
{
  "hits": [
    {
      "kind": "node",
      "ref_id": 5,
      "file_id": 1,
      "node_id": 5,
      "path": "src/avatar.ts",
      "name": "saveAvatar",
      "snippet": "function saveAvatar() { return upload(); }",
      "score": 1.0
    }
  ]
}
```

Use this when the agent needs quick discovery before requesting a fuller context pack.

### `ckg_status`

Checks whether the local index is stale without updating it.

Output:

```json
{
  "repo_id": 1,
  "db_path": "/repo/.ckg/ckg.sqlite",
  "indexed_files": 120,
  "scan_mode": "internal_git_delta",
  "scanned": 3,
  "needs_index": true,
  "changed_files": ["src/a.ts", "src/new.ts"],
  "new_files": ["src/new.ts"],
  "modified_files": ["src/a.ts"],
  "deleted_files": ["src/old.ts"]
}
```

Use this before `ckg_task_context` when the caller wants explicit control over
whether to run `ckg_index`. Retrieval tools still support `auto_index: true`.

### `ckg_task_context`

Builds a task-focused context pack.

Input:

```json
{
  "task": "Fix bug: user cannot upload avatar",
  "max_tokens": 12000,
  "hops": 2,
  "response_mode": "brief"
}
```

`max_tokens` budgets the whole task-context response, not only
`context_pack`. CKG uses an approximate `1 token ~= 4 chars` budget, keeps the
usable context pack as the primary payload, and aggressively limits
`relevant_files`, `relevant_symbols`, `suggested_tests`, `subgraph.nodes`, and
`subgraph.edges`. In `--compact` MCP mode, use graph tools such as
`ast_graph`, `dependency_graph`, or `call_graph` when a full raw graph is needed.
For MCP, `response_mode` defaults to `brief` and returns a small shape: `query`,
`context`, `files`, `symbols`, `tests`, and a graph count plus sample edges. Use
`response_mode: "normal"` only when raw task-context fields are needed.

Output:

```json
{
  "query": "Fix bug: user cannot upload avatar",
  "context": "## File: ...",
  "files": ["src/avatar.ts"],
  "symbols": [{ "name": "uploadAvatar", "path": "src/avatar.ts" }],
  "tests": ["src/avatar.test.ts"],
  "graph": {
    "nodes": 3,
    "edges": 2,
    "sample_edges": ["route -REFERENCES-> uploadAvatar"]
  }
}
```

Use this as the primary entrypoint for agent context retrieval.

### `grep`

Index-aware regex grep over indexed files. Set `regex` to `false` for substring matching.

Input:

```json
{
  "query": "uploadAvatar",
  "path_glob": "src/*.ts",
  "case_sensitive": false,
  "regex": true,
  "limit": 100
}
```

Prefixed tool name when server name is `ckg`: `ckg_grep`.

Current limitation: this searches indexed text files only, not arbitrary filesystem paths.

### `glob`

Index-aware file path glob.

Input:

```json
{
  "pattern": "src/**/*.ts",
  "limit": 200
}
```

Prefixed tool name when server name is `ckg`: `ckg_glob`.

### `read`

Reads an indexed repo file with optional line range.

Input:

```json
{
  "path": "src/avatar.ts",
  "offset": 10,
  "limit": 80,
  "line_numbers": true
}
```

Prefixed tool name when server name is `ckg`: `ckg_read`.

If the file is inside the repo but not indexed yet, `read` falls back to a safe
repo-local filesystem read. Most retrieval tools accept `"auto_index": false`;
the default is `true`.

### LSP-like tools

These tools are best-effort lookups from the local index, not live language-server results.

| MCP tool | Prefixed tool name with server `ckg` | Input |
|---|---|---|
| `workspace_symbols` | `ckg_workspace_symbols` | `{ "query": "Avatar", "limit": 100 }` |
| `document_symbols` | `ckg_document_symbols` | `{ "path": "src/avatar.ts" }` |
| `definition` | `ckg_definition` | `{ "query": "saveAvatar", "limit": 20 }` |
| `definition_at` | `ckg_definition_at` | `{ "path": "src/avatar.ts", "line": 10, "character": 5 }` |
| `references` | `ckg_references` | `{ "node_id": 42, "limit": 200 }` |
| `references_at` | `ckg_references_at` | `{ "path": "src/avatar.ts", "line": 10, "character": 5 }` |
| `call_hierarchy` | `ckg_call_hierarchy` | `{ "node_id": 42, "direction": "both", "limit": 200 }` |
| `call_hierarchy_at` | `ckg_call_hierarchy_at` | `{ "path": "src/avatar.ts", "line": 10, "character": 5, "direction": "both" }` |
| `imports` | `ckg_imports` | `{ "node_id": 42, "limit": 200 }` |
| `dependents` | `ckg_dependents` | `{ "node_id": 42, "limit": 200 }` |
| `suggested_tests` | `ckg_suggested_tests` | `{ "task": "Fix avatar upload", "limit": 20 }` |

### MCP resources

CKG exposes MCP resources in addition to tools.

Static resources:

- `ckg://repo/summary`
- `ckg://graphs/ast`
- `ckg://graphs/dependency`
- `ckg://graphs/call`
- `ckg://graphs/product-flow`
- `ckg://graphs/test`
- `ckg://summaries/semantic`

Resource templates:

- `ckg://files/{path}`
- `ckg://nodes/{id}`

Use `list_mcp_resources`, `list_mcp_resource_templates`, and
`read_mcp_resource` with the MCP server name, for example `ckg`.

### `ckg_ast_graph`

Returns the symbol-level AST graph.

Input:

```json
{
  "limit": 500
}
```

Output shape:

```json
{
  "nodes": [],
  "edges": []
}
```

Includes:
- `CONTAINS`: repository/directory/file hierarchy.
- `DEFINES`: file to symbol/endpoint.

Current limitation: this is symbol-level AST, not full expression/statement AST.

### `ckg_dependency_graph`

Returns dependency/import graph.

Input:

```json
{
  "limit": 500
}
```

Output shape:

```json
{
  "nodes": [],
  "edges": []
}
```

Includes:
- import symbol nodes such as `import:./upload`
- resolved local file imports when possible
- relative JS/TS imports
- `tsconfig.json` `baseUrl` and `paths`
- named and namespace imports

### `ckg_call_graph`

Returns caller/callee graph.

Input:

```json
{
  "limit": 500
}
```

Output shape:

```json
{
  "nodes": [],
  "edges": []
}
```

Includes:
- in-file calls
- cross-file calls through named imports
- cross-file calls through namespace imports
- cross-file calls through resolved tsconfig aliases

Current limitation: heuristic name matching, not type-aware method dispatch.

### `ckg_product_flow_graph`

Returns product-flow entrypoints and handler references.

Input:

```json
{
  "limit": 500
}
```

Output:

```json
{
  "entrypoints": [],
  "subgraph": {
    "nodes": [],
    "edges": []
  }
}
```

Includes:
- `Endpoint` nodes for route-like calls such as `router.post('/avatar', handler)`.
- `REFERENCES` edges from endpoint to local or imported handler when resolved.

Current limitation: does not yet build full endpoint -> service -> repository -> database chain.

### `ckg_test_graph`

Returns test nodes and test-to-code edges.

Input:

```json
{
  "limit": 500
}
```

Output:

```json
{
  "tests": [],
  "subgraph": {
    "nodes": [],
    "edges": []
  }
}
```

Includes:
- `Test` nodes detected by file/name heuristics.
- `TESTS` edges when calls resolve to local/imported symbols.

Current limitation: no coverage import and no deep framework semantics yet.

### `ckg_semantic_summaries`

Returns indexed semantic summaries.

Input:

```json
{
  "limit": 200
}
```

Output:

```json
{
  "summaries": []
}
```

Includes:
- leading comments/doc comments when available
- signature fallback
- file/source labels

Current limitation: no LLM-generated summaries yet.

### `ckg_neighborhood`

Returns graph neighborhood around a node.

Input:

```json
{
  "node_id": 5,
  "hops": 2
}
```

Output:

```json
{
  "nodes": [],
  "edges": []
}
```

Use this after `ckg_search` or another graph tool returns a specific `node_id`.

### `ckg_file`

Returns current file content and metadata by repository-relative path.

Input:

```json
{
  "path": "src/avatar.ts"
}
```

Output:

```json
{
  "path": "src/avatar.ts",
  "language": "typescript",
  "size": 1234,
  "hash": "...",
  "content": "..."
}
```

Use this for targeted file reads. Prefer graph/context tools first to avoid dumping too much code.

## Example JSON-RPC Calls

Initialize:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

List tools:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
```

Call task context:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "tools/call",
  "params": {
    "name": "ckg_task_context",
    "arguments": {
      "task": "Fix avatar upload",
      "max_tokens": 12000,
      "hops": 2
    }
  }
}
```

Call graph:

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "tools/call",
  "params": {
    "name": "ckg_call_graph",
    "arguments": {
      "limit": 500
    }
  }
}
```
