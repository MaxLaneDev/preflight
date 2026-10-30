#[allow(dead_code)]
mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use common::Fixture;
use futures_core::Stream;
use preflight::live::{self, Query};
use std::{pin::Pin, time::Duration};
use tower::ServiceExt;

#[tokio::test]
async fn streams_are_bounded_and_disconnected_consumers_release_capacity() {
    let f = Fixture::new();
    let query = || Query {
        hints: true,
        project_id: None,
        run_id: None,
        since: 0,
    };
    let responses = (0..live::MAX_STREAMS)
        .map(|_| live::subscribe(&f.engine, query()).unwrap())
        .collect::<Vec<_>>();
    assert!(live::subscribe(&f.engine, query()).is_err());
    let rejected = preflight::api::router(f.engine.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/live?hints=true")
                .header("Host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    let response = preflight::api::router(f.engine.clone())
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("Host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drop(responses);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(response) = live::subscribe(&f.engine, query()) {
                drop(response);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn hints_reach_consumers_after_database_changes_without_exposing_run_data() {
    let f = Fixture::new();
    let response = live::subscribe(
        &f.engine,
        Query {
            hints: true,
            project_id: None,
            run_id: None,
            since: 0,
        },
    )
    .unwrap();
    let mut stream = response.into_body().into_data_stream();
    let first = tokio::time::timeout(
        Duration::from_secs(3),
        std::future::poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let first = String::from_utf8_lossy(&first).to_string();
    assert!(first.contains("\"hint\":true"));
    assert!(!first.contains("project_id"));
    let project = f.project.root.join("another-project");
    std::fs::create_dir(&project).unwrap();
    f.engine
        .register(preflight::model::RegisterProject {
            name: "changed for hint check".into(),
            path: project.to_string_lossy().into_owned(),
        })
        .unwrap();
    let second = tokio::time::timeout(
        Duration::from_secs(3),
        std::future::poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_ne!(first, String::from_utf8_lossy(&second));
    let snapshot = live::snapshot(
        &f.engine,
        &Query {
            hints: false,
            project_id: Some(f.project.id.clone()),
            run_id: None,
            since: 0,
        },
    )
    .unwrap();
    assert!(snapshot.get("completion_cursor").is_some());
}
