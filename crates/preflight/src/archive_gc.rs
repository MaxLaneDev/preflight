//! File-only archive retention, sharing audited GC plans without inventing local runs.
use crate::{
    archives as a,
    engine::Engine,
    model::{Fault, invalid},
    storage_gc as gc,
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub project_id: String,
    pub pipeline_key: Option<String>,
    /// Exclusive Unix millisecond cutoff, frozen in the approved plan.
    pub finished_before: i64,
    /// Optional soft target for retained compressed archive bytes. TTL still applies.
    pub target_bytes: Option<u64>,
    #[serde(default = "keep")]
    pub keep_latest: usize,
    pub after: Option<String>,
    #[serde(default = "limit")]
    pub limit: usize,
}
fn keep() -> usize {
    1
}
fn limit() -> usize {
    20
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub project_id: String,
    pub archive_id: String,
    pub pinned: bool,
    pub expected_revision: u64,
    pub request_id: String,
    pub actor: crate::model::Actor,
    pub reason: String,
}
const SCOPE: &str = "a.project_id=?1 AND a.state='ready' AND NOT EXISTS(SELECT 1 FROM archive_disposals d WHERE d.archive_id=a.id) AND (?2 IS NULL OR (a.imported=0 AND EXISTS(SELECT 1 FROM run_pipeline_identity i WHERE i.run_id=a.run_id AND i.pipeline_key=?2)))";
fn path(e: &Engine, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id)?;
    Ok(a::archive_root(e)?.join(format!("{id}.tar.zst")))
}
fn latest(e: &Engine, r: &Preview) -> Result<BTreeSet<String>> {
    let rows = {
        let c = e.db.connection()?;
        let sql = format!(
            "SELECT a.id,a.sha256,a.file_bytes FROM archive_inventory a WHERE {SCOPE} ORDER BY a.id DESC LIMIT ?3"
        );
        c.prepare(&sql)?
            .query_map(params![r.project_id, r.pipeline_key, r.keep_latest], |v| {
                Ok((
                    v.get::<_, String>(0)?,
                    v.get::<_, String>(1)?,
                    v.get::<_, u64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut retained = BTreeSet::new();
    for (id, sha, bytes) in rows {
        let mut file = a::open_read(&path(e, &id)?).map_err(|_| {
            invalid("latest retained archive is unavailable; no removal authorized")
        })?;
        fs2::FileExt::try_lock_shared(&file).map_err(|_| Fault::Busy)?;
        ensure!(
            file.metadata()?.len() == bytes && a::file_hash(&mut file)? == sha,
            invalid("latest retained archive changed; no removal authorized")
        );
        retained.insert(id);
    }
    Ok(retained)
}

fn observed(e: &Engine, id: &str, expected: u64) -> Result<u64> {
    let p = path(e, id)?;
    match std::fs::symlink_metadata(p) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
        Ok(m) => {
            ensure!(
                m.is_file() && !m.is_symlink() && m.len() == expected,
                invalid("archive accounting changed or linked; inspect the file before cleanup")
            );
            Ok(m.len())
        }
    }
}
pub(crate) fn scope_bytes(e: &Engine, project: &str, pipeline: Option<&str>) -> Result<u64> {
    let records = {
        let c = e.db.connection()?;
        let sql =
            format!("SELECT a.id,a.file_bytes FROM archive_inventory a WHERE {SCOPE} LIMIT 10001");
        c.prepare(&sql)?
            .query_map(params![project, pipeline], |v| {
                Ok((v.get::<_, String>(0)?, v.get::<_, u64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    ensure!(
        records.len() <= 10000,
        invalid("archive quota accounting exceeds 10000 files; no removal authorized")
    );
    let mut bytes = 0u64;
    for (id, size) in records {
        bytes = bytes
            .checked_add(observed(e, &id, size)?)
            .context("archive byte overflow")?;
    }
    Ok(bytes)
}
pub fn pin(e: &Engine, r: Pin) -> Result<Value> {
    let _gate = e.gc_gate.try_lock().map_err(|_| Fault::Busy)?;
    crate::control::validate_name(&r.request_id)?;
    ensure!(
        !r.reason.trim().is_empty()
            && r.reason.len() <= 512
            && r.expected_revision < i64::MAX as u64,
        invalid("bounded reason and revision required")
    );
    let request = serde_json::to_string(&r)?;
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    if let Some((old,result))=tx.query_row("SELECT request_json,result_json FROM archive_pin_requests WHERE project_id=?1 AND request_id=?2",params![r.project_id,r.request_id],|v|Ok((v.get::<_,String>(0)?,v.get::<_,String>(1)?))).optional()?{ensure!(old==request,Fault::IdempotencyConflict);return Ok(serde_json::from_str(&result)?);}
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM archive_inventory WHERE id=?1 AND project_id=?2 AND state='ready' AND NOT EXISTS(SELECT 1 FROM archive_disposals WHERE archive_id=?1))",params![r.archive_id,r.project_id],|v|v.get(0))?;
    ensure!(
        exists,
        invalid("only available ready archives can be pinned")
    );
    let revision: u64 = tx.query_row(
        "SELECT COALESCE((SELECT revision FROM archive_pins WHERE archive_id=?1),0)",
        [&r.archive_id],
        |v| v.get(0),
    )?;
    ensure!(revision == r.expected_revision, Fault::Conflict);
    let result = json!({"archive_id":r.archive_id,"pinned":r.pinned,"revision":revision+1});
    tx.execute("INSERT INTO archive_pins VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(archive_id) DO UPDATE SET pinned=excluded.pinned,revision=excluded.revision,actor=excluded.actor,reason=excluded.reason,updated_at=excluded.updated_at",params![r.archive_id,r.pinned,revision+1,serde_json::to_string(&r.actor)?,r.reason,crate::db::now()])?;
    tx.execute(
        "INSERT INTO archive_pin_requests VALUES (?1,?2,?3,?4)",
        params![r.project_id, r.request_id, request, result.to_string()],
    )?;
    tx.commit()?;
    Ok(result)
}
pub async fn preview(e: Arc<Engine>, r: Preview) -> Result<Value> {
    tokio::task::spawn_blocking(move || preview_sync(&e, r)).await?
}
fn preview_sync(e: &Engine, r: Preview) -> Result<Value> {
    let _gate = e.gc_gate.try_lock().map_err(|_| Fault::Busy)?;
    e.db.project(&r.project_id)?;
    ensure!(
        (1..=100).contains(&r.limit) && (1..=100).contains(&r.keep_latest),
        invalid("limit and keep_latest must be 1..100")
    );
    ensure!(
        r.finished_before > 0 && r.finished_before <= crate::db::now(),
        invalid("finished_before must be a positive past Unix millisecond timestamp")
    );
    ensure!(
        r.pipeline_key
            .as_ref()
            .is_none_or(|v| !v.is_empty() && v.len() <= 512),
        invalid("invalid pipeline key")
    );
    ensure!(
        r.target_bytes.is_none_or(|v| v <= 9_007_199_254_740_991),
        invalid("target_bytes exceeds exact integer range")
    );
    if let Some(id) = &r.after {
        uuid::Uuid::parse_str(id).map_err(|_| invalid("invalid cursor"))?;
    }
    let retained = latest(e, &r)?;
    let rows = {
        let c = e.db.connection()?;
        let sql = format!(
            "SELECT a.id,a.sha256,a.file_bytes,COALESCE(p.pinned,0),COALESCE(p.revision,0) FROM archive_inventory a LEFT JOIN archive_pins p ON p.archive_id=a.id WHERE {SCOPE} AND a.finished_at<?3 AND (?4 IS NULL OR a.id>?4) ORDER BY a.id LIMIT ?5"
        );
        c.prepare(&sql)?
            .query_map(
                params![
                    r.project_id,
                    r.pipeline_key,
                    r.finished_before,
                    r.after,
                    r.limit + 1
                ],
                |v| {
                    Ok((
                        v.get::<_, String>(0)?,
                        v.get::<_, String>(1)?,
                        v.get::<_, u64>(2)?,
                        v.get::<_, bool>(3)?,
                        v.get::<_, u64>(4)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = rows.len() > r.limit;
    let mut entries = Vec::new();
    let mut items = Vec::new();
    let mut cursor = None;
    let mut total = 0u64;
    let mut remaining = r
        .target_bytes
        .map(|_| scope_bytes(e, &r.project_id, r.pipeline_key.as_deref()))
        .transpose()?;
    for (id, sha, bytes, pinned, revision) in rows.into_iter().take(r.limit) {
        cursor = Some(id.clone());
        let mut reason = if pinned {
            Some("archive is pinned".to_owned())
        } else if retained.contains(&id) {
            Some("archive is among latest retained copies".into())
        } else if remaining
            .zip(r.target_bytes)
            .is_some_and(|(n, target)| n <= target)
        {
            Some("archive byte target already met".into())
        } else {
            None
        };
        let identity = if reason.is_none() {
            match observed(e, &id, bytes).and_then(|actual| {
                ensure!(actual > 0, invalid("archive file absent or empty"));
                gc::file_identity(&std::fs::symlink_metadata(path(e, &id)?)?)
            }) {
                Ok(identity) => Some(identity),
                Err(error) => {
                    reason = Some(error.to_string());
                    None
                }
            }
        } else {
            None
        };
        let eligible = reason.is_none();
        entries
            .push(json!({"archive_id":id,"eligible":eligible,"file_bytes":bytes,"reason":reason}));
        if let Some(identity) = identity {
            total += bytes;
            remaining = remaining.map(|v| v.saturating_sub(bytes));
            items.push((id, sha, bytes, identity, revision));
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let value = json!({"plan_id":id,"project_id":r.project_id,"category":"run_archives","request":r,"entries":entries,"estimated_file_bytes":total,"next_cursor":if more{cursor}else{None},"retained":["latest copies and pinned archives","active archive readers","run history and test results","original artifacts and restored directories"],"accounting":"compressed regular-file lengths; no filesystem free-space claim"});
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    tx.execute(
        "INSERT INTO storage_gc_plans(id,project_id,preview_json,created_at) VALUES (?1,?2,?3,?4)",
        params![id, r.project_id, value.to_string(), crate::db::now()],
    )?;
    for (i, (archive, sha, bytes, identity, revision)) in items.into_iter().enumerate() {
        tx.execute("INSERT INTO archive_gc_items(plan_id,ordinal,archive_id,sha256,bytes,identity,pin_revision,state) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending')",params![id,i,archive,sha,bytes,identity,revision])?;
    }
    tx.commit()?;
    Ok(value)
}
pub(crate) fn is_plan(e: &Engine, project: &str, id: &str) -> Result<bool> {
    Ok(e.db.connection()?.query_row("SELECT COALESCE(json_extract(preview_json,'$.category')='run_archives',0) FROM storage_gc_plans WHERE id=?1 AND project_id=?2",params![id,project],|v|v.get(0)).optional()?.unwrap_or(false))
}
fn set(e: &Engine, plan: &str, i: u64, state: &str, reason: Option<&str>) -> Result<()> {
    e.db.connection()?.execute(
        "UPDATE archive_gc_items SET state=?3,reason=?4 WHERE plan_id=?1 AND ordinal=?2",
        params![plan, i, state, reason],
    )?;
    Ok(())
}
fn quarantine(e: &Engine, plan: &str, id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(plan)?;
    uuid::Uuid::parse_str(id)?;
    let base = a::archive_root(e)?.join(".trash");
    a::private_dir(&base)?;
    let base = base.join(plan);
    a::private_dir(&base)?;
    Ok(base.join(format!("{id}.tar.zst")))
}
pub(crate) fn execute(e: &Engine, r: gc::Execute) -> Result<Value> {
    let _gate = e.gc_gate.try_lock().map_err(|_| Fault::Busy)?;
    crate::control::validate_name(&r.request_id)?;
    ensure!(
        !r.actor.trim().is_empty()
            && r.actor.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 1024,
        invalid("bounded actor and reason required")
    );
    let request = serde_json::to_string(&r)?;
    let preview: Value = {
        let mut c = e.db.connection()?;
        let tx = c.transaction()?;
        let(old,result,preview):(Option<String>,Option<String>,String)=tx.query_row("SELECT request_json,result_json,preview_json FROM storage_gc_plans WHERE id=?1 AND project_id=?2",params![r.plan_id,r.project_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?))).optional()?.ok_or(Fault::NotFound)?;
        if let Some(old) = old {
            ensure!(old == request, Fault::IdempotencyConflict);
        }
        if let Some(result) = result {
            return Ok(serde_json::from_str(&result)?);
        }
        let other:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM storage_gc_plans WHERE project_id=?1 AND request_id=?2 AND id<>?3)",params![r.project_id,r.request_id,r.plan_id],|v|v.get(0))?;
        ensure!(!other, Fault::IdempotencyConflict);
        tx.execute(
            "UPDATE storage_gc_plans SET request_id=?2,request_json=?3 WHERE id=?1",
            params![r.plan_id, r.request_id, request],
        )?;
        tx.commit()?;
        serde_json::from_str(&preview)?
    };
    let scope: Preview = serde_json::from_value(preview["request"].clone())?;
    let rows = {
        let c = e.db.connection()?;
        c.prepare("SELECT ordinal,archive_id,sha256,bytes,identity,pin_revision,state FROM archive_gc_items WHERE plan_id=?1 ORDER BY ordinal")?.query_map([&r.plan_id],|v|Ok((v.get::<_,u64>(0)?,v.get::<_,String>(1)?,v.get::<_,String>(2)?,v.get::<_,u64>(3)?,v.get::<_,String>(4)?,v.get::<_,u64>(5)?,v.get::<_,String>(6)?)))?.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (i, id, sha, bytes, identity, revision, state) in rows {
        if matches!(state.as_str(), "deleted" | "skipped") {
            continue;
        }
        let outcome = (|| -> Result<()> {
            let source = path(e, &id)?;
            let target = quarantine(e, &r.plan_id, &id)?;
            let deleting = state == "deleting";
            let retained = latest(e, &scope)?;
            ensure!(
                retained.len() >= scope.keep_latest,
                invalid("latest retained copies unavailable; archive deletion suspended")
            );
            if !deleting {
                let record = a::get(
                    e,
                    &a::Id {
                        project_id: r.project_id.clone(),
                        archive_id: id.clone(),
                    },
                )?;
                let reason = if record["state"] != "ready" {
                    Some("archive no longer available")
                } else if record["pinned"] == true
                    || record["pin_revision"].as_u64() != Some(revision)
                {
                    Some("archive pin changed after preview")
                } else if retained.contains(&id) {
                    Some("latest retained copy")
                } else if scope
                    .target_bytes
                    .map(|target| {
                        scope_bytes(e, &r.project_id, scope.pipeline_key.as_deref())
                            .map(|bytes| bytes <= target)
                    })
                    .transpose()?
                    .unwrap_or(false)
                {
                    Some("archive byte target already met")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    set(e, &r.plan_id, i, "skipped", Some(reason))?;
                    return Ok(());
                }
            }
            ensure!(
                !(source.try_exists()? && target.try_exists()?),
                invalid("ambiguous archive cleanup paths")
            );
            let actual = if target.try_exists()? {
                ensure!(deleting, invalid("unowned quarantine file"));
                target.clone()
            } else {
                source.clone()
            };
            if !actual.try_exists()? {
                set(
                    e,
                    &r.plan_id,
                    i,
                    if deleting { "deleted" } else { "skipped" },
                    Some("file absent; no bytes credited"),
                )?;
                return Ok(());
            }
            let mut file = a::open_read(&actual)?;
            if fs2::FileExt::try_lock_exclusive(&file).is_err() {
                set(
                    e,
                    &r.plan_id,
                    i,
                    if deleting { "deleting" } else { "blocked" },
                    Some("active archive reader; replay identical execute later"),
                )?;
                return Ok(());
            }
            ensure!(
                file.metadata()?.len() == bytes
                    && gc::file_identity(&file.metadata()?)? == identity
                    && a::file_hash(&mut file)? == sha,
                invalid("archive changed after preview; file retained")
            );
            if !deleting {
                let mut c = e.db.connection()?;
                let tx = c.transaction()?;
                tx.execute(
                    "INSERT INTO archive_disposals VALUES (?1,?2,?3,?4)",
                    params![id, r.plan_id, i, crate::db::now()],
                )?;
                tx.execute("UPDATE archive_gc_items SET state='deleting',reason=NULL WHERE plan_id=?1 AND ordinal=?2",params![r.plan_id,i])?;
                tx.commit()?;
            }
            if actual == source {
                std::fs::rename(&source, &target)?;
                gc::sync_directory(source.parent().context("archive parent")?)?;
                gc::sync_directory(target.parent().context("quarantine parent")?)?;
            }
            // The open exclusive lock protects readers while the verified inode is unlinked.
            ensure!(
                gc::file_identity(&std::fs::symlink_metadata(&target)?)? == identity,
                invalid("archive quarantine changed")
            );
            std::fs::remove_file(&target)?;
            gc::sync_directory(target.parent().context("quarantine parent")?)?;
            e.db.connection()?.execute("UPDATE archive_gc_items SET state='deleted',removed_bytes=?3,reason=NULL WHERE plan_id=?1 AND ordinal=?2",params![r.plan_id,i,bytes])?;
            Ok(())
        })();
        if let Err(error) = outcome {
            let persisted: String = e.db.connection()?.query_row(
                "SELECT state FROM archive_gc_items WHERE plan_id=?1 AND ordinal=?2",
                params![r.plan_id, i],
                |v| v.get(0),
            )?;
            let reason = error.to_string().chars().take(512).collect::<String>();
            set(
                e,
                &r.plan_id,
                i,
                if persisted == "deleting" {
                    "deleting"
                } else {
                    "skipped"
                },
                Some(&reason),
            )?;
        }
    }
    let mut result = status(
        e,
        &gc::Query {
            project_id: r.project_id,
            plan_id: r.plan_id.clone(),
        },
    )?;
    if result["complete"] == true {
        if let Some(target) = scope.target_bytes
            && let Ok(bytes) = scope_bytes(e, &scope.project_id, scope.pipeline_key.as_deref())
        {
            result["retained_file_bytes"] = json!(bytes);
            result["target_met"] = json!(bytes <= target);
        }
        e.db.connection()?.execute(
            "UPDATE storage_gc_plans SET result_json=?2 WHERE id=?1",
            params![r.plan_id, result.to_string()],
        )?;
    }
    Ok(result)
}
pub(crate) fn status(e: &Engine, r: &gc::Query) -> Result<Value> {
    let c = e.db.connection()?;
    let (audit, result, preview): (Option<String>, Option<String>, String) = c
        .query_row(
            "SELECT request_json,result_json,preview_json FROM storage_gc_plans WHERE id=?1 AND project_id=?2",
            params![r.plan_id, r.project_id],
            |v| Ok((v.get(0)?, v.get(1)?, v.get(2)?)),
        )
        .optional()?
        .ok_or(Fault::NotFound)?;
    if let Some(result) = result {
        return Ok(serde_json::from_str(&result)?);
    }
    let items=c.prepare("SELECT archive_id,state,reason,bytes,removed_bytes FROM archive_gc_items WHERE plan_id=?1 ORDER BY ordinal")?.query_map([&r.plan_id],|v|Ok(json!({"archive_id":v.get::<_,String>(0)?,"state":v.get::<_,String>(1)?,"reason":v.get::<_,Option<String>>(2)?,"file_bytes":v.get::<_,u64>(3)?,"removed_file_bytes":v.get::<_,u64>(4)?})))?.collect::<rusqlite::Result<Vec<Value>>>()?;
    drop(c);
    let preview: Value = serde_json::from_str(&preview)?;
    let scope: Preview = serde_json::from_value(preview["request"].clone())?;
    Ok(
        json!({"retained_file_bytes":null,"target_bytes":scope.target_bytes,"target_met":null,"plan_id":r.plan_id,"project_id":r.project_id,"category":"run_archives","complete":items.iter().all(|v|v["state"]=="deleted"||v["state"]=="skipped"),"deleted_file_bytes":items.iter().map(|v|v["removed_file_bytes"].as_u64().unwrap_or(0)).sum::<u64>(),"items":items,"audit":audit.map(|v|serde_json::from_str::<Value>(&v)).transpose()?,"accounting":"regular file bytes unlinked; no measured free-space claim","retry":"identical execute resumes blocked readers and interrupted deletion"}),
    )
}
