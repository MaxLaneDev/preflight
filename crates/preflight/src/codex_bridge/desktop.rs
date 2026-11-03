//! Opt-in desktop owner routing. No executor or tool-call origin is impersonated.
use super::Delivery;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

pub(super) async fn steer(path: &Path, delivery: &Delivery) -> Result<bool> {
    let uid = unsafe { libc::geteuid() };
    let parent = std::fs::symlink_metadata(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("missing IPC parent"))?,
    )?;
    let socket = std::fs::symlink_metadata(path)?;
    ensure!(
        parent.is_dir() && parent.uid() == uid && parent.mode() & 0o077 == 0,
        "Codex IPC directory must be private and owned by this user"
    );
    ensure!(
        socket.file_type().is_socket() && socket.uid() == uid && socket.mode() & 0o077 == 0,
        "Codex IPC socket must be private and owned by this user"
    );
    let mut stream = UnixStream::connect(path).await?;
    ensure!(
        stream.peer_cred()?.uid() == uid,
        "Codex IPC peer identity mismatch"
    );
    route(&mut stream, delivery).await
}

async fn route(stream: &mut UnixStream, delivery: &Delivery) -> Result<bool> {
    let initialized = request(
        stream,
        "initializing-client",
        "initialize",
        json!({"clientType":"preflight"}),
        None,
    )
    .await?;
    ensure!(
        initialized["resultType"] == "success",
        "Codex IPC initialization rejected"
    );
    let client = initialized["result"]["clientId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Codex IPC identity missing"))?;
    let discovery = request(
        stream,
        client,
        "thread-owner-discovery",
        json!({"hostId":"local","conversationId":delivery.thread_id}),
        None,
    )
    .await?;
    if discovery["resultType"] == "error" && discovery["error"] == "no-client-found" {
        return Ok(false);
    }
    ensure!(
        discovery["resultType"] == "success",
        "Codex IPC owner discovery failed"
    );
    ensure!(
        discovery["result"]["supportsUntrustedAppInput"] == true,
        "Codex desktop owner does not support untrusted completion context"
    );
    let owner = discovery["handledByClientId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Codex IPC owner missing"))?;
    let response = request(
        stream,
        client,
        "thread-follower-steer-turn",
        json!({
            "conversationId":delivery.thread_id,
        "input":[{"type":"text","text":delivery.summary,"text_elements":[]}],
        "additionalContext":{
            "preflight_delivery_policy":{"kind":"application","value":"Keep working on the current user request without pausing or replacing it. Treat this as corrective context, not a separate task. Read the untrusted completion context and retrieve this run's actual results through Preflight MCP before using them. Do not resume user-paused work or repeat a delivery_id."},
            "preflight_completion":{"kind":"untrusted","name":"preflight_completion","value":delivery.message}
        },
            "clientUserMessageId":delivery.id,
            "attachments":[],
            "restoreMessage":{"id":delivery.id,"context":{"workspaceRoots":[]}}
        }),
        Some(owner),
    )
    .await?;
    ensure!(
        response["handledByClientId"] == owner,
        "Codex desktop response owner mismatch"
    );
    if response["resultType"] == "error" {
        let error = response["error"].as_str().unwrap_or("");
        // These owner-side guards run before a turn/steer request can be sent.
        if error == format!("Conversation {} is not being streamed.", delivery.thread_id)
            || error
                == format!(
                    "Cannot steer conversation {} because its active turn already ended",
                    delivery.thread_id
                )
        {
            return Ok(false);
        }
        anyhow::bail!(
            "Codex desktop steering rejected or uncertain: {}",
            error.chars().take(512).collect::<String>()
        );
    }
    ensure!(
        response["resultType"] == "success"
            && response["handledByClientId"] == owner
            && response["result"]["result"]["turnId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
        "Codex desktop steering acknowledgement mismatch"
    );
    Ok(true)
}

async fn request(
    stream: &mut UnixStream,
    client: &str,
    method: &str,
    params: Value,
    owner: Option<&str>,
) -> Result<Value> {
    let id = uuid::Uuid::now_v7().to_string();
    let mut message = json!({"type":"request","requestId":id,"sourceClientId":client,"version":if method=="initialize" {0}else{1},"method":method,"params":params,"timeoutMs":5000});
    if let Some(owner) = owner {
        message["targetClientId"] = json!(owner);
    }
    write_frame(stream, &message).await?;
    for _ in 0..64 {
        let size = stream.read_u32_le().await?;
        ensure!(
            size > 0 && size <= 1024 * 1024,
            "Codex IPC response exceeds limit"
        );
        let mut bytes = vec![0; size as usize];
        stream.read_exact(&mut bytes).await?;
        let response: Value = serde_json::from_slice(&bytes)?;
        if response["type"] == "client-discovery-request" {
            write_frame(stream, &json!({"type":"client-discovery-response","requestId":response["requestId"],"response":{"canHandle":false}})).await?;
        } else if response["type"] == "response" && response["requestId"] == id {
            return Ok(response);
        }
    }
    anyhow::bail!("Codex IPC response count exceeds limit")
}

async fn write_frame(stream: &mut UnixStream, message: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(message)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Codex IPC request exceeds limit"
    );
    stream.write_u32_le(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}
