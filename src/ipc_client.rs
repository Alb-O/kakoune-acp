use std::path::Path;

use anyhow::{Context, Result, anyhow};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::ipc::{DaemonRequest, DaemonResponse};

pub async fn roundtrip(path: &Path, request: &DaemonRequest) -> Result<DaemonResponse> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("failed to connect to {}", path.display()))?;
    send_request(stream, request).await
}

async fn send_request(stream: UnixStream, request: &DaemonRequest) -> Result<DaemonResponse> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let payload = serde_json::to_string(request)?;
    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Err(anyhow!("daemon closed the connection"));
    }
    let response: DaemonResponse = serde_json::from_str(line.trim_end())
        .with_context(|| format!("invalid response from daemon: {line}"))?;
    Ok(response)
}
