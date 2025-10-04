use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, Context, Result};

pub fn resolve_socket_path(explicit: Option<PathBuf>, session: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        ensure_parent_exists(&path)?;
        return Ok(path);
    }

    let session_name = session.unwrap_or("default");
    let sanitized = sanitize_session_name(session_name);
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(env::temp_dir);
    let directory = base.join("kakoune-acp");
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "failed to create socket directory at {}",
            directory.display()
        )
    })?;
    Ok(directory.join(format!("{sanitized}.sock")))
}

pub fn send_to_kak(session: &str, command: &str) -> Result<()> {
    let mut child = Command::new("kak")
        .arg("-p")
        .arg(session)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn kak -p {session}"))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| anyhow!("failed to acquire kak stdin"))?
        .write_all(command.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!("kak exited with status {status}"));
    }
    Ok(())
}

pub fn format_info_command(client: Option<&str>, title: &str, body: &str) -> String {
    let info = format!("info -title {} {}\n", kak_quote(title), kak_quote(body));
    match client {
        Some(client) => format!("eval -client {} %{{{info}}}\n", kak_quote(client)),
        None => info,
    }
}

pub fn kak_quote(value: &str) -> String {
    let escaped = value.replace('\'', "''");
    format!("'{}'", escaped)
}

fn sanitize_session_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

fn ensure_parent_exists(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
    }
    Ok(())
}
