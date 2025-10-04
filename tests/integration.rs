use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use tempfile::TempDir;
use tokio::{
    fs,
    io::AsyncWriteExt,
    process::{Child, Command},
    time::{Instant, sleep},
};

struct DaemonHandle {
    socket_path: PathBuf,
    _tempdir: TempDir,
    child: Child,
}

impl DaemonHandle {
    async fn spawn() -> Result<Self> {
        let kakoune_acp = cargo_bin("kakoune-acp");
        let agent = cargo_bin("mock-acp-agent");
        let tempdir = TempDir::new()?;
        let socket_path = tempdir.path().join("daemon.sock");

        let child = Command::new(&kakoune_acp)
            .arg("daemon")
            .arg("--socket")
            .arg(&socket_path)
            .arg("--cwd")
            .arg(tempdir.path())
            .arg("--")
            .arg(&agent)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("failed to spawn kakoune-acp daemon")?;

        wait_for_socket(&socket_path).await?;

        Ok(Self {
            socket_path,
            _tempdir: tempdir,
            child,
        })
    }

    fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    fn working_dir(&self) -> &Path {
        self._tempdir.path()
    }

    async fn shutdown(mut self) -> Result<String> {
        let kakoune_acp = cargo_bin("kakoune-acp");
        let shutdown_output = Command::new(&kakoune_acp)
            .arg("shutdown")
            .arg("--socket")
            .arg(&self.socket_path)
            .output()
            .await
            .context("failed to send shutdown request")?;

        if !shutdown_output.status.success() {
            let _ = self.child.start_kill();
        }

        match tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await {
            Ok(waited) => {
                let _ = waited.context("failed to wait for daemon shutdown")?;
            }
            Err(_) => {
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
            }
        }

        anyhow::ensure!(
            shutdown_output.status.success(),
            "shutdown command failed: {}",
            String::from_utf8_lossy(&shutdown_output.stderr)
        );

        let stdout = String::from_utf8(shutdown_output.stdout)
            .context("shutdown output was not valid UTF-8")?;
        Ok(stdout)
    }
}

