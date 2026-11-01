//! Bounded stdio relay to the existing local MCP backend; never opens an engine.
use anyhow::{Result, bail, ensure};
use rmcp::{
    RoleServer,
    model::{ClientNotification, ErrorData, JsonRpcMessage},
    transport::{
        StreamableHttpClientTransport, Transport, async_rw::AsyncRwTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use std::{net::IpAddr, time::Duration};

fn endpoint(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("invalid MCP endpoint"))?;
    ensure!(
        url.scheme() == "http"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/mcp"
            && url
                .host_str()
                .unwrap_or("")
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback()),
        "MCP endpoint must be a plain loopback HTTP /mcp URL; use a local tunnel for remote servers"
    );
    Ok(url.into())
}

pub async fn run(args: &[String]) -> Result<()> {
    let mut server = "http://127.0.0.1:4170/mcp".to_owned();
    let mut credential = None;
    let mut flags = std::collections::BTreeSet::new();
    for pair in args.chunks(2) {
        ensure!(
            pair.len() == 2 && flags.insert(pair[0].as_str()),
            "usage: preflight mcp-proxy [--server URL] [--token-env NAME]"
        );
        match pair[0].as_str() {
            "--server" => server = pair[1].clone(),
            "--token-env" => {
                ensure!(
                    !pair[1].is_empty()
                        && pair[1].bytes().enumerate().all(|(i, b)| b == b'_'
                            || b.is_ascii_alphabetic()
                            || (i > 0 && b.is_ascii_digit())),
                    "invalid credential environment name"
                );
                let token = std::env::var(&pair[1]).map_err(|_| {
                    anyhow::anyhow!("MCP credential environment variable is unavailable")
                })?;
                ensure!(!token.is_empty(), "MCP credential is empty");
                let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| anyhow::anyhow!("invalid MCP credential"))?;
                value.set_sensitive(true);
                credential = Some(value);
            }
            _ => bail!("usage: preflight mcp-proxy [--server URL] [--token-env NAME]"),
        }
    }
    let server = endpoint(&server)?;
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(value) = credential {
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .default_headers(headers)
        .build()?;
    // Expired sessions must not cause implicit replays of mutating tool calls.
    let config =
        StreamableHttpClientTransportConfig::with_uri(server).reinit_on_expired_session(false);
    let mut upstream = StreamableHttpClientTransport::with_client(client, config);
    let mut downstream =
        AsyncRwTransport::<RoleServer, _, _>::new(tokio::io::stdin(), tokio::io::stdout());
    let mut pending = tokio::task::JoinSet::new();
    let outcome: Result<()> = async {
        loop {
            tokio::select! {
                message = downstream.receive() => {
                    let Some(message) = message else { break; };
                    // Stdio flush is not an HTTP acknowledgement: preserve the initialization barrier.
                    if matches!(&message, JsonRpcMessage::Notification(n) if matches!(&n.notification, ClientNotification::InitializedNotification(_))) {
                        upstream.send(message).await.map_err(|_| anyhow::anyhow!("Preflight MCP initialization failed; check the server and access token"))?;
                        continue;
                    }
                    ensure!(pending.len() < 64, "MCP relay capacity exceeded; inspect operation state before retrying");
                    let id = match &message { JsonRpcMessage::Request(r) => Some(r.id.clone()), _ => None };
                    let send = upstream.send(message);
                    pending.spawn(async move { (id, send.await) });
                }
                message = upstream.receive() => {
                    let Some(message) = message else { bail!("Preflight MCP connection closed; check the server and access token"); };
                    downstream.send(message).await?;
                }
                Some(sent) = pending.join_next(), if !pending.is_empty() => {
                    let (id, result) = sent.map_err(|_| anyhow::anyhow!("MCP relay task failed"))?;
                    if result.is_err() {
                        if let Some(id) = id {
                            downstream.send(JsonRpcMessage::error(ErrorData::internal_error("Upstream MCP request failed; its outcome may be unknown. Inspect operation state before retrying.", None), Some(id))).await?;
                        } else {
                            bail!("Preflight MCP control message failed; check the server and access token");
                        }
                    }
                }
            }
        }
        Ok(())
    }.await;
    pending.abort_all();
    let _ = tokio::time::timeout(Duration::from_secs(5), upstream.close()).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_reject_external_hosts_credentials_and_queries() {
        for value in ["http://127.0.0.1:4170/mcp", "http://[::1]:4170/mcp"] {
            assert!(endpoint(value).is_ok());
        }
        for value in [
            "http://localhost:4170/mcp",
            "https://127.0.0.1/mcp",
            "http://example.com/mcp",
            "http://user:secret@127.0.0.1/mcp",
            "http://127.0.0.1/mcp?token=secret",
            "http://127.0.0.1/",
        ] {
            let error = endpoint(value).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
    }
}
