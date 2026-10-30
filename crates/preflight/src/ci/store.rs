use super::*;
use crate::{
    model::{RunState, TestReport},
    pipeline::PipelineFile,
};
use providers::{RemoteArtifact, RemoteJob, RemoteRun};
use std::collections::BTreeSet;
pub(super) fn stage(id: &str) -> crate::pipeline::StageDefinition {
    serde_json::from_value(
        json!({"id":id,"command":{"program":"external-ci","args":[],"timeout_seconds":1}}),
    )
    .expect("fixed external stage")
}
pub(super) fn ensure_run(e: &Engine, s: &Source, remote: &RemoteRun, sync: &str) -> Result<String> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let key = s.key()?;
    let old:Option<(String,String)>=tx.query_row("SELECT run_id,metadata_json FROM ci_runs WHERE source_key=?1 AND provider_run_id=?2 AND provider_revision=?3",params![key,remote.id,remote.revision],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let id = if let Some((id, metadata)) = old {
        let previous: Value = serde_json::from_str(&metadata)?;
        ensure!(
            previous["git_sha"] == remote.metadata["git_sha"]
                && previous["provider_status"] == remote.metadata["provider_status"],
            invalid(
                "completed provider identity changed; preserve existing import and inspect provider revision"
            )
        );
        id
    } else {
        let id = uuid::Uuid::now_v7().to_string();
        let definition = stage("ci-summary");
        let file: PipelineFile =
            serde_json::from_value(json!({"version":1,"pipeline":{"stages":[definition]}}))?;
        let workspace = e.config.data_dir.join("workspaces").join(&id);
        crate::storage_gc::directory(&e.config.data_dir)?;
        let base = e.config.data_dir.join("workspaces");
        if !base.try_exists()? {
            std::fs::create_dir(&base)?;
        }
        crate::storage_gc::directory(&base)?;
        std::fs::create_dir(&workspace)?;
        tx.execute("INSERT INTO runs(id,project_id,status,command_json,created_at,finished_at,workspace,pipeline_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![id,s.project_id,remote.state.as_str(),serde_json::to_string(&definition.command)?,remote.created,remote.finished,workspace.to_string_lossy(),serde_json::to_string(&file)?])?;
        tx.execute("INSERT INTO stage_runs(id,run_id,name,ordinal,definition_json,status,started_at,finished_at) VALUES (?1,?2,'ci-summary',0,?3,?4,?5,?6)",params![uuid::Uuid::now_v7().to_string(),id,serde_json::to_string(&definition)?,remote.state.as_str(),remote.started,remote.finished])?;
        let pipeline_key = format!("ci:{}:{}", &key[..16], remote.workflow);
        tx.execute("INSERT INTO run_pipeline_identity(run_id,pipeline_key,name,saved_name) VALUES (?1,?2,?3,NULL)",params![id,pipeline_key,remote.name])?;
        tx.execute("INSERT INTO ci_runs(run_id,source_key,source_name,provider,provider_run_id,provider_revision,workflow_key,git_sha,web_url,metadata_json,coverage_json,progress_json,state,imported_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'{}',json_object('resumable',json('true')),'partial',?11,?11)",params![id,key,s.name,s.provider.name(),remote.id,remote.revision,remote.workflow,remote.sha,remote.url,remote.metadata.to_string(),crate::db::now()])?;
        tx.execute("INSERT INTO ci_jobs(run_id,stage_id,provider_job_id,metadata_json,log_state) VALUES (?1,'ci-summary','summary',?2,'complete')",params![id,json!({"name":"Workflow summary","role":"summary","web_url":remote.url,"provider_status":remote.metadata["provider_status"]}).to_string()])?;
        tx.execute("INSERT OR IGNORE INTO completion_events(run_id,project_id,status,finished_at) VALUES (?1,?2,?3,?4)",params![id,s.project_id,remote.state.as_str(),remote.finished])?;
        id
    };
    tx.execute(
        "INSERT OR IGNORE INTO ci_sync_runs VALUES (?1,?2)",
        params![sync, id],
    )?;
    tx.commit()?;
    Ok(id)
}
pub(super) fn job(e: &Engine, run: &str, j: &RemoteJob) -> Result<()> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM ci_jobs WHERE run_id=?1 AND stage_id=?2)",
        params![run, j.stage],
        |r| r.get(0),
    )?;
    if !exists {
        let count: u64 =
            tx.query_row("SELECT count(*) FROM ci_jobs WHERE run_id=?1", [run], |r| {
                r.get(0)
            })?;
        ensure!(count < 502, invalid("CI job count exceeds import limit"));
        let definition = stage(&j.stage);
        tx.execute("INSERT INTO stage_runs(id,run_id,name,ordinal,definition_json,status,started_at,finished_at,reason) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![uuid::Uuid::now_v7().to_string(),run,j.stage,count,serde_json::to_string(&definition)?,j.state.as_str(),j.started,j.finished,j.reason])?;
        tx.execute("INSERT INTO ci_jobs(run_id,stage_id,provider_job_id,metadata_json) VALUES (?1,?2,?3,?4)",params![run,j.stage,j.id,j.metadata.to_string()])?;
    }
    tx.commit()?;
    Ok(())
}
pub(super) fn update_definition(c: &rusqlite::Connection, run: &str) -> Result<()> {
    let definitions = c
        .prepare("SELECT definition_json FROM stage_runs WHERE run_id=?1 ORDER BY ordinal")?
        .query_map([run], |r| r.get::<_, String>(0))?
        .map(|v| -> Result<Value> { Ok(serde_json::from_str(&v?)?) })
        .collect::<Result<Vec<_>>>()?;
    c.execute(
        "UPDATE runs SET pipeline_json=?2 WHERE id=?1",
        params![
            run,
            json!({"version":1,"pipeline":{"max_parallel_stages":2,"stages":definitions}})
                .to_string()
        ],
    )?;
    Ok(())
}
pub(super) fn artifact(e: &Engine, run: &str, a: &RemoteArtifact) -> Result<()> {
    let c = e.db.connection()?;
    c.execute("INSERT INTO ci_artifacts(run_id,artifact_id,stage_id,name,provider_json,state) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(run_id,artifact_id) DO UPDATE SET provider_json=excluded.provider_json",params![run,a.id,a.stage,a.name,a.metadata.to_string(),if a.expired{"provider_expired"}else{"metadata"}])?;
    Ok(())
}
pub(super) fn progress(e: &Engine, run: &str) -> Result<Value> {
    let text: String = e.db.connection()?.query_row(
        "SELECT progress_json FROM ci_runs WHERE run_id=?1",
        [run],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&text)?)
}
pub(super) fn save_progress(e: &Engine, run: &str, progress: &Value) -> Result<()> {
    e.db.connection()?.execute(
        "UPDATE ci_runs SET progress_json=?2,updated_at=?3 WHERE run_id=?1",
        params![run, progress.to_string(), crate::db::now()],
    )?;
    Ok(())
}
pub(super) fn finish_run(
    e: &Engine,
    run: &str,
    progress: &Value,
    issues: &[Value],
) -> Result<Value> {
    let c = e.db.connection()?;
    let pending: u64 = c.query_row(
        "SELECT count(*) FROM ci_jobs WHERE run_id=?1 AND log_state!='complete'",
        [run],
        |r| r.get(0),
    )?;
    let tests: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM stage_runs WHERE run_id=?1 AND tests_json IS NOT NULL)",
        [run],
        |r| r.get(0),
    )?;
    let partial = progress["jobs_complete"] != true
        || progress["artifacts_complete"] != true
        || pending > 0
        || progress["tests_unavailable"] == true
        || !issues.is_empty();
    let coverage = json!({"jobs_complete":progress["jobs_complete"]==true,"artifact_metadata_complete":progress["artifacts_complete"]==true,"logs_complete":pending==0,"structured_tests":if tests{"published_report"}else{"unknown"},"resumable":progress["resumable"]==true,"test_issue":progress["test_issue"],"issues":issues.iter().take(20).collect::<Vec<_>>(),"issues_truncated":issues.len()>20,"timestamp_origin":"provider run/job timestamps; log timestamps are local ingestion observations","dependencies":"provider API does not establish full DAG dependencies"});
    c.execute("UPDATE ci_runs SET coverage_json=?2,progress_json=?3,state=?4,updated_at=?5 WHERE run_id=?1",params![run,coverage.to_string(),progress.to_string(),if partial{"partial"}else{"complete"},crate::db::now()])?;
    Ok(
        json!({"run_id":run,"partial":partial,"jobs_complete":coverage["jobs_complete"],"logs_complete":coverage["logs_complete"],"structured_tests":coverage["structured_tests"],"issue_count":issues.len(),"resumable":progress["resumable"]==true}),
    )
}
pub(super) fn pending_logs(
    e: &Engine,
    run: &str,
    force: bool,
) -> Result<Vec<(String, String, usize)>> {
    let c = e.db.connection()?;
    Ok(c.prepare("SELECT stage_id,provider_job_id,log_bytes FROM ci_jobs WHERE run_id=?1 AND provider_job_id!='summary' AND (log_state IN ('pending','partial') OR (?2 AND log_state='unavailable')) ORDER BY stage_id LIMIT 500")?.query_map(params![run,force],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
}
pub(super) fn log_state(e: &Engine, run: &str, stage: &str, state: &str) -> Result<()> {
    e.db.connection()?.execute(
        "UPDATE ci_jobs SET log_state=?3 WHERE run_id=?1 AND stage_id=?2",
        params![run, stage, state],
    )?;
    Ok(())
}
pub(super) fn append_log(
    e: &Engine,
    run: &str,
    stage: &str,
    offset: usize,
    raw: &[u8],
    text: &str,
    complete: bool,
) -> Result<()> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    crate::evidence_expiry::available(&tx, run, crate::evidence_expiry::Category::Logs)?;
    let sha = crate::hashing::hex(Sha256::digest(raw));
    let previous: Option<String> = tx
        .query_row(
            "SELECT sha256 FROM ci_log_parts WHERE run_id=?1 AND stage_id=?2 AND byte_offset=?3",
            params![run, stage, offset],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        ensure!(
            previous == sha,
            invalid("provider log bytes changed for an already imported window")
        );
        return Ok(());
    }
    let current: usize = tx.query_row(
        "SELECT log_bytes FROM ci_jobs WHERE run_id=?1 AND stage_id=?2",
        params![run, stage],
        |r| r.get(0),
    )?;
    ensure!(
        current == offset,
        invalid("CI log continuation changed; retry with current progress")
    );
    let mut sequence:i64=tx.query_row("SELECT COALESCE(max(sequence),0) FROM log_events WHERE run_id=?1 AND stage_id=?2 AND stream='stdout'",params![run,stage],|r|r.get(0))?;
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + 65536).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1
        }
        let piece = &text[start..end];
        sequence += 1;
        tx.execute("INSERT INTO log_events(run_id,stage_id,stream,sequence,timestamp,text,ingested_at) VALUES (?1,?2,'stdout',?3,?4,?5,?4)",params![run,stage,sequence,crate::db::now(),piece])?;
        tx.execute(
            "INSERT INTO log_search(rowid,text) VALUES (?1,?2)",
            params![tx.last_insert_rowid(), piece],
        )?;
        start = end;
    }
    tx.execute(
        "INSERT INTO ci_log_parts VALUES (?1,?2,?3,?4,?5)",
        params![run, stage, offset, sha, raw.len()],
    )?;
    tx.execute(
        "UPDATE ci_jobs SET log_bytes=?3,log_state=?4 WHERE run_id=?1 AND stage_id=?2",
        params![
            run,
            stage,
            offset + raw.len(),
            if complete { "complete" } else { "partial" }
        ],
    )?;
    tx.commit()?;
    Ok(())
}
pub(super) fn tests(e: &Engine, run: &str, report: TestReport) -> Result<()> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    let mut definition = stage("ci-tests");
    definition.kind = crate::pipeline::StageKind::Test;
    let status = if report.counts.failed > 0 {
        RunState::Failed
    } else {
        RunState::Succeeded
    };
    let previous: Option<String> = tx
        .query_row(
            "SELECT tests_json FROM stage_runs WHERE run_id=?1 AND name='ci-tests'",
            [run],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(previous) = previous {
        let old: TestReport = serde_json::from_str(&previous)?;
        for case in &old.cases {
            if let Some(new) = report.cases.iter().find(|v| v.id == case.id) {
                ensure!(
                    case.name == new.name
                        && case.status == new.status
                        && case.message == new.message,
                    "provider test evidence changed for an existing case; original evidence retained"
                );
            }
        }
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM stage_runs WHERE run_id=?1 AND name='ci-tests')",
        [run],
        |r| r.get(0),
    )?;
    if !exists {
        let ordinal: u64 = tx.query_row(
            "SELECT count(*) FROM stage_runs WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO stage_runs(id,run_id,name,ordinal,definition_json,status,tests_json) VALUES (?1,?2,'ci-tests',?3,?4,?5,?6)",params![uuid::Uuid::now_v7().to_string(),run,ordinal,serde_json::to_string(&definition)?,status.as_str(),serde_json::to_string(&report)?])?;
        tx.execute("INSERT INTO ci_jobs(run_id,stage_id,provider_job_id,metadata_json,log_state) VALUES (?1,'ci-tests','summary',?2,'complete')",params![run,json!({"name":"Published test report","role":"test_report"}).to_string()])?;
        update_definition(&tx, run)?;
    } else {
        tx.execute(
            "UPDATE stage_runs SET tests_json=?2,status=?3 WHERE run_id=?1 AND name='ci-tests'",
            params![run, serde_json::to_string(&report)?, status.as_str()],
        )?;
    }
    if !crate::evidence_expiry::expired(&tx, run, crate::evidence_expiry::Category::Logs)? {
        let mut sequence:i64=tx.query_row("SELECT COALESCE(max(sequence),0) FROM log_events WHERE run_id=?1 AND stage_id='ci-tests'",[run],|r|r.get(0))?;
        for case in &report.cases {
            if let Some(output) = &case.output {
                for (stream, text) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
                    if text.is_empty() {
                        continue;
                    }
                    let sha = crate::hashing::hex(Sha256::digest(text.as_bytes()));
                    let old:Option<String>=tx.query_row("SELECT sha256 FROM ci_test_outputs WHERE run_id=?1 AND case_id=?2 AND stream=?3",params![run,case.id,stream],|r|r.get(0)).optional()?;
                    if let Some(old) = old {
                        ensure!(
                            old == sha,
                            "provider test output changed for an existing case; original evidence retained"
                        );
                        continue;
                    }
                    sequence += 1;
                    tx.execute("INSERT INTO log_events(run_id,stage_id,stream,sequence,timestamp,text,ingested_at,test_id) VALUES (?1,'ci-tests',?2,?3,?4,?5,?4,?6)",params![run,stream,sequence,crate::db::now(),text,case.id])?;
                    tx.execute(
                        "INSERT INTO log_search(rowid,text) VALUES (?1,?2)",
                        params![tx.last_insert_rowid(), text],
                    )?;
                    tx.execute(
                        "INSERT INTO ci_test_outputs VALUES (?1,?2,?3,?4)",
                        params![run, case.id, stream, sha],
                    )?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(())
}
pub(super) fn selected_artifacts(e: &Engine, run: &str) -> Result<Vec<RemoteArtifact>> {
    let c = e.db.connection()?;
    Ok(c.prepare("SELECT artifact_id,stage_id,name,provider_json FROM ci_artifacts WHERE run_id=?1 ORDER BY artifact_id LIMIT 500")?.query_map([run],|r|{let metadata:Value=serde_json::from_str(&r.get::<_,String>(3)?).unwrap_or(Value::Null);Ok(RemoteArtifact{id:r.get(0)?,stage:r.get(1)?,name:r.get(2)?,expired:metadata["expired"]==true,metadata})})?.collect::<rusqlite::Result<Vec<_>>>()?)
}
#[allow(clippy::type_complexity)] // One bounded SQLite row; it never crosses the API boundary.
pub(super) fn get(e: &Engine, r: &Get) -> Result<Value> {
    let c = e.db.connection()?;
    let row:Option<(String,String,String,String,Option<String>,String,String,String)>=c.query_row("SELECT c.source_name,c.provider,c.provider_run_id,c.provider_revision,c.web_url,c.metadata_json,c.coverage_json,c.state FROM ci_runs c JOIN runs r ON r.id=c.run_id WHERE c.run_id=?1 AND r.project_id=?2",params![r.run_id,r.project_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?,v.get(3)?,v.get(4)?,v.get(5)?,v.get(6)?,v.get(7)?))).optional()?;
    let (s, p, id, revision, url, metadata, coverage, state) = row.ok_or(Fault::NotFound)?;
    let jobs=c.prepare("SELECT stage_id,metadata_json,log_state,log_bytes FROM ci_jobs WHERE run_id=?1 ORDER BY stage_id LIMIT 51 OFFSET ?2")?.query_map(params![r.run_id,r.offset],|v|Ok(json!({"stage_id":v.get::<_,String>(0)?,"metadata":serde_json::from_str::<Value>(&v.get::<_,String>(1)?).unwrap_or(Value::Null),"logs":v.get::<_,String>(2)?,"log_bytes":v.get::<_,u64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let artifacts=c.prepare("SELECT artifact_id,stage_id,name,provider_json,state,sha256,size,path,EXISTS(SELECT 1 FROM artifact_expiry x WHERE x.run_id=ci_artifacts.run_id AND x.stage_id=ci_artifacts.stage_id) FROM ci_artifacts WHERE run_id=?1 ORDER BY artifact_id LIMIT 51 OFFSET ?2")?.query_map(params![r.run_id,r.offset],|v|Ok(json!({"artifact_id":v.get::<_,String>(0)?,"stage_id":v.get::<_,String>(1)?,"name":v.get::<_,String>(2)?,"provider":serde_json::from_str::<Value>(&v.get::<_,String>(3)?).unwrap_or(Value::Null),"state":if v.get::<_,bool>(8)?{"artifact_expired".into()}else{v.get::<_,String>(4)?},"sha256":v.get::<_,Option<String>>(5)?,"size":v.get::<_,Option<u64>>(6)?,"path":v.get::<_,Option<String>>(7)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut count = r.limit.min(50);
    ensure!(
        count > 0 && (4096..=65536).contains(&r.max_bytes) && r.offset <= 100000,
        invalid("limit must be 1..50, offset 0..100000, max_bytes 4096..65536")
    );
    loop {
        let more = jobs.len() > count || artifacts.len() > count;
        let value = json!({"run_id":r.run_id,"project_id":r.project_id,"source":s,"provider":p,"provider_run_id":id,"revision":revision,"web_url":url,"metadata":serde_json::from_str::<Value>(&metadata)?,"coverage":serde_json::from_str::<Value>(&coverage)?,"import_state":state,"jobs":jobs.iter().take(count).collect::<Vec<_>>(),"artifacts":artifacts.iter().take(count).collect::<Vec<_>>(),"next_offset":more.then_some(r.offset+count),"read_only":true});
        if serde_json::to_vec(&value)?.len() <= r.max_bytes {
            return Ok(value);
        }
        ensure!(
            count > 1,
            invalid("CI metadata exceeds budget; increase max_bytes")
        );
        count -= 1;
    }
}
pub(super) fn lease<'a>(e: &'a Engine, run: &str) -> Result<crate::leases::Lease<'a>> {
    e.db.begin_operation(run, "ci_import", BTreeSet::from([format!("run:{run}")]))
}
