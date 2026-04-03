# opencodesearch

`opencodesearch` is an asynchronous Rust code search system with a Model Context Protocol (MCP) server.
It indexes large repositories into vector + keyword backends and serves search results through MCP tools.

## Features
- Fully async runtime (`tokio`)
- 4 isolated processes:
  - orchestrator (state machine + supervision)
  - background ingestor
  - MCP server process
  - git watchdog process
- Required crates integrated and used in runtime code:
  - `opencodesearchparser`
  - `qdrant-client`
  - `ollama-rs`
  - `rmcp`
- Hybrid retrieval:
  - semantic search (Qdrant vectors)
  - keyword search (Quickwit HTTP + local shadow fallback)
- MCP server compatible with MCP clients using streamable HTTP and stdio transports

## Architecture
State machine in orchestrator:
- `SPINUP`: load `config.json`
- `NORMAL`: run `ingestor` + `mcp` + `watchdog`
- `UPDATE`: keep `watchdog`, stop `ingestor` + `mcp` during update window
- `CLOSING`: stop all children gracefully

Update flow:
- watchdog tracks git commits since last sync
- when threshold (`commit_threshold`) is reached:
  - send `UPDATE_START` to orchestrator
  - pull + compute changed/deleted files
  - remove stale docs
  - reindex changed files
  - send `UPDATE_END`

## Requirements
- Rust stable toolchain
- Docker + Docker Compose
- Local network access to:
  - Ollama (`11434`)
  - Qdrant (`6333` HTTP, `6334` gRPC)
  - Quickwit (`7280`)

## Configuration
`config.json` schema:

```json
{
  "codebase": {
    "directory_path": "/path/to/massive/repo",
    "git_branch": "main",
    "commit_threshold": 50,
    "mcp_server_name": "My cool codebase",
    "mcp_server_url": "http://localhost:9443",
    "background_indexing_threads": 2
  },
  "ollama": {
    "server_url": "http://localhost:11434",
    "embedding_model": "qwen3-embedding:0.6b",
    "context_size": 2000
  },
  "qdrant": {
    "server_url": "http://localhost:6334",
    "collection_name": "opencodesearch-code-chunks",
    "api_key": null
  },
  "quickwit": {
    "quickwit_url": "http://localhost:7280",
    "quickwit_index_id": "opencodesearch-code-chunks"
  }
}
```

Important:
- `qdrant.server_url` should target the gRPC endpoint port (`6334`) for `qdrant-client`.
- `quickwit.quickwit_url` should target HTTP (`7280`).

## Start Backend Services
Run all local dependencies:

```bash
docker compose up -d
```

Check containers:

```bash
docker ps
```

## Running the System

### 1) Orchestrator mode (recommended)
Starts and supervises all child processes.

```bash
cargo run -- orchestrator --config config.json
```

### 2) Individual process modes
You can run each process directly for debugging.

Ingestor:
```bash
cargo run -- ingestor --config config.json
```

MCP server:
```bash
cargo run -- mcp --config config.json
```

MCP server over stdio (for local MCP clients):
```bash
cargo run -- mcp-stdio --config config.json
```

Watchdog (requires orchestrator IPC env):
```bash
OPENCODESEARCH_IPC_SOCKET=/tmp/opencodesearch.sock cargo run -- watchdog --config config.json
```

## MCP Server Usage
The MCP server supports:
- streamable HTTP via `cargo run -- mcp --config config.json`
- stdio via `cargo run -- mcp-stdio --config config.json`

Implemented MCP tool:
- `search_code`
  - input:
    - `query: string`
    - `limit?: number` (default 8, max 50)
  - output (structured JSON): array of objects with
    - `snippet`
    - `path`
    - `start_line`
    - `end_line`
    - `score`
    - `source`

### Example tool input

```json
{
  "query": "which function changes obj variable",
  "limit": 5
}
```

### Result shape

```json
{
  "hits": [
    {
      "path": "/repo/module.py",
      "snippet": "def mutate(obj): ...",
      "start_line": 10,
      "end_line": 22,
      "score": 0.92,
      "source": "qdrant"
    }
  ]
}
```

## Using With MCP Clients
This server supports both:
- streamable HTTP (`cargo run -- mcp --config config.json`)
- local stdio (`cargo run -- mcp-stdio --config config.json`)

### OpenAI Codex
Codex supports both stdio and streamable HTTP MCP servers.

Stdio (CLI):
```bash
codex mcp add opencodesearch -- \
  cargo run --quiet --manifest-path /home/brosnan/opencodesearch/Cargo.toml -- \
  mcp-stdio --config /home/brosnan/opencodesearch/config.json
```

Remote HTTP (`~/.codex/config.toml` or `.codex/config.toml`):
```toml
[mcp_servers.opencodesearch]
url = "http://localhost:9443/"
```

Then verify:
```bash
codex mcp list
```

### OpenCode
OpenCode config uses the `mcp` section in `opencode.json` (or `opencode.jsonc`).

Remote HTTP:
```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "opencodesearch": {
      "type": "remote",
      "url": "http://localhost:9443/",
      "enabled": true
    }
  }
}
```

