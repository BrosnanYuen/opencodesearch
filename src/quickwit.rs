use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::types::{CodeChunk, SearchHit};

/// Thin quickwit HTTP client with a local shadow index fallback.
#[derive(Clone)]
pub struct QuickwitStore {
    pub base_url: String,
    pub index_id: String,
    client: reqwest::Client,
    shadow_path: std::path::PathBuf,
}

impl QuickwitStore {
    /// Create a new quickwit client.
    pub fn new(base_url: impl Into<String>, index_id: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let index_id = index_id.into();
        let shadow_path = std::path::PathBuf::from(".opencodesearch").join("quickwit_shadow.jsonl");

        Self {
            base_url,
            index_id,
            client: reqwest::Client::new(),
            shadow_path,
        }
    }

    /// Basic availability check for the Quickwit service.
    pub async fn health_check(&self) -> Result<()> {
        let url = format!("{}/health/livez", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("quickwit health check request failed")?;

        if response.status() == StatusCode::OK {
            Ok(())
        } else {
            anyhow::bail!("quickwit unhealthy: status {}", response.status())
        }
    }

    /// Persist chunks in a local shadow index and attempt remote ingest.
    pub async fn index_chunks(&self, chunks: &[CodeChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        // Ensure local state directory exists.
        if let Some(parent) = self.shadow_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        // Append chunks as JSONL to guarantee keyword search availability.
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.shadow_path)
            .await
            .with_context(|| format!("failed to open {}", self.shadow_path.display()))?;

        for chunk in chunks {
            let line = serde_json::to_string(chunk)
                .context("failed serializing chunk for shadow index")?;
            file.write_all(line.as_bytes())
                .await
                .context("failed writing shadow index line")?;
            file.write_all(b"\n")
                .await
                .context("failed writing shadow index newline")?;
        }

        // Best-effort remote ingest to quickwit endpoint.
        let ingest_url = format!(
            "{}/api/v1/{}/ingest",
            self.base_url.trim_end_matches('/'),
            self.index_id
        );

        let mut body = String::new();
        for chunk in chunks {
            let line = serde_json::json!({
                "path": chunk.path,
                "snippet": chunk.snippet,
                "start_line": chunk.start_line,
                "end_line": chunk.end_line,
            });
            body.push_str(&line.to_string());
            body.push('\n');
        }

        let _ = self
            .client
            .post(ingest_url)
            .header("content-type", "application/x-ndjson")
            .body(body)
            .send()
            .await;

        Ok(())
    }

    /// Simple keyword search across shadow index and optional quickwit backend.
    pub async fn keyword_search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();

        // Search local shadow index first for deterministic behavior in tests.
        if self.shadow_path.exists() {
            let content = tokio::fs::read_to_string(&self.shadow_path)
                .await
                .with_context(|| format!("failed reading {}", self.shadow_path.display()))?;

            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }

                let parsed = serde_json::from_str::<CodeChunk>(line);
                if let Ok(chunk) = parsed {
                    let hay = format!("{}\n{}", chunk.path, chunk.snippet).to_ascii_lowercase();
                    if hay.contains(&query.to_ascii_lowercase()) {
                        hits.push(SearchHit {
                            path: chunk.path,
                            snippet: chunk.snippet,
                            start_line: chunk.start_line as i64,
                            end_line: chunk.end_line as i64,
                            score: 1.0,
                            source: "quickwit-shadow".to_string(),
                        });
                    }
                }

                if hits.len() >= limit {
                    break;
                }
            }
        }

        // Try querying quickwit as best-effort and merge when available.
        let search_url = format!(
            "{}/api/v1/{}/search",
            self.base_url.trim_end_matches('/'),
            self.index_id
        );

        let request_body = serde_json::json!({
            "query": query,
            "max_hits": limit,
        });

        if let Ok(response) = self
            .client
            .post(search_url)
            .json(&request_body)
            .send()
            .await
        {
            if let Ok(json) = response.json::<Value>().await {
                if let Some(remote_hits) = json.get("hits").and_then(|v| v.as_array()) {
                    for item in remote_hits {
                        let path = item
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let snippet = item
                            .get("snippet")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let start_line = item
                            .get("start_line")
                            .and_then(|v| v.as_u64())
                            .unwrap_or_default() as usize;
                        let end_line = item
                            .get("end_line")
                            .and_then(|v| v.as_u64())
                            .unwrap_or_default() as usize;

                        hits.push(SearchHit {
                            path,
                            snippet,
                            start_line: start_line as i64,
                            end_line: end_line as i64,
                            score: 1.0,
                            source: "quickwit".to_string(),
                        });

                        if hits.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        Ok(hits)
    }

    /// Delete stale documents by rebuilding shadow index without removed paths.
    pub async fn delete_paths(&self, paths: &[String]) -> Result<()> {
        if !self.shadow_path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&self.shadow_path)
            .await
            .with_context(|| format!("failed reading {}", self.shadow_path.display()))?;

        let mut kept = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            if let Ok(chunk) = serde_json::from_str::<CodeChunk>(line) {
                if !paths.iter().any(|p| p == &chunk.path) {
                    kept.push(line.to_string());
                }
            }
        }

        let rewritten = if kept.is_empty() {
            String::new()
        } else {
            format!("{}\n", kept.join("\n"))
        };

        tokio::fs::write(&self.shadow_path, rewritten)
            .await
            .with_context(|| format!("failed writing {}", self.shadow_path.display()))?;

        Ok(())
    }

    /// Delete all stored code from local shadow index and quickwit index (best effort).
    pub async fn delete_all_code(&self) -> Result<()> {
        if self.shadow_path.exists() {
            tokio::fs::write(&self.shadow_path, "")
                .await
                .with_context(|| format!("failed clearing {}", self.shadow_path.display()))?;
        }

        // Best-effort quickwit clear: attempt index deletion.
        let delete_index_url = format!(
            "{}/api/v1/indexes/{}",
            self.base_url.trim_end_matches('/'),
            self.index_id
        );
        let _ = self.client.delete(delete_index_url).send().await;

        // Best-effort quickwit clear: attempt document delete-all endpoint.
        let delete_docs_url = format!(
            "{}/api/v1/{}/delete",
            self.base_url.trim_end_matches('/'),
            self.index_id
        );
        let _ = self
            .client
            .post(delete_docs_url)
            .json(&serde_json::json!({ "query": "*" }))
            .send()
            .await;

        Ok(())
    }
}
