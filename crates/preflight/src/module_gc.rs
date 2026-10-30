//! Reclaim unpinned, unused module copies while retaining producer artifacts and history.
use crate::{
    engine::Engine,
    model::invalid,
    storage_gc::{self, Preview},
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) fn path(e: &Engine, project: &str, digest: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(project)?;
    crate::modules::validate_digest(digest)?;
    let parent = e.config.data_dir.join("modules").join(project);
    storage_gc::directory(&e.config.data_dir)?;
    storage_gc::directory(&e.config.data_dir.join("modules"))?;
    storage_gc::directory(&parent)?;
    Ok(parent.join(digest))
}
pub(crate) fn protection(
    c: &rusqlite::Connection,
    project: &str,
    digest: &str,
    scope: Option<&str>,
    path: &Path,
) -> Result<Option<String>> {
    let pinned: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM module_pins WHERE project_id=?1 AND sha256=?2 AND pinned=1)",
        params![project, digest],
        |r| r.get(0),
    )?;
    if pinned {
        return Ok(Some("module has an explicit retention pin".into()));
    }
    // Include all projects: legacy module resource names are digest-scoped, not project-scoped.
    let busy: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM operation_resources o JOIN operation_leases l ON l.id=o.lease_id WHERE o.resource='module:'||?1 AND l.state IN ('active','abandoned'))",[digest],|r|r.get(0))?;
    if busy {
        return Ok(Some("active or abandoned module operation".into()));
    }
    let used: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM module_inputs i JOIN runs r ON r.id=i.run_id WHERE r.project_id=?1 AND json_extract(i.artifact_json,'$.sha256')=?2)",params![project,digest],|r|r.get(0))?;
    if used {
        return Ok(Some("module has retained consumer references".into()));
    }
    let mut q = c.prepare("SELECT r.status,s.definition_json FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN stage_runs s ON s.run_id=a.run_id AND s.name=a.stage_id WHERE r.project_id=?1 AND a.sha256=?2")?;
    for row in q.query_map(params![project, digest], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (state, definition) = row?;
        if state != "succeeded" {
            return Ok(Some("producer is not successfully completed".into()));
        }
        if let Some(scope) = scope {
            let d: Value = serde_json::from_str(&definition)?;
            if d["cache"]["pipeline_id"].as_str() != Some(scope) {
                return Ok(Some(
                    "module is shared with another pipeline or an unscoped producer".into(),
                ));
            }
        }
    }
    // Artifacts are normally separate copies; never delete a directory containing any retained evidence.
    let mut q = c.prepare("SELECT path FROM artifacts UNION ALL SELECT path FROM module_inputs")?;
    for p in q.query_map([], |r| r.get::<_, String>(0))? {
        if Path::new(&p?).starts_with(path) {
            return Ok(Some(
                "retained artifact or input resides in this directory".into(),
            ));
        }
    }
    let encoded: String = c.query_row(
        "SELECT artifact_json FROM modules WHERE project_id=?1 AND sha256=?2",
        params![project, digest],
        |r| r.get(0),
    )?;
    let mut artifact: crate::model::Artifact = serde_json::from_str(&encoded)?;
    let original=c.query_row("SELECT path FROM artifacts WHERE run_id=?1 AND stage_id=?2 AND name=?3 AND sha256=?4 AND size=?5",params![artifact.run_id,artifact.stage_id,artifact.name,digest,artifact.size],|r|r.get::<_,String>(0));
    let Ok(original) = original else {
        return Ok(Some("retained producer artifact is missing".into()));
    };
    artifact.path = PathBuf::from(original);
    // Reuse bounded verification for files and trees; never discard the last verified copy.
    let checked = tempfile::tempdir()?;
    if crate::artifacts::verify_copy(&artifact, &checked.path().join("verified")).is_err() {
        return Ok(Some("retained producer artifact cannot be verified".into()));
    }
    Ok(None)
}
fn preview_sync(e: &Engine, r: Preview) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    e.db.project(&r.project_id)?;
    if let Some(scope) = &r.pipeline_id {
        crate::pipeline::identifier(scope)?;
    }
    if let Some(cursor) = &r.before {
        crate::modules::validate_digest(cursor)?;
    }
    if let Some(cutoff) = r.finished_before {
        ensure!(
            cutoff > 0 && cutoff <= crate::db::now(),
            crate::model::invalid("finished_before must be a past Unix millisecond timestamp")
        );
    }
    let rows = {
        let c = e.db.connection()?;
        let mut q=c.prepare("SELECT m.sha256,m.artifact_json FROM modules m WHERE m.project_id=?1 AND (?2 IS NULL OR m.sha256<?2) AND (?3 IS NULL OR EXISTS(SELECT 1 FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN stage_runs s ON s.run_id=a.run_id AND s.name=a.stage_id WHERE r.project_id=m.project_id AND a.sha256=m.sha256 AND json_extract(s.definition_json,'$.cache.pipeline_id')=?3)) AND (?5 IS NULL OR EXISTS(SELECT 1 FROM module_publications t WHERE t.project_id=m.project_id AND t.sha256=m.sha256 AND t.published_at<?5)) ORDER BY m.sha256 DESC LIMIT ?4")?;
        q.query_map(
            params![
                r.project_id,
                r.before,
                r.pipeline_id,
                r.limit + 1,
                r.finished_before
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = rows.len() > r.limit;
    let mut entries = Vec::new();
    let mut items = Vec::new();
    let mut total = 0u64;
    let mut cursor = None;
    for (digest, artifact) in rows.into_iter().take(r.limit) {
        cursor = Some(digest.clone());
        let artifact: crate::model::Artifact = serde_json::from_str(&artifact)?;
        let candidate = (|| -> Result<Option<(String, u64, Vec<storage_gc::Removal>)>> {
            let p = path(e, &r.project_id, &digest)?;
            if !p.try_exists()? {
                entries
                    .push(json!({"sha256":digest,"eligible":false,"reason":"copy already absent"}));
                return Ok(None);
            }
            // The database cannot expand a deletion beyond the derived digest directory.
            ensure!(
                artifact.path == p.join("content"),
                "module storage path is not canonical"
            );
            if let Some(reason) = protection(
                &*e.db.connection()?,
                &r.project_id,
                &digest,
                r.pipeline_id.as_deref(),
                &p,
            )? {
                entries.push(json!({"sha256":digest,"eligible":false,"reason":reason}));
                return Ok(None);
            }
            let result = storage_gc::inventory(&p)?;
            entries.push(json!({"sha256":digest,"eligible":true,"path":p,"file_bytes":result.1}));
            Ok(Some(result))
        })();
        match candidate {
            Ok(Some((hash, bytes, manifest))) => {
                total += bytes;
                items.push((
                    artifact,
                    digest,
                    hash,
                    bytes,
                    serde_json::to_string(&manifest)?,
                ));
            }
            Ok(None) => {}
            Err(error) => {
                entries.push(json!({"sha256":digest,"eligible":false,"reason":error.to_string()}))
            }
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let plan = json!({"plan_id":id,"project_id":r.project_id,"pipeline_id":r.pipeline_id,"category":"module_copies","finished_before":r.finished_before,"entries":entries,"estimated_file_bytes":total,"next_cursor":if more{cursor}else{None},"retained":["producer artifacts","module identities and contracts","run history","sources","test/log evidence"],"execute_tool":"storage_gc_execute","status_tool":"storage_gc_status","effect":"evicts an unused published copy; republish its producer artifact to restore module availability"});
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    tx.execute("INSERT INTO storage_gc_plans(id,project_id,pipeline_id,preview_json,created_at) VALUES (?1,?2,?3,?4,?5)",params![id,r.project_id,r.pipeline_id,plan.to_string(),crate::db::now()])?;
    for (i, (artifact, digest, hash, bytes, manifest)) in items.into_iter().enumerate() {
        tx.execute("INSERT INTO storage_gc_items(plan_id,ordinal,run_id,stage_id,fingerprint,manifest_json,bytes,state,object_key) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8)",params![id,i,artifact.run_id,artifact.stage_id,hash,manifest,bytes,digest])?;
    }
    tx.commit()?;
    Ok(plan)
}
pub async fn preview(e: Arc<Engine>, r: Preview) -> Result<Value> {
    tokio::task::spawn_blocking(move || preview_sync(&e, r)).await?
}
