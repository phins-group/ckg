#!/usr/bin/env bash
# Copyright (c) 2026 PHINs Group
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

FILES="${FILES:-1000}"
ROOT="${ROOT:-$(mktemp -d "${TMPDIR:-/tmp}/ckg-bench.XXXXXX")}"
REPO="$ROOT/repo"
CKG_BIN="${CKG_BIN:-$(pwd)/target/release/ckg}"

now_secs() {
  perl -MTime::HiRes=time -e 'printf "%.6f\n", time'
}

elapsed_ms() {
  perl -e 'printf "%.0f", (($ARGV[1] - $ARGV[0]) * 1000)' "$1" "$2"
}

bytes() {
  wc -c < "$1" | tr -d ' '
}

run_timed() {
  local label="$1"
  shift
  local start end
  start="$(now_secs)"
  "$@" > "$ROOT/$label.out" 2> "$ROOT/$label.err"
  end="$(now_secs)"
  elapsed_ms "$start" "$end"
}

run_timed_to_file() {
  local label="$1"
  local outfile="$2"
  shift 2
  local start end
  start="$(now_secs)"
  "$@" > "$outfile" 2> "$ROOT/$label.err"
  end="$(now_secs)"
  elapsed_ms "$start" "$end"
}

generate_fixture() {
  rm -rf "$REPO"
  mkdir -p "$REPO/src/features" "$REPO/src/routes" "$REPO/src/tests"

  cat > "$REPO/package.json" <<'JSON'
{
  "scripts": {
    "test": "vitest run"
  }
}
JSON

  cat > "$REPO/tsconfig.json" <<'JSON'
{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@features/*": ["src/features/*"]
    }
  }
}
JSON

  cat > "$REPO/src/shared.ts" <<'TS'
export function normalizeAvatarName(value: string): string {
  return value.trim().toLowerCase();
}

export function auditFeature(name: string): string {
  return `audit:${name}`;
}
TS

  local i n route_file test_file
  i=1
  while [ "$i" -le "$FILES" ]; do
    n="$(printf "%04d" "$i")"
    cat > "$REPO/src/features/feature$n.ts" <<TS
import { normalizeAvatarName, auditFeature } from "../shared";

export type Feature${n}Payload = {
  avatarName: string;
  userId: string;
};

export function feature${n}Upload(payload: Feature${n}Payload): string {
  const normalized = normalizeAvatarName(payload.avatarName);
  return auditFeature("feature$n:" + normalized + ":" + payload.userId);
}

export class Feature${n}Service {
  uploadAvatar(payload: Feature${n}Payload): string {
    return feature${n}Upload(payload);
  }
}
TS

    if [ $((i % 20)) -eq 0 ]; then
      route_file="$REPO/src/routes/feature$n.route.ts"
      cat > "$route_file" <<TS
import { feature${n}Upload } from "../features/feature$n";

router.post("/api/features/$n/avatar", feature${n}Upload);
TS
    fi

    if [ $((i % 25)) -eq 0 ]; then
      test_file="$REPO/src/tests/feature$n.test.ts"
      cat > "$test_file" <<TS
import { feature${n}Upload } from "../features/feature$n";

test("feature$n upload uses normalized avatar name", () => {
  feature${n}Upload({ avatarName: " Avatar ", userId: "u$n" });
});
TS
    fi

    i=$((i + 1))
  done
}

if [ "${CKG_SKIP_BUILD:-0}" != "1" ]; then
  cargo build --release > "$ROOT/build.out" 2> "$ROOT/build.err"
fi

generate_fixture

cold_ms="$(run_timed cold-index "$CKG_BIN" index "$REPO" --full)"
noop_ms="$(run_timed noop-index "$CKG_BIN" index "$REPO")"

target_file="$REPO/src/features/feature0500.ts"
if [ "$FILES" -lt 500 ]; then
  target_file="$REPO/src/features/feature$(printf "%04d" "$FILES").ts"
fi
printf '\nexport const benchmarkMutation = "changed";\n' >> "$target_file"

one_file_ms="$(run_timed one-file-index "$CKG_BIN" index "$REPO")"
status_ms="$(run_timed status "$CKG_BIN" mcp "$REPO" --compact <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}
EOF
)"

search_json="$ROOT/search.json"
task_json="$ROOT/task-context.json"
search_ms="$(run_timed_to_file search "$search_json" "$CKG_BIN" search "feature0500Upload" --repo-path "$REPO" --limit 10 --json)"
task_ms="$(run_timed_to_file task-context "$task_json" "$CKG_BIN" task-context "$REPO" "Fix feature0500 upload bug" --max-tokens 800 --hops 2 --json)"

db_file="$REPO/.ckg/ckg.sqlite"
db_bytes="$(bytes "$db_file")"
search_bytes="$(bytes "$search_json")"
task_bytes="$(bytes "$task_json")"
total_files="$(find "$REPO" -type f | grep -v '/.ckg/' | wc -l | tr -d ' ')"
sqlite_mb="$(perl -e 'printf "%.2f", $ARGV[0] / 1024 / 1024' "$db_bytes")"

cat <<MD
## Benchmark Result

Measured with \`scripts/benchmark.sh\`.

Environment:

- Machine: $(uname -s) $(uname -m) $(uname -r)
- Rust: $(rustc -V)
- ckg binary: $CKG_BIN
- Fixture path: $REPO

Fixture:

- Generated TypeScript feature files: $FILES
- Total source/config files: $total_files
- Route files: $((FILES / 20))
- Test files: $((FILES / 25))

| Fixture | Cold index | No-op incremental | 1-file incremental | Status check | Search JSON | task_context 800 | SQLite size |
|---|---:|---:|---:|---:|---:|---:|---:|
| $FILES TS files | ${cold_ms} ms | ${noop_ms} ms | ${one_file_ms} ms | ${status_ms} ms | ${search_ms} ms / ${search_bytes} bytes | ${task_ms} ms / ${task_bytes} bytes | ${sqlite_mb} MB |

Notes:

- Cold index runs \`ckg index --full\`.
- No-op incremental runs \`ckg index\` immediately after cold index.
- 1-file incremental appends one line to one TypeScript file, then runs \`ckg index\`.
- Status check calls MCP \`status\` through stdio and does not update the index.
- Results are machine-dependent and intended as a reproducible sample, not a universal performance guarantee.
MD

if [ "${KEEP_BENCH:-0}" != "1" ]; then
  rm -rf "$ROOT"
fi
