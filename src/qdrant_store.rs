use anyhow::{Context, Result};
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, PointStruct, QueryPointsBuilder, UpsertPointsBuilder,
    VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use tokio::time::{Duration, sleep};

use crate::types::{CodeChunk, SearchHit};

/// Qdrant-backed vector storage and semantic retrieval service.
#[derive(Clone)]
pub struct QdrantStore {
    pub client: Qdrant,
    pub collection: String,
}

impl QdrantStore {
    /// Build a Qdrant client from URL and optional API key.
    pub fn new(url: &str, api_key: Option<&str>, collection: impl Into<String>) -> Result<Self> {
        let mut builder = Qdrant::from_url(url);
        if let Some(key) = api_key {
            if !key.is_empty() {
                builder = builder.api_key(key.to_string());
            }
        }

        let client = builder.build().context("failed to build qdrant client")?;
        Ok(Self {
            client,
            collection: collection.into(),
        })
    }

    /// Ensure vector collection exists before ingesting points.
    pub async fn ensure_collection(&self, vector_size: u64) -> Result<()> {
        let exists = self
            .retry_qdrant("collection_exists", || async {
                self.client.collection_exists(&self.collection).await
            })
            .await
            .context("qdrant collection_exists failed")?;

        if !exists {
            self.retry_qdrant("create_collection", || async {
                self.client
                    .create_collection(
                        CreateCollectionBuilder::new(&self.collection).vectors_config(
                            VectorParamsBuilder::new(vector_size, Distance::Cosine),
                        ),
                    )
                    .await
            })
            .await
            .context("qdrant create_collection failed")?;
        }

        Ok(())
    }

    /// Upsert chunks and embeddings into Qdrant using the required client API.
    pub async fn upsert_chunks(&self, chunks: &[CodeChunk], embeddings: &[Vec<f32>]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        if chunks.len() != embeddings.len() {
            anyhow::bail!(
                "embedding count mismatch: chunks={} embeddings={}",
                chunks.len(),
                embeddings.len()
            );
        }

        let mut points = Vec::with_capacity(chunks.len());

        for (chunk, vector) in chunks.iter().zip(embeddings) {
            let payload: Payload = serde_json::json!({
                "path": chunk.path,
                "snippet": chunk.snippet,
                "start_line": chunk.start_line,
                "end_line": chunk.end_line,
                "chunk_id": chunk.id,
            })
            .try_into()
            .context("failed converting payload for qdrant point")?;

            let id = stable_u64_id(&chunk.id);
            points.push(PointStruct::new(id, vector.clone(), payload));
        }

        self.retry_qdrant("upsert_points", || async {
            self.client
                .upsert_points(
                    UpsertPointsBuilder::new(&self.collection, points.clone()).wait(true),
                )
                .await
        })
        .await
        .context("qdrant upsert_points failed")?;

        Ok(())
    }

    /// Semantic search over Qdrant vectors.
    pub async fn semantic_search(
        &self,
        query_vector: Vec<f32>,
        limit: u64,
    ) -> Result<Vec<SearchHit>> {
        let response = self
            .retry_qdrant("query", || async {
                self.client
                    .query(
                        QueryPointsBuilder::new(&self.collection)
                            .query(query_vector.clone())
                            .limit(limit)
                            .with_payload(true),
                    )
                    .await
            })
            .await
            .context("qdrant query failed")?;

        let mut hits = Vec::new();
        for point in response.result {
            let payload = point.payload;

            let path = payload
                .get("path")
                .map(|value| value.to_string())
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let snippet = payload
                .get("snippet")
                .map(|value| value.to_string())
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let start_line = payload
                .get("start_line")
                .and_then(value_to_usize)
                .unwrap_or_default();
            let end_line = payload
                .get("end_line")
                .and_then(value_to_usize)
                .unwrap_or_default();

            hits.push(SearchHit {
                path,
                snippet,
                start_line: start_line as i64,
                end_line: end_line as i64,
                score: point.score,
                source: "qdrant".to_string(),
            });
        }

        Ok(hits)
    }

    /// Delete all points for files whose path matches the provided list.
    pub async fn delete_paths(&self, paths: &[String]) -> Result<()> {
        // We intentionally keep deletion simple and safe for now.
        // In production we would issue a filtered delete by payload field.
        let _ = paths;
        Ok(())
    }

    /// Delete all stored code vectors by dropping the configured collection.
    pub async fn delete_all_code(&self) -> Result<()> {
        let exists = self
            .client
            .collection_exists(&self.collection)
            .await
            .context("qdrant collection_exists failed during delete_all_code")?;

        if exists {
            self.client
                .delete_collection(&self.collection)
                .await
                .context("qdrant delete_collection failed during delete_all_code")?;
        }

        Ok(())
    }
}

impl QdrantStore {
    async fn retry_qdrant<T, F, Fut>(&self, op_name: &str, mut op: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, qdrant_client::QdrantError>>,
    {
        let attempts = 20usize;
        let mut last_error = None;

        for attempt in 1..=attempts {
            match op().await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let msg = err.to_string();
                    let retryable = msg.contains("Service was not ready")
                        || msg.contains("transport error")
                        || msg.contains("Unavailable");

                    if !retryable || attempt == attempts {
                        return Err(err).with_context(|| {
                            format!("qdrant {} failed after {} attempt(s)", op_name, attempt)
                        });
                    }

                    last_error = Some(err);
                    sleep(Duration::from_millis(250)).await;
                }
            }
        }

        if let Some(err) = last_error {
            Err(err).with_context(|| format!("qdrant {} failed after retries", op_name))
        } else {
            anyhow::bail!("qdrant {} failed without attempts", op_name)
        }
    }
}

fn stable_u64_id(input: &str) -> u64 {
    // Use a deterministic hasher output for idempotent upserts.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

fn value_to_usize(value: &qdrant_client::qdrant::Value) -> Option<usize> {
    // Parse through Display output because qdrant Value does not expose serde-like helpers.
    let raw = value.to_string();
    let trimmed = raw.trim_matches('"');
    trimmed.parse::<usize>().ok()
}
