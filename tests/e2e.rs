#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use tempfile::TempDir;

struct TestHarness {
    tempdir: TempDir,
    socket_path: PathBuf,
    binary_path: PathBuf,
    child: Child,
}

fn mock_agent_path() -> Result<PathBuf> {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    if let Some(path) = CACHE.get() {
        return Ok(path.clone());
    }

    let status = Command::new("cargo")
        .args(["build", "--example", "mock_agent"])
        .status()
        .context("failed to build mock_agent example")?;
    if !status.success() {
        bail!("cargo build --example mock_agent exited with {status}");
    }

    let target_dir = env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());
    let built_path = PathBuf::from(target_dir)
        .join("debug")
        .join("examples")
        .join(format!("mock_agent{}", env::consts::EXE_SUFFIX));
    let absolute = fs::canonicalize(&built_path)
        .with_context(|| format!("failed to canonicalize {}", built_path.display()))?;
    Ok(CACHE.get_or_init(|| absolute).clone())
}

impl TestHarness {
    fn new() -> Result<Self> {
        let agent_path = mock_agent_path()?;
        let tempdir = tempfile::tempdir()?;
        let socket_path = tempdir.path().join("daemon.sock");
        let binary_path = cargo_bin("kakoune-acp");

        let mut command = Command::new(&binary_path);
        command
            .arg("daemon")
            .arg("--socket")
            .arg(&socket_path)
            .arg("--cwd")
            .arg(tempdir.path())
            .arg("--")
            .arg(agent_path)
            .current_dir(tempdir.path())
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = command.spawn().context("failed to spawn daemon")?;
        wait_for_socket(&mut child, &socket_path)?;

        Ok(Self {
            tempdir,
            socket_path,
            binary_path,
            child,
        })
    }

