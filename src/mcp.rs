use anyhow::Result;
use anyhow::{Context, bail};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Once;

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

    /// Serve MCP over HTTPS using streamable HTTP transport.
    pub async fn run_https(self, mcp_server_url: &str) -> Result<()> {
        install_rustls_crypto_provider()?;

        let parsed = reqwest::Url::parse(mcp_server_url)
            .with_context(|| format!("invalid mcp_server_url {}", mcp_server_url))?;

        if parsed.scheme() != "https" {
            bail!(
                "mcp_server_url must use https scheme, got '{}'",
                parsed.scheme()
            );
        }

        let host = parsed
            .host_str()
            .context("mcp_server_url is missing host")?
            .to_string();
        let port = parsed.port_or_known_default().unwrap_or(443);
        let raw_path = parsed.path();
        let path = if raw_path.is_empty() || raw_path == "/" {
            "/".to_string()
        } else {
            format!("/{}", raw_path.trim_matches('/'))
        };

        let cert_path = std::env::var("OPENCODESEARCH_TLS_CERT_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("certs/localhost-cert.pem"));
        let key_path = std::env::var("OPENCODESEARCH_TLS_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("certs/localhost-key.pem"));

        let rustls = RustlsConfig::from_pem_file(cert_path, key_path)
            .await
            .context("failed to load TLS certificate and key")?;

        let mut addrs = tokio::net::lookup_host((host.as_str(), port))
            .await
            .with_context(|| format!("failed resolving MCP bind host {}", host))?;
        let bind_addr = addrs
            .next()
            .with_context(|| format!("host {} resolved to no addresses", host))?;

        let service: rmcp::transport::StreamableHttpService<
            OpenCodeSearchMcpServer,
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
        > = rmcp::transport::StreamableHttpService::new(
            move || Ok(self.clone()),
            Default::default(),
            rmcp::transport::StreamableHttpServerConfig::default(),
        );

        let app = build_mcp_router(path.clone(), service);
        eprintln!(
            "MCP HTTPS server listening on https://{}:{}{}",
            bind_addr.ip(),
            bind_addr.port(),
            path
        );

        axum_server::bind_rustls(bind_addr, rustls)
            .serve(app.into_make_service())
            .await
            .context("https MCP server exited with error")?;
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

fn install_rustls_crypto_provider() -> Result<()> {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        // If another crate already installed one, install_default returns Err(existing provider).
        // That condition is acceptable, so we intentionally ignore the return value.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });

    Ok(())
}

fn build_mcp_router(
    path: String,
    service: rmcp::transport::StreamableHttpService<
        OpenCodeSearchMcpServer,
        rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
    >,
) -> Router {
    if path == "/" {
        Router::new().route_service("/", service)
    } else {
        Router::new().nest_service(path.as_str(), service)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for OpenCodeSearchMcpServer {}

#[tool_router(router = tool_router)]
impl OpenCodeSearchMcpServer {
    /// Search code snippets using both semantic and keyword retrieval.
    #[tool(
        name = "search_code",
        description = "Larsescale search local codebase and return snippets + path + line ranges"
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
