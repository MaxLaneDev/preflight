//! Operator-attested release of crash-abandoned protection; never process termination.
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
    pub after: Option<String>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resolve {
    pub project_id: String,
    pub lease_id: String,
    pub request_id: String,
    pub actor: String,
    pub reason: String,
    /// Explicit operator attestation, not a claim established by the server.
    pub process_stopped: bool,
}
impl Db {
    pub fn abandoned_operations(&self, r: &Query) -> Result<Value> {
        self.project(&r.project_id)?;
        let c = self.connection()?;
        let rows = c.prepare("SELECT l.id,l.run_id,l.kind FROM operation_leases l JOIN runs r ON r.id=l.run_id WHERE r.project_id=?1 AND l.state='abandoned' AND (?2 IS NULL OR l.id>?2) ORDER BY l.id LIMIT 101")?
            .query_map(params![r.project_id,r.after], |row| Ok(json!({"lease_id":row.get::<_,String>(0)?,"run_id":row.get::<_,String>(1)?,"kind":row.get::<_,String>(2)?})))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > 100;
        let rows = rows.into_iter().take(100).collect::<Vec<_>>();
        let next = if more {
            rows.last().map(|r| r["lease_id"].clone())
        } else {
            None
        };
        Ok(
            json!({"project_id":r.project_id,"operations":rows,"next_cursor":next,"policy":"inspect runs_operations for resources; verify processes stopped before explicit resolution"}),
        )
    }
    pub fn resolve_operation(&self, r: &Resolve) -> Result<Value> {
        crate::control::validate_name(&r.request_id)?;
        ensure!(
            r.process_stopped,
            invalid("operator must verify process termination before releasing protection")
        );
        ensure!(
            !r.actor.trim().is_empty() && r.actor.len() <= 128,
            invalid("actor must contain 1..128 bytes")
        );
        ensure!(
            !r.reason.trim().is_empty() && r.reason.len() <= 1024,
            invalid("reason must contain 1..1024 bytes")
        );
        let request = serde_json::to_string(r)?;
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old:Option<(String,String)>=tx.query_row("SELECT request_json,result_json FROM operation_resolutions WHERE project_id=?1 AND request_id=?2",params![r.project_id,r.request_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((previous, result)) = old {
            ensure!(previous == request, Fault::IdempotencyConflict);
            return Ok(serde_json::from_str(&result)?);
        }
        let managed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM container_jobs WHERE lease_id=?1 AND state!='cleaned')",
            [&r.lease_id],
            |row| row.get(0),
        )?;
        ensure!(
            !managed,
            invalid(
                "container lease requires verified Docker cleanup; process_stopped does not prove daemon quiescence"
            )
        );
        let lease:Option<(String,String,String)>=tx.query_row("SELECT l.run_id,l.state,r.status FROM operation_leases l JOIN runs r ON r.id=l.run_id WHERE l.id=?1 AND r.project_id=?2",params![r.lease_id,r.project_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        let (run, state, status) = lease.ok_or(Fault::NotFound)?;
        ensure!(
            state == "abandoned",
            invalid("only abandoned operations may be resolved")
        );
        let terminal = |s: &str| {
            matches!(
                s,
                "succeeded"
                    | "failed"
                    | "cancelled"
                    | "timed_out"
                    | "infrastructure_error"
                    | "blocked"
                    | "skipped"
            )
        };
        ensure!(terminal(&status), invalid("operation run is not terminal"));
        let resources = tx
            .prepare(
                "SELECT resource FROM operation_resources WHERE lease_id=?1 ORDER BY resource",
            )?
            .query_map([&r.lease_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for resource in &resources {
            if let Some(id) = resource.strip_prefix("run:") {
                let status: String =
                    tx.query_row("SELECT status FROM runs WHERE id=?1", [id], |row| {
                        row.get(0)
                    })?;
                ensure!(terminal(&status), invalid("referenced run is not terminal"));
            }
        }
        let result = json!({"project_id":r.project_id,"lease_id":r.lease_id,"run_id":run,"state":"released","actor":r.actor,"reason":r.reason,"resources":resources,"process_termination":"operator_attested","physical_bytes_deleted":0});
        tx.execute("UPDATE operation_leases SET state='released',finished_at=?2 WHERE id=?1 AND state='abandoned'",params![r.lease_id,crate::db::now()])?;
        tx.execute("INSERT INTO operation_resolutions(project_id,request_id,lease_id,request_json,result_json) VALUES (?1,?2,?3,?4,?5)",params![r.project_id,r.request_id,r.lease_id,request,serde_json::to_string(&result)?])?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cache::{CleanupExecute, CleanupPreview},
        model::{CommandSpec, StartBuild},
    };
    use std::collections::BTreeSet;

    #[test]
    fn audited_release_unblocks_only_its_scope_and_survives_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("db");
        let db = Db::new(&path).unwrap();
        let project = db.register("p", tmp.path()).unwrap();
        db.insert_run(
            "r",
            &StartBuild {
                project_id: project.id.clone(),
                command: CommandSpec {
                    program: "tool".into(),
                    args: vec![],
                    timeout_seconds: 5,
                },
            },
            tmp.path(),
        )
        .unwrap();
        db.cache_publish("r", "native", "compiler", "key").unwrap();
        let lease = db.begin_operation("r", "build", BTreeSet::new()).unwrap();
        let id = lease.id.clone();
        let mut request = Resolve {
            project_id: project.id.clone(),
            lease_id: id.clone(),
            request_id: "resolve-one".into(),
            actor: "operator".into(),
            reason: "verified child exited".into(),
            process_stopped: true,
        };
        assert!(db.resolve_operation(&request).is_err());
        std::mem::forget(lease);
        db.reconcile().unwrap();
        assert_eq!(
            db.abandoned_operations(&Query {
                project_id: project.id.clone(),
                after: None
            })
            .unwrap()["operations"][0]["lease_id"],
            id
        );
        let preview = CleanupPreview {
            project_id: project.id.clone(),
            pipeline_id: Some("compiler".into()),
        };
        let plan = db.cache_cleanup_preview(&preview).unwrap();
        let execute = CleanupExecute {
            plan_id: plan["plan_id"].as_str().unwrap().into(),
        };
        let blocked = db.cache_cleanup_execute(&execute).unwrap();
        assert_eq!(blocked["protected"].as_array().unwrap().len(), 1);
        request.process_stopped = false;
        assert!(db.resolve_operation(&request).is_err());
        request.process_stopped = true;
        request.project_id = "wrong".into();
        assert!(db.resolve_operation(&request).is_err());
        request.project_id = project.id.clone();
        let resolved = db.resolve_operation(&request).unwrap();
        assert_eq!(resolved["state"], "released");
        let operation = db.operations("r").unwrap()["operations"][0].clone();
        assert!(
            operation["finished_at"].as_i64().unwrap() >= operation["started_at"].as_i64().unwrap()
        );
        assert_eq!(
            db.operations("r").unwrap()["operations"][0]["resolution"],
            resolved
        );
        assert_eq!(resolved, db.resolve_operation(&request).unwrap());
        request.reason = "different".into();
        assert!(db.resolve_operation(&request).is_err());
        request.reason = "verified child exited".into();
        assert_eq!(db.cache_cleanup_execute(&execute).unwrap(), blocked);
        let plan = db.cache_cleanup_preview(&preview).unwrap();
        let cleaned = db
            .cache_cleanup_execute(&CleanupExecute {
                plan_id: plan["plan_id"].as_str().unwrap().into(),
            })
            .unwrap();
        assert_eq!(cleaned["invalidated"].as_array().unwrap().len(), 1);
        assert!(
            db.abandoned_operations(&Query {
                project_id: project.id.clone(),
                after: None
            })
            .unwrap()["operations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        drop(db);
        let db = Db::new(&path).unwrap();
        assert_eq!(resolved, db.resolve_operation(&request).unwrap());
        assert_eq!(
            db.connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM operation_resolutions", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        request.request_id = "different-key".into();
        assert!(db.resolve_operation(&request).is_err());
    }

    #[test]
    fn referenced_running_run_and_other_leases_remain_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::new(&tmp.path().join("db")).unwrap();
        let project = db.register("p", tmp.path()).unwrap();
        for id in ["r", "owner"] {
            db.insert_run(
                id,
                &StartBuild {
                    project_id: project.id.clone(),
                    command: CommandSpec {
                        program: "tool".into(),
                        args: vec![],
                        timeout_seconds: 5,
                    },
                },
                tmp.path(),
            )
            .unwrap();
        }
        let lease = db
            .begin_operation(
                "r",
                "shared_execution",
                BTreeSet::from(["run:owner".into()]),
            )
            .unwrap();
        let id = lease.id.clone();
        std::mem::forget(lease);
        db.reconcile().unwrap();
        db.connection()
            .unwrap()
            .execute("UPDATE runs SET status='running' WHERE id='owner'", [])
            .unwrap();
        let request = Resolve {
            project_id: project.id,
            lease_id: id,
            request_id: "resolve".into(),
            actor: "operator".into(),
            reason: "inspection".into(),
            process_stopped: true,
        };
        assert!(db.resolve_operation(&request).is_err());
        assert_eq!(
            db.operations("r").unwrap()["operations"][0]["state"],
            "abandoned"
        );
        db.connection()
            .unwrap()
            .execute("UPDATE runs SET status='cancelled' WHERE id='owner'", [])
            .unwrap();
        let other = db.begin_operation("r", "other", BTreeSet::new()).unwrap();
        db.resolve_operation(&request).unwrap();
        let state: String = db
            .connection()
            .unwrap()
            .query_row(
                "SELECT state FROM operation_leases WHERE id=?1",
                [&other.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "active");
    }
}
