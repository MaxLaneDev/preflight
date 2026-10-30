//! Explicit expiry of run-owned output bytes; immutable result metadata survives.
use crate::{engine::Engine, model::invalid, storage_gc};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub project_id: String,
    pub pipeline_id: Option<String>,
    pub before: Option<String>,
    /// Explicitly acknowledge that published output bytes will become unavailable.
    pub expire_artifacts: bool,
    /// Only consider runs completed before this Unix millisecond timestamp.
    pub finished_before: i64,
    #[serde(default = "limit")]
    pub limit: usize,
}
fn limit() -> usize {
    20
}
pub(crate) fn path(e: &Engine, run: &str, stage: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(run)?;
    crate::pipeline::identifier(stage)?;
    let base = e.config.data_dir.join("workspaces");
    for p in [
        &e.config.data_dir,
        &base,
        &base.join(run),
        &base.join(run).join("artifacts"),
    ] {
        storage_gc::directory(p)?;
    }
    Ok(base.join(run).join("artifacts").join(stage))
}
pub(crate) fn protection(
    c: &rusqlite::Connection,
    run: &str,
    stage: &str,
) -> Result<Option<String>> {
    protection_except_cache(c, run, stage, None)
}
pub(crate) fn protection_except_cache(
    c: &rusqlite::Connection,
    run: &str,
    stage: &str,
    entry: Option<&str>,
) -> Result<Option<String>> {
    let state: String = c.query_row("SELECT status FROM runs WHERE id=?1", [run], |r| r.get(0))?;
    if state != "succeeded" {
        return Ok(Some(
            "only successful runs may expire; failure evidence retained".into(),
        ));
    }
    let pinned: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM artifact_pins WHERE run_id=?1 AND stage_id=?2 AND pinned=1)",
        params![run, stage],
        |r| r.get(0),
    )?;
    if pinned {
        return Ok(Some(
            "explicit artifact retention owner protects this stage".into(),
        ));
    }
    let busy:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM operation_leases l WHERE l.state IN ('active','abandoned') AND (l.run_id=?1 OR EXISTS(SELECT 1 FROM operation_resources o WHERE o.lease_id=l.id AND o.resource='run:'||?1)))",[run],|r|r.get(0))?;
    if busy {
        return Ok(Some("active or abandoned operation protects run".into()));
    }
    let cached:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM stage_cache WHERE producer_run=?1 AND producer_stage=?2 AND state='active' AND (?3 IS NULL OR id<>?3))",params![run,stage,entry],|r|r.get(0))?;
    if cached {
        return Ok(Some(
            "active build-cache entry; invalidate it separately first".into(),
        ));
    }
    let module:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN modules m ON m.project_id=r.project_id AND m.sha256=a.sha256 WHERE a.run_id=?1 AND a.stage_id=?2)",params![run,stage],|r|r.get(0))?;
    if module {
        return Ok(Some(
            "published module retains its producer artifact, including evicted copies".into(),
        ));
    }
    let experiment: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM experiment_runs WHERE run_id=?1 OR baseline_run=?1)",
        [run],
        |r| r.get(0),
    )?;
    if experiment {
        return Ok(Some("experiment or baseline evidence is retained".into()));
    }
    let root: String = c.query_row(
        "SELECT p.root FROM projects p JOIN runs r ON r.project_id=p.id WHERE r.id=?1",
        [run],
        |r| r.get(0),
    )?;
    if saved_baseline(&PathBuf::from(root), run)? {
        return Ok(Some(
            "saved experiment revision retains this baseline".into(),
        ));
    }
    let shared: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM shared_executions WHERE run_id=?1)",
        [run],
        |r| r.get(0),
    )?;
    if shared {
        return Ok(Some("shared execution evidence is retained".into()));
    }
    Ok(None)
}
fn saved_baseline(root: &std::path::Path, run: &str) -> Result<bool> {
    let directory = crate::control::config_path(root, ".preflight/experiments")?;
    if !directory.try_exists()? {
        return Ok(false);
    }
    storage_gc::directory(&directory)?;
    let mut count = 0;
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        storage_gc::directory(&entry.path())?;
        for revision in std::fs::read_dir(entry.path())? {
            let revision = revision?;
            count += 1;
            ensure!(
                count <= 10000,
                "too many saved experiment revisions to verify retention"
            );
            let bytes = crate::fixtures::read(&revision.path(), 65536)?;
            let value: Value = serde_json::from_slice(&bytes)?;
            if value["definition"]["baseline_run"].as_str() == Some(run) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn preview_sync(e: &Engine, r: Preview, pipeline_key: Option<String>) -> Result<Value> {
    ensure!(
        r.expire_artifacts,
        invalid("expire_artifacts=true explicitly acknowledges evidence-byte expiry")
    );
    ensure!(
        (1..=100).contains(&r.limit)
            && r.finished_before > 0
            && r.finished_before <= crate::db::now(),
        invalid(
            "limit must be 1..100 and finished_before must be a past Unix millisecond timestamp"
        )
    );
    e.db.project(&r.project_id)?;
    if let Some(p) = &r.pipeline_id {
        crate::pipeline::identifier(p)?;
    }
    let rows = {
        let c = e.db.connection()?;
        let mut q=c.prepare("SELECT r.id,s.name FROM runs r JOIN stage_runs s ON s.run_id=r.id WHERE r.project_id=?1 AND r.finished_at<?2 AND EXISTS(SELECT 1 FROM artifacts a WHERE a.run_id=r.id AND a.stage_id=s.name) AND (?3 IS NULL OR json_extract(s.definition_json,'$.cache.pipeline_id')=?3) AND (?4 IS NULL OR r.id||'/'||s.name<?4) AND (?6 IS NULL OR EXISTS(SELECT 1 FROM run_pipeline_identity p WHERE p.run_id=r.id AND p.pipeline_key=?6)) ORDER BY r.id DESC,s.name DESC LIMIT ?5")?;
        q.query_map(
            params![
                r.project_id,
                r.finished_before,
                r.pipeline_id,
                r.before,
                r.limit + 1,
                pipeline_key
            ],
            |v| Ok((v.get::<_, String>(0)?, v.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = rows.len() > r.limit;
    let mut entries = Vec::new();
    let mut items = Vec::new();
    let mut total = 0u64;
    let mut cursor = None;
    for (run, stage) in rows.into_iter().take(r.limit) {
        cursor = Some(format!("{run}/{stage}"));
        let candidate = (|| -> Result<Option<_>> {
            if let Some(reason) = protection(&*e.db.connection()?, &run, &stage)? {
                entries
                    .push(json!({"run_id":run,"stage_id":stage,"eligible":false,"reason":reason}));
                return Ok(None);
            }
            let p = path(e, &run, &stage)?;
            if !p.try_exists()? {
                entries.push(json!({"run_id":run,"stage_id":stage,"eligible":false,"reason":"output directory already absent"}));
                return Ok(None);
            }
            for a in
                e.db.artifacts(&run)?
                    .into_iter()
                    .filter(|a| a.stage_id == stage)
            {
                ensure!(a.path == p.join(&a.name), "artifact path is not canonical");
            }
            let result = storage_gc::inventory(&p)?;
            entries.push(json!({"run_id":run,"stage_id":stage,"eligible":true,"file_bytes":result.1,"path":p}));
            Ok(Some(result))
        })();
        match candidate {
            Ok(Some((hash, bytes, manifest))) => {
                total += bytes;
                items.push((run, stage, hash, bytes, serde_json::to_string(&manifest)?));
            }
            Ok(None) => {}
            Err(err) => entries.push(
                json!({"run_id":run,"stage_id":stage,"eligible":false,"reason":err.to_string()}),
            ),
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let plan = json!({"plan_id":id,"project_id":r.project_id,"pipeline_id":r.pipeline_id,"pipeline_key":pipeline_key,"category":"artifact_expiry","expire_artifacts":true,"finished_before":r.finished_before,"entries":entries,"estimated_file_bytes":total,"next_cursor":if more{cursor}else{None},"execute_tool":"storage_gc_execute","retained":["artifact metadata and SHA","run history","sources","tests and logs"],"effect":"approved output bytes become artifact_expired; rerun the pipeline to regenerate"});
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    tx.execute("INSERT INTO storage_gc_plans(id,project_id,pipeline_id,preview_json,created_at) VALUES (?1,?2,?3,?4,?5)",params![id,r.project_id,r.pipeline_id,plan.to_string(),crate::db::now()])?;
    for (i, (run, stage, hash, bytes, manifest)) in items.into_iter().enumerate() {
        tx.execute("INSERT INTO storage_gc_items(plan_id,ordinal,run_id,stage_id,fingerprint,manifest_json,bytes,state,object_key) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8)",params![id,i,run,stage,hash,manifest,bytes,format!("artifacts:{stage}")])?;
    }
    tx.commit()?;
    Ok(plan)
}
pub async fn preview(e: Arc<Engine>, r: Preview) -> Result<Value> {
    tokio::task::spawn_blocking(move || preview_sync(&e, r, None)).await?
}
impl crate::db::Db {
    pub fn artifacts_report(&self, run: &str) -> Result<Vec<Value>> {
        self.run(run)?;
        let artifacts = self.artifacts(run)?;
        let c = self.connection()?;
        artifacts
            .into_iter()
            .map(|a| {
                let expired: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM artifact_expiry WHERE run_id=?1 AND stage_id=?2)",
                    params![a.run_id, a.stage_id],
                    |r| r.get(0),
                )?;
                let available = !expired && a.path.try_exists()?;
                let pinned:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM artifact_pins WHERE run_id=?1 AND stage_id=?2 AND pinned=1)",params![a.run_id,a.stage_id],|r|r.get(0))?;
                let mut v = serde_json::to_value(a)?;
                v["stage_retained_by_pins"]=json!(pinned);
                v["availability"] = json!(if expired {
                    "artifact_expired"
                } else if available {
                    "available"
                } else {
                    "artifact_missing"
                });
                Ok(v)
            })
            .collect()
    }
    pub(crate) fn require_artifact_available(&self, run: &str, stage: &str) -> Result<()> {
        let expired: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifact_expiry WHERE run_id=?1 AND stage_id=?2)",
            params![run, stage],
            |r| r.get(0),
        )?;
        ensure!(
            !expired,
            invalid("artifact_expired: output bytes were explicitly released by retention")
        );
        Ok(())
    }
}

pub async fn preview_pipeline(e: Arc<Engine>, r: Preview, key: Option<String>) -> Result<Value> {
    tokio::task::spawn_blocking(move || preview_sync(&e, r, key)).await?
}
