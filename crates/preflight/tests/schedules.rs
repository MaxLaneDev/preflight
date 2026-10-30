#[allow(dead_code)]
mod common;
use preflight::{
    control::RunQuery,
    engine::Engine,
    profiles::{self, SavedStart},
    schedules as s,
};
use serde_json::{Value, json};
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn setup(f: &common::Fixture) {
    f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":f.project.id,"name":"check","expected_revision":0,"definition":{"version":1,"pipeline":{"stages":[{"id":"native","command":f.request(&[],5).command}]}}})).unwrap()).unwrap();
}
fn create(f: &common::Fixture, key: &str, at: i64) -> s::Create {
    serde_json::from_value(json!({"project_id":f.project.id,"name":key,"pipeline":{"name":"check","revision":1},"at_ms":at,"misfire":"run_once","grace_ms":1000,"request_id":key,"actor":"user","reason":"schedule test"})).unwrap()
}
fn id(f: &common::Fixture, v: &Value) -> s::Id {
    s::Id {
        project_id: f.project.id.clone(),
        schedule_id: v["schedule_id"].as_str().unwrap().into(),
    }
}
fn change(f: &common::Fixture, v: &Value, action: &str, key: &str) -> s::Change {
    serde_json::from_value(json!({"project_id":f.project.id,"schedule_id":v["schedule_id"],"action":action,"expected_revision":v["revision"],"request_id":key,"actor":"user","reason":"test control"})).unwrap()
}
fn due(project: &str) -> s::Due {
    s::Due {
        project_id: Some(project.into()),
        limit: 32,
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_due_idempotency_and_recovery_after_submission() {
    let f = common::Fixture::new();
    setup(&f);
    let request = create(&f, "once", now() - 10);
    let scheduled = s::create(&f.engine, &request).unwrap();
    assert_eq!(
        s::create(&f.engine, &request).unwrap()["schedule_id"],
        scheduled["schedule_id"]
    );
    let mut conflicting = request.clone();
    conflicting.at_ms += 1;
    assert!(s::create(&f.engine, &conflicting).is_err());
    let a = f.engine.clone();
    let b = f.engine.clone();
    let p = f.project.id.clone();
    let q = p.clone();
    let (one, two) = tokio::join!(
        tokio::spawn(async move { s::run_due(&a, &due(&p)) }),
        tokio::spawn(async move { s::run_due(&b, &due(&q)) })
    );
    one.unwrap().unwrap();
    two.unwrap().unwrap();
    let report = s::get(&f.engine, &id(&f, &scheduled)).unwrap();
    assert_eq!(report["state"], "submitted");
    let run = report["run_id"].as_str().unwrap().to_string();
    common::finished(&f.engine, &run).await;
    let runs = f
        .engine
        .db
        .query_runs(&RunQuery {
            project_id: Some(f.project.id.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
    // Simulate a crash after run insertion but before linking the schedule.
    let connection =
        rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE schedules SET state='dispatching',run_id=NULL WHERE id=?1",
            [scheduled["schedule_id"].as_str().unwrap()],
        )
        .unwrap();
    drop(connection);
    let config = f.engine.config.clone();
    let common::Fixture {
        temp,
        engine: e,
        project,
    } = f;
    drop(e);
    let e = Engine::open(config).unwrap();
    s::run_due(&e, &due(&project.id)).unwrap();
    let report = s::get(
        &e,
        &s::Id {
            project_id: project.id,
            schedule_id: scheduled["schedule_id"].as_str().unwrap().into(),
        },
    )
    .unwrap();
    assert_eq!(report["run_id"], run);
    drop(e);
    drop(temp);
}
#[tokio::test]
async fn pause_resume_cancel_misfire_and_future_absolute_time() {
    let f = common::Fixture::new();
    setup(&f);
    let v = s::create(&f.engine, &create(&f, "pause", now() - 100)).unwrap();
    let pause = change(&f, &v, "pause", "pause-request");
    let paused = s::change(&f.engine, &pause).unwrap();
    assert_eq!(paused["state"], "paused");
    assert!(
        s::run_due(&f.engine, &due(&f.project.id)).unwrap()["dispatched"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let resumed = s::change(&f.engine, &change(&f, &paused, "resume", "resume")).unwrap();
    assert_eq!(resumed["state"], "pending");
    assert_eq!(s::change(&f.engine, &pause).unwrap(), paused);
    let cancelled = s::change(&f.engine, &change(&f, &resumed, "cancel", "cancel")).unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    assert!(s::change(&f.engine, &change(&f, &resumed, "resume", "stale")).is_err());
    let mut skip = create(&f, "skip", now() - 10000);
    skip.misfire = s::Misfire::Skip;
    let skipped = s::create(&f.engine, &skip).unwrap();
    let future = s::create(&f.engine, &create(&f, "future", now() + 3600000)).unwrap();
    s::run_due(&f.engine, &due(&f.project.id)).unwrap();
    assert_eq!(
        s::get(&f.engine, &id(&f, &skipped)).unwrap()["state"],
        "skipped"
    );
    assert_eq!(
        s::get(&f.engine, &id(&f, &future)).unwrap()["state"],
        "pending"
    );
    assert!(
        s::get(
            &f.engine,
            &s::Id {
                project_id: "foreign".into(),
                schedule_id: future["schedule_id"].as_str().unwrap().into()
            }
        )
        .is_err()
    );
}
#[tokio::test]
async fn claimed_schedule_recovers_and_background_dispatch_stops() {
    let f = common::Fixture::new();
    setup(&f);
    let v = s::create(&f.engine, &create(&f, "recovered", now() - 100)).unwrap();
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    c.execute(
        "UPDATE schedules SET state='dispatching' WHERE id=?1",
        [v["schedule_id"].as_str().unwrap()],
    )
    .unwrap();
    drop(c);
    let worker = tokio::spawn(s::run(f.engine.clone()));
    let run = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let status = s::get(&f.engine, &id(&f, &v)).unwrap();
            if let Some(run) = status["run_id"].as_str() {
                break run.to_string();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    common::finished(&f.engine, &run).await;
    f.engine.shutdown().await.unwrap();
    worker.await.unwrap();
}
#[tokio::test]
async fn saved_start_key_is_the_same_during_recovery() {
    let f = common::Fixture::new();
    setup(&f);
    let v = s::create(&f.engine, &create(&f, "identity", now() - 100)).unwrap();
    let run = profiles::start(
        &f.engine,
        SavedStart {
            project_id: f.project.id.clone(),
            name: "check".into(),
            revision: Some(1),
            profile: None,
            idempotency_key: Some(format!("schedule:{}", v["schedule_id"].as_str().unwrap())),
        },
    )
    .unwrap();
    common::finished(&f.engine, &run.id).await;
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    c.execute(
        "UPDATE schedules SET state='dispatching' WHERE id=?1",
        [v["schedule_id"].as_str().unwrap()],
    )
    .unwrap();
    drop(c);
    s::run_due(&f.engine, &due(&f.project.id)).unwrap();
    assert_eq!(s::get(&f.engine, &id(&f, &v)).unwrap()["run_id"], run.id);
}

#[tokio::test]
async fn mcp_http_and_two_real_cli_invocations_do_not_duplicate() {
    use rmcp::{
        ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport,
    };
    let f = common::Fixture::new();
    setup(&f);
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
    let preview_request = json!({"at_ms":1793232000000_i64,"repeat":{"cron":"0 9 * * 1-5","timezone":"Asia/Yekaterinburg","overlap":"skip"},"count":3});
    let preview_response = client
        .call_tool(
            CallToolRequestParams::new("schedules_preview")
                .with_arguments(preview_request.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(preview_response.is_error, Some(true));
    let from_mcp: Value =
        serde_json::from_str(&preview_response.content[0].as_text().unwrap().text).unwrap();
    let from_http: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{address}/api/v1/schedules/preview"))
        .json(&preview_request)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(from_mcp, from_http);
    assert_eq!(from_mcp["at_ms"].as_array().unwrap().len(), 3);
    let editable = create(&f, "edit-transport", now() + 86_400_000);
    let created = s::create(&f.engine, &editable).unwrap();
    let mut definition = editable.clone();
    definition.name = "edited-transport".into();
    definition.request_id = "edit-transport-once".into();
    let update =
        json!({"schedule_id":created["schedule_id"],"expected_revision":1,"definition":definition});
    let edited = client
        .call_tool(
            CallToolRequestParams::new("schedules_update")
                .with_arguments(update.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(edited.is_error, Some(true));
    let edited: Value = serde_json::from_str(&edited.content[0].as_text().unwrap().text).unwrap();
    let replay: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{address}/api/v1/schedules/update"))
        .json(&update)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay, edited);
    assert_eq!(edited["name"], "edited-transport");
    let r = create(&f, "transport", now() - 100);
    let response = client
        .call_tool(
            CallToolRequestParams::new("schedules_create").with_arguments(
                serde_json::to_value(&r)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let v: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let paused: Value = http
        .post(format!("http://{address}/api/v1/schedules/change"))
        .json(&change(&f, &v, "pause", "pause-http"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(paused["state"], "paused");
    let response = client
        .call_tool(
            CallToolRequestParams::new("schedules_get").with_arguments(
                json!({"project_id":f.project.id,"schedule_id":v["schedule_id"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true));
    let read: Value = serde_json::from_str(&response.content[0].as_text().unwrap().text).unwrap();
    assert_eq!(read["revision"], paused["revision"]);
    let resumed: Value = http
        .post(format!("http://{address}/api/v1/schedules/change"))
        .json(&change(&f, &paused, "resume", "resume-http"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed["state"], "pending");
    let mut config = f.engine.config.clone();
    config.bind = address;
    let file = f.temp.path().join("cli.toml");
    std::fs::write(&file, toml::to_string(&config).unwrap()).unwrap();
    async fn cli(file: &std::path::Path) {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_preflight"))
                .arg("--config")
                .arg(file)
                .arg("run-due")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _: Value = serde_json::from_slice(&output.stdout).unwrap();
    }
    tokio::join!(cli(&file), cli(&file));
    let report = s::get(&f.engine, &id(&f, &v)).unwrap();
    assert_eq!(report["state"], "submitted");
    common::finished(&f.engine, report["run_id"].as_str().unwrap()).await;
    let runs = f
        .engine
        .db
        .query_runs(&RunQuery {
            project_id: Some(f.project.id.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
    client.cancel().await.unwrap();
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn interval_reuses_request_but_creates_distinct_occurrences_and_recovers() {
    let f = common::Fixture::new();
    setup(&f);
    let mut request = create(&f, "interval", now() - 120_000);
    request.repeat = Some(s::Repeat::Interval {
        every_seconds: 60,
        overlap: s::Overlap::Queue,
    });
    let original = s::create(&f.engine, &request).unwrap();
    s::run_due(&f.engine, &due(&f.project.id)).unwrap();
    let first = s::get(&f.engine, &id(&f, &original)).unwrap();
    assert_eq!(first["state"], "pending");
    assert!(first["at_ms"].as_i64().unwrap() > now());
    let first_run = first["run_id"].as_str().unwrap().to_string();
    common::finished(&f.engine, &first_run).await;
    assert_eq!(
        s::create(&f.engine, &request).unwrap()["schedule_id"],
        original["schedule_id"]
    );
    let conn =
        rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    let second_at = now() - 100;
    conn.execute(
        "UPDATE schedules SET at_ms=?2 WHERE id=?1",
        rusqlite::params![original["schedule_id"].as_str().unwrap(), second_at],
    )
    .unwrap();
    s::run_due(&f.engine, &due(&f.project.id)).unwrap();
    let second = s::get(&f.engine, &id(&f, &original)).unwrap();
    assert_ne!(second["run_id"], first["run_id"]);
    common::finished(&f.engine, second["run_id"].as_str().unwrap()).await;
    // Replay a dispatch after losing its final schedule update.
    conn.execute(
        "UPDATE schedules SET state='dispatching',at_ms=?2 WHERE id=?1",
        rusqlite::params![original["schedule_id"].as_str().unwrap(), second_at],
    )
    .unwrap();
    s::run_due(&f.engine, &due(&f.project.id)).unwrap();
    let replay = s::get(&f.engine, &id(&f, &original)).unwrap();
    assert_eq!(replay["run_id"], second["run_id"]);
    assert_eq!(replay["recent_occurrences"].as_array().unwrap().len(), 2);
    assert_eq!(f.engine.db.runs().unwrap().len(), 2);
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn interval_overlap_skip_and_queue_preserve_running_attempt() {
    for overlap in [s::Overlap::Skip, s::Overlap::Queue] {
        let f = common::Fixture::new();
        f.engine.db.save_pipeline(&serde_json::from_value(json!({"project_id":f.project.id,"name":"check","expected_revision":0,"definition":{"version":1,"pipeline":{"stages":[{"id":"wait","command":f.request(&["sleep"],30).command}]}}})).unwrap()).unwrap();
        let mut request = create(&f, "overlap", now() - 1000);
        request.repeat = Some(s::Repeat::Interval {
            every_seconds: 60,
            overlap: overlap.clone(),
        });
        let original = s::create(&f.engine, &request).unwrap();
        s::run_due(&f.engine, &due(&f.project.id)).unwrap();
        let first = s::get(&f.engine, &id(&f, &original)).unwrap();
        let conn =
            rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
        let at = now() - 10;
        conn.execute(
            "UPDATE schedules SET at_ms=?2 WHERE id=?1",
            rusqlite::params![original["schedule_id"].as_str().unwrap(), at],
        )
        .unwrap();
        s::run_due(&f.engine, &due(&f.project.id)).unwrap();
        let current = s::get(&f.engine, &id(&f, &original)).unwrap();
        assert_eq!(current["run_id"], first["run_id"]);
        assert_eq!(f.engine.db.runs().unwrap().len(), 1);
        match overlap {
            s::Overlap::Skip => {
                assert!(current["at_ms"].as_i64().unwrap() > now());
                assert_eq!(current["recent_occurrences"][0]["state"], "skipped");
            }
            s::Overlap::Queue => assert_eq!(current["at_ms"], at),
        }
        s::change(&f.engine, &change(&f, &current, "pause", "pause-repeat")).unwrap();
        assert!(
            !f.engine
                .db
                .run(first["run_id"].as_str().unwrap())
                .unwrap()
                .status
                .terminal()
        );
        f.engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn calendar_preview_dispatch_history_and_audited_delete() {
    let f = common::Fixture::new();
    setup(&f);
    let mut r = create(&f, "calendar", now() - 120_000);
    r.repeat = Some(s::Repeat::Calendar {
        cron: "* * * * *".into(),
        timezone: "Asia/Yekaterinburg".into(),
        overlap: s::Overlap::Queue,
    });
    let preview = s::preview(&s::Preview {
        at_ms: r.at_ms,
        repeat: r.repeat.clone(),
        count: 3,
    })
    .unwrap();
    let created = s::create(&f.engine, &r).unwrap();
    assert_eq!(created["at_ms"], preview["at_ms"][0]);
    s::run_due(&f.engine, &due(&f.project.id)).unwrap();
    let done = s::get(&f.engine, &id(&f, &created)).unwrap();
    assert_eq!(done["state"], "pending");
    common::finished(&f.engine, done["run_id"].as_str().unwrap()).await;
    let history = s::history(
        &f.engine,
        &s::History {
            project_id: f.project.id.clone(),
            schedule_id: created["schedule_id"].as_str().unwrap().into(),
            before_at: None,
        },
    )
    .unwrap();
    assert_eq!(history["occurrences"][0]["run_status"], "succeeded");
    let deleted = s::change(&f.engine, &change(&f, &done, "delete", "delete-calendar")).unwrap();
    assert_eq!(deleted["deleted"], true);
    assert!(
        s::list(
            &f.engine,
            &s::List {
                project_id: f.project.id.clone(),
                before: None
            }
        )
        .unwrap()["schedules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s::get(&f.engine, &id(&f, &created)).unwrap()["run_id"],
        done["run_id"]
    );
    assert!(s::change(&f.engine, &change(&f, &deleted, "resume", "revive")).is_err());
    assert!(
        s::history(
            &f.engine,
            &s::History {
                project_id: "other".into(),
                schedule_id: created["schedule_id"].as_str().unwrap().into(),
                before_at: None
            }
        )
        .is_err()
    );
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn occurrence_history_pages_without_duplicates() {
    let f = common::Fixture::new();
    setup(&f);
    let created = s::create(&f.engine, &create(&f, "history", now() + 60_000)).unwrap();
    let schedule = created["schedule_id"].as_str().unwrap().to_string();
    let c = rusqlite::Connection::open(f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    for at in 1..=25 {
        c.execute(
            "INSERT INTO schedule_occurrences VALUES (?1,?2,'skipped',NULL,'history fixture')",
            rusqlite::params![schedule, at],
        )
        .unwrap();
    }
    let first = s::history(
        &f.engine,
        &s::History {
            project_id: f.project.id.clone(),
            schedule_id: schedule.clone(),
            before_at: None,
        },
    )
    .unwrap();
    assert_eq!(first["occurrences"].as_array().unwrap().len(), 20);
    let second = s::history(
        &f.engine,
        &s::History {
            project_id: f.project.id.clone(),
            schedule_id: schedule,
            before_at: first["next_cursor"].as_i64(),
        },
    )
    .unwrap();
    assert_eq!(second["occurrences"].as_array().unwrap().len(), 5);
    assert!(second["next_cursor"].is_null());
    assert!(
        first["occurrences"].as_array().unwrap().last().unwrap()["at_ms"]
            .as_i64()
            .unwrap()
            > second["occurrences"][0]["at_ms"].as_i64().unwrap()
    );
    f.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn edit_preserves_identity_creation_replay_and_pause_state() {
    let f = common::Fixture::new();
    setup(&f);
    let original = create(&f, "editable", now() + 120_000);
    let first = s::create(&f.engine, &original).unwrap();
    let paused = s::change(&f.engine, &change(&f, &first, "pause", "pause-edit")).unwrap();
    let mut replacement = original.clone();
    replacement.name = "renamed".into();
    replacement.at_ms += 60_000;
    replacement.request_id = "edit-one".into();
    let request = s::Update {
        schedule_id: first["schedule_id"].as_str().unwrap().into(),
        expected_revision: paused["revision"].as_u64().unwrap(),
        definition: replacement,
    };
    let edited = s::update(&f.engine, &request).unwrap();
    assert_eq!(edited["schedule_id"], first["schedule_id"]);
    assert_eq!(edited["state"], "paused");
    assert_eq!(edited["name"], "renamed");
    assert_eq!(edited["at_ms"], request.definition.at_ms);
    assert_eq!(s::update(&f.engine, &request).unwrap(), edited);
    assert_eq!(s::create(&f.engine, &original).unwrap(), edited);
    let mut stale = request;
    stale.definition.request_id = "edit-stale".into();
    assert!(s::update(&f.engine, &stale).is_err());
    stale.expected_revision = edited["revision"].as_u64().unwrap();
    stale.definition.pipeline.revision = 999;
    assert!(s::update(&f.engine, &stale).is_err());
    let fetched = s::get(&f.engine, &id(&f, &edited)).unwrap();
    for (key, value) in edited.as_object().unwrap() {
        assert_eq!(&fetched[key], value);
    }
    let cancelled = s::change(&f.engine, &change(&f, &edited, "cancel", "cancel-edit")).unwrap();
    stale.expected_revision = cancelled["revision"].as_u64().unwrap();
    stale.definition.pipeline.revision = 1;
    assert!(s::update(&f.engine, &stale).is_err());
}