async fn wait_for_socket(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::try_exists(path).await? {
            break;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("socket {} was not created in time", path.display());
        }
        sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn run_status(socket_path: &Path) -> Result<Value> {
    let kakoune_acp = cargo_bin("kakoune-acp");
    let output = Command::new(&kakoune_acp)
        .arg("status")
        .arg("--socket")
        .arg(socket_path)
        .arg("--json")
        .output()
        .await
        .context("failed to query daemon status")?;
    anyhow::ensure!(
        output.status.success(),
        "status command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status_json: Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse status response as JSON")?;
    Ok(status_json)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_transcript_workflows() -> Result<()> {
    let daemon = DaemonHandle::spawn().await?;
    let socket_path = daemon.socket_path().clone();

    let status = run_status(&socket_path).await?;
    assert_eq!(status["running"], Value::Bool(true));
    assert!(status["session_id"].is_string());

    let plain_context = daemon.working_dir().join("context.txt");
    tokio::fs::write(&plain_context, "Important context from the project").await?;

    let kakoune_acp = cargo_bin("kakoune-acp");
    let plain_output = Command::new(&kakoune_acp)
        .arg("prompt")
        .arg("--socket")
        .arg(&socket_path)
        .arg("--prompt")
        .arg("Summarise the important context")
        .arg("--context-file")
        .arg(&plain_context)
        .arg("--output")
        .arg("plain")
        .output()
        .await
        .context("failed to run plain prompt command")?;

    anyhow::ensure!(
        plain_output.status.success(),
        "plain prompt failed: {}",
        String::from_utf8_lossy(&plain_output.stderr)
    );

    let plain_stdout = String::from_utf8(plain_output.stdout)
        .context("plain prompt output was not valid UTF-8")?;
    assert!(plain_stdout.contains("=== Prompt ==="));
    assert!(plain_stdout.contains("Summarise the important context"));
    assert!(plain_stdout.contains("=== Context ==="));
    assert!(plain_stdout.contains("apply_suggestion"));
    assert!(plain_stdout.contains("[plan]"));
    assert!(plain_stdout.contains("[commands]"));
    assert!(plain_stdout.contains("[thought] Thinking about"));
    assert!(plain_stdout.contains("[tool write_summary] Completed"));
    assert!(plain_stdout.contains("[system] Current mode: writer"));
    assert!(plain_stdout.contains("Stop reason: EndTurn"));

    let json_context = daemon.working_dir().join("notes.txt");
    tokio::fs::write(&json_context, "Streamed context snippet").await?;

    let json_output = Command::new(&kakoune_acp)
        .arg("prompt")
        .arg("--socket")
        .arg(&socket_path)
        .arg("--prompt")
        .arg("Explain how the daemon collected transcript events")
        .arg("--context-file")
        .arg(&json_context)
        .arg("--output")
        .arg("json")
        .output()
        .await
        .context("failed to run json prompt command")?;

    anyhow::ensure!(
        json_output.status.success(),
        "json prompt failed: {}",
        String::from_utf8_lossy(&json_output.stderr)
    );

    let json_stdout =
        String::from_utf8(json_output.stdout).context("json prompt output was not valid UTF-8")?;
    let result_json: Value =
        serde_json::from_str(&json_stdout).context("invalid JSON from prompt output")?;
    assert_eq!(
        result_json["user_prompt"],
        "Explain how the daemon collected transcript events"
    );
    assert_eq!(result_json["stop_reason"], "end_turn");
    assert_eq!(
        result_json["context"]
            .as_array()
            .map(|entries| entries.len()),
        Some(1)
    );

    let transcript = result_json["transcript"]
        .as_array()
        .context("transcript was not an array")?;
    assert!(
        transcript
            .iter()
            .any(|event| event["kind"] == "agent_message")
    );
    assert!(transcript.iter().any(|event| event["kind"] == "plan"));
    assert!(transcript.iter().any(|event| event["kind"] == "tool_call"));

    daemon.shutdown().await.map(|_| ())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_kak_commands_output_wraps_in_info_command() -> Result<()> {
    let daemon = DaemonHandle::spawn().await?;
    let socket_path = daemon.socket_path().clone();

    let context_file = daemon.working_dir().join("context.txt");
    tokio::fs::write(&context_file, "Context from file\n").await?;

    let kakoune_acp = cargo_bin("kakoune-acp");
    let output = Command::new(&kakoune_acp)
        .arg("prompt")
        .arg("--socket")
        .arg(&socket_path)
        .arg("--prompt")
        .arg("Render kak commands")
        .arg("--context-file")
        .arg(&context_file)
        .arg("--output")
        .arg("kak-commands")
        .arg("--title")
        .arg("Integration Title")
        .arg("--client")
        .arg("main")
        .output()
        .await
        .context("failed to run prompt command with kak-commands output")?;

    anyhow::ensure!(
        output.status.success(),
        "prompt kak-commands failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout)
        .context("prompt kak-commands output was not valid UTF-8")?;
    assert!(stdout.starts_with("eval -client 'main' %{"));
    assert!(stdout.contains("info -title 'Integration Title'"));
    assert!(stdout.contains("=== Prompt ==="));
    assert!(stdout.contains("Render kak commands"));
    assert!(stdout.contains("[1] file:"));

    daemon.shutdown().await.map(|_| ())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_status_and_shutdown_roundtrip() -> Result<()> {
    let daemon = DaemonHandle::spawn().await?;
    let socket_path = daemon.socket_path().clone();

    let status = run_status(&socket_path).await?;
    assert_eq!(status["running"], Value::Bool(true));
    assert!(status["session_id"].is_string());

    let message = daemon.shutdown().await?;
    assert!(message.contains("daemon shut down"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_send_to_kak_requires_session() -> Result<()> {
    let daemon = DaemonHandle::spawn().await?;
    let socket_path = daemon.socket_path().clone();

    let kakoune_acp = cargo_bin("kakoune-acp");
    let output = Command::new(&kakoune_acp)
        .arg("prompt")
        .arg("--socket")
        .arg(&socket_path)
        .arg("--prompt")
        .arg("trigger send to kak")
        .arg("--send-to-kak")
        .output()
        .await
        .context("failed to run prompt command with send-to-kak")?;

    assert!(
        !output.status.success(),
        "send-to-kak prompt unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires a Kakoune session"));

    daemon.shutdown().await.map(|_| ())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_send_to_kak_when_available() -> Result<()> {
    if !kak_available().await {
        eprintln!("kak executable not found, skipping integration test");
        return Ok(());
    }

    let daemon = DaemonHandle::spawn().await?;
    let socket_path = daemon.socket_path().clone();

    let session_name = format!("acp-test-{}", std::process::id());
    let ui_observation = env::var_os("KAKOUNE_ACP_UI_TEST").is_some();

    let Some(kak_path) = find_kak() else {
        eprintln!("kak executable not found, skipping integration test");
        return Ok(());
    };

    let mut kak_command = Command::new(&kak_path);
    if !ui_observation {
        kak_command.arg("-ui").arg("dummy").arg("-n");
        kak_command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    } else {
        kak_command
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
    }
    kak_command.arg("-s").arg(&session_name);
    let mut kak_process = kak_command
        .spawn()
        .context("failed to launch kakoune for send-to-kak test")?;

    if ui_observation {
        sleep(Duration::from_secs(2)).await;
    }

    let kakoune_acp = cargo_bin("kakoune-acp");
    let output = Command::new(&kakoune_acp)
        .arg("prompt")
        .arg("--socket")
        .arg(&socket_path)
        .arg("--prompt")
        .arg("Send this transcript into Kakoune")
        .arg("--output")
        .arg("plain")
        .arg("--send-to-kak")
        .arg("--session")
        .arg(&session_name)
        .arg("--title")
        .arg("ACP test transcript")
        .output()
        .await
        .context("failed to run prompt with send-to-kak")?;

    anyhow::ensure!(
        output.status.success(),
        "prompt command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    if ui_observation {
        sleep(Duration::from_secs(3)).await;
    }

    send_to_kak(&session_name, "quit").await?;

    if ui_observation {
        let _ = tokio::time::timeout(Duration::from_secs(15), kak_process.wait()).await;
    } else if tokio::time::timeout(Duration::from_millis(200), kak_process.wait())
        .await
        .is_err()
    {
        let _ = kak_process.start_kill();
        let _ = kak_process.wait().await;
    }

    daemon.shutdown().await.map(|_| ())
}

fn find_kak() -> Option<PathBuf> {
    if let Some(path) = env::var_os("KAKOUNE_ACP_KAK") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    if let Ok(output) = std::process::Command::new("which").arg("kak").output() {
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !raw.is_empty() {
                let candidate = PathBuf::from(raw);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    let base = PathBuf::from("kak");
    if base.is_absolute() && base.exists() {
        return Some(base);
    }

    let mut candidates = Vec::new();

    if let Some(path_var) = env::var_os("PATH") {
        candidates.extend(env::split_paths(&path_var).map(|entry| entry.join("kak")));
    }

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join(".nix-profile/bin/kak"),
            home.join(".local/state/nix/profiles/profile/bin/kak"),
            home.join(".local/bin/kak"),
        ]);
    }

    candidates.extend([
        PathBuf::from("/run/current-system/sw/bin/kak"),
        PathBuf::from("/nix/var/nix/profiles/default/bin/kak"),
    ]);

    candidates.into_iter().find(|candidate| candidate.exists())
}

async fn kak_available() -> bool {
    let Some(kak_path) = find_kak() else {
        return false;
    };

    Command::new(&kak_path)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn send_to_kak(session: &str, command: &str) -> Result<()> {
    let Some(kak_path) = find_kak() else {
        anyhow::bail!("kak executable not found in PATH");
    };

    let mut process = Command::new(&kak_path)
        .arg("-p")
        .arg(session)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to connect to kak session {session}"))?;

    if let Some(mut stdin) = process.stdin.take() {
        stdin
            .write_all(format!("{command}\n").as_bytes())
            .await
            .context("failed to write command to kak session")?;
    }

    let status = process
        .wait()
        .await
        .context("failed to wait for kak -p command")?;
    anyhow::ensure!(status.success(), "kak -p exited with status {status}");
    Ok(())
}
