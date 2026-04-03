use anyhow::Result;
use anyhow::{Context, bail};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use rmcp::handler::server::{
    router::tool::ToolRouter,
    wrapper::{Json, Parameters},
};
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Once;

use crate::indexing::IndexingRuntime;
use crate::types::SearchHit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindScheme {
    Http,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedMcpServerUrl {
    scheme: BindScheme,
    host: String,
    port: u16,
    path: String,
}

/// JSON input for MCP search tool calls.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
}

/// Structured MCP tool output.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
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
        let mcp_server_name = indexing.config.codebase.mcp_server_name.clone();
        let mut tool_router = Self::tool_router();
        if let Some(route) = tool_router.map.get_mut("search_code") {
            route.attr.description = Some(Cow::Owned(format!(
                "Largescale codebase search for {} and return snippets + path + line ranges",
                mcp_server_name
            )));
        }

        Self {
            indexing,
            tool_router,
        }
    }

    /// Serve MCP over streamable HTTP transport.
    ///
    /// `mcp_server_url` can be either `http://...` or `https://...`.
    pub async fn run_streamable_http(self, mcp_server_url: &str) -> Result<()> {
        let parsed = parse_mcp_server_url(mcp_server_url)?;

        let mut addrs = tokio::net::lookup_host((parsed.host.as_str(), parsed.port))
            .await
            .with_context(|| format!("failed resolving MCP bind host {}", parsed.host))?;
        let bind_addr = addrs
            .next()
            .with_context(|| format!("host {} resolved to no addresses", parsed.host))?;

        let service: rmcp::transport::StreamableHttpService<
            OpenCodeSearchMcpServer,
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
        > = rmcp::transport::StreamableHttpService::new(
            move || Ok(self.clone()),
            Default::default(),
            rmcp::transport::StreamableHttpServerConfig::default(),
        );

        let app = build_mcp_router(parsed.path.clone(), service);

        match parsed.scheme {
            BindScheme::Http => {
                eprintln!(
                    "MCP HTTP server listening on http://{}:{}{}",
                    bind_addr.ip(),
                    bind_addr.port(),
                    parsed.path
                );
                axum_server::bind(bind_addr)
                    .serve(app.into_make_service())
                    .await
                    .context("http MCP server exited with error")?;
            }
            BindScheme::Https => {
                install_rustls_crypto_provider()?;

                let cert_path = std::env::var("OPENCODESEARCH_TLS_CERT_PATH")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("certs/localhost-cert.pem"));
                let key_path = std::env::var("OPENCODESEARCH_TLS_KEY_PATH")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("certs/localhost-key.pem"));

                let rustls = RustlsConfig::from_pem_file(cert_path, key_path)
                    .await
                    .context("failed to load TLS certificate and key")?;

                eprintln!(
                    "MCP HTTPS server listening on https://{}:{}{}",
                    bind_addr.ip(),
                    bind_addr.port(),
                    parsed.path
                );
                axum_server::bind_rustls(bind_addr, rustls)
                    .serve(app.into_make_service())
                    .await
                    .context("https MCP server exited with error")?;
            }
        }

        Ok(())
    }

    /// Serve MCP over stdio transport for local MCP clients.
    pub async fn run_stdio(self) -> Result<()> {
        let service = rmcp::serve_server(self, rmcp::transport::stdio())
            .await
            .context("failed to start stdio MCP server")?;
        let _ = service
            .waiting()
            .await
            .context("stdio MCP server task join failed")?;
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

fn parse_mcp_server_url(mcp_server_url: &str) -> Result<ParsedMcpServerUrl> {
    let parsed = reqwest::Url::parse(mcp_server_url)
        .with_context(|| format!("invalid mcp_server_url {}", mcp_server_url))?;

    let scheme = match parsed.scheme() {
        "http" => BindScheme::Http,
        "https" => BindScheme::Https,
        other => bail!(
            "mcp_server_url must use http or https scheme, got '{}'",
            other
        ),
    };

    let host = parsed
        .host_str()
        .context("mcp_server_url is missing host")?
        .to_string();

    let default_port = match scheme {
        BindScheme::Http => 80,
        BindScheme::Https => 443,
    };
    let port = parsed.port().unwrap_or(default_port);

    let raw_path = parsed.path();
    let path = if raw_path.is_empty() || raw_path == "/" {
        "/".to_string()
    } else {
        format!("/{}", raw_path.trim_matches('/'))
    };

    Ok(ParsedMcpServerUrl {
        scheme,
        host,
        port,
        path,
    })
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
        description = "Largescale codebase search for {mcp_server_name} and return snippets + path + line ranges"
    )]
    pub async fn search_code(
        &self,
        Parameters(input): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResponse>, rmcp::ErrorData> {
        let limit = input.limit.unwrap_or(8).max(1).min(50);
        let results = self
            .search_internal(&input.query, limit)
            .await
            .map_err(|err| rmcp::ErrorData::internal_error(err.to_string(), None))?;
        Ok(Json(SearchResponse { hits: results }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, CodebaseConfig, OllamaConfig, QdrantConfig, QuickwitConfig};
    use std::path::PathBuf;
    use tokio::time::Duration;

    #[test]
    fn tool_description_contains_configured_server_name() {
        let cfg = AppConfig {
            codebase: CodebaseConfig {
                directory_path: PathBuf::from("."),
                git_branch: "main".to_string(),
                commit_threshold: 50,
                mcp_server_name: "My cool codebase".to_string(),
                mcp_server_url: "http://localhost:9443".to_string(),
                background_indexing_threads: 1,
            },
            ollama: OllamaConfig {
                server_url: "http://localhost:11434".to_string(),
                embedding_model: "qwen3-embedding:0.6b".to_string(),
                context_size: 2000,
            },
            qdrant: QdrantConfig {
                server_url: "http://localhost:6334".to_string(),
                collection_name: "opencodesearch-code-chunks".to_string(),
                api_key: None,
            },
            quickwit: QuickwitConfig {
                quickwit_url: "http://localhost:7280".to_string(),
                quickwit_index_id: "opencodesearch-code-chunks".to_string(),
            },
        };

        let runtime = IndexingRuntime::from_config(cfg).expect("runtime should build");
        let server = OpenCodeSearchMcpServer::new(runtime);
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|item| item.name == "search_code")
            .expect("search_code tool should exist");
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(description.contains("My cool codebase"));
    }

    #[test]
    fn mcp_server_url_parsing_supports_http_and_https() {
        let http = parse_mcp_server_url("http://127.0.0.1:9000/mcp").expect("http parse");
        assert_eq!(http.scheme, BindScheme::Http);
        assert_eq!(http.port, 9000);
        assert_eq!(http.path, "/mcp");

        let https = parse_mcp_server_url("https://localhost").expect("https parse");
        assert_eq!(https.scheme, BindScheme::Https);
        assert_eq!(https.port, 443);
        assert_eq!(https.path, "/");
    }

    #[test]
    fn mcp_server_url_parsing_rejects_unsupported_scheme() {
        let err = parse_mcp_server_url("tcp://localhost:9443").expect_err("must reject tcp");
        assert!(err.to_string().contains("http or https"));
    }

    #[tokio::test]
    async fn search_code_returns_mcp_error_when_backend_fails() {
        let cfg = AppConfig {
            codebase: CodebaseConfig {
                directory_path: PathBuf::from("."),
                git_branch: "main".to_string(),
                commit_threshold: 50,
                mcp_server_name: "Test".to_string(),
                mcp_server_url: "http://localhost:9443".to_string(),
                background_indexing_threads: 1,
            },
            ollama: OllamaConfig {
                server_url: "http://127.0.0.1:1".to_string(),
                embedding_model: "qwen3-embedding:0.6b".to_string(),
                context_size: 2000,
            },
            qdrant: QdrantConfig {
                server_url: "http://localhost:6334".to_string(),
                collection_name: "opencodesearch-code-chunks".to_string(),
                api_key: None,
            },
            quickwit: QuickwitConfig {
                quickwit_url: "http://localhost:7280".to_string(),
                quickwit_index_id: "opencodesearch-code-chunks".to_string(),
            },
        };

        let runtime = IndexingRuntime::from_config(cfg).expect("runtime should build");
        let server = OpenCodeSearchMcpServer::new(runtime);
        let payload = SearchRequest {
            query: "where is obj changed".to_string(),
            limit: Some(3),
        };

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            server.search_code(Parameters(payload)),
        )
        .await
        .expect("search must return quickly");
        assert!(result.is_err(), "expected MCP protocol error result");
    }
}
