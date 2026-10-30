//! Frozen, audited cache-output eviction plans; reuse existing artifact quarantine and protection.
use crate::{engine::Engine, model::invalid, storage_gc as gc};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub project_id: String,
    pub pipeline_id: Option<String>,
    pub pipeline_key: Option<String>,
    pub run_id: Option<String>,
    pub finished_before: i64,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "limit")]
    pub limit: usize,
    pub acknowledge_loss: bool,
}
fn limit() -> usize {
    20
}
pub(crate) fn protection(
    c: &rusqlite::Connection,
    project: &str,
    run: &str,
    stage: &str,
    entry: &str,
) -> Result<Option<String>> {
    let owned:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM stage_cache WHERE id=?1 AND project_id=?2 AND producer_run=?3 AND producer_stage=?4 AND state='active')",params![entry,project,run,stage],|r|r.get(0))?;
    if !owned {
        return Ok(Some(
            "cache entry changed or invalidated; create a new preview".into(),
        ));
    }
    crate::artifact_expiry::protection_except_cache(c, run, stage, Some(entry))
}
fn preview_sync(e: &Engine, r: Preview) -> Result<Value> {
    ensure!(
        r.acknowledge_loss,
        invalid("acknowledge_loss=true required: cached output bytes will expire")
    );
    ensure!(
        (1..=100).contains(&r.limit)
            && r.offset <= 100000
            && r.finished_before > 0
            && r.finished_before <= crate::db::now(),
        invalid("bounded page and past finished_before required")
    );
    e.db.project(&r.project_id)?;
    if let Some(p) = &r.pipeline_id {
        crate::pipeline::identifier(p)?;
    }
    let c = e.db.connection()?;
    let rows=c.prepare("SELECT c.id,c.producer_run,c.producer_stage,COALESCE(a.hits,0),COALESCE(a.last_used,r.finished_at),COALESCE(a.rebuild_ms,0) FROM stage_cache c JOIN runs r ON r.id=c.producer_run LEFT JOIN cache_access a ON a.cache_id=c.id WHERE c.project_id=?1 AND (?2 IS NULL OR c.pipeline_id=?2) AND c.state='active' AND r.finished_at<?3 AND (?7 IS NULL OR EXISTS(SELECT 1 FROM run_pipeline_identity i WHERE i.run_id=r.id AND i.pipeline_key=?7)) AND (?8 IS NULL OR r.id=?8) ORDER BY MAX(COALESCE(a.rebuild_ms,0),1)*1.0*(COALESCE(a.hits,0)+1)/MAX(?4-COALESCE(a.last_used,r.finished_at),1),COALESCE(a.last_used,r.finished_at),c.id LIMIT ?5 OFFSET ?6")?.query_map(params![r.project_id,r.pipeline_id,r.finished_before,crate::db::now(),r.limit+1,r.offset,r.pipeline_key,r.run_id],|v|Ok((v.get::<_,String>(0)?,v.get::<_,String>(1)?,v.get::<_,String>(2)?,v.get::<_,u64>(3)?,v.get::<_,i64>(4)?,v.get::<_,u64>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > r.limit;
    let mut entries = Vec::new();
    let mut items = Vec::new();
    let mut total = 0;
    for (entry, run, stage, hits, last_used, rebuild_ms) in rows.into_iter().take(r.limit) {
        if let Some(reason) = protection(&c, &r.project_id, &run, &stage, &entry)? {
            entries.push(json!({"entry_id":entry,"run_id":run,"stage_id":stage,"eligible":false,"reason":reason}));
            continue;
        }
        let measured =
            crate::artifact_expiry::path(e, &run, &stage).and_then(|p| gc::inventory(&p));
        match measured {
            Ok((h, b, m)) => {
                total += b;
                entries.push(json!({"entry_id":entry,"run_id":run,"stage_id":stage,"eligible":true,"file_bytes":b,"hits":hits,"last_used":last_used,"rebuild_ms":rebuild_ms}));
                items.push((entry, run, stage, h, b, serde_json::to_string(&m)?));
            }
            Err(error) => {
                entries.push(json!({"entry_id":entry,"eligible":false,"reason":error.to_string()}))
            }
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let next = more.then(|| (r.offset + r.limit).to_string());
    let value = json!({"plan_id":id,"project_id":r.project_id,"pipeline_id":r.pipeline_id,"category":"cache_artifacts","pipeline_key":r.pipeline_key,"run_id":r.run_id,"finished_before":r.finished_before,"entries":entries,"estimated_file_bytes":total,"next_cursor":next,"ranking":"ascending rebuild_ms*(hits+1)/idle_ms, then least-recent use; unknown historical cost uses 1ms","retained":["metadata","pinned, module, baseline, leased and multiply-owned outputs"],"pagination":"live ranking; each returned plan freezes exact objects"});
    c.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        c.execute("INSERT INTO storage_gc_plans(id,project_id,pipeline_id,preview_json,created_at) VALUES (?1,?2,?3,?4,?5)",params![id,r.project_id,r.pipeline_id,value.to_string(),crate::db::now()])?;
        for (i, (entry, run, stage, h, b, m)) in items.into_iter().enumerate() {
            c.execute("INSERT INTO storage_gc_items(plan_id,ordinal,run_id,stage_id,fingerprint,manifest_json,bytes,state,object_key) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8)",params![id,i,run,stage,h,m,b,format!("cache_artifacts:{entry}")])?;
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
