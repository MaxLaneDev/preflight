//! Project/pipeline navigation and bounded individual test/source evidence.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tests {
    pub query: Option<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub stage_id: Option<String>,
    pub project_id: String,
    pub run_id: String,
    #[serde(default)]
    pub offset: usize,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub project_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub test_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pipelines {
    pub project_id: String,
}
pub(crate) fn identity(
    pipeline: Option<&str>,
    command: &str,
    submission: Option<&Value>,
) -> (String, String, Option<String>) {
    if let Some(s) = submission
        && s["operation"] == "pipeline_retry"
        && let Some(key) = s["pipeline_identity"]["key"].as_str()
    {
        return (
            key.into(),
            s["pipeline_identity"]["name"]
                .as_str()
                .unwrap_or("Pipeline")
                .into(),
            s["pipeline_identity"]["saved"].as_str().map(str::to_owned),
        );
    }
    if let Some(s) = submission
        && s["operation"] == "pipeline_run_saved"
        && let Some(name) = s["request"]["name"].as_str()
    {
        return (format!("saved:{name}"), name.into(), Some(name.into()));
    }
    let digest = crate::hashing::hex(Sha256::digest(pipeline.unwrap_or(command).as_bytes()));
    let label = if pipeline.is_some() {
        "Pipeline"
    } else {
        "Command"
    };
    (
        format!("definition:{digest}"),
        format!("{label} {}", &digest[..12]),
        None,
    )
}
pub(crate) fn backfill(c: &rusqlite::Connection) -> Result<()> {
    let rows=c.prepare("SELECT r.id,r.pipeline_json,r.command_json,s.request_json FROM runs r LEFT JOIN submissions s ON s.run_id=r.id")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, pipeline, command, submission) in rows {
        let s = submission
            .map(|s| serde_json::from_str::<Value>(&s))
            .transpose()?;
        let (key, name, saved) = identity(pipeline.as_deref(), &command, s.as_ref());
        c.execute(
            "INSERT INTO run_pipeline_identity VALUES (?1,?2,?3,?4)",
            params![id, key, name, saved],
        )?;
    }
    Ok(())
}
pub fn pipelines(e: &Engine, r: &Pipelines) -> Result<Value> {
    e.db.project(&r.project_id)?;
    let c = e.db.connection()?;
    let rows=c.prepare("SELECT p.pipeline_key,p.name,p.saved_name,COUNT(*),MAX(r.id) FROM run_pipeline_identity p JOIN runs r ON r.id=p.run_id WHERE r.project_id=?1 AND NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=r.project_id AND t.restored_at IS NULL AND ((t.kind='pipeline' AND t.target_id=p.pipeline_key) OR (t.kind='run' AND t.target_id=r.id))) AND NOT EXISTS(SELECT 1 FROM shared_executions x WHERE x.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM experiment_runs x WHERE x.run_id=r.id) GROUP BY p.pipeline_key ORDER BY MAX(r.id) DESC LIMIT 101")?.query_map([&r.project_id],|r|Ok(json!({"pipeline_key":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"saved_name":r.get::<_,Option<String>>(2)?,"runs":r.get::<_,u64>(3)?,"latest_run":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(
        json!({"project_id":r.project_id,"truncated":rows.len()>100,"pipelines":rows.into_iter().take(100).collect::<Vec<_>>()}),
    )
}
pub fn tests(e: &Engine, r: &Tests) -> Result<Value> {
    ensure!(
        r.query
            .as_ref()
            .is_none_or(|q| q.len() <= 256 && !q.chars().any(char::is_control)),
        invalid("test query must be at most 256 bytes without control characters")
    );
    let run = e.db.run(&r.run_id)?;
    ensure!(run.project_id == r.project_id, Fault::NotFound);
    let mut values = Vec::new();
    let mut summaries = Vec::new();
    for stage in run.stages {
        let kind = crate::testing::kind(&stage.definition);
        if let Some(report) = crate::testing::observed(&e.db, &r.run_id, &stage)? {
            summaries.push(json!({"stage_id":stage.definition.id,"kind":kind,"counts":report.counts,"cases_complete":report.cases_complete,"case_count":report.cases.len()}));
            for case in report.cases {
                if r.stage_id
                    .as_ref()
                    .is_some_and(|id| id != &stage.definition.id)
                    || r.kind
                        .as_ref()
                        .is_some_and(|value| !value.is_empty() && value != kind)
                    || r.status
                        .as_ref()
                        .is_some_and(|value| value != "all" && value != &case.status)
                    || !crate::search::matches_query(
                        &format!(
                            "{} {} {} {} {}",
                            case.name, case.id, case.class_name, case.suite, stage.definition.id
                        ),
                        r.query.as_deref().unwrap_or(""),
                    )
                {
                    continue;
                }
                values.push(json!({"stage_id":stage.definition.id,"kind":kind,"native_fixture":stage.definition.fixtures.is_some(),"case":case}));
            }
        } else {
            summaries.push(json!({"stage_id":stage.definition.id,"kind":kind,"cases_complete":false,"case_count":0,"counts":null}));
        }
    }
    let total = values.len();
    let next = r.offset.saturating_add(100);
    Ok(
        json!({"project_id":r.project_id,"run_id":r.run_id,"stages":summaries,"total":total,"cases":values.into_iter().skip(r.offset).take(100).collect::<Vec<_>>(),"next_cursor":(next<total).then_some(next),"fixture_evidence":"native fixtures are included; fixtures_results/fixture_diff retain detailed checks"}),
    )
}
pub fn source(e: &Engine, r: &Source) -> Result<Value> {
    let run = e.db.run(&r.run_id)?;
    ensure!(run.project_id == r.project_id, Fault::NotFound);
    if run.external.is_some() {
        return Ok(
            json!({"available":false,"reason":"External test report; local source snapshot was not imported"}),
        );
    }
    if crate::evidence_expiry::expired(
        &*e.db.connection()?,
        &run.id,
        crate::evidence_expiry::Category::SourceSnapshots,
    )? {
        return Ok(json!({"available":false,"reason":"source_snapshots_expired"}));
    }
    let stage = run
        .stages
        .iter()
        .find(|s| s.definition.id == r.stage_id)
        .ok_or(Fault::NotFound)?;
    if let Some(action) = &stage.definition.fixtures {
        let case = action
            .suite
            .cases
            .iter()
            .find(|case| case.id == r.test_id)
            .ok_or(Fault::NotFound)?;
        let content = case.files.get(&case.execute.entry).ok_or(Fault::NotFound)?;
        let files=case.files.iter().map(|(path,text)|json!({"path":path,"bytes":text.len(),"sha256":crate::hashing::hex(Sha256::digest(text.as_bytes()))})).collect::<Vec<_>>();
        return Ok(
            json!({"available":true,"provenance":"recorded_fixture_definition","virtual":true,"path":case.execute.entry,"line":1,"code":content.chars().take(16000).collect::<String>(),"code_truncated":content.chars().count()>16000,"sha256":crate::hashing::hex(Sha256::digest(content.as_bytes())),"vscode_uri":null,"opens_snapshot":true,"files":files}),
        );
    }
    let case = stage
        .tests
        .as_ref()
        .and_then(|t| t.cases.iter().find(|c| c.id == r.test_id))
        .ok_or(Fault::NotFound)?;
    let Some(file) = &case.file else {
        return Ok(json!({"available":false,"reason":"test report has no source location"}));
    };
    crate::pipeline::relative(file)?;
    let relative = if stage.definition.cwd == "." {
        file.clone()
    } else {
        format!("{}/{}", stage.definition.cwd, file)
    };
    let root = if run.pipeline.is_some() {
        run.workspace.join("source")
    } else {
        run.workspace.clone()
    };
    let path = crate::pipeline::contained(&root, &relative)?;
    if !path.is_file() {
        return Ok(json!({"available":false,"reason":"reported source is not in captured inputs"}));
    }
    let bytes = crate::fixtures::read(&path, 256 * 1024)?;
    let sha = crate::hashing::hex(Sha256::digest(&bytes));
    let manifest =
        e.db.inputs(&r.run_id)?
            .context("source manifest unavailable")?;
    let key = relative.strip_prefix("./").unwrap_or(&relative);
    ensure!(
        manifest.entries.get(key).and_then(|v| v.sha256.as_ref()) == Some(&sha),
        invalid("source does not match captured input digest")
    );
    let project = e.db.project(&r.project_id)?;
    let original = crate::pipeline::contained(&project.root, &relative)?;
    let original_matches = original.is_file()
        && crate::fixtures::read(&original, 256 * 1024).is_ok_and(|v| v == bytes);
    let mut url = reqwest::Url::parse("vscode://file")?;
    let open_path = if original_matches { &original } else { &path };
    url.set_path(&format!(
        "{}:{}",
        open_path.to_string_lossy(),
        case.line.unwrap_or(1)
    ));
    Ok(
        json!({"available":true,"run_id":r.run_id,"stage_id":r.stage_id,"test_id":r.test_id,"path":path,"original_path":original,"original_matches":original_matches,"sha256":sha,"line":case.line,"code":String::from_utf8(bytes).context("source is not UTF-8")?,"vscode_uri":url.as_str(),"opens_snapshot":!original_matches}),
    )
}
