#[allow(dead_code)]
mod common;
use common::Fixture;
use preflight::{control, search};
use rusqlite::params;
use serde_json::{Value, json};
fn seed(f: &Fixture) -> String {
    let id = uuid::Uuid::now_v7().to_string();
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    c.execute("INSERT INTO runs(id,project_id,status,command_json,created_at,finished_at,workspace) VALUES (?1,?2,'succeeded','{}',1,2,?3)",params![id,f.project.id,f.temp.path().to_str().unwrap()]).unwrap();
    c.execute("INSERT INTO run_pipeline_identity(run_id,pipeline_key,name) VALUES (?1,'definition:123456789abcdef','Compiler pipeline')",[&id]).unwrap();
    c.execute("INSERT INTO stage_runs(id,run_id,name,ordinal,definition_json,status,tests_json) VALUES (?1,?2,'compiler',0,'{}','succeeded',?3)",params![uuid::Uuid::now_v7().to_string(),id,json!({"cases":[{"id":"test-uuid-123","name":"nested::ParseWidget","class_name":"Parser","suite":"Unit","status":"passed"}]}).to_string()]).unwrap();
    c.execute("INSERT INTO fixture_results(run_id,stage_id,case_id,ordinal,status) VALUES (?1,'compiler','fixture/echo',12,'succeeded')",[&id]).unwrap();
    c.execute("INSERT INTO artifacts(run_id,stage_id,name,kind,sha256,size,path) VALUES (?1,'compiler','compiler.exe','executable',?2,1,'/captured/compiler')",params![id,"a".repeat(64)]).unwrap();
    c.execute(
        "INSERT INTO modules(project_id,sha256,artifact_json) VALUES (?1,?2,?3)",
        params![
            f.project.id,
            "a".repeat(64),
            json!({"run_id":id,"stage_id":"compiler"}).to_string()
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO run_inputs(run_id,manifest_json) VALUES (?1,?2)",
        params![
            id,
            json!({"entries":{"src/main.sx":{"kind":"file","sha256":"b".repeat(64)}}}).to_string()
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO run_sources(run_id,request_json) VALUES (?1,?2)",
        params![id, json!({"commit":"c".repeat(40)}).to_string()],
    )
    .unwrap();
    c.execute(
        "INSERT INTO stage_keys(run_id,stage_id,evidence_json) VALUES (?1,'compiler',?2)",
        params![id, json!({"sha256":"e".repeat(64)}).to_string()],
    )
    .unwrap();
    f.engine
        .db
        .append_stage_log(
            &id,
            "compiler",
            "stderr",
            1,
            &format!("Parser exploded token={}\n", "d".repeat(64)),
        )
        .unwrap();
    id
}
fn find(f: &Fixture, q: &str) -> Value {
    search::search(
        &f.engine,
        &serde_json::from_value(json!({"query":q})).unwrap(),
    )
    .unwrap()
}
#[test]
fn ids_hashes_test_names_sources_and_literal_search_return_targets() {
    let f = Fixture::new();
    let id = seed(&f);
    for (term, kind) in [
        (&id[..], "run"),
        ("ParseWidget", "test"),
        ("fixture/echo", "test"),
        ("aaaaaaaaaaaa", "artifact"),
        ("bbbbbbbbbbbb", "input"),
        ("cccccccccccc", "source"),
        ("eeeeeeeeeeee", "stagekey"),
        ("Compiler pipeline", "pipeline"),
    ] {
        let v = find(&f, term);
        assert!(
            v["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["kind"] == kind),
            "{term}: {v}"
        );
    }
    let v = find(&f, "ParseWidget");
    assert_eq!(v["results"][0]["target"]["test"], "nested::ParseWidget");
    assert_eq!(v["results"][0]["target"]["run"], id);
    assert!(
        find(&f, "%_ OR 1=1")["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        find(&f, "dddddddddddd")["results"]
            .as_array()
            .unwrap()
            .is_empty(),
        "raw credential must not be searched"
    );
    assert_eq!(find(&f, "#1")["results"][0]["title"], "Log #1");
    let logs = find(&f, "Parser exploded");
    assert!(
        logs["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "log")
    );
    assert!(!logs.to_string().contains(&"d".repeat(64)));
}
#[test]
fn pagination_unicode_scope_and_hidden_runs() {
    let f = Fixture::new();
    let id = seed(&f);
    let root = f.temp.path().join("other");
    std::fs::create_dir(&root).unwrap();
    let project = f
        .engine
        .register(serde_json::from_value(json!({"name":"ДРУГОЙ ПРОЕКТ","path":root})).unwrap())
        .unwrap();
    assert_eq!(find(&f, "другой")["results"][0]["project_id"], project.id);
    let v = search::search(
        &f.engine,
        &serde_json::from_value(json!({"query":"другой","project_id":f.project.id})).unwrap(),
    )
    .unwrap();
    assert!(v["results"].as_array().unwrap().is_empty());
    let mut after = None;
    let mut keys = std::collections::BTreeSet::new();
    for _ in 0..30 {
        let v = search::search(
            &f.engine,
            &search::Query {
                query: "compiler".into(),
                project_id: None,
                after,
                limit: 1,
            },
        )
        .unwrap();
        for row in v["results"].as_array().unwrap() {
            assert!(
                keys.insert(row["key"].as_str().unwrap().to_owned()),
                "duplicate page row"
            );
        }
        after = v["next_cursor"].as_str().map(str::to_owned);
        if after.is_none() {
            break;
        }
    }
    assert!(keys.len() >= 3);
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    c.execute(
        "INSERT INTO management_trash(project_id,kind,target_id,deleted_at) VALUES (?1,'run',?2,1)",
        params![f.project.id, id],
    )
    .unwrap();
    assert!(
        find(&f, "ParseWidget")["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        find(&f, "Parser exploded")["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn saved_configs_and_validation() {
    let f = Fixture::new();
    let r=serde_json::from_value(json!({"project_id":f.project.id,"name":"release","expected_revision":0,"definition":{"version":1,"pipeline":{"stages":[{"id":"build","command":{"program":"echo","args":["ok"],"timeout_seconds":30}}]}}})).unwrap();
    let saved = control::save(&f.project.root, &r).unwrap();
    assert!(
        find(&f, "release")["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["target"]["pipeline"] == "saved:release")
    );
    assert!(
        !find(&f, saved["sha256"].as_str().unwrap())["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for r in [
        json!({"query":""}),
        json!({"query":"ok","limit":101}),
        json!({"query":"ok","after":"bad"}),
        json!({"query":"ok","project_id":"missing"}),
    ] {
        assert!(search::search(&f.engine, &serde_json::from_value(r).unwrap()).is_err());
    }
}
#[tokio::test]
async fn mcp_and_http_share_global_results_and_project_deep_links() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = Fixture::new();
    seed(&f);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let app = preflight::api::router(f.engine.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{url}/mcp"
        )))
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .call_tool(
            CallToolRequestParams::new("search_global")
                .with_arguments(json!({"query":"ParseWidget"}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let mcp: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let rest: Value = http
        .get(format!("{url}/api/v1/search?query=ParseWidget"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rest, mcp);
    let project: Value = http
        .get(format!("{url}/api/v1/projects/{}", f.project.id))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(project["id"], f.project.id);
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
    f.engine.shutdown().await.unwrap();
}
