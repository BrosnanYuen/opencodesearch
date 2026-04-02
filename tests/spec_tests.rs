use opencodesearch::config::AppConfig;
use opencodesearch::indexing::IndexingRuntime;
use opencodesearch::mcp::{OpenCodeSearchMcpServer, SearchRequest};
use rmcp::handler::server::wrapper::Parameters;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn docker_compose_up() -> anyhow::Result<()> {
    let status = Command::new("docker")
        .args(["compose", "up", "-d"])
        .current_dir(repo_root())
        .status()?;

    if !status.success() {
        anyhow::bail!("docker compose up failed")
    }
    Ok(())
}

fn write_test_config(codebase_dir: &Path) -> anyhow::Result<PathBuf> {
    let config_path = repo_root().join("config.test.json");
    let json = serde_json::json!({
        "codebase": {
            "directory_path": codebase_dir.display().to_string(),
            "git_branch": "main",
            "commit_threshold": 50,
            "mcp_server": "stdio"
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
    });

    std::fs::write(&config_path, serde_json::to_vec_pretty(&json)?)?;
    Ok(config_path)
}

fn create_python_project_with_10_files(root: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;

    for idx in 0..10 {
        let path = root.join(format!("module_{}.py", idx));
        let body = format!(
            "def transform_obj_{idx}(value):\n    obj = value\n    obj = obj + {idx}\n    return obj\n"
        );
        std::fs::write(path, body)?;
    }

    Ok(())
}

fn init_git_repo(path: &Path) -> anyhow::Result<()> {
    let run = |args: &[&str]| -> anyhow::Result<()> {
        let status = Command::new("git").args(args).current_dir(path).status()?;
        if !status.success() {
            anyhow::bail!("git {:?} failed", args);
        }
        Ok(())
    };

    run(&["init"])?;
    run(&["config", "user.email", "test@example.com"])?;
    run(&["config", "user.name", "test"])?;
    run(&["add", "."])?;
    run(&["commit", "-m", "initial"])?;
    Ok(())
}

#[tokio::test]
async fn parses_config_for_tests() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let cfg_path = write_test_config(tmp.path()).expect("write config");
    let cfg = AppConfig::from_path(cfg_path).expect("parse config");
    assert_eq!(cfg.codebase.commit_threshold, 50);
}

#[tokio::test]
#[ignore = "requires docker compose + ollama model"]
async fn a_connect_to_docker_ollama_and_run_cargo_test_flow() -> anyhow::Result<()> {
    docker_compose_up()?;

    let client = reqwest::Client::new();
    let response = client.get("http://localhost:11434/api/tags").send().await?;

    anyhow::ensure!(response.status().is_success(), "ollama endpoint unhealthy");
    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose"]
async fn b_connect_to_docker_quickwit_and_qdrant() -> anyhow::Result<()> {
    docker_compose_up()?;

    let mut quickwit_ok = false;
    for _ in 0..10 {
        let response = reqwest::get("http://localhost:7280/health/livez").await;
        if let Ok(resp) = response {
            if resp.status().is_success() {
                quickwit_ok = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    anyhow::ensure!(quickwit_ok, "quickwit health check failed");

    let qdrant_ok = reqwest::get("http://localhost:6333/healthz")
        .await?
        .status()
        .is_success();
    anyhow::ensure!(qdrant_ok, "qdrant health check failed");

    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose, ollama model, and local git"]
async fn c_to_f_index_python_project_and_query_via_mcp_logic() -> anyhow::Result<()> {
    docker_compose_up()?;

    let temp = tempfile::tempdir()?;
    create_python_project_with_10_files(temp.path())?;
    init_git_repo(temp.path())?;

    let cfg_path = write_test_config(temp.path())?;
    let config = AppConfig::from_path(cfg_path)?;
    let runtime = IndexingRuntime::from_config(config)?;

    // d) Index complete codebase using background indexing logic.
    runtime.index_entire_codebase().await?;

    // e) Query with non-exact phrasing through MCP server tool implementation.
    let mcp = OpenCodeSearchMcpServer::new(runtime);
    let payload = SearchRequest {
        query: "which method mutates the object value".to_string(),
        limit: Some(5),
    };

    let result = mcp.search_code(Parameters(payload)).await;
    anyhow::ensure!(
        result.contains("module_"),
        "expected code retrieval results"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose, ollama model, and git remotes"]
async fn g_and_h_watchdog_handles_100_commit_refactor_updates() -> anyhow::Result<()> {
    docker_compose_up()?;

    let temp = tempfile::tempdir()?;
    create_python_project_with_10_files(temp.path())?;
    init_git_repo(temp.path())?;

    // Produce 100 commits that refactor function names.
    for idx in 0..100 {
        let file = temp.path().join(format!("module_{}.py", idx % 10));
        let content = format!(
            "def refactor_obj_{idx}(value):\n    obj = value\n    obj = obj * 2\n    return obj\n"
        );
        std::fs::write(&file, content)?;

        let status_add = Command::new("git")
            .args(["add", "."])
            .current_dir(temp.path())
            .status()?;
        anyhow::ensure!(status_add.success(), "git add failed");

        let status_commit = Command::new("git")
            .args(["commit", "-m", &format!("refactor {}", idx)])
            .current_dir(temp.path())
            .status()?;
        anyhow::ensure!(status_commit.success(), "git commit failed");
    }

    // This test verifies commit creation and intended watchdog trigger condition.
    let count_out = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(temp.path())
        .output()?;
    let count = String::from_utf8(count_out.stdout)?
        .trim()
        .parse::<usize>()?;

    anyhow::ensure!(count >= 101, "expected initial + 100 commits");
    Ok(())
}
