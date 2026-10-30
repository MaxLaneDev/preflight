//! Optional, independently distributed client. No engine/database dependency.
use anyhow::{Result, ensure};
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
struct Connection {
    server: String,
    http: reqwest::Client,
}
fn validate_server(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        url.scheme() == "http"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "server must be a plain loopback HTTP origin"
    );
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    ensure!(
        host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        "server must be a loopback IP; use a local tunnel for remote servers"
    );
    Ok(value.trim_end_matches('/').into())
}
async fn guard(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let valid = host
        .and_then(|h| h.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|a| matches!(a.host(), "127.0.0.1" | "localhost" | "[::1]"));
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .map(|v| v.to_str().unwrap_or(""));
    if !valid || origin.is_some_and(|o| o.strip_prefix("http://") != host) {
        return (StatusCode::FORBIDDEN, "Local same-origin requests only").into_response();
    }
    let mut response = next.run(request).await;
    let mut nonce_bytes = [0u8; 16];
    if getrandom::fill(&mut nonce_bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let nonce = nonce_bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/html"))
    {
        let (parts, body) = response.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, 256 * 1024).await else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        response = Response::from_parts(
            parts,
            axum::body::Body::from(
                String::from_utf8_lossy(&bytes).replace("__EDITOR_STYLE_NONCE__", &nonce),
            ),
        );
    }
    response.headers_mut().insert("content-security-policy",format!("default-src 'self'; script-src 'self'; style-src 'self' 'nonce-{nonce}'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'").parse().unwrap());
    for (key, value) in [
        ("x-content-type-options", "nosniff"),
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(key),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}
async fn proxy(State(c): State<Arc<Connection>>, request: Request) -> Response {
    // Retired pages must release their per-tab streams when this client is upgraded.
    if request.uri().path() == "/api/v1/live"
        && !request
            .uri()
            .query()
            .is_some_and(|q| q.split('&').any(|p| p == "hints=true"))
    {
        return (StatusCode::GONE, axum::Json(serde_json::json!({"error":{"message":"Reload this page to use shared realtime updates"}}))).into_response();
    }
    let result = async {
        ensure!(
            matches!(
                *request.method(),
                axum::http::Method::GET | axum::http::Method::POST
            ),
            "unsupported method"
        );
        let uri = request
            .uri()
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("");
        ensure!(uri.starts_with("/api/v1/"), "unsupported API path");
        let url = format!("{}{uri}", c.server);
        let live = request.uri().path() == "/api/v1/live";
        let ci_artifact = request.uri().path() == "/api/v1/ci/artifact-file";
        let archive = ci_artifact || request.uri().path() == "/api/v1/archive/download";
        let archive_work = matches!(
            request.uri().path(),
            "/api/v1/archive/restore" | "/api/v1/archive/verify" | "/api/v1/storage-gc-execute"
        );
        let archive_import = request.uri().path() == "/api/v1/archive/import";
        let method = request.method().clone();
        let body_limit = if request.uri().path() == "/api/v1/pipeline-editor/check" {
            512 * 1024
        } else {
            64 * 1024
        };
        let body = if archive_import {
            reqwest::Body::wrap_stream(request.into_body().into_data_stream())
        } else {
            axum::body::to_bytes(request.into_body(), body_limit)
                .await?
                .into()
        };
        let mut remote = c
            .http
            .request(method, url)
            .timeout(Duration::from_secs(
                if live || archive || archive_work || archive_import {
                    3600
                } else {
                    30
                },
            ))
            .header(
                header::CONTENT_TYPE,
                if archive_import {
                    "application/octet-stream"
                } else {
                    "application/json"
                },
            )
            .body(body)
            .send()
            .await?;
        let status = remote.status();
        if (live || archive) && status.is_success() {
            let encrypted_archive = archive
                && !ci_artifact
                && remote.headers().get(header::CONTENT_TYPE).is_some_and(|v| {
                    v.to_str().ok().and_then(|v| v.split(';').next())
                        == Some("application/octet-stream")
                });
            ensure!(
                remote.headers().get(header::CONTENT_TYPE).is_some_and(|v| v
                    .as_bytes()
                    .starts_with(if live {
                        b"text/event-stream"
                    } else if ci_artifact {
                        b"application/zip"
                    } else if encrypted_archive {
                        b"application/octet-stream"
                    } else {
                        b"application/zstd"
                    })),
                "invalid streamed content type"
            );
            let disposition = remote.headers().get(header::CONTENT_DISPOSITION).cloned();
            let length = remote.headers().get(header::CONTENT_LENGTH).cloned();
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                loop {
                    let chunk =
                        tokio::select! { _=tx.closed()=>break, chunk=remote.chunk()=>chunk };
                    match chunk {
                        Ok(Some(bytes)) => {
                            if tx.send(Ok(bytes)).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = tx.send(Err(error)).await;
                            break;
                        }
                    }
                }
            });
            let mut response = (
                [
                    (
                        header::CONTENT_TYPE,
                        if live {
                            "text/event-stream"
                        } else if ci_artifact {
                            "application/zip"
                        } else if encrypted_archive {
                            "application/octet-stream"
                        } else {
                            "application/zstd"
                        },
                    ),
                    (
                        header::CACHE_CONTROL,
                        if live { "no-cache" } else { "no-store" },
                    ),
                ],
                axum::body::Body::from_stream(LiveBytes(rx)),
            )
                .into_response();
            if archive && let Some(value) = disposition {
                response
                    .headers_mut()
                    .insert(header::CONTENT_DISPOSITION, value);
            }
            if archive && let Some(value) = length {
                response.headers_mut().insert(header::CONTENT_LENGTH, value);
            }
            return Ok(response);
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = remote.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "response exceeds 4 MiB; use paged evidence"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok::<_, anyhow::Error>(
            (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                Bytes::from(bytes),
            )
                .into_response(),
        )
    }
    .await;
    result.unwrap_or_else(|error| {
        (
            StatusCode::BAD_GATEWAY,
            axum::Json(serde_json::json!({"error":{"message":error.to_string()}})),
        )
            .into_response()
    })
}
fn router(c: Arc<Connection>) -> Router {
    preflight_client::router()
        .route(
            "/connection",
            get(|State(c): State<Arc<Connection>>| async move {
                axum::Json(serde_json::json!({"server":c.server}))
            }),
        )
        .route("/api/v1/{*path}", any(proxy))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(guard))
        .with_state(c)
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut server = "http://127.0.0.1:4170".to_string();
    let mut token_env = None;
    let mut bind: SocketAddr = "127.0.0.1:4172".parse()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                println!(
                    "{}",
                    serde_json::json!({"component":"preflight-client","version":env!("CARGO_PKG_VERSION"),"minimum_protocol":"1.43.0","os":std::env::consts::OS,"arch":std::env::consts::ARCH})
                );
                return Ok(());
            }
            "--server" => {
                server = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing --server value"))?
            }
            "--token-env" => {
                token_env = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("missing --token-env value"))?,
                )
            }
            "--bind" => {
                bind = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing --bind value"))?
                    .parse()?
            }
            "--help" | "-h" => {
                println!(
                    "preflight-client [--server http://127.0.0.1:4170] [--bind 127.0.0.1:4172] [--token-env ENV_NAME]\nOpen the printed URL. No browser is launched automatically."
                );
                return Ok(());
            }
            _ => anyhow::bail!("unknown argument {arg}"),
        }
    }
    ensure!(bind.ip().is_loopback(), "client bind must be loopback");
    let mut headers = header::HeaderMap::new();
    if let Some(name) = token_env {
        let token = std::env::var(name).map_err(|_| {
            anyhow::anyhow!("API credential environment variable is not configured")
        })?;
        ensure!(
            (32..=512).contains(&token.len()) && token.bytes().all(|c| c.is_ascii_graphic()),
            "API credential must contain 32..512 non-whitespace ASCII characters"
        );
        let mut value = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
        value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, value);
    }
    let connection = Arc::new(Connection {
        server: validate_server(&server)?,
        http: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()?,
    });
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("Preflight client: http://{}", listener.local_addr()?);
    axum::serve(listener, router(connection))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

