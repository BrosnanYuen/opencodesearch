//! Core library for the `opencodesearch` MCP code search server.

// Keep modules small and focused so each process can reuse the same logic.
pub mod chunking;
pub mod config;
pub mod indexing;
pub mod mcp;
pub mod orchestrator;
pub mod qdrant_store;
pub mod quickwit;
pub mod types;
pub mod watchdog;