Local stdio:
```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "opencodesearch": {
      "type": "local",
      "command": [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        "/home/brosnan/opencodesearch/Cargo.toml",
        "--",
        "mcp-stdio",
        "--config",
        "/home/brosnan/opencodesearch/config.json"
      ],
      "enabled": true
    }
  }
}
```

### Claude Code
Claude Code supports HTTP, SSE, and stdio MCP transports.

Remote HTTP:
```bash
claude mcp add --transport http opencodesearch http://localhost:9443/
```

Local stdio:
```bash
claude mcp add --transport stdio opencodesearch -- \
  cargo run --quiet --manifest-path /home/brosnan/opencodesearch/Cargo.toml -- \
  mcp-stdio --config /home/brosnan/opencodesearch/config.json
```

Then verify:
```bash
claude mcp list
```

### TLS / HTTPS Notes
- Default local config uses `http://localhost:9443`.
- For `https://...`, provide a certificate trusted by your MCP client.
- TLS cert and key defaults:
  - `certs/localhost-cert.pem`
  - `certs/localhost-key.pem`
- Override TLS file paths with:
  - `OPENCODESEARCH_TLS_CERT_PATH`
  - `OPENCODESEARCH_TLS_KEY_PATH`
- For Codex specifically, you can provide a custom CA bundle with `CODEX_CA_CERTIFICATE`.

References:
- Codex MCP docs: https://developers.openai.com/codex/mcp
- OpenCode MCP docs: https://opencode.ai/docs/mcp-servers/
- Claude Code MCP docs: https://code.claude.com/docs/en/mcp

### Quick curl test
Use the included script:

```bash
./test_mcp_curl.sh
```

Optional:
- `MCP_URL=https://localhost:9443/ MCP_INSECURE=1 ./test_mcp_curl.sh`

The script performs the required MCP HTTP handshake steps:
1. `initialize`
2. extract `mcp-session-id` from response headers
3. send `notifications/initialized` with the same `mcp-session-id`
4. call `tools/call` for `search_code`

### Manual curl sequence
Initialize and capture session id:

```bash
curl -sS -D headers.txt http://localhost:9443/ \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"curl-test","version":"1.0"}}}'
```

Send initialized notification:

```bash
SESSION_ID="$(awk 'tolower($1)=="mcp-session-id:"{print $2}' headers.txt | tr -d '\r' | tail -n 1)"
curl -sS http://localhost:9443/ \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "mcp-session-id: ${SESSION_ID}" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}'
```

Call the MCP tool:

```bash
curl -N http://localhost:9443/ \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "mcp-session-id: ${SESSION_ID}" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_code","arguments":{"query":"which function mutates obj","limit":5}}}'
```

## Rust API Documentation
The crate exposes reusable modules for embedding, indexing, MCP serving, and process control.

### Modules
- `config`: parse typed app config (`AppConfig`)
- `chunking`: parse/split source files into chunks (`chunk_file`)
- `indexing`: indexing runtime (`IndexingRuntime`)
- `qdrant_store`: vector storage + semantic query (`QdrantStore`)
- `quickwit`: keyword storage/query (`QuickwitStore`)
- `mcp`: MCP server type (`OpenCodeSearchMcpServer`)
- `watchdog`: git update monitor (`WatchdogProcess`)
- `orchestrator`: multi-process supervisor (`Orchestrator`)

### Minimal Rust indexing example

```rust
use opencodesearch::config::AppConfig;
use opencodesearch::indexing::IndexingRuntime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_path("config.json")?;
    let runtime = IndexingRuntime::from_config(config)?;

    runtime.index_entire_codebase().await?;
    Ok(())
}
```

### Minimal Rust semantic search example

```rust
use opencodesearch::config::AppConfig;
use opencodesearch::indexing::IndexingRuntime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_path("config.json")?;
    let runtime = IndexingRuntime::from_config(config)?;

    let query_vec = runtime.embed_query("where is object mutated") .await?;
    let hits = runtime.qdrant.semantic_search(query_vec, 5).await?;

    for hit in hits {
        println!("{}:{}-{}", hit.path, hit.start_line, hit.end_line);
    }

    Ok(())
}
```

### Minimal Rust MCP server embedding

```rust
use opencodesearch::config::AppConfig;
use opencodesearch::indexing::IndexingRuntime;
use opencodesearch::mcp::OpenCodeSearchMcpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_path("config.json")?;
    let runtime = IndexingRuntime::from_config(config)?;
    OpenCodeSearchMcpServer::new(runtime)
        .run_streamable_http("http://localhost:9443")
        .await
}
```

## Testing
### Standard tests
```bash
cargo test
```

### Live container integration tests
Requires running Docker services and local git:

```bash
cargo test -- --ignored
```

Current ignored integration tests validate:
- Ollama connectivity
- Quickwit + Qdrant connectivity
- full indexing flow on generated Python project
- retrieval through MCP search path with non-exact query phrasing
- 100-commit refactor scenario for watchdog threshold behavior

## Troubleshooting
- Quickwit health endpoint: use `http://localhost:7280/health/livez`
- If embeddings fail, confirm Ollama model availability:
  - `qwen3-embedding:0.6b`
- Qdrant client requires gRPC port (`6334`) in config
- If integration tests fail on startup race, rerun after a short container warmup
