//! Bundled Web UI assets shared by the server and compatibility client.
use axum::{Router, http::header, routing::get};

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            "/notification-center.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("notification-center.mjs"),
                )
            }),
        )
        .route(
            "/releases.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("releases.mjs"),
                )
            }),
        )
        .route(
            "/environment-editor.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("environment-editor.mjs"),
                )
            }),
        )
        .route(
            "/favicon-32.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../../../assets/branding/favicon-32.png").as_slice(),
                )
            }),
        )
        .route(
            "/apple-touch-icon.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../../../assets/branding/apple-touch-icon.png").as_slice(),
                )
            }),
        )
        .route(
            "/live-client.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("live-client.mjs"),
                )
            }),
        )
        .route(
            "/live-worker.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("live-worker.js"),
                )
            }),
        )
        .route(
            "/",
            get(|| async { axum::response::Html(include_str!("index.html")) }),
        )
        .route(
            "/executions",
            get(|| async { axum::response::Html(include_str!("index.html")) }),
        )
        .route(
            "/settings",
            get(|| async { axum::response::Html(include_str!("index.html")) }),
        )
        .route(
            "/projects/{*path}",
            get(|| async { axum::response::Html(include_str!("index.html")) }),
        )
        .route(
            "/log-window.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("log-window.mjs"),
                )
            }),
        )
        .route(
            "/virtual-list.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("virtual-list.mjs"),
                )
            }),
        )
        .route(
            "/console.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("console.mjs"),
                )
            }),
        )
        .route(
            "/ci.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("ci.mjs"),
                )
            }),
        )
        .route(
            "/ci-settings.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("ci-settings.mjs"),
                )
            }),
        )
        .route(
            "/runtime-settings.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("runtime-settings.mjs"),
                )
            }),
        )
        .route(
            "/settings.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    include_str!("settings.mjs"),
                )
            }),
        )
        .route(
            "/search.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    include_str!("search.mjs"),
                )
            }),
        )
        .route(
            "/routing.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("routing.mjs"),
                )
            }),
        )
        .route(
            "/retry.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("retry.mjs"),
                )
            }),
        )
        .route(
            "/interactions.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("interactions.mjs"),
                )
            }),
        )
        .route(
            "/localization.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("localization.mjs"),
                )
            }),
        )
        .route(
            "/diagnostics.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("diagnostics.mjs"),
                )
            }),
        )
        .route(
            "/archives.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("archives.mjs"),
                )
            }),
        )
        .route(
            "/management.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("management.mjs"),
                )
            }),
        )
        .route(
            "/executions.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("executions.mjs"),
                )
            }),
        )
        .route(
            "/editor-bundle.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("editor-bundle.mjs"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("app.js"),
                )
            }),
        )
        .route(
            "/selects.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("selects.mjs"),
                )
            }),
        )
        .route(
            "/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("app.css"),
                )
            }),
        )
}

pub fn is_asset(path: &str) -> bool {
    matches!(
        path,
        "/notification-center.mjs"
            | "/releases.mjs"
            | "/environment-editor.mjs"
            | "/favicon-32.png"
            | "/apple-touch-icon.png"
            | "/live-worker.js"
            | "/live-client.mjs"
            | "/"
            | "/executions"
            | "/settings"
            | "/log-window.mjs"
            | "/virtual-list.mjs"
            | "/console.mjs"
            | "/ci.mjs"
            | "/ci-settings.mjs"
            | "/runtime-settings.mjs"
            | "/settings.mjs"
            | "/search.mjs"
            | "/routing.mjs"
            | "/retry.mjs"
            | "/interactions.mjs"
            | "/localization.mjs"
            | "/diagnostics.mjs"
            | "/archives.mjs"
            | "/management.mjs"
            | "/executions.mjs"
            | "/editor-bundle.mjs"
            | "/app.js"
            | "/selects.mjs"
            | "/app.css"
    ) || path.starts_with("/projects/")
}
