#[allow(dead_code)]
mod common;
use preflight::{
    engine::Engine,
    log_archive::{Query, Request},
};
use serde_json::Value;
fn snapshot(e: &Engine, run: &str, stage: Option<&str>) -> Value {
    let mut cursor = 0;
    let mut pages = Vec::new();
    loop {
        let page = e.db.stage_logs(run, stage, cursor).unwrap();
        let next = page.next_cursor;
        pages.push(serde_json::to_value(page).unwrap());
        if let Some(id) = next {
            cursor = id;
        } else {
            break;
        }
    }
    serde_json::json!(pages)
}
#[tokio::test]
async fn chunks_preserve_event_cursors_filters_search_and_restart() {
    let f = common::Fixture::new();
    let a = f.start(&[], 5);
    let b = f.start(&[], 5);
    common::finished(&f.engine, &a.id).await;
    common::finished(&f.engine, &b.id).await;
    for n in 0..600 {
        f.engine
            .db
            .append_stage_log(
                &a.id,
                if n % 2 == 0 { "native" } else { "other" },
                "stdout",
                100 + n,
                &format!("needle {n} Привет token=secret {}\n", "repeat ".repeat(20)),
            )
            .unwrap();
        if n % 50 == 0 {
            f.engine
                .db
                .append_stage_log(&b.id, "native", "stderr", 100 + n, "foreign")
                .unwrap();
        }
    }
    let all = snapshot(&f.engine, &a.id, None);
    let selected = snapshot(&f.engine, &a.id, Some("other"));
    let foreign = snapshot(&f.engine, &b.id, None);
    let search = || preflight::log_search::Query {
        project_id: f.project.id.clone(),
        query: "needle".into(),
        run_id: Some(a.id.clone()),
        stage_id: None,
        before: None,
        limit: 10,
    };
    let evidence = preflight::log_search::search(&f.engine, &search()).unwrap();
    assert!(!evidence.to_string().contains("token=secret"));
    let r = Request {
        project_id: f.project.id.clone(),
        run_id: a.id.clone(),
        max_chunks: 1,
    };
    let partial = f.engine.db.logs_archive(&r).unwrap();
    assert_eq!(partial["more"], true);
    assert_eq!(snapshot(&f.engine, &a.id, None), all);
    let result = f
        .engine
        .db
        .logs_archive(&Request {
            max_chunks: 16,
            ..r
        })
        .unwrap();
    assert_eq!(result["complete"], true);
    assert!(
        result["stored_payload_bytes"].as_u64().unwrap()
            < result["original_text_bytes"].as_u64().unwrap() / 2
    );
    assert_eq!(snapshot(&f.engine, &a.id, Some("other")), selected);
    assert_eq!(snapshot(&f.engine, &b.id, None), foreign);
    assert_eq!(
        preflight::log_search::search(&f.engine, &search()).unwrap(),
        evidence
    );
    let config = f.engine.config.clone();
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = f;
    drop(e);
    let e = Engine::open(config).unwrap();
    assert_eq!(snapshot(&e, &a.id, None), all);
    assert_eq!(
        e.db.logs_archive(&Request {
            project_id: project.id.clone(),
            run_id: a.id.clone(),
            max_chunks: 16
        })
        .unwrap(),
        result
    );
    assert!(
        e.db.logs_storage(&Query {
            project_id: "other-project".into(),
            run_id: a.id.clone()
        })
        .is_err()
    );
    let c = rusqlite::Connection::open(e.config.data_dir.join("preflight.sqlite3")).unwrap();
    c.execute(
        "UPDATE log_chunks SET payload=x'00' WHERE run_id=?1",
        [&a.id],
    )
    .unwrap();
    assert!(e.db.logs(&a.id, 0).is_err());
    drop(c);
    drop(e);
    drop(temp);
}
#[tokio::test]
async fn active_runs_rejected_oversized_records_retained_and_late_logs_resume() {
    let f = common::Fixture::new();
    let active = f.start(&["sleep"], 5);
    let r = Request {
        project_id: f.project.id.clone(),
        run_id: active.id.clone(),
        max_chunks: 4,
    };
    assert!(f.engine.db.logs_archive(&r).is_err());
    f.engine
        .cancel(preflight::model::CancelRun {
            run_id: active.id.clone(),
            actor: preflight::model::Actor::User,
            reason: "test".into(),
        })
        .unwrap();
    common::finished(&f.engine, &active.id).await;
    let text = "x".repeat(512 * 1024 + 1);
    f.engine
        .db
        .append_stage_log(&active.id, "native", "stdout", 100, &text)
        .unwrap();
    let before = snapshot(&f.engine, &active.id, None);
    let result = f.engine.db.logs_archive(&r).unwrap();
    assert_eq!(result["oversized_events_retained_raw"], 1);
    assert_eq!(result["more"], false);
    assert_eq!(snapshot(&f.engine, &active.id, None), before);
    f.engine
        .db
        .append_stage_log(&active.id, "native", "stdout", 101, "late log")
        .unwrap();
    assert_eq!(
        f.engine
            .db
            .logs_storage(&Query {
                project_id: f.project.id.clone(),
                run_id: active.id.clone()
            })
            .unwrap()["more"],
        true
    );
    let result = f.engine.db.logs_archive(&r).unwrap();
    assert_eq!(result["more"], false);
}
#[tokio::test]
async fn background_archive_stops_with_engine() {
    let f = common::Fixture::new();
    let a = f.start(&[], 5);
    common::finished(&f.engine, &a.id).await;
    let before = snapshot(&f.engine, &a.id, None);
    let worker = tokio::spawn(preflight::log_archive::run(f.engine.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if f.engine
                .db
                .logs_storage(&Query {
                    project_id: f.project.id.clone(),
                    run_id: a.id.clone(),
                })
                .unwrap()["complete"]
                == true
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(snapshot(&f.engine, &a.id, None), before);
    f.engine.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn mcp_archive_http_storage_and_reads_share_compressed_payloads() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    use serde_json::json;
    let f = common::Fixture::new();
    let run = f.start(&[], 5);
    common::finished(&f.engine, &run.id).await;
    let before = serde_json::to_value(f.engine.db.logs(&run.id, 0).unwrap()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let signal = stop.clone();
    let app = preflight::api::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(signal.cancelled_owned())
            .await
            .unwrap()
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    let response = client
        .call_tool(
            CallToolRequestParams::new("logs_archive").with_arguments(
                json!({"project_id":f.project.id,"run_id":run.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let value: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(value["complete"], true);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let status: Value = http
        .get(format!(
            "http://{address}/api/v1/logs-storage?project_id={}&run_id={}",
            f.project.id, run.id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status, value);
    let logs: Value = http
        .get(format!("http://{address}/api/v1/runs/{}/logs", run.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(logs, before);
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
}
