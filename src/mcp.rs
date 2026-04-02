use anyhow::Result;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};

use crate::indexing::IndexingRuntime;
use crate::types::SearchHit;

/// JSON input for MCP search tool calls.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
}

/// MCP server process that exposes code search to compatible clients.
#[derive(Clone)]
pub struct OpenCodeSearchMcpServer {
    indexing: IndexingRuntime,
    tool_router: ToolRouter<Self>,
}

impl OpenCodeSearchMcpServer {
    /// Build MCP server state with shared indexing runtime.
    pub fn new(indexing: IndexingRuntime) -> Self {
        Self {
            indexing,
            tool_router: Self::tool_router(),
        }
    }

    /// Serve over stdio transport for Claude Code / Codex / opencode compatibility.
    pub async fn run_stdio(self) -> Result<()> {
        let running = self.serve(rmcp::transport::stdio()).await?;
        let _ = running.waiting().await?;
        Ok(())
    }

    async fn search_internal(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // Run semantic search via embeddings and Qdrant.
        let query_vector = self.indexing.embed_query(query).await?;
        let mut semantic = self
            .indexing
            .qdrant
            .semantic_search(query_vector, limit as u64)
            .await?;

        // Run keyword search via quickwit pipeline.
        let mut keyword = self
            .indexing
            .quickwit
            .keyword_search(query, limit)
            .await
            .unwrap_or_default();

        // Merge and de-duplicate by path + line span.
        semantic.append(&mut keyword);
        semantic.sort_by(|a, b| b.score.total_cmp(&a.score));

        let mut deduped = Vec::new();
        for hit in semantic {
            let exists = deduped.iter().any(|existing: &SearchHit| {
                existing.path == hit.path
                    && existing.start_line == hit.start_line
                    && existing.end_line == hit.end_line
            });

            if !exists {
                deduped.push(hit);
            }
            if deduped.len() >= limit {
                break;
            }
        }

        Ok(deduped)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for OpenCodeSearchMcpServer {}

#[tool_router(router = tool_router)]
impl OpenCodeSearchMcpServer {
    /// Search code snippets using both semantic and keyword retrieval.
    #[tool(
        name = "search_code",
        description = "Search code snippets and return snippets + path + line ranges"
    )]
    pub async fn search_code(&self, Parameters(input): Parameters<SearchRequest>) -> String {
        let limit = input.limit.unwrap_or(8).max(1).min(50);

        match self.search_internal(&input.query, limit).await {
            Ok(results) => match serde_json::to_string_pretty(&results) {
                Ok(serialized) => serialized,
                Err(err) => format!("{{\"error\":\"serialization failed: {}\"}}", err),
            },
            Err(err) => format!("{{\"error\":\"search failed: {}\"}}", err),
        }
    }
}
