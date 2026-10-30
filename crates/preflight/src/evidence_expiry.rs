//! Explicit expiry retains identities and result metadata; failure and owned evidence stays protected.
use crate::{engine::Engine, model::invalid, storage_gc as gc};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc};
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Logs,
    SourceSnapshots,
}
impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Self::Logs => "logs",
            Self::SourceSnapshots => "source_snapshots",
        }
    }
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub project_id: String,
    pub pipeline_id: Option<String>,
    pub pipeline_key: Option<String>,
    pub run_id: Option<String>,
    pub category: Category,
    pub acknowledge_loss: bool,
    pub finished_before: i64,
    pub before: Option<String>,
    #[serde(default = "limit")]
    pub limit: usize,
}
fn limit() -> usize {
    20
}
pub(crate) fn available(c: &rusqlite::Connection, run: &str, category: Category) -> Result<()> {
    ensure!(
        !expired(c, run, category)?,
        invalid(format!(
            "{}_expired: retained metadata is available; payload was explicitly expired",
            category.name()
        ))
    );
    Ok(())
}
pub(crate) fn expired(c: &rusqlite::Connection, run: &str, category: Category) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM evidence_expiry WHERE run_id=?1 AND category=?2)",
        params![run, category.name()],
        |r| r.get(0),
    )?)
}
pub(crate) fn source_path(e: &Engine, run: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(run)?;
    let base = e.config.data_dir.join("workspaces");
    gc::directory(&e.config.data_dir)?;
    gc::directory(&base)?;
    gc::directory(&base.join(run))?;
    Ok(base.join(run).join("source"))
}
pub(crate) fn protection(
    c: &rusqlite::Connection,
    run: &str,
    category: Category,
    cutoff: i64,
) -> Result<Option<String>> {
    let eligible:bool=c.query_row("SELECT status='succeeded' AND finished_at IS NOT NULL AND finished_at<?2 AND (?3='logs' OR pipeline_json IS NOT NULL) FROM runs WHERE id=?1",params![run,cutoff,category.name()],|r|r.get(0))?;
    if !eligible {
        return Ok(Some("only old successful runs qualify; mixed native workspaces and failure evidence are retained".into()));
    }
    // The artifact policy already checks baseline revisions, modules, owners, shared executions and leases.
    let stages = c
        .prepare("SELECT name FROM stage_runs WHERE run_id=?1")?
        .query_map([run], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for stage in stages {
        if let Some(reason) = crate::artifact_expiry::protection(c, run, &stage)? {
            return Ok(Some(reason));
        }
    }
    Ok(None)
}
pub(crate) fn logs_inventory(c: &rusqlite::Connection, run: &str) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut count = 0usize;
    for sql in [
        "SELECT id,text FROM log_events WHERE run_id=?1 ORDER BY id",
        "SELECT id,hex(payload) FROM log_chunks WHERE run_id=?1 ORDER BY id",
        "SELECT l.id,f.text FROM log_events l JOIN log_search f ON f.rowid=l.id WHERE l.run_id=?1 ORDER BY l.id",
    ] {
        let mut q = c.prepare(sql)?;
        let mut rows = q.query([run])?;
        while let Some(row) = rows.next()? {
            count += 1;
            ensure!(
                count <= 100000,
                invalid("log inventory exceeds 100000 records; no expiry approved")
            );
            let id: i64 = row.get(0)?;
            let text: String = row.get(1)?;
            hash.update(id.to_le_bytes());
            hash.update((text.len() as u64).to_le_bytes());
            hash.update(text.as_bytes());
            bytes = bytes
                .checked_add(if sql.contains("hex(payload)") {
                    text.len() as u64 / 2
                } else {
                    text.len() as u64
                })
                .ok_or_else(|| invalid("log size overflow"))?;
        }
    }
    Ok((crate::hashing::hex(hash.finalize()), bytes))
}
fn preview_sync(e: &Engine, r: Preview) -> Result<Value> {
    ensure!(
        r.acknowledge_loss,
        invalid("acknowledge_loss=true is required; payload expiry is irreversible")
    );
    ensure!(
        (1..=100).contains(&r.limit)
            && r.finished_before > 0
            && r.finished_before <= crate::db::now(),
        invalid(
            "limit must be 1..100 and finished_before a positive past Unix millisecond timestamp"
        )
    );
    e.db.project(&r.project_id)?;
    if let Some(id) = &r.pipeline_id {
        crate::pipeline::identifier(id)?;
    }
    let c = e.db.connection()?;
    let rows=c.prepare("SELECT r.id FROM runs r WHERE r.project_id=?1 AND r.finished_at<?2 AND (?3 IS NULL OR r.id<?3) AND (?4 IS NULL OR EXISTS(SELECT 1 FROM stage_runs s WHERE s.run_id=r.id AND json_extract(s.definition_json,'$.cache.pipeline_id')=?4)) AND (?6 IS NULL OR EXISTS(SELECT 1 FROM run_pipeline_identity i WHERE i.run_id=r.id AND i.pipeline_key=?6)) AND (?7 IS NULL OR r.id=?7) ORDER BY r.id DESC LIMIT ?5")?.query_map(params![r.project_id,r.finished_before,r.before,r.pipeline_id,r.limit+1,r.pipeline_key,r.run_id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > r.limit;
    let mut cursor = None;
    let mut entries = Vec::new();
    let mut items = Vec::new();
    let mut total = 0;
    for run in rows.into_iter().take(r.limit) {
        cursor = Some(run.clone());
        if expired(&c, &run, r.category)? {
            entries.push(json!({"run_id":run,"eligible":false,"reason":"already expired"}));
            continue;
        }
        if let Some(reason) = protection(&c, &run, r.category, r.finished_before)? {
            entries.push(json!({"run_id":run,"eligible":false,"reason":reason}));
            continue;
        }
        let measured = match r.category {
            Category::Logs => logs_inventory(&c, &run).map(|(h, b)| (h, b, "[]".to_string())),
            Category::SourceSnapshots => source_path(e, &run)
                .and_then(|p| gc::inventory(&p))
                .and_then(|(h, b, m)| Ok((h, b, serde_json::to_string(&m)?))),
        };
        match measured {
            Ok((h, b, m)) => {
                total += b;
                entries.push(json!({"run_id":run,"eligible":true,"file_bytes":b}));
                items.push((run, h, b, m));
            }
            Err(error) => {
                entries.push(json!({"run_id":run,"eligible":false,"reason":error.to_string()}))
            }
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let value = json!({"plan_id":id,"project_id":r.project_id,"pipeline_id":r.pipeline_id,"category":r.category,"pipeline_key":r.pipeline_key,"run_id":r.run_id,"finished_before":r.finished_before,"entries":entries,"estimated_file_bytes":total,"next_cursor":if more{cursor}else{None},"retained":["event identities and timestamps","run/test/input/artifact metadata","failures, pins, baselines, modules, active leases"],"accounting":"logical retained payload bytes; SQLite deletion reuses pages and does not promise filesystem shrinkage"});
    c.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        c.execute("INSERT INTO storage_gc_plans(id,project_id,pipeline_id,preview_json,created_at) VALUES (?1,?2,?3,?4,?5)",params![id,r.project_id,r.pipeline_id,value.to_string(),crate::db::now()])?;
        for (i, (run, h, b, m)) in items.into_iter().enumerate() {
            c.execute("INSERT INTO storage_gc_items(plan_id,ordinal,run_id,stage_id,fingerprint,manifest_json,bytes,state,object_key) VALUES (?1,?2,?3,'evidence',?4,?5,?6,'pending',?7)",params![id,i,run,h,m,b,format!("evidence:{}",r.category.name())])?;
        }
        Ok(())
    })();
    if result.is_ok() {
        c.execute_batch("COMMIT")?
    } else {
        c.execute_batch("ROLLBACK")?
    }
    result?;
    Ok(value)
}
pub async fn preview(e: Arc<Engine>, r: Preview) -> Result<Value> {
    tokio::task::spawn_blocking(move || preview_sync(&e, r)).await?
}
pub(crate) fn expire_logs(
    c: &rusqlite::Connection,
    run: &str,
    plan: &str,
    ordinal: u64,
    fingerprint: &str,
) -> Result<()> {
    c.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        let cutoff:i64=c.query_row("SELECT json_extract(preview_json,'$.finished_before') FROM storage_gc_plans WHERE id=?1",[plan],|r|r.get(0))?;
        if let Some(reason) = protection(c, run, Category::Logs, cutoff)? {
            c.execute("UPDATE storage_gc_items SET state='skipped',reason=?3 WHERE plan_id=?1 AND ordinal=?2",params![plan,ordinal,reason])?;
            return Ok(());
        }
        ensure!(
            logs_inventory(c, run)?.0 == fingerprint,
            invalid("log payload changed since preview; create a new preview")
        );
        c.execute(
            "INSERT INTO evidence_expiry VALUES (?1,'logs',?2,?3,?4)",
            params![run, plan, ordinal, crate::db::now()],
        )?;
        c.execute("DELETE FROM diagnostics WHERE run_id=?1", [run])?;
        c.execute("DELETE FROM diagnostic_scans WHERE run_id=?1", [run])?;
        c.execute(
            "DELETE FROM log_search WHERE rowid IN (SELECT id FROM log_events WHERE run_id=?1)",
            [run],
        )?;
        c.execute(
            "UPDATE log_events SET text='',chunk_id=NULL WHERE run_id=?1",
            [run],
        )?;
        c.execute("DELETE FROM log_chunks WHERE run_id=?1", [run])?;
        c.execute("UPDATE storage_gc_items SET state='deleted',reason='payload expired; SQLite pages reusable, no filesystem bytes credited' WHERE plan_id=?1 AND ordinal=?2",params![plan,ordinal])?;
        Ok(())
    })();
    if result.is_ok() {
        c.execute_batch("COMMIT")?
    } else {
        c.execute_batch("ROLLBACK")?
    }
    result
}
