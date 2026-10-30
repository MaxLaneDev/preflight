//! Opt-in bounded retention passes; every removal uses the shared audited GC engine.
use crate::{engine::Engine, model::invalid, storage_gc as gc};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    StageWorkspaces,
    ArtifactExpiry,
    Logs,
    SourceSnapshots,
    ModuleCopies,
    CacheArtifacts,
    RunArchives,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quota {
    pub high_bytes: u64,
    pub low_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskPressure {
    pub start_below_bytes: u64,
    pub stop_at_bytes: u64,
}
impl DiskPressure {
    fn needs_cleanup(&self, available: u64, total: u64, triggered: bool) -> Result<bool> {
        ensure!(
            total > 0 && available <= total,
            invalid("invalid filesystem space measurement")
        );
        ensure!(
            self.stop_at_bytes <= total,
            invalid("disk-space target exceeds filesystem capacity; no deletion approved")
        );
        Ok(available
            < if triggered {
                self.stop_at_bytes
            } else {
                self.start_below_bytes
            })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub name: String,
    pub project_id: String,
    pub pipeline_id: Option<String>,
    pub category: Category,
    pub ttl_seconds: u64,
    pub interval_seconds: u64,
    pub batch_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_pressure: Option<DiskPressure>,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        crate::control::validate_name(&self.name)?;
        uuid::Uuid::parse_str(&self.project_id)?;
        ensure!(
            !matches!(self.category, Category::RunArchives) || self.pipeline_id.is_none(),
            invalid(
                "automatic run_archives policies require project scope; manual archive preview supports UI pipeline keys"
            )
        );
        if let Some(scope) = &self.pipeline_id {
            crate::pipeline::identifier(scope)?;
        }
        ensure!(
            (60..=31_536_000).contains(&self.ttl_seconds),
            invalid("storage policy ttl_seconds must be 60..31536000")
        );
        ensure!(
            (60..=86_400).contains(&self.interval_seconds),
            invalid("storage policy interval_seconds must be 60..86400")
        );
        ensure!(
            (1..=100).contains(&self.batch_limit),
            invalid("storage policy batch_limit must be 1..100")
        );
        if let Some(quota) = &self.quota {
            ensure!(
                quota.high_bytes <= 9_007_199_254_740_991 && quota.low_bytes < quota.high_bytes,
                invalid("quota requires 0 <= low_bytes < high_bytes <= 9007199254740991")
            );
        }
        if let Some(disk) = &self.disk_pressure {
            ensure!(
                !matches!(self.category, Category::Logs),
                invalid(
                    "logs use TTL or logical quota; SQLite deletion does not release filesystem pages"
                )
            );
            ensure!(
                self.quota.is_none(),
                invalid("choose either quota or disk_pressure for a policy")
            );
            ensure!(
                disk.start_below_bytes > 0
                    && disk.start_below_bytes < disk.stop_at_bytes
                    && disk.stop_at_bytes <= 9_007_199_254_740_991,
                invalid(
                    "disk_pressure requires 0 < start_below_bytes < stop_at_bytes <= 9007199254740991"
                )
            );
        }
        Ok(())
    }
    fn hash(&self) -> Result<String> {
        Ok(crate::hashing::hex(Sha256::digest(serde_json::to_vec(
            self,
        )?)))
    }
}
pub fn categories() -> Value {
    json!([
        Category::StageWorkspaces,
        Category::ArtifactExpiry,
        Category::Logs,
        Category::SourceSnapshots,
        Category::ModuleCopies,
        Category::CacheArtifacts,
        Category::RunArchives
    ])
}
pub fn status(e: &Engine) -> Result<Value> {
    let c = e.db.connection()?;
    let mut policies = Vec::new();
    for policy in &e.current_config().storage_policies {
        let hash = policy.hash()?;
        let cycles=c.prepare("SELECT id,cutoff,cursor,plan_id,state,error,created_at,updated_at,observation_json FROM storage_policy_cycles WHERE policy_hash=?1 ORDER BY created_at DESC,id DESC LIMIT 3")?.query_map([&hash],|r|{
            let observation=r.get::<_,Option<String>>(8)?.map(|v|serde_json::from_str::<Value>(&v).unwrap_or_else(|_|json!({"complete":false,"reason":"invalid stored observation"})));
            Ok(json!({"cycle_id":r.get::<_,String>(0)?,"finished_before":r.get::<_,i64>(1)?,"cursor":r.get::<_,Option<String>>(2)?,"plan_id":r.get::<_,Option<String>>(3)?,"state":r.get::<_,String>(4)?,"error":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,i64>(6)?,"updated_at":r.get::<_,i64>(7)?,"quota_observation":if policy.disk_pressure.is_none(){observation.clone()}else{None},"disk_observation":if policy.disk_pressure.is_some(){observation}else{None}}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let available: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [&policy.project_id],
            |r| r.get(0),
        )?;
        policies.push(json!({"configuration":policy,"policy_hash":hash,"project_available":available,"recent_cycles":cycles}));
    }
    Ok(
        json!({"enabled":!policies.is_empty(),"policies":policies,"authorization":"operator storage_policies configuration; changes apply live","retained":["result/input/event metadata","pinned or referenced payloads","failure evidence","tests","history"],"supported_categories":categories(),"quota_mode":"optional high_bytes/low_bytes; soft known-file-length targets; TTL and GC protections always apply","disk_pressure_mode":"optional start_below_bytes/stop_at_bytes; filesystem space available to this process at data_dir; scoped GC only","category":"explicit operator-selected payload categories only; logs use logical accounting"}),
    )
}
async fn pass(e: &Arc<Engine>, policy: &Policy) -> Result<()> {
    policy.validate()?;
    e.db.project(&policy.project_id)?;
    let hash = policy.hash()?;
    let now = crate::db::now();
    let cycle = {
        let mut c = e.db.connection()?;
        let tx = c.transaction()?;
        let old:Option<(String,String,i64)>=tx.query_row("SELECT id,state,updated_at FROM storage_policy_cycles WHERE policy_hash=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[&hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let id = if let Some((id, state, updated)) = old {
            if state == "active" {
                id
            } else if now.saturating_sub(updated) < (policy.interval_seconds * 1000) as i64 {
                return Ok(());
            } else {
                create_cycle(&tx, policy, &hash, now)?
            }
        } else {
            create_cycle(&tx, policy, &hash, now)?
        };
        tx.commit()?;
        id
    };
    let outcome = page(e, policy, &cycle).await;
    if let Err(error) = &outcome {
        e.db.connection()?.execute(
            "UPDATE storage_policy_cycles SET state=?4,error=?2,updated_at=?3 WHERE id=?1",
            params![
                cycle,
                error.to_string().chars().take(1024).collect::<String>(),
                crate::db::now(),
                if matches!(
                    error.downcast_ref::<crate::model::Fault>(),
                    Some(crate::model::Fault::Busy)
                ) {
                    "active"
                } else {
                    "failed"
                }
            ],
        )?;
    }
    outcome
}
fn create_cycle(c: &rusqlite::Connection, policy: &Policy, hash: &str, now: i64) -> Result<String> {
    let previous:Option<(String,Option<String>)> = c.query_row("SELECT state,observation_json FROM storage_policy_cycles WHERE policy_hash=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[hash],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let resume = if let Some((state, Some(observation))) = previous {
        state == "failed" && serde_json::from_str::<Value>(&observation)?["triggered"] == true
    } else {
        false
    };
    let id = uuid::Uuid::now_v7().to_string();
    let cutoff = now
        .checked_sub((policy.ttl_seconds * 1000) as i64)
        .filter(|v| *v > 0)
        .ok_or_else(|| invalid("storage policy cutoff out of range"))?;
    c.execute("INSERT INTO storage_policy_cycles(id,policy_hash,policy_json,project_id,cutoff,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,'active',?6,?6)",params![id,hash,serde_json::to_string(policy)?,policy.project_id,cutoff,now])?;
    if resume {
        c.execute("UPDATE storage_policy_cycles SET observation_json=?2 WHERE id=?1",params![id,json!({"complete":false,"triggered":true,"reason":"continuing unmet pressure target"}).to_string()])?;
    }
    Ok(id)
}
async fn page(e: &Arc<Engine>, policy: &Policy, cycle: &str) -> Result<()> {
    let (cutoff, cursor, mut plan): (i64, Option<String>, Option<String>) =
        e.db.connection()?.query_row(
            "SELECT cutoff,cursor,plan_id FROM storage_policy_cycles WHERE id=?1",
            [cycle],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
    if plan.is_none() {
        if policy.quota.is_some() || policy.disk_pressure.is_some() {
            let previous: Option<String> = e.db.connection()?.query_row(
                "SELECT observation_json FROM storage_policy_cycles WHERE id=?1",
                [cycle],
                |r| r.get(0),
            )?;
            let triggered = previous
                .as_deref()
                .map(serde_json::from_str::<Value>)
                .transpose()?
                .is_some_and(|v| v["triggered"] == true);
            if !needs_cleanup(e, policy, cycle, triggered).await? {
                e.db.connection()?.execute("UPDATE storage_policy_cycles SET state='complete',error=NULL,updated_at=?2 WHERE id=?1",params![cycle,crate::db::now()])?;
                return Ok(());
            }
            // Persist the high-watermark crossing before approving any deletion page.
            e.db.connection()?.execute("UPDATE storage_policy_cycles SET observation_json=json_set(observation_json,'$.triggered',json('true')) WHERE id=?1",[cycle])?;
        }
        let preview = match policy.category {
            Category::RunArchives => {
                crate::archive_gc::preview(
                    e.clone(),
                    crate::archive_gc::Preview {
                        project_id: policy.project_id.clone(),
                        pipeline_key: None,
                        finished_before: cutoff,
                        target_bytes: policy.quota.as_ref().map(|q| q.low_bytes),
                        keep_latest: 1,
                        after: cursor,
                        limit: policy.batch_limit,
                    },
                )
                .await?
            }
            Category::ModuleCopies => {
                crate::module_gc::preview(
                    e.clone(),
                    gc::Preview {
                        project_id: policy.project_id.clone(),
                        pipeline_id: policy.pipeline_id.clone(),
                        before: cursor,
                        finished_before: Some(cutoff),
                        limit: policy.batch_limit,
                    },
                )
                .await?
            }
            Category::CacheArtifacts => {
                crate::cache_eviction::preview(
                    e.clone(),
                    crate::cache_eviction::Preview {
                        project_id: policy.project_id.clone(),
                        pipeline_id: policy.pipeline_id.clone(),
                        pipeline_key: None,
                        run_id: None,
                        finished_before: cutoff,
                        acknowledge_loss: true,
                        offset: cursor.as_deref().map(str::parse).transpose()?.unwrap_or(0),
                        limit: policy.batch_limit,
                    },
                )
                .await?
            }

            Category::Logs | Category::SourceSnapshots => {
                crate::evidence_expiry::preview(
                    e.clone(),
                    crate::evidence_expiry::Preview {
                        project_id: policy.project_id.clone(),
                        pipeline_id: policy.pipeline_id.clone(),
                        pipeline_key: None,
                        run_id: None,
                        category: if matches!(policy.category, Category::Logs) {
                            crate::evidence_expiry::Category::Logs
                        } else {
                            crate::evidence_expiry::Category::SourceSnapshots
                        },
                        acknowledge_loss: true,
                        finished_before: cutoff,
                        before: cursor,
                        limit: policy.batch_limit,
                    },
                )
                .await?
            }
            Category::StageWorkspaces => {
                gc::preview(
                    e.clone(),
                    gc::StagePreview {
                        project_id: policy.project_id.clone(),
                        pipeline_id: policy.pipeline_id.clone(),
                        before: cursor,
                        finished_before: Some(cutoff),
                        limit: policy.batch_limit,
                    },
                )
                .await?
            }
            Category::ArtifactExpiry => {
                crate::artifact_expiry::preview(
                    e.clone(),
                    crate::artifact_expiry::Preview {
                        project_id: policy.project_id.clone(),
                        pipeline_id: policy.pipeline_id.clone(),
                        before: cursor,
                        expire_artifacts: true,
                        finished_before: cutoff,
                        limit: policy.batch_limit,
                    },
                )
                .await?
            }
        };
        let id = preview["plan_id"]
            .as_str()
            .ok_or_else(|| invalid("GC preview did not return plan_id"))?
            .to_string();
        let next = preview["next_cursor"].as_str();
        e.db.connection()?.execute(
            "UPDATE storage_policy_cycles SET plan_id=?2,next_cursor=?3,error=NULL,updated_at=?4 WHERE id=?1",
            params![cycle, id, next, crate::db::now()],
        )?;
        plan = Some(id);
    }
    let plan = plan.unwrap();
    let result = gc::execute(
        e.clone(),
        gc::Execute {
            project_id: policy.project_id.clone(),
            plan_id: plan.clone(),
            request_id: crate::hashing::hex(Sha256::digest(format!("{cycle}:{plan}"))),
            actor: "storage_policy".into(),
            reason: format!("Configured policy {} cycle {cycle}", policy.name),
        },
    )
    .await?;
    if matches!(policy.category, Category::RunArchives) && result["complete"] != true {
        return Err(crate::model::Fault::Busy.into());
    }
    ensure!(
        result["complete"] == true,
        invalid("GC pass incomplete; inspect durable plan")
    );
    let at_target = if policy.quota.is_some() || policy.disk_pressure.is_some() {
        let required = needs_cleanup(e, policy, cycle, true).await?;
        let exhausted: bool = e.db.connection()?.query_row(
            "SELECT next_cursor IS NULL FROM storage_policy_cycles WHERE id=?1",
            [cycle],
            |r| r.get(0),
        )?;
        ensure!(
            !required || !exhausted,
            invalid(if policy.disk_pressure.is_some() {
                "disk-space target unmet; eligible candidate scan exhausted; protected/recent data retained"
            } else {
                "quota target unmet; eligible candidate scan exhausted; protected/recent data retained"
            })
        );
        !required
    } else {
        false
    };
    let deleted = if matches!(policy.category, Category::CacheArtifacts) {
        result["items"].as_array().map_or(0, |rows| {
            rows.iter().filter(|v| v["state"] == "deleted").count()
        })
    } else {
        0
    };
    e.db.connection()?.execute("UPDATE storage_policy_cycles SET state=CASE WHEN ?3 OR next_cursor IS NULL THEN 'complete' ELSE 'active' END,cursor=CASE WHEN ?4>0 AND next_cursor IS NOT NULL THEN CAST(MAX(CAST(next_cursor AS INTEGER)-?4,0) AS TEXT) ELSE next_cursor END,plan_id=CASE WHEN ?3 OR next_cursor IS NULL THEN plan_id ELSE NULL END,next_cursor=NULL,error=NULL,updated_at=?2 WHERE id=?1",params![cycle,crate::db::now(),at_target,deleted])?;
    Ok(())
}
async fn needs_cleanup(
    e: &Arc<Engine>,
    policy: &Policy,
    cycle: &str,
    triggered: bool,
) -> Result<bool> {
    if let Some(quota) = &policy.quota {
        let bytes = observe_quota(e, policy, cycle, triggered).await?;
        return Ok(bytes
            > if triggered {
                quota.low_bytes
            } else {
                quota.high_bytes
            });
    }
    let disk = policy
        .disk_pressure
        .as_ref()
        .ok_or_else(|| invalid("missing storage pressure condition"))?;
    let path = e.config.data_dir.clone();
    let measured = tokio::task::spawn_blocking(move || -> Result<(u64, u64)> {
        crate::storage_gc::directory(&path)?;
        let stats = fs2::statvfs(&path)?;
        Ok((stats.available_space(), stats.total_space()))
    })
    .await?;
    let observation = match &measured {
        Ok((available, total)) => {
            json!({"complete":true,"available_bytes":available,"total_bytes":total,"measured_at":crate::db::now(),"triggered":triggered,"condition":disk,"accounting":"filesystem space available to non-privileged processes at data_dir; live sample, not reclaimable bytes"})
        }
        Err(error) => {
            json!({"complete":false,"measured_at":crate::db::now(),"triggered":triggered,"reason":error.to_string().chars().take(1024).collect::<String>()})
        }
    };
    e.db.connection()?.execute(
        "UPDATE storage_policy_cycles SET observation_json=?2,updated_at=?3 WHERE id=?1",
        params![cycle, observation.to_string(), crate::db::now()],
    )?;
    let (available, total) = measured?;
    disk.needs_cleanup(available, total, triggered)
}
/// Complete bounded accounting is required; partial totals never authorize removal.
async fn observe_quota(
    e: &Arc<Engine>,
    policy: &Policy,
    cycle: &str,
    triggered: bool,
) -> Result<u64> {
    let mut cursor = None;
    let mut remaining = 200_000usize;
    let mut total = 0u64;
    let mut complete = false;
    let category = match policy.category {
        Category::RunArchives => {
            let engine = e.clone();
            let project = policy.project_id.clone();
            let bytes = tokio::task::spawn_blocking(move || {
                crate::archive_gc::scope_bytes(&engine, &project, None)
            })
            .await??;
            let observation = json!({"complete":true,"observed_file_bytes":bytes,"measured_at":crate::db::now(),"triggered":triggered,"accounting":"registered ready compressed archive file lengths; excludes imports inbox, quarantine and restorations"});
            e.db.connection()?.execute(
                "UPDATE storage_policy_cycles SET observation_json=?2,updated_at=?3 WHERE id=?1",
                params![cycle, observation.to_string(), crate::db::now()],
            )?;
            return Ok(bytes);
        }
        Category::StageWorkspaces => crate::storage_usage::Category::StageWorkspaces,
        Category::ArtifactExpiry => crate::storage_usage::Category::Artifacts,
        Category::Logs => crate::storage_usage::Category::Logs,
        Category::SourceSnapshots => crate::storage_usage::Category::SourceSnapshots,
        Category::ModuleCopies => crate::storage_usage::Category::ModuleCopies,
        Category::CacheArtifacts => crate::storage_usage::Category::CacheArtifacts,
    };
    for _ in 0..100 {
        if remaining == 0 {
            break;
        }
        let page = crate::storage_usage::usage(
            e.clone(),
            crate::storage_usage::Query {
                project_id: policy.project_id.clone(),
                pipeline_id: policy.pipeline_id.clone(),
                category,
                before: cursor,
                limit: 100,
                max_entries: remaining,
            },
        )
        .await?;
        total = total
            .checked_add(
                page["page_file_bytes"]
                    .as_u64()
                    .ok_or_else(|| invalid("invalid usage total"))?,
            )
            .ok_or_else(|| invalid("quota byte count overflow"))?;
        remaining = remaining
            .saturating_sub(page["entries_scanned"].as_u64().unwrap_or(remaining as u64) as usize);
        if page["page_complete"] != true {
            break;
        }
        cursor = page["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            complete = true;
            break;
        }
    }
    let observation = json!({"complete":complete,"observed_file_bytes":total,"measured_at":crate::db::now(),"triggered":triggered,"quota":policy.quota,"accounting":"known file lengths, not free disk; bounded live best effort","max_objects":10000,"max_entries":200000});
    e.db.connection()?.execute(
        "UPDATE storage_policy_cycles SET observation_json=?2,updated_at=?3 WHERE id=?1",
        params![cycle, observation.to_string(), crate::db::now()],
    )?;
    ensure!(
        complete,
        invalid(
            "quota measurement incomplete (active data, filesystem protection or scan budget); no new deletion page approved"
        )
    );
    Ok(total)
}
/// One page per configured policy. Restart resumes only policies still explicitly configured.
pub async fn tick(e: &Arc<Engine>) -> Result<()> {
    let Ok(_guard) = e.policy_gate.try_lock() else {
        return Ok(());
    };
    for policy in &e.current_config().storage_policies {
        if e.is_stopping() {
            break;
        }
        let hash = policy.hash()?;
        if !e
            .current_config()
            .storage_policies
            .iter()
            .any(|p| p.hash().is_ok_and(|current| current == hash))
        {
            continue;
        }
        if let Err(error) = pass(e, policy).await {
            eprintln!("storage policy {}: {error:#}", policy.name);
        }
    }
    Ok(())
}
pub async fn run(e: Arc<Engine>) {
    let stop = e.transport_stop();
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tokio::select! {biased;_=stop.cancelled()=>break,_=timer.tick()=>{}}
        if let Err(error) = tick(&e).await {
            eprintln!("storage policies: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disk_trigger_has_hysteresis_and_refuses_unreachable_targets() {
        let disk = DiskPressure {
            start_below_bytes: 100,
            stop_at_bytes: 200,
        };
        assert!(disk.needs_cleanup(99, 1000, false).unwrap());
        assert!(!disk.needs_cleanup(100, 1000, false).unwrap());
        assert!(disk.needs_cleanup(150, 1000, true).unwrap());
        assert!(!disk.needs_cleanup(200, 1000, true).unwrap());
        assert!(disk.needs_cleanup(99, 199, false).is_err());
        assert!(disk.needs_cleanup(1001, 1000, false).is_err());
    }
    #[test]
    fn policies_are_explicit_bounded_and_reject_unsupported_storage_classes() {
        let value = json!({"name":"stage-ttl","project_id":uuid::Uuid::now_v7().to_string(),"pipeline_id":"compiler","category":"stage_workspaces","ttl_seconds":86400,"interval_seconds":3600,"batch_limit":20});
        let policy: Policy = serde_json::from_value(value.clone()).unwrap();
        assert!(policy.validate().is_ok());
        for field in ["ttl_seconds", "interval_seconds", "batch_limit"] {
            let mut invalid = value.clone();
            invalid[field] = json!(0);
            assert!(
                serde_json::from_value::<Policy>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(
            serde_json::to_value(&policy)
                .unwrap()
                .get("quota")
                .is_none(),
            "old policy hashes must not gain an omitted field"
        );
        for (high, low, valid) in [
            (100, 0, true),
            (100, 99, true),
            (0, 0, false),
            (100, 100, false),
            (100, 101, false),
        ] {
            let mut quota = value.clone();
            quota["quota"] = json!({"high_bytes":high,"low_bytes":low});
            assert_eq!(
                serde_json::from_value::<Policy>(quota)
                    .unwrap()
                    .validate()
                    .is_ok(),
                valid
            );
        }
        assert!(
            serde_json::to_value(&policy)
                .unwrap()
                .get("disk_pressure")
                .is_none()
        );
        for (start, stop, valid) in [(1, 2, true), (0, 2, false), (2, 2, false), (3, 2, false)] {
            let mut disk = value.clone();
            disk["disk_pressure"] = json!({"start_below_bytes":start,"stop_at_bytes":stop});
            assert_eq!(
                serde_json::from_value::<Policy>(disk)
                    .unwrap()
                    .validate()
                    .is_ok(),
                valid
            );
        }
        let mut both = value.clone();
        both["quota"] = json!({"high_bytes":100,"low_bytes":0});
        both["disk_pressure"] = json!({"start_below_bytes":100,"stop_at_bytes":200});
        assert!(
            serde_json::from_value::<Policy>(both)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut artifacts = value.clone();
        artifacts["category"] = json!("artifact_expiry");
        assert!(
            serde_json::from_value::<Policy>(artifacts)
                .unwrap()
                .validate()
                .is_ok()
        );
        let mut sources = value;
        sources["category"] = json!("source_snapshots");
        assert!(
            serde_json::from_value::<Policy>(sources)
                .unwrap()
                .validate()
                .is_ok()
        );
    }
}