struct LiveBytes(tokio::sync::mpsc::Receiver<Result<Bytes, reqwest::Error>>);
impl futures_core::Stream for LiveBytes {
    type Item = Result<Bytes, reqwest::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[test]
    fn rejects_remote_origins_and_credentials() {
        for url in [
            "https://127.0.0.1",
            "http://example.com",
            "http://user@127.0.0.1",
            "http://127.0.0.1/path",
            "http://127.0.0.1?x",
        ] {
            assert!(validate_server(url).is_err(), "{url}");
        }
        assert!(validate_server("http://127.0.0.1:4170").is_ok());
    }
    #[tokio::test]
    async fn serves_separate_assets_and_rejects_cross_origin() {
        let c = Arc::new(Connection {
            server: "http://127.0.0.1:1".into(),
            http: reqwest::Client::new(),
        });
        for (host, origin, status) in [
            ("localhost", None, StatusCode::OK),
            ("evil.test", None, StatusCode::FORBIDDEN),
            ("localhost", Some("http://evil.test"), StatusCode::FORBIDDEN),
        ] {
            let mut builder = axum::http::Request::builder().uri("/").header("host", host);
            if let Some(origin) = origin {
                builder = builder.header("origin", origin);
            }
            let response = router(c.clone())
                .oneshot(builder.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);
        }
        for (path, status) in [
            (
                "/projects/project/pipelines/saved%3Abuild/runs/run/tests",
                StatusCode::OK,
            ),
            ("/projects/project/experiments", StatusCode::OK),
            ("/projects/project/schedules", StatusCode::OK),
            ("/routing.mjs", StatusCode::OK),
            ("/search.mjs", StatusCode::OK),
            ("/settings.mjs", StatusCode::OK),
            ("/favicon-32.png", StatusCode::OK),
            ("/apple-touch-icon.png", StatusCode::OK),
            ("/runtime-settings.mjs", StatusCode::OK),
            ("/console.mjs", StatusCode::OK),
            ("/virtual-list.mjs", StatusCode::OK),
            ("/log-window.mjs", StatusCode::OK),
            ("/localization.mjs", StatusCode::OK),
            ("/interactions.mjs", StatusCode::OK),
            ("/retry.mjs", StatusCode::OK),
            ("/messages-ru.mjs", StatusCode::NOT_FOUND),
            ("/settings", StatusCode::OK),
            ("/unknown-page", StatusCode::NOT_FOUND),
        ] {
            let response = router(c.clone())
                .oneshot(
                    axum::http::Request::builder()
                        .uri(path)
                        .header("host", "localhost")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{path}");
        }
    }
    #[tokio::test]
    async fn editor_styles_use_a_matching_nonce_without_inline_script_permissions() {
        let c = Arc::new(Connection {
            server: "http://127.0.0.1:1".into(),
            http: reqwest::Client::new(),
        });
        let response = router(c)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/executions")
                    .header("host", "localhost")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let policy = response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_owned();
        let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        let nonce = html
            .split("name=\"editor-style-nonce\" content=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert_eq!(nonce.len(), 32);
        assert!(nonce.bytes().all(|v| v.is_ascii_hexdigit()));
        assert!(policy.contains(&format!("'nonce-{nonce}'")));
        assert!(policy.contains("script-src 'self';"));
        assert!(!policy.contains("unsafe-inline"));
        assert!(!html.contains("__EDITOR_STYLE_NONCE__"));
    }
    #[tokio::test]
    async fn retired_per_page_streams_are_closed_without_contacting_upstream() {
        use tower::ServiceExt;
        let connection = Arc::new(Connection {
            server: "http://127.0.0.1:1".into(),
            http: reqwest::Client::new(),
        });
        let response = router(connection)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/live?project_id=old")
                    .header("Host", "localhost")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
    }
    #[tokio::test]
    async fn proxy_forwards_sse_before_upstream_finishes() {
        let upstream = Router::new().route(
            "/api/v1/live",
            get(|| async {
                let (tx, rx) = tokio::sync::mpsc::channel(4);
                tokio::spawn(async move {
                    tx.send(Ok(Bytes::from_static(b"event: change\ndata: first\n\n")))
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(700)).await;
                    let _ = tx
                        .send(Ok(Bytes::from_static(b"event: change\ndata: second\n\n")))
                        .await;
                });
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    axum::body::Body::from_stream(LiveBytes(rx)),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let connection = Arc::new(Connection {
            server: format!("http://{address}"),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            axum::serve(listener, router(connection)).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut response = tokio::time::timeout(
            Duration::from_millis(500),
            client
                .get(format!("http://{address}/api/v1/live?hints=true"))
                .send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let chunk = tokio::time::timeout(Duration::from_millis(500), response.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&chunk).contains("first"));
        let chunk = response.chunk().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&chunk).contains("second"));
        task.abort();
        proxy.abort();
    }
    #[tokio::test]
    async fn archive_import_proxy_streams_binary_beyond_json_limit() {
        let upstream = Router::new().route(
            "/api/v1/archive/import",
            axum::routing::post(|request: Request| async move {
                assert_eq!(
                    request.headers()[header::CONTENT_TYPE],
                    "application/octet-stream"
                );
                let bytes = axum::body::to_bytes(request.into_body(), 8 * 1024 * 1024)
                    .await
                    .unwrap();
                assert!(bytes.iter().all(|v| *v == 42));
                axum::Json(serde_json::json!({"received":bytes.len()}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let connection = Arc::new(Connection {
            server: format!("http://{addr}"),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
        });
        let size = 4 * 1024 * 1024 + 123;
        let response = router(connection)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/archive/import?project_id=test")
                    .header("host", "localhost")
                    .body(axum::body::Body::from(vec![42u8; size]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["received"], size);
        task.abort();
    }
    #[tokio::test]
    async fn archive_proxy_streams_beyond_json_limit_with_download_headers() {
        for (endpoint, mime, disposition, filename) in [
            (
                "/api/v1/archive/download",
                "application/zstd",
                "attachment; filename=\"test.tar.zst\"",
                "test.tar.zst",
            ),
            (
                "/api/v1/archive/download",
                "application/octet-stream",
                "attachment; filename=\"test.tar.zst.age\"",
                "test.tar.zst.age",
            ),
            (
                "/api/v1/ci/artifact-file",
                "application/zip",
                "attachment; filename=\"ci.zip\"",
                "ci.zip",
            ),
        ] {
            let data = vec![42u8; 4 * 1024 * 1024 + 123];
            let expected = data.len();
            let upstream = Router::new().route(
                endpoint,
                get(move || {
                    let data = data.clone();
                    async move {
                        (
                            [
                                (header::CONTENT_TYPE, mime),
                                (header::CONTENT_DISPOSITION, disposition),
                            ],
                            data,
                        )
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let upstream = tokio::spawn(async move {
                axum::serve(listener, upstream).await.unwrap();
            });
            let connection = Arc::new(Connection {
                server: format!("http://{addr}"),
                http: reqwest::Client::new(),
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                axum::serve(listener, router(connection)).await.unwrap();
            });
            let response =
                reqwest::get(format!("http://{addr}{endpoint}?project_id=p&archive_id=a"))
                    .await
                    .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()[header::CONTENT_TYPE], mime);
            assert!(
                response.headers()[header::CONTENT_DISPOSITION]
                    .to_str()
                    .unwrap()
                    .contains(filename)
            );
            assert_eq!(response.content_length(), Some(expected as u64));
            assert_eq!(response.bytes().await.unwrap().len(), expected);
            upstream.abort();
            task.abort();
        }
    }
}
