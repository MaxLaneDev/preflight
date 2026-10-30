//! Durable explicit retention and reference evidence; no filesystem deletion.
use crate::{
    db::Db,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub project_id: String,
    pub sha256: String,
    #[serde(default)]
    pub offset: u32,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub project_id: String,
    pub sha256: String,
    /// Stable retention owner, unique within this project and digest.
    pub owner: String,
    pub pipeline_id: Option<String>,
    pub pinned: bool,
    /// Zero creates a new owner; subsequent changes require the returned revision.
    pub expected_revision: u64,
    pub actor: String,
    pub reason: String,
    /// Idempotency key scoped to the project; exact replay returns its original result.
    pub request_id: String,
}
impl Db {
    pub fn module_pin_set(&self, r: &Pin) -> Result<Value> {
        crate::modules::validate_digest(&r.sha256)?;
        for id in [&r.owner, &r.request_id] {
            crate::control::validate_name(id)?;
        }
        if let Some(id) = &r.pipeline_id {
            crate::pipeline::identifier(id)?;
        }
        ensure!(
            !r.actor.trim().is_empty() && r.actor.len() <= 128,
            invalid("actor must contain 1..128 bytes")
        );
        ensure!(
            !r.reason.trim().is_empty() && r.reason.len() <= 1024,
            invalid("reason must contain 1..1024 bytes")
        );
        ensure!(
            r.expected_revision < i64::MAX as u64,
            invalid("revision out of range")
        );
        let request = serde_json::to_string(r)?;
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replay:Option<(String,String)>=tx.query_row("SELECT request_json,result_json FROM module_pin_changes WHERE project_id=?1 AND request_id=?2",params![r.project_id,r.request_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((old, result)) = replay {
            ensure!(old == request, Fault::IdempotencyConflict);
            return Ok(serde_json::from_str(&result)?);
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM modules WHERE project_id=?1 AND sha256=?2)",
            params![r.project_id, r.sha256],
            |row| row.get(0),
        )?;
        ensure!(exists, Fault::NotFound);
        if r.pinned {
            let evicted: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM module_evictions WHERE project_id=?1 AND sha256=?2)",
                params![r.project_id, r.sha256],
                |row| row.get(0),
            )?;
            ensure!(
                !evicted,
                invalid("republish the evicted module before pinning")
            );
        }
        let previous:Option<(u64,Option<String>)>=tx.query_row("SELECT revision,pipeline_id FROM module_pins WHERE project_id=?1 AND sha256=?2 AND owner=?3",params![r.project_id,r.sha256,r.owner],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        ensure!(
            previous.as_ref().map_or(0, |v| v.0) == r.expected_revision,
            Fault::Conflict
        );
        if let Some((_, scope)) = previous {
            ensure!(
                scope == r.pipeline_id,
                invalid("retention owner pipeline scope is immutable")
            );
        }
        ensure!(
            r.expected_revision > 0 || r.pinned,
            invalid("cannot release an unknown retention owner")
        );
        let revision = r.expected_revision + 1;
        tx.execute("INSERT INTO module_pins VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(project_id,sha256,owner) DO UPDATE SET pinned=excluded.pinned,revision=excluded.revision,actor=excluded.actor,reason=excluded.reason",params![r.project_id,r.sha256,r.owner,r.pipeline_id,r.pinned,revision,r.actor,r.reason])?;
        let result = json!({"project_id":r.project_id,"sha256":r.sha256,"owner":r.owner,"pipeline_id":r.pipeline_id,"pinned":r.pinned,"revision":revision,"actor":r.actor,"reason":r.reason,"request_id":r.request_id,"physical_bytes_deleted":0});
        tx.execute("INSERT INTO module_pin_changes(project_id,request_id,request_json,result_json) VALUES (?1,?2,?3,?4)",params![r.project_id,r.request_id,request,serde_json::to_string(&result)?])?;
        tx.commit()?;
        Ok(result)
    }
    pub fn module_retention(&self, r: &Query) -> Result<Value> {
        crate::modules::validate_digest(&r.sha256)?;
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        let artifact: String = tx
            .query_row(
                "SELECT artifact_json FROM modules WHERE project_id=?1 AND sha256=?2",
                params![r.project_id, r.sha256],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Fault::NotFound)?;
        let artifact: crate::model::Artifact = serde_json::from_str(&artifact)?;
        let counts=tx.prepare("SELECT kind,COUNT(*) FROM module_retention_references WHERE project_id=?1 AND sha256=?2 GROUP BY kind ORDER BY kind")?.query_map(params![r.project_id,r.sha256],|row|Ok((row.get::<_,String>(0)?,row.get::<_,u64>(1)?)))?.collect::<rusqlite::Result<std::collections::BTreeMap<_,_>>>()?;
        let total: u64 = counts.values().sum();
        let refs=tx.prepare("SELECT kind,owner_id,run_id,stage_id,pipeline_id,state FROM module_retention_references WHERE project_id=?1 AND sha256=?2 ORDER BY kind,owner_id,run_id,stage_id,pipeline_id LIMIT 100 OFFSET ?3")?.query_map(params![r.project_id,r.sha256,r.offset],|row|Ok(json!({"kind":row.get::<_,String>(0)?,"owner_id":row.get::<_,String>(1)?,"run_id":row.get::<_,Option<String>>(2)?,"stage_id":row.get::<_,Option<String>>(3)?,"pipeline_id":row.get::<_,Option<String>>(4)?,"state":row.get::<_,String>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        // Pin records include released owners so clients can safely re-pin using their revision.
        let pins=tx.prepare("SELECT owner,pipeline_id,pinned,revision,actor,reason FROM module_pins WHERE project_id=?1 AND sha256=?2 ORDER BY owner LIMIT 100 OFFSET ?3")?.query_map(params![r.project_id,r.sha256,r.offset],|row|Ok(json!({"owner":row.get::<_,String>(0)?,"pipeline_id":row.get::<_,Option<String>>(1)?,"pinned":row.get::<_,bool>(2)?,"revision":row.get::<_,u64>(3)?,"actor":row.get::<_,String>(4)?,"reason":row.get::<_,String>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let pin_total: u64 = tx.query_row(
            "SELECT COUNT(*) FROM module_pins WHERE project_id=?1 AND sha256=?2",
            params![r.project_id, r.sha256],
            |row| row.get(0),
        )?;
        let evicted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM module_evictions WHERE project_id=?1 AND sha256=?2)",
            params![r.project_id, r.sha256],
            |row| row.get(0),
        )?;
        tx.commit()?;
        let next = u64::from(r.offset) + 100;
        Ok(
            json!({"project_id":r.project_id,"sha256":r.sha256,"storage_path":artifact.path,"logical_bytes":artifact.size,"protected":true,"protection_scope":"retained identity and history; copy eligibility requires module_gc_preview","counts":counts,"references":refs,"total":total,"next_cursor":(next<total).then_some(next),"pins":pins,"pin_total":pin_total,"pins_next_cursor":(next<pin_total).then_some(next),"copy_available":!evicted,"physical_deletion_enabled":true,"policy":"module_gc_preview determines eligibility; pins and consumed inputs protect copies; eviction preserves producer artifacts and history; pagination is a live view, not a deletion plan"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Artifact, CommandSpec, StartBuild};
    use std::collections::BTreeSet;
    fn setup(db: &Db, root: &std::path::Path) -> (String, Artifact) {
        let project = db.register("p", root).unwrap();
        db.insert_run(
            "producer",
            &StartBuild {
                project_id: project.id.clone(),
                command: CommandSpec {
                    program: "tool".into(),
                    args: vec![],
                    timeout_seconds: 5,
                },
            },
            root,
        )
        .unwrap();
        let a = Artifact {
            run_id: "producer".into(),
            stage_id: "native".into(),
            name: "module".into(),
            kind: crate::pipeline::ArtifactKind::File,
            sha256: "a".repeat(64),
            size: 7,
            path: root.join("module"),
        };
        std::fs::write(&a.path, "content").unwrap();
        db.finish_stage(
            "producer",
            "native",
            crate::model::RunState::Succeeded,
            Some(0),
            None,
            None,
            std::slice::from_ref(&a),
        )
        .unwrap();
        db.publish_module(&project.id, &a, None).unwrap();
        (project.id, a)
    }
    fn pin(project: &str, digest: &str) -> Pin {
        Pin {
            project_id: project.into(),
            sha256: digest.into(),
            owner: "compiler-baseline".into(),
            pipeline_id: Some("compiler".into()),
            pinned: true,
            expected_revision: 0,
            actor: "user".into(),
            reason: "retain baseline".into(),
            request_id: "first".into(),
        }
    }
    #[test]
    fn references_survive_scoped_invalidation_unpin_and_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        let db = Db::new(&path).unwrap();
        let (project, a) = setup(&db, temp.path());
        db.record_module_input("producer", "native", &temp.path().join("input"), &a)
            .unwrap();
        db.cache_publish("producer", "native", "compiler", "k")
            .unwrap();
        db.cache_publish("producer", "native", "other", "k")
            .unwrap();
        let mut r = pin(&project, &a.sha256);
        let first = db.module_pin_set(&r).unwrap();
        assert_eq!(db.module_pin_set(&r).unwrap(), first);
        let query = Query {
            project_id: project.clone(),
            sha256: a.sha256.clone(),
            offset: 0,
        };
        let lease = db
            .begin_operation(
                "producer",
                "consume",
                BTreeSet::from([format!("module:{}", a.sha256)]),
            )
            .unwrap();
        let report = db.module_retention(&query).unwrap();
        assert_eq!(report["counts"]["pin"], 1);
        assert_eq!(report["counts"]["operation"], 1);
        assert_eq!(report["counts"]["consumed_input"], 1);
        drop(lease);
        assert_eq!(report["counts"]["cache_entry"], 2);
        let plan = db
            .cache_cleanup_preview(&crate::cache::CleanupPreview {
                project_id: project.clone(),
                pipeline_id: Some("compiler".into()),
            })
            .unwrap();
        let cleaned = db
            .cache_cleanup_execute(&crate::cache::CleanupExecute {
                plan_id: plan["plan_id"].as_str().unwrap().into(),
            })
            .unwrap();
        assert_eq!(cleaned["invalidated"].as_array().unwrap().len(), 1);
        assert_eq!(
            db.module_retention(&query).unwrap()["counts"]["cache_entry"],
            1
        );
        let lease = db
            .begin_operation(
                "producer",
                "consume",
                BTreeSet::from([format!("module:{}", a.sha256)]),
            )
            .unwrap();
        std::mem::forget(lease);
        db.reconcile().unwrap();
        let report = db.module_retention(&query).unwrap();
        assert!(
            report["references"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["kind"] == "operation" && r["state"] == "abandoned")
        );
        r.pinned = false;
        r.expected_revision = 1;
        r.request_id = "release".into();
        let released = db.module_pin_set(&r).unwrap();
        assert_eq!(released["revision"], 2);
        drop(db);
        let db = Db::new(&path).unwrap();
        assert_eq!(db.module_pin_set(&r).unwrap(), released);
        let report = db.module_retention(&query).unwrap();
        assert!(report["counts"].get("pin").is_none());
        assert_eq!(report["pins"][0]["revision"], 2);
        assert_eq!(report["counts"]["publication"], 1);
        assert_eq!(report["counts"]["producer"], 1);
        assert_eq!(std::fs::read_to_string(a.path).unwrap(), "content");
        r.reason = "different request".into();
        assert!(db.module_pin_set(&r).unwrap_err().is::<Fault>());
        r.request_id = "stale".into();
        assert!(db.module_pin_set(&r).is_err());
        r.expected_revision = 2;
        r.pipeline_id = Some("other".into());
        assert!(db.module_pin_set(&r).is_err());
        assert_eq!(
            db.module_pin_set(&pin(&project, &query.sha256)).unwrap(),
            first
        );
    }
    #[test]
    fn pins_are_project_isolated_paged_and_competing_updates_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        let db = Db::new(&path).unwrap();
        let (project, a) = setup(&db, temp.path());
        let other_dir = temp.path().join("other");
        std::fs::create_dir(&other_dir).unwrap();
        let other = db.register("other", &other_dir).unwrap();
        assert!(db.module_pin_set(&pin(&other.id, &a.sha256)).is_err());
        assert!(
            db.module_retention(&Query {
                project_id: other.id,
                sha256: a.sha256.clone(),
                offset: 0
            })
            .is_err()
        );
        for i in 0..101 {
            let mut r = pin(&project, &a.sha256);
            r.owner = format!("owner-{i:03}");
            r.request_id = r.owner.clone();
            db.module_pin_set(&r).unwrap();
        }
        let mut query = Query {
            project_id: project.clone(),
            sha256: a.sha256.clone(),
            offset: 0,
        };
        let report = db.module_retention(&query).unwrap();
        assert_eq!(report["pin_total"], 101);
        assert_eq!(report["pins"].as_array().unwrap().len(), 100);
        assert_eq!(report["pins_next_cursor"], 100);
        query.offset = 100;
        let tail = db.module_retention(&query).unwrap();
        assert_eq!(tail["pins"].as_array().unwrap().len(), 1);
        assert!(tail["pins_next_cursor"].is_null());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = (0..2)
            .map(|i| {
                let db = Db::new(&path).unwrap();
                let barrier = barrier.clone();
                let mut r = pin(&project, &a.sha256);
                r.owner = "owner-000".into();
                r.expected_revision = 1;
                r.pinned = false;
                r.request_id = format!("race-{i}");
                std::thread::spawn(move || {
                    barrier.wait();
                    db.module_pin_set(&r).is_ok()
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|h| h.join().unwrap().then_some(()))
                .count(),
            1
        );
        let c = db.connection().unwrap();
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM module_pin_changes", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            102
        );
    }
}
