use anyhow::{Context, Result};
use ollama_rs::Ollama;
use ollama_rs::generation::embeddings::request::{EmbeddingsInput, GenerateEmbeddingsRequest};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::chunking::chunk_file;
use crate::config::AppConfig;
use crate::qdrant_store::QdrantStore;
use crate::quickwit::QuickwitStore;
use crate::types::CodeChunk;

/// Shared indexing runtime used by initial ingest and incremental updates.
#[derive(Clone)]
pub struct IndexingRuntime {
    pub config: AppConfig,
    pub qdrant: QdrantStore,
    pub quickwit: QuickwitStore,
    ollama: Ollama,
}

impl IndexingRuntime {
    /// Construct runtime clients from application config.
    pub fn from_config(config: AppConfig) -> Result<Self> {
        let ollama = Ollama::try_new(config.ollama.server_url.clone())
            .context("invalid ollama server url")?;

        let qdrant = QdrantStore::new(
            &config.qdrant.server_url,
            config.qdrant.api_key.as_deref(),
            &config.quickwit.quickwit_index_id,
        )?;

        let quickwit = QuickwitStore::new(
            config.quickwit.quickwit_url.clone(),
            config.quickwit.quickwit_index_id.clone(),
        );

        Ok(Self {
            config,
            qdrant,
            quickwit,
            ollama,
        })
    }

    /// Run a full-codebase traversal and index all supported text files.
    pub async fn index_entire_codebase(&self) -> Result<()> {
        let all_files = collect_candidate_files(&self.config.codebase.directory_path)?;
        self.index_files(&all_files).await
    }

    /// Index a specific list of files.
    pub async fn index_files(&self, files: &[PathBuf]) -> Result<()> {
        // Stage A: parse/chunk every file.
        let mut all_chunks = Vec::new();
        for file in files {
            let chunks = chunk_file(file, self.config.ollama.context_size)
                .with_context(|| format!("failed chunking file {}", file.display()))?;
            for chunk in chunks {
                all_chunks.push(chunk);
            }
        }

        if all_chunks.is_empty() {
            return Ok(());
        }

        // Stage B: embed chunks through ollama-rs API.
        let embeddings = self.generate_embeddings(&all_chunks).await?;
        let vector_size = embeddings.first().map(|v| v.len() as u64).unwrap_or(1024);

        // Stage C: persist vectors and raw text indexes.
        self.qdrant.ensure_collection(vector_size).await?;
        self.qdrant.upsert_chunks(&all_chunks, &embeddings).await?;
        self.quickwit.index_chunks(&all_chunks).await?;

        Ok(())
    }

    /// Produce one embedding per chunk using ollama `generate_embeddings`.
    pub async fn generate_embeddings(&self, chunks: &[CodeChunk]) -> Result<Vec<Vec<f32>>> {
        let mut output = Vec::with_capacity(chunks.len());

        for chunk in chunks {
            let request = GenerateEmbeddingsRequest::new(
                self.config.ollama.embedding_model.clone(),
                EmbeddingsInput::Single(chunk.snippet.clone()),
            );

            let response = self
                .ollama
                .generate_embeddings(request)
                .await
                .with_context(|| {
                    format!(
                        "ollama embedding request failed for {}:{}-{}",
                        chunk.path, chunk.start_line, chunk.end_line
                    )
                })?;

            if let Some(first) = response.embeddings.first() {
                output.push(first.clone());
            }
        }

        Ok(output)
    }

    /// Embed free-text query for semantic search.
    pub async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let request = GenerateEmbeddingsRequest::new(
            self.config.ollama.embedding_model.clone(),
            EmbeddingsInput::Single(query.to_string()),
        );
        let response = self
            .ollama
            .generate_embeddings(request)
            .await
            .context("failed embedding query with ollama")?;

        if let Some(vec) = response.embeddings.into_iter().next() {
            Ok(vec)
        } else {
            anyhow::bail!("ollama returned no embedding vector")
        }
    }

    /// Remove stale paths from both vector and keyword storage.
    pub async fn delete_paths(&self, paths: &[String]) -> Result<()> {
        self.qdrant.delete_paths(paths).await?;
        self.quickwit.delete_paths(paths).await?;
        Ok(())
    }
}

/// Collect candidate source files while skipping VCS and binary-like files.
pub fn collect_candidate_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(root).follow_links(false).into_iter() {
        let entry = match entry {
            Ok(item) => item,
            Err(_) => continue,
        };

        let path = entry.path();
        if entry.file_type().is_dir() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if [".git", "node_modules", "target", ".venv", "dist"].contains(&name) {
                continue;
            }
            continue;
        }

        // Only index known text-like source files for predictable behavior.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if ["rs", "py", "js", "ts", "c", "cpp", "h", "hpp", "java", "go"].contains(&ext.as_str()) {
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_source_files_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.py"), "def x():\n    return 1").expect("write py");
        std::fs::write(dir.path().join("b.txt"), "ignored").expect("write txt");

        let files = collect_candidate_files(dir.path()).expect("collect must work");
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.py"));
    }
}