    fn workspace(&self) -> &Path {
        self.tempdir.path()
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    fn cli_command(&self) -> Command {
        let mut command = Command::new(&self.binary_path);
        command.current_dir(self.workspace()).env_remove("RUST_LOG");
        command
    }

    fn prompt_command(&self) -> Command {
        let mut command = self.cli_command();
        command
            .arg("prompt")
            .arg("--socket")
            .arg(self.socket_path());
        command
    }

    fn status_command(&self) -> Command {
        let mut command = self.cli_command();
        command
            .arg("status")
            .arg("--socket")
            .arg(self.socket_path());
        command
    }

    fn shutdown(mut self) -> Result<Output> {
        let mut command = Command::new(&self.binary_path);
        command
            .current_dir(self.workspace())
            .env_remove("RUST_LOG")
            .arg("shutdown")
            .arg("--socket")
            .arg(&self.socket_path);
        let output = command.output().context("failed to run shutdown command")?;
        let _ = output.status;
        let _ = self.child.wait();
        Ok(output)
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for_socket(child: &mut Child, socket: &Path) -> Result<()> {
    let start = Instant::now();
    let timeout = Duration::from_secs(10);
    while start.elapsed() < timeout {
        if socket.exists() {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .context("failed to poll daemon process state")?
        {
            bail!("daemon exited before creating socket with status {status}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("daemon did not create socket within {timeout:?}"))
}

fn require_success(context: &str, output: Output) -> Result<String> {
    if !output.status.success() {
        bail!(
            "{context} failed: status={}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout)?;
    if !output.stderr.is_empty() {
        eprintln!(
            "{context} emitted stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(stdout)
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(program);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn wait_for_kak_session(kak: &Path, session: &str, timeout: Duration) -> Result<bool> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let output = Command::new(kak)
            .arg("-l")
            .output()
            .context("failed to list kakoune sessions")?;
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.lines().any(|line| line.trim() == session) {
                return Ok(true);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(false)
}

fn send_kak_command(kak: &Path, session: &str, command: &str) -> Result<()> {
    let mut child = Command::new(kak)
        .arg("-p")
        .arg(session)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to send command to kakoune session {session}"))?;
    child
        .stdin
        .as_mut()
        .context("kakoune pipe stdin missing")?
        .write_all(command.as_bytes())?;
    child.stdin.as_mut().unwrap().write_all(b"\n")?;
    let status = child.wait()?;
    if !status.success() {
        bail!("kak -p exited with status {status}");
    }
    Ok(())
}

#[test]
fn prompt_json_transcript_includes_rich_events() -> Result<()> {
    let harness = TestHarness::new()?;
    let context_file = harness.workspace().join("context.md");
    std::fs::write(&context_file, "Remember to highlight the tool calls.\n")?;

    let output = harness
        .prompt_command()
        .arg("--prompt")
        .arg("Summarise the integration behaviour")
        .arg("--context")
        .arg("Focus on plan and tool call events")
        .arg("--context-file")
        .arg(&context_file)
        .arg("--output")
        .arg("json")
        .output()
        .context("failed to run prompt command")?;
    let stdout = require_success("prompt json", output)?;

    let result: Value = serde_json::from_str(stdout.trim()).context("invalid json output")?;
    assert_eq!(result["stop_reason"], "end_turn");

    let context = result["context"].as_array().context("context missing")?;
    assert_eq!(context.len(), 2);
    assert_eq!(context[0]["text"], "Focus on plan and tool call events");
    assert!(context[1]["label"].as_str().unwrap().contains("context.md"));

    let transcript = result["transcript"]
        .as_array()
        .context("transcript missing")?;
    assert!(
        transcript
            .iter()
            .any(|event| event["kind"] == "user_message")
    );
    assert!(
        transcript
            .iter()
            .any(|event| event["kind"] == "agent_thought"),
        "transcript missing agent_thought: {:?}",
        transcript
    );
    assert!(transcript.iter().any(|event| event["kind"] == "plan"));
    assert!(
        transcript
            .iter()
            .any(|event| event["kind"] == "available_commands")
    );
    assert!(transcript.iter().any(|event| event["kind"] == "tool_call"));
    assert!(
        transcript
            .iter()
            .any(|event| event["kind"] == "tool_call_update")
    );
    assert!(
        transcript
            .iter()
            .any(|event| event["kind"] == "system_message")
    );
    assert!(
        transcript
            .iter()
            .any(|event| event["kind"] == "agent_message")
    );

    harness.shutdown()?;
    Ok(())
}

#[test]
fn prompt_plain_output_renders_sections() -> Result<()> {
    let harness = TestHarness::new()?;
    let output = harness
        .prompt_command()
        .arg("--prompt")
        .arg("Show me plain formatting")
        .arg("--context")
        .arg("Plain mode context")
        .output()
        .context("failed to run prompt command")?;
    let stdout = require_success("prompt plain", output)?;

    assert!(stdout.contains("=== Prompt ==="));
    assert!(stdout.contains("[agent]"));
    assert!(stdout.contains("[thought]"));
    assert!(stdout.contains("[plan]"));
    assert!(stdout.contains("[commands]"));
    assert!(stdout.contains("[tool demo-tool]"));

    harness.shutdown()?;
    Ok(())
}

#[test]
fn prompt_kak_commands_output_wraps_in_info_command() -> Result<()> {
    let harness = TestHarness::new()?;
    let context_file = harness.workspace().join("context.txt");
    std::fs::write(&context_file, "Context from file\n")?;

    let output = harness
        .prompt_command()
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
        .context("failed to run prompt command with kak-commands output")?;
    let stdout = require_success("prompt kak-commands", output)?;

    assert!(stdout.starts_with("eval -client 'main' %{"));
    assert!(stdout.contains("info -title 'Integration Title'"));
    assert!(stdout.contains("=== Prompt ==="));
    assert!(stdout.contains("Render kak commands"));
    assert!(stdout.contains("[1] file:"));

    harness.shutdown()?;
    Ok(())
}

#[test]
fn daemon_status_and_shutdown_roundtrip() -> Result<()> {
    let harness = TestHarness::new()?;
    let status_output = harness
        .status_command()
        .arg("--json")
        .output()
        .context("failed to run status command")?;
    let stdout = require_success("status json", status_output)?;
    let status: Value = serde_json::from_str(stdout.trim()).context("invalid status json")?;
    assert_eq!(status["running"], true);
    assert!(status["session_id"].as_str().is_some());

    let shutdown_output = harness.shutdown()?;
    let message = require_success("shutdown", shutdown_output)?;
    assert!(message.contains("daemon shut down"));

    Ok(())
}

#[test]
fn prompt_send_to_kak_requires_session() -> Result<()> {
    let harness = TestHarness::new()?;
    let output = harness
        .prompt_command()
        .arg("--prompt")
        .arg("trigger send to kak")
        .arg("--send-to-kak")
        .output()
        .context("failed to run prompt command with send-to-kak")?;

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires a Kakoune session"));

    harness.shutdown()?;
    Ok(())
}

#[test]
fn prompt_can_send_to_running_kakoune_session() -> Result<()> {
    let Some(kak_path) = find_in_path("kak") else {
        eprintln!("skipping kakoune integration test because kak is not available");
        return Ok(());
    };

    let harness = TestHarness::new()?;
    let session_name = format!("acp-test-{}", std::process::id());
    let mut kak_command = Command::new(&kak_path);
    if std::env::var_os("KAKOUNE_ACP_UI_TEST").is_none() {
        kak_command.arg("-n");
    }
    kak_command.arg("-s").arg(&session_name);

    let mut kak_child = match kak_command.spawn() {
        Ok(child) => child,
        Err(err) => {
            eprintln!("skipping kakoune integration test: failed to launch kak: {err}");
            harness.shutdown()?;
            return Ok(());
        }
    };

    if !wait_for_kak_session(&kak_path, &session_name, Duration::from_secs(3))? {
        eprintln!(
            "skipping kakoune integration test: session {session_name} did not become available"
        );
        let _ = kak_child.kill();
        let _ = kak_child.wait();
        harness.shutdown()?;
        return Ok(());
    }

    let output = harness
        .prompt_command()
        .arg("--prompt")
        .arg("Display integration status")
        .arg("--session")
        .arg(&session_name)
        .arg("--title")
        .arg("ACP integration")
        .arg("--send-to-kak")
        .output()
        .context("failed to run prompt with send-to-kak")?;
    let stdout = require_success("prompt send-to-kak", output)?;
    assert!(stdout.contains("=== Prompt ==="));

    let _ = send_kak_command(&kak_path, &session_name, "quit!");
    let _ = kak_child.wait();
    harness.shutdown()?;
    Ok(())
}
