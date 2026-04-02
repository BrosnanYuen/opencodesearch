# opencodesearch

Asynchronous Rust MCP code search server with 4 isolated processes:
- orchestrator state machine
- background ingestor
- MCP stdio server
- git watchdog

## Required crates used
- `opencodesearchparser`
- `qdrant-client`
- `ollama-rs`
- `rmcp`

## Run services
```bash
docker compose up -d
```

## Run orchestrator
```bash
cargo run -- orchestrator --config config.json
```

## Run tests
```bash
cargo test
cargo test -- --ignored
```
