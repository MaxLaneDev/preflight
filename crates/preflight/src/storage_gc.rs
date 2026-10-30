//! Explicit reclamation of disposable stage copies; evidence and source snapshots are retained.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub project_id: String,
    pub pipeline_id: Option<String>,
    pub before: Option<String>,
    pub finished_before: Option<i64>,
    #[serde(default = "limit")]
    pub limit: usize,
}
/// Stage cleanup can additionally retain recently completed runs.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StagePreview {
    pub project_id: String,
    pub pipeline_id: Option<String>,
    pub before: Option<String>,
    /// Optional exclusive cutoff in Unix milliseconds. Fixed in the approved plan.
    pub finished_before: Option<i64>,
    #[serde(default = "limit")]
    pub limit: usize,
}
fn limit() -> usize {
    20
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Execute {
    pub project_id: String,
    pub plan_id: String,
    pub request_id: String,
    pub actor: String,
    pub reason: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub plan_id: String,
}
pub(crate) fn directory(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.is_symlink(),
        "GC path is not a real directory"
    );
    Ok(())
}
fn stage_path(e: &Engine, run: &str, stage: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(run)?;
    crate::pipeline::identifier(stage)?;
    let parent = e
        .config
        .data_dir
        .join("workspaces")
        .join(run)
        .join("stages");
    for path in [
        e.config.data_dir.join("workspaces"),
        e.config.data_dir.join("workspaces").join(run),
        parent.clone(),
    ] {
        directory(&path)?;
    }
    Ok(parent.join(stage))
}
/// Metadata fingerprint prevents silently widening an approved tree; never follows links.
#[derive(Serialize, Deserialize, PartialEq)]
pub(crate) struct Removal {
    path: String,
    directory: bool,
    identity: String,
}
pub(crate) fn file_identity(m: &std::fs::Metadata) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if m.is_dir() {
            return Ok(format!("{}:{}", m.dev(), m.ino()));
        }
        Ok(format!(
            "{}:{}:{}:{}:{}",
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(if m.is_dir() {
            "directory".into()
        } else {
            format!("{}:{:?}", m.len(), m.modified()?)
        })
    }
}
pub(crate) fn inventory(root: &Path) -> Result<(String, u64, Vec<Removal>)> {
    directory(root)?;
    let mut stack = vec![root.to_path_buf()];
    let mut records = Vec::new();
    let mut manifest = Vec::new();
    let mut bytes = 0u64;
    while let Some(path) = stack.pop() {
        ensure!(
            records.len() < 200_000,
            "stage tree exceeds GC inventory limit"
        );
        let m = std::fs::symlink_metadata(&path)?;
        ensure!(
            !m.is_symlink() && (m.is_dir() || m.is_file()),
            "stage contains symlink or special file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                !m.is_file() || m.nlink() == 1,
                "stage contains hard-linked files"
            );
            records.push(format!(
                "{:?}:{}:{}:{}:{}:{}",
                path.strip_prefix(root)?,
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec()
            ));
        }
        #[cfg(not(unix))]
        {
            records.push(format!(
                "{:?}:{}:{:?}:{}",
                path.strip_prefix(root)?,
                m.len(),
                m.modified()?,
                m.is_dir()
            ));
        }
        manifest.push(Removal {
            path: path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF8 GC path")?
                .into(),
            directory: m.is_dir(),
            identity: file_identity(&m)?,
        });
        if m.is_file() {
            bytes = bytes.checked_add(m.len()).context("GC size overflow")?;
        } else {
            for entry in std::fs::read_dir(&path)? {
                stack.push(entry?.path());
            }
        }
    }
    records.sort();
    let hash = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&records)?));
    Ok((hash, bytes, manifest))
}
fn protection(c: &rusqlite::Connection, run: &str, path: &Path) -> Result<Option<String>> {
    let state: String = c.query_row("SELECT status FROM runs WHERE id=?1", [run], |r| r.get(0))?;
    if !matches!(
        state.as_str(),
        "succeeded"
            | "failed"
            | "cancelled"
            | "infrastructure_error"
            | "timed_out"
            | "blocked"
            | "skipped"
    ) {
        return Ok(Some("run is not terminal".into()));
    }
    let busy:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM operation_leases l WHERE l.state IN ('active','abandoned') AND (l.run_id=?1 OR EXISTS(SELECT 1 FROM operation_resources r WHERE r.lease_id=l.id AND r.resource='run:'||?1)))",[run],|r|r.get(0))?;
    if busy {
        return Ok(Some("active or abandoned operation protects run".into()));
    }
    let mut q=c.prepare("SELECT path FROM artifacts UNION ALL SELECT json_extract(artifact_json,'$.path') FROM modules")?;
    for item in q.query_map([], |r| r.get::<_, String>(0))? {
        if Path::new(&item?).starts_with(path) {
            return Ok(Some(
                "published artifact or module references this path".into(),
            ));
        }
    }
    Ok(None)
}
fn preview_sync(e: &Engine, r: StagePreview) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    e.db.project(&r.project_id)?;
    if let Some(p) = &r.pipeline_id {
        crate::pipeline::identifier(p)?;
    }
    if let Some(cutoff) = r.finished_before {
        ensure!(
            cutoff > 0 && cutoff <= crate::db::now(),
            invalid("finished_before must be a positive past Unix millisecond timestamp")
        );
    }
    // Cursor is a run UUID plus stage name; identical stable cache pipeline IDs define scope.
    let rows = {
        let c = e.db.connection()?;
        let mut q=c.prepare("SELECT r.id,s.name FROM runs r JOIN stage_runs s ON s.run_id=r.id WHERE r.project_id=?1 AND r.pipeline_json IS NOT NULL AND (?2 IS NULL OR json_extract(s.definition_json,'$.cache.pipeline_id')=?2) AND (?3 IS NULL OR r.id||'/'||s.name<?3) AND (?5 IS NULL OR r.finished_at < ?5) ORDER BY r.id DESC,s.name DESC LIMIT ?4")?;
        q.query_map(
            params![
                r.project_id,
                r.pipeline_id,
                r.before,
                r.limit + 1,
                r.finished_before
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = rows.len() > r.limit;
    let mut items = Vec::new();
    let mut entries = Vec::new();
    let mut cursor = None;
    let mut total = 0u64;
    for (run, stage) in rows.into_iter().take(r.limit) {
        cursor = Some(format!("{run}/{stage}"));
        let path = match stage_path(e, &run, &stage) {
            Ok(p) => p,
            Err(error) => {
                entries.push(json!({"run_id":run,"stage_id":stage,"eligible":false,"reason":error.to_string()}));
                continue;
            }
        };
        if !path.try_exists()? {
            entries.push(
                json!({"run_id":run,"stage_id":stage,"eligible":false,"reason":"already absent"}),
            );
            continue;
        }
        let protected = protection(&*e.db.connection()?, &run, &path)?;
        if let Some(reason) = protected {
            entries.push(json!({"run_id":run,"stage_id":stage,"eligible":false,"reason":reason}));
            continue;
        }
        match inventory(&path) {
            Ok((hash, bytes, manifest)) => {
                total += bytes;
                entries.push(json!({"run_id":run,"stage_id":stage,"eligible":true,"path":path,"file_bytes":bytes}));
                items.push((run, stage, hash, bytes, serde_json::to_string(&manifest)?));
            }
            Err(error) => entries.push(
                json!({"run_id":run,"stage_id":stage,"eligible":false,"reason":error.to_string()}),
            ),
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let value = json!({"plan_id":id,"project_id":r.project_id,"pipeline_id":r.pipeline_id,"category":"stage_workspaces","finished_before":r.finished_before,"entries":entries,"estimated_file_bytes":total,"next_cursor":if more{cursor}else{None},"retained":["source snapshots","published artifacts","modules","logs","test results","run history"],"accounting":"regular file lengths; filesystem free-space changes may differ"});
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    tx.execute("INSERT INTO storage_gc_plans(id,project_id,pipeline_id,preview_json,created_at) VALUES (?1,?2,?3,?4,?5)",params![id,r.project_id,r.pipeline_id,value.to_string(),crate::db::now()])?;
    for (i, (run, stage, hash, bytes, manifest)) in items.into_iter().enumerate() {
        tx.execute(
            "INSERT INTO storage_gc_items(plan_id,ordinal,run_id,stage_id,fingerprint,manifest_json,bytes,state,reason) VALUES (?1,?2,?3,?4,?5,?7,?6,'pending',NULL)",
            params![id, i, run, stage, hash, bytes, manifest],
        )?;
    }
    tx.commit()?;
    Ok(value)
}
pub async fn preview(e: Arc<Engine>, r: StagePreview) -> Result<Value> {
    tokio::task::spawn_blocking(move || preview_sync(&e, r)).await?
}
fn set(e: &Engine, plan: &str, i: u64, state: &str, reason: Option<&str>) -> Result<()> {
    e.db.connection()?.execute(
        "UPDATE storage_gc_items SET state=?3,reason=?4 WHERE plan_id=?1 AND ordinal=?2",
        params![plan, i, state, reason],
    )?;
    Ok(())
}
fn quarantine(e: &Engine, plan: &str, i: u64) -> Result<PathBuf> {
    uuid::Uuid::parse_str(plan)?;
    let base = e.config.data_dir.join("gc-quarantine");
    std::fs::create_dir_all(&base)?;
    directory(&base)?;
    let dir = base.join(plan);
    std::fs::create_dir_all(&dir)?;
    directory(&dir)?;
    Ok(dir.join(i.to_string()))
}
pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}
fn remove_approved(root: &Path, manifest: &[Removal]) -> Result<()> {
    let (_, _, current) = inventory(root)?;
    let expected: std::collections::BTreeMap<_, _> =
        manifest.iter().map(|v| (&v.path, v)).collect();
    for entry in &current {
        ensure!(
            expected.get(&entry.path).is_some_and(|old| *old == entry),
            "quarantine contains an unapproved or changed path"
        );
    }
    let mut paths = current;
    paths.sort_by_key(|v| std::cmp::Reverse(std::path::Path::new(&v.path).components().count()));
    for entry in paths {
        let path = root.join(&entry.path);
        let mut parent = path.parent();
        while let Some(dir) = parent {
            if !dir.starts_with(root) {
                break;
            }
            directory(dir)?;
            parent = dir.parent();
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.is_symlink() && file_identity(&metadata)? == entry.identity,
            "quarantine path changed during deletion"
        );
        if entry.directory {
            std::fs::remove_dir(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}
fn execute_sync(e: &Engine, r: Execute) -> Result<Value> {
    let _gate = e.gc_gate.try_lock().map_err(|_| Fault::Busy)?;
    crate::control::validate_name(&r.request_id)?;
    ensure!(
        !r.actor.trim().is_empty()
            && r.actor.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 1024,
        invalid("bounded actor and reason required")
    );
    let encoded = serde_json::to_string(&r)?;
    {
        let mut c = e.db.connection()?;
        let tx = c.transaction()?;
        let old:Option<(Option<String>,Option<String>)>=tx.query_row("SELECT request_json,result_json FROM storage_gc_plans WHERE id=?1 AND project_id=?2",params![r.plan_id,r.project_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (previous, result) = old.ok_or(Fault::NotFound)?;
        if let Some(previous) = previous {
            ensure!(previous == encoded, Fault::IdempotencyConflict);
        }
        if let Some(result) = result {
            return Ok(serde_json::from_str(&result)?);
        }
        let other:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM storage_gc_plans WHERE project_id=?1 AND request_id=?2 AND id<>?3)",params![r.project_id,r.request_id,r.plan_id],|r|r.get(0))?;
        ensure!(!other, Fault::IdempotencyConflict);
        tx.execute(
            "UPDATE storage_gc_plans SET request_id=?2,request_json=?3 WHERE id=?1",
            params![r.plan_id, r.request_id, encoded],
        )?;
        tx.commit()?;
    }
    let rows = {
        let c = e.db.connection()?;
        let mut q=c.prepare("SELECT ordinal,run_id,stage_id,fingerprint,bytes,state,manifest_json,object_key FROM storage_gc_items WHERE plan_id=?1 ORDER BY ordinal")?;
        q.query_map([&r.plan_id], |r| {
            Ok((
                r.get::<_, u64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (i, run, stage, hash, bytes, mut state, manifest, object) in rows {
        if matches!(state.as_str(), "deleted" | "skipped" | "failed") {
            continue;
        }
        let work = (|| -> Result<()> {
            if object.as_deref() == Some("evidence:logs") {
                return crate::evidence_expiry::expire_logs(
                    &*e.db.connection()?,
                    &run,
                    &r.plan_id,
                    i,
                    &hash,
                );
            }
            let source_target = object.as_deref() == Some("evidence:source_snapshots");
            let target = quarantine(e, &r.plan_id, i)?;
            if state != "quarantined" {
                let cache_entry = object
                    .as_deref()
                    .and_then(|s| s.strip_prefix("cache_artifacts:"));
                let artifact_target = cache_entry.is_some()
                    || object
                        .as_deref()
                        .is_some_and(|v| v.starts_with("artifacts:"));
                let source = if source_target {
                    crate::evidence_expiry::source_path(e, &run)?
                } else if artifact_target {
                    crate::artifact_expiry::path(e, &run, &stage)?
                } else if let Some(digest) = &object {
                    crate::module_gc::path(e, &r.project_id, digest)?
                } else {
                    stage_path(e, &run, &stage)?
                };
                if target.try_exists()? {
                    ensure!(
                        state == "moving"
                            && (object.is_some() && !artifact_target && !source_target
                                || !source.try_exists()?),
                        "ambiguous GC recovery paths"
                    );
                    directory(&target)?;
                } else {
                    if !source.try_exists()? {
                        set(
                            e,
                            &r.plan_id,
                            i,
                            "skipped",
                            Some("source absent; no bytes credited"),
                        )?;
                        return Ok(());
                    }
                    let actual = inventory(&source)?;
                    if actual.0 != hash || actual.1 != bytes {
                        set(
                            e,
                            &r.plan_id,
                            i,
                            "skipped",
                            Some("tree changed after preview"),
                        )?;
                        return Ok(());
                    }
                    let c = e.db.connection()?;
                    let protected = if source_target {
                        let cutoff:i64=c.query_row("SELECT json_extract(preview_json,'$.finished_before') FROM storage_gc_plans WHERE id=?1",[&r.plan_id],|r|r.get(0))?;
                        let other:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM evidence_expiry WHERE run_id=?1 AND category='source_snapshots' AND plan_id<>?2)",params![run,r.plan_id],|v|v.get(0))?;
                        ensure!(!other, "source expiry is owned by another plan");
                        crate::evidence_expiry::protection(
                            &c,
                            &run,
                            crate::evidence_expiry::Category::SourceSnapshots,
                            cutoff,
                        )?
                    } else if let Some(entry) = cache_entry {
                        if state=="moving" && c.query_row("SELECT EXISTS(SELECT 1 FROM artifact_expiry WHERE run_id=?1 AND stage_id=?2 AND plan_id=?3 AND ordinal=?4)",params![run,stage,r.plan_id,i],|v|v.get::<_,bool>(0))? {
                            crate::artifact_expiry::protection_except_cache(&c,&run,&stage,Some(entry))?
                        } else {crate::cache_eviction::protection(&c, &r.project_id, &run, &stage, entry)?}
                    } else if artifact_target {
                        crate::artifact_expiry::protection(&c, &run, &stage)?
                    } else if let Some(digest) = &object {
                        let scope: Option<String> = c.query_row(
                            "SELECT pipeline_id FROM storage_gc_plans WHERE id=?1",
                            [&r.plan_id],
                            |v| v.get(0),
                        )?;
                        if state == "moving" && !c.query_row("SELECT EXISTS(SELECT 1 FROM module_evictions WHERE project_id=?1 AND sha256=?2 AND plan_id=?3 AND ordinal=?4)",params![r.project_id,digest,r.plan_id,i],|v|v.get::<_,bool>(0))? {
                            Some("module republished after interrupted cleanup; create a new preview".into())
                        } else {
                            let cutoff:Option<i64>=c.query_row("SELECT json_extract(preview_json,'$.finished_before') FROM storage_gc_plans WHERE id=?1",[&r.plan_id],|v|v.get(0))?;
                            let recent:bool=c.query_row("SELECT ?3 IS NOT NULL AND NOT EXISTS(SELECT 1 FROM module_publications WHERE project_id=?1 AND sha256=?2 AND published_at<?3)",params![r.project_id,digest,cutoff],|v|v.get(0))?;
                            if recent{Some("module publication is recent or historical timestamp unknown".into())}else{crate::module_gc::protection(&c, &r.project_id, digest, scope.as_deref(), &source)?}
                        }
                    } else {
                        let cutoff: Option<i64> = c.query_row("SELECT json_extract(preview_json,'$.finished_before') FROM storage_gc_plans WHERE id=?1",[&r.plan_id],|row|row.get(0))?;
                        let old_enough: bool = c.query_row("SELECT ?2 IS NULL OR (finished_at IS NOT NULL AND finished_at < ?2) FROM runs WHERE id=?1",params![run,cutoff],|row|row.get(0))?;
                        if !old_enough {
                            Some("run no longer meets approved retention cutoff".into())
                        } else {
                            protection(&c, &run, &source)?
                        }
                    };
                    if let Some(reason) = protected {
                        drop(c);
                        set(e, &r.plan_id, i, "skipped", Some(&reason))?;
                        return Ok(());
                    }
                    // Durable intent precedes rename; the DB mutex excludes lease/publication acquisition.
                    c.execute_batch("BEGIN IMMEDIATE")?;
                    let intent = (|| -> Result<()> {
                        if let Some(entry) = cache_entry {
                            c.execute(
                                "UPDATE stage_cache SET state='invalidated' WHERE id=?1",
                                [entry],
                            )?;
                        }
                        if source_target {
                            c.execute("INSERT INTO evidence_expiry VALUES (?1,'source_snapshots',?2,?3,?4) ON CONFLICT(run_id,category) DO NOTHING",params![run,r.plan_id,i,crate::db::now()])?;
                        } else if artifact_target {
                            c.execute("INSERT INTO artifact_expiry(run_id,stage_id,plan_id,ordinal) VALUES (?1,?2,?3,?4) ON CONFLICT(run_id,stage_id) DO NOTHING",params![run,stage,r.plan_id,i])?;
                        } else if let Some(digest) = &object {
                            c.execute("INSERT INTO module_evictions(project_id,sha256,plan_id,ordinal) VALUES (?1,?2,?3,?4) ON CONFLICT(project_id,sha256) DO UPDATE SET plan_id=excluded.plan_id,ordinal=excluded.ordinal",params![r.project_id,digest,r.plan_id,i])?;
                        }
                        c.execute("UPDATE storage_gc_items SET state='moving',reason=NULL WHERE plan_id=?1 AND ordinal=?2",params![r.plan_id,i])?;
                        Ok(())
                    })();
                    if let Err(error) = intent {
                        c.execute_batch("ROLLBACK")?;
                        return Err(error);
                    }
                    c.execute_batch("COMMIT")?;
                    std::fs::rename(&source, &target)?;
                    sync_directory(source.parent().context("source parent")?)?;
                    sync_directory(target.parent().context("target parent")?)?;
                }
                set(e, &r.plan_id, i, "quarantined", None)?;
                state = "quarantined".into();
            }
            if target.try_exists()? {
                directory(&target)?;
                remove_approved(&target, &serde_json::from_str::<Vec<Removal>>(&manifest)?)?;
                sync_directory(target.parent().context("quarantine parent")?)?;
            }
            set(e, &r.plan_id, i, "deleted", None)?;
            Ok(())
        })();
        if let Err(error) = work {
            // A quarantined tree is resumable; an unchanged source can be previewed again.
            let persisted: String = e.db.connection()?.query_row(
                "SELECT state FROM storage_gc_items WHERE plan_id=?1 AND ordinal=?2",
                params![r.plan_id, i],
                |r| r.get(0),
            )?;
            let next = if matches!(persisted.as_str(), "quarantined" | "moving") {
                persisted.as_str()
            } else {
                "failed"
            };
            set(
                e,
                &r.plan_id,
                i,
                next,
                Some(&error.to_string().chars().take(1024).collect::<String>()),
            )?;
        }
    }
    let result = status(
        e,
        &Query {
            project_id: r.project_id,
            plan_id: r.plan_id.clone(),
        },
    )?;
    if result["complete"] == true {
        e.db.connection()?.execute(
            "UPDATE storage_gc_plans SET result_json=?2 WHERE id=?1",
            params![r.plan_id, result.to_string()],
        )?;
    }
    Ok(result)
}
pub async fn execute(e: Arc<Engine>, r: Execute) -> Result<Value> {
    if crate::archive_gc::is_plan(&e, &r.project_id, &r.plan_id)? {
        return tokio::task::spawn_blocking(move || crate::archive_gc::execute(&e, r)).await?;
    }
    tokio::task::spawn_blocking(move || execute_sync(&e, r)).await?
}
pub fn status(e: &Engine, r: &Query) -> Result<Value> {
    if crate::archive_gc::is_plan(e, &r.project_id, &r.plan_id)? {
        return crate::archive_gc::status(e, r);
    }
    let c = e.db.connection()?;
    let (scope,audit,result):(Option<String>,Option<String>,Option<String>)=c.query_row("SELECT pipeline_id,request_json,result_json FROM storage_gc_plans WHERE id=?1 AND project_id=?2",params![r.plan_id,r.project_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(Fault::NotFound)?;
    if let Some(result) = result {
        return Ok(serde_json::from_str(&result)?);
    }
    let items=c.prepare("SELECT run_id,stage_id,state,bytes,reason,object_key FROM storage_gc_items WHERE plan_id=?1 ORDER BY ordinal")?.query_map([&r.plan_id],|r|Ok(json!({"run_id":r.get::<_,String>(0)?,"stage_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"file_bytes":r.get::<_,u64>(3)?,"reason":r.get::<_,Option<String>>(4)?,"object_key":r.get::<_,Option<String>>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let category: String = c.query_row(
        "SELECT json_extract(preview_json,'$.category') FROM storage_gc_plans WHERE id=?1",
        [&r.plan_id],
        |v| v.get(0),
    )?;
    let cutoff: Option<i64> = c.query_row(
        "SELECT json_extract(preview_json,'$.finished_before') FROM storage_gc_plans WHERE id=?1",
        [&r.plan_id],
        |row| row.get(0),
    )?;
    let complete = items
        .iter()
        .all(|v| matches!(v["state"].as_str(), Some("deleted" | "skipped" | "failed")));
    let bytes: u64 = items
        .iter()
        .filter(|v| v["state"] == "deleted" && v["object_key"] != "evidence:logs")
        .map(|v| v["file_bytes"].as_u64().unwrap_or(0))
        .sum();
    Ok(
        json!({"plan_id":r.plan_id,"project_id":r.project_id,"pipeline_id":scope,"category":category,"finished_before":cutoff,"complete":complete,"deleted_file_bytes":bytes,"expired_log_payload_bytes":items.iter().filter(|v|v["state"]=="deleted"&&v["object_key"]=="evidence:logs").map(|v|v["file_bytes"].as_u64().unwrap_or(0)).sum::<u64>(),"items":items,"audit":audit.map(|s|serde_json::from_str::<Value>(&s)).transpose()?,"accounting":"lengths of files removed from approved trees, not measured free disk space","retry":"replay identical execute to resume moving/quarantined items"}),
    )
}
