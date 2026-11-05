#![cfg(unix)]
#[allow(dead_code)]
mod common;
use preflight::{catalog, model::RegisterProject};
use serde_json::{Value, json};
fn page(f: &common::Fixture, kind: &str, after: Option<String>, query: &str) -> Value {
    catalog::list(
        &f.engine,
        &serde_json::from_value(
            json!({"kind":kind,"project_id":f.project.id,"after":after,"query":query,"limit":25}),
        )
        .unwrap(),
    )
    .unwrap()
}
#[test]
fn pages_and_search_reach_beyond_hundred_without_duplicates() {
    let f = common::Fixture::new();
    for n in 0..130 {
        let root = f.temp.path().join(format!("workspace-{n:03}"));
        std::fs::create_dir(&root).unwrap();
        f.engine
            .register(RegisterProject {
                name: format!("Workspace {n:03}"),
                path: root.to_string_lossy().into_owned(),
            })
            .unwrap();
        for kind in ["pipelines", "experiments"] {
            let path = f
                .project
                .root
                .join(format!(".preflight/{kind}/item-{n:03}"));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("1.json"), "{}").unwrap();
        }
    }
    for (kind, total) in [
        ("workspaces", 131),
        ("pipelines", 130),
        ("saved_pipelines", 130),
        ("experiments", 130),
    ] {
        let mut seen = std::collections::BTreeSet::new();
        let mut cursor = None;
        loop {
            let result = page(&f, kind, cursor, "");
            let items = result["items"].as_array().unwrap();
            assert!(items.len() <= 25);
            for item in items {
                assert!(seen.insert(item["key"].as_str().unwrap().to_owned()));
            }
            cursor = result["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), total, "{kind}");
        assert_eq!(
            page(
                &f,
                kind,
                None,
                if kind == "workspaces" {
                    "Workspace 129"
                } else {
                    "129"
                }
            )["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    let q =
        serde_json::from_value(json!({"kind":"pipelines","project_id":f.project.id,"limit":101}))
            .unwrap();
    assert!(catalog::list(&f.engine, &q).is_err());
    let q = serde_json::from_value(json!({"kind":"experiments","project_id":"foreign"})).unwrap();
    assert!(catalog::list(&f.engine, &q).is_err());
}
#[tokio::test]
async fn live_stream_emits_changes_and_catalog_matches_http() {
    let f = common::Fixture::new();
    let mut request = f.request(&[], 5);
    request.command.program = "sh".into();
    request.command.args = vec!["-c".into(), "printf first; sleep 2; printf last".into()];
    let run = f.engine.start(request).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let engine = f.engine.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, preflight::api::router(engine))
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    let base = format!("http://{address}/api/v1");
    let response: Value = client
        .get(format!("{base}/catalog?kind=workspaces&limit=25"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response, page(&f, "workspaces", None, ""));
    let mut response = client
        .get(format!(
            "{base}/live?project_id={}&run_id={}",
            f.project.id, run.id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let first = tokio::time::timeout(std::time::Duration::from_secs(3), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("event: change"));
    common::finished(&f.engine, &run.id).await;
    let mut stream = String::new();
    for _ in 0..4 {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(3), response.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        stream.push_str(&String::from_utf8_lossy(&chunk));
        if stream.contains("succeeded") {
            break;
        }
    }
    assert!(stream.contains("succeeded"));
    assert!(stream.contains("log_cursor"));
    drop(response);
    assert_eq!(
        client
            .get(format!("{base}/live?project_id=foreign"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    server.abort();
}
#[tokio::test]
async fn global_sse_closes_on_engine_shutdown_with_reader_connected() {
    let f = common::Fixture::new();
    let engine = f.engine.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, preflight::api::router(engine))
            .await
            .unwrap();
    });
    let mut response = reqwest::get(format!("http://{addr}/api/v1/live"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.chunk().await.unwrap().is_some());
    f.engine.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while response.chunk().await.unwrap().is_some() {}
    })
    .await
    .expect("live SSE must close on shutdown");
    server.abort();
}

#[test]
fn workspace_and_pipeline_search_match_ids_paths_unicode_and_trimmed_terms() {
    let f = common::Fixture::new();
    for query in [
        format!("  {}  ", f.project.id),
        f.project.root.to_string_lossy().into_owned(),
    ] {
        assert_eq!(
            page(&f, "workspaces", None, &query)["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    let id = uuid::Uuid::now_v7().to_string();
    let hash = "f".repeat(64);
    let key = format!("definition:{hash}");
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    c.execute("INSERT INTO runs(id,project_id,status,command_json,created_at,finished_at,workspace) VALUES (?1,?2,'succeeded','{}',1,2,?3)",rusqlite::params![id,f.project.id,f.temp.path().to_str().unwrap()]).unwrap();
    c.execute("INSERT INTO run_pipeline_identity(run_id,pipeline_key,name) VALUES (?1,?2,'Компилятор 测试')",rusqlite::params![id,key]).unwrap();
    for query in ["  КОМПИЛЯТОР  ", "测试", &hash[..12], &key] {
        let result = page(&f, "pipelines", None, query);
        assert_eq!(
            result["items"].as_array().unwrap().len(),
            1,
            "{query}: {result}"
        );
    }
    let saved = f.project.root.join(".preflight/pipelines/unstarted");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::write(saved.join("1.json"), "{}").unwrap();
    assert_eq!(
        page(&f, "saved_pipelines", None, " saved:unstarted ")["items"][0]["name"],
        "unstarted"
    );
    for query in [
        id.clone(),
        f.project.id.clone(),
        hash[..12].into(),
        "  КОМПИЛЯТОР  ".into(),
    ] {
        let q = serde_json::from_value(
            serde_json::json!({"view":"recent","project_id":f.project.id,"query":query}),
        )
        .unwrap();
        let result = preflight::executions::list(&f.engine, &q).unwrap();
        assert_eq!(result["items"][0]["target_id"], id, "{query}: {result}");
    }
    assert!(
        page(&f, "pipelines", None, "%_ OR 1=1")["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pipeline_and_execution_pages_order_by_completion_not_creation() {
    let f = common::Fixture::new();
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    for (id, key, created, finished) in [
        ("older", "definition:z", 1, 500),
        ("newer", "definition:a", 2, 300),
    ] {
        c.execute("INSERT INTO runs(id,project_id,status,command_json,created_at,finished_at,workspace) VALUES (?1,?2,'succeeded','{}',?3,?4,'')",rusqlite::params![id,f.project.id,created,finished]).unwrap();
        c.execute(
            "INSERT INTO run_pipeline_identity(run_id,pipeline_key,name) VALUES (?1,?2,?2)",
            rusqlite::params![id, key],
        )
        .unwrap();
    }
    let first = catalog::list(
        &f.engine,
        &serde_json::from_value(json!({"kind":"pipelines","project_id":f.project.id,"limit":1}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["items"][0]["pipeline_key"], "definition:z");
    let second = catalog::list(&f.engine,&serde_json::from_value(json!({"kind":"pipelines","project_id":f.project.id,"limit":1,"after":first["next_cursor"]})).unwrap()).unwrap();
    assert_eq!(second["items"][0]["pipeline_key"], "definition:a");
    assert!(second["next_cursor"].is_null());
    for view in ["all", "recent"] {
        let rows = preflight::executions::list(
            &f.engine,
            &serde_json::from_value(json!({"view":view,"project_id":f.project.id})).unwrap(),
        )
        .unwrap();
        assert_eq!(rows["items"][0]["target_id"], "older");
    }
}
