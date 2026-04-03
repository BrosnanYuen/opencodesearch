use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The top-level application configuration loaded from `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub codebase: CodebaseConfig,
    pub ollama: OllamaConfig,
    pub qdrant: QdrantConfig,
    pub quickwit: QuickwitConfig,
}

/// Codebase and orchestration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseConfig {
    pub directory_path: PathBuf,
    pub git_branch: String,
    pub commit_threshold: usize,
    pub mcp_server: String,
    pub background_indexing_threads: usize,
}

/// Ollama embedding settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    pub server_url: String,
    pub embedding_model: String,
    pub context_size: usize,
}

/// Qdrant connection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    pub server_url: String,
    pub api_key: Option<String>,
}

/// Quickwit connection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickwitConfig {
    pub quickwit_url: String,
    pub quickwit_index_id: String,
}

impl AppConfig {
    /// Load config from a JSON file on disk.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        // Read the raw file bytes first for better error context.
        let raw = std::fs::read_to_string(path_ref)
            .with_context(|| format!("failed to read config file at {}", path_ref.display()))?;
        // Parse strict JSON into the typed config structure.
        let cfg = serde_json::from_str::<Self>(&raw)
            .with_context(|| format!("invalid config json at {}", path_ref.display()))?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_expected_schema() {
        // Keep this sample aligned with AGENT.md required config schema.
        let json = r#"{
            "codebase": {
                "directory_path": "/tmp/repo",
                "git_branch": "main",
                "commit_threshold": 50,
                "mcp_server": "stdio",
                "background_indexing_threads": 2
            },
            "ollama": {
                "server_url": "http://localhost:11434",
                "embedding_model": "qwen3-embedding:0.6b",
                "context_size": 5000
            },
            "qdrant": {
                "server_url": "http://localhost:6334",
                "api_key": null
            },
            "quickwit": {
                "quickwit_url": "http://localhost:7280",
                "quickwit_index_id": "opencodesearch-code-chunks"
            }
        }"#;

        let parsed = serde_json::from_str::<AppConfig>(json).expect("config must parse");
        assert_eq!(parsed.codebase.commit_threshold, 50);
        assert_eq!(parsed.codebase.background_indexing_threads, 2);
        assert_eq!(parsed.ollama.embedding_model, "qwen3-embedding:0.6b");
    }
}
