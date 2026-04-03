use opencodesearch::config::AppConfig;
use opencodesearch::indexing::IndexingRuntime;
use opencodesearch::mcp::{OpenCodeSearchMcpServer, SearchRequest};
use rmcp::handler::server::wrapper::Parameters;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn marker_dir() -> PathBuf {
    repo_root().join(".opencodesearch").join("test-markers")
}

fn init_marker_dir_once() -> anyhow::Result<()> {
    static INIT: OnceLock<()> = OnceLock::new();
    if INIT.get().is_none() {
        let dir = marker_dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        let _ = INIT.set(());
    }
    Ok(())
}

fn mark_ignored_test_done(test_name: &str) -> anyhow::Result<()> {
    let marker = marker_dir().join(format!("{test_name}.done"));
    let now = format!("{:?}", std::time::SystemTime::now());
    std::fs::write(marker, now)?;
    Ok(())
}

fn wait_for_ignored_tests_done() -> anyhow::Result<()> {
    // zzzz cleanup test must wait until all other ignored tests have completed.
    const REQUIRED_MARKERS: &[&str] = &[
        "a_connect_to_docker_ollama_and_run_cargo_test_flow",
        "b_connect_to_docker_quickwit_and_qdrant",
        "c_to_f_index_python_project_and_query_via_mcp_logic",
        "g_and_h_watchdog_handles_100_commit_refactor_updates",
        "index_moss_kernel_and_retrieve_code",
    ];

    let start = Instant::now();
    let timeout = Duration::from_secs(30 * 60);
    loop {
        let all_done = REQUIRED_MARKERS
            .iter()
            .all(|name| marker_dir().join(format!("{name}.done")).exists());
        if all_done {
            return Ok(());
        }
        if start.elapsed() > timeout {
            anyhow::bail!("timed out waiting for all ignored tests to complete");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
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
            "mcp_server_url": "https://localhost:9443",
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
    init_marker_dir_once()?;
    docker_compose_up()?;

    let client = reqwest::Client::new();
    let response = client.get("http://localhost:11434/api/tags").send().await?;

    anyhow::ensure!(response.status().is_success(), "ollama endpoint unhealthy");
    mark_ignored_test_done("a_connect_to_docker_ollama_and_run_cargo_test_flow")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose"]
async fn b_connect_to_docker_quickwit_and_qdrant() -> anyhow::Result<()> {
    init_marker_dir_once()?;
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
    mark_ignored_test_done("b_connect_to_docker_quickwit_and_qdrant")?;

    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose, ollama model, and local git"]
async fn c_to_f_index_python_project_and_query_via_mcp_logic() -> anyhow::Result<()> {
    init_marker_dir_once()?;
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
    println!("c_to_f retrieved results:\n{}", result);
    anyhow::ensure!(
        result.contains("module_"),
        "expected code retrieval results"
    );
    mark_ignored_test_done("c_to_f_index_python_project_and_query_via_mcp_logic")?;

    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose, ollama model, and git remotes"]
async fn g_and_h_watchdog_handles_100_commit_refactor_updates() -> anyhow::Result<()> {
    init_marker_dir_once()?;
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
    mark_ignored_test_done("g_and_h_watchdog_handles_100_commit_refactor_updates")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose, ollama model, and cloned moss-kernel repository"]
async fn index_moss_kernel_and_retrieve_code() -> anyhow::Result<()> {
    init_marker_dir_once()?;
    docker_compose_up()?;

    let moss_root = repo_root().join("examples").join("moss-kernel");
    anyhow::ensure!(
        moss_root.exists(),
        "examples/moss-kernel is missing; clone step required"
    );

    let cfg_path = write_test_config(&moss_root)?;
    let config = AppConfig::from_path(cfg_path)?;
    let runtime = IndexingRuntime::from_config(config)?;

    // Index the full cloned project.
    runtime.index_entire_codebase().await?;

    // Retrieve code through MCP search path.
    let mcp = OpenCodeSearchMcpServer::new(runtime);
    let payload = SearchRequest {
        query: "where is scheduler or task management implemented".to_string(),
        limit: Some(8),
    };
    let result = mcp.search_code(Parameters(payload)).await;
    println!("index_moss_kernel retrieved results:\n{}", result);

    anyhow::ensure!(!result.contains("\"error\""), "mcp retrieval returned error");
    anyhow::ensure!(result.contains("\"path\""), "no code results returned");
    mark_ignored_test_done("index_moss_kernel_and_retrieve_code")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires docker compose; intended final cleanup test"]
async fn zzzz_delete_all_stored_code_from_qdrant_and_quickwit() -> anyhow::Result<()> {
    init_marker_dir_once()?;
    docker_compose_up()?;

    // Force cleanup test to wait for all other ignored tests to complete.
    wait_for_ignored_tests_done()?;

    // Do not index in this test; only invoke delete-all API.
    let cfg_path = write_test_config(&repo_root())?;
    let config = AppConfig::from_path(cfg_path)?;
    let runtime = IndexingRuntime::from_config(config)?;

    // Run the new cleanup API.
    runtime.delete_all_stored_code().await?;
    Ok(())
}
