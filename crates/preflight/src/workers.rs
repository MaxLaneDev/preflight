//! Authenticated worker identity, liveness and recovery-aware job readiness.
use crate::{
    db::Db,
    model::{Fault, invalid},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) fn blocked_operations(c: &rusqlite::Connection, worker: &str) -> rusqlite::Result<u64> {
    c.query_row("SELECT count(*) FROM worker_jobs j JOIN operation_leases l ON l.id=j.operation_id WHERE j.worker_id=?1 AND l.state='abandoned'",[worker],|row|row.get(0))
}

type EnrollmentRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
);

pub const PROTOCOL: u32 = 1;
pub const HEARTBEAT_TTL: i64 = 30;
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("OS randomness unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) fn hash(secret: &str) -> Result<String> {
    ensure!(
        secret.len() == 64
            && secret
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        invalid("invalid worker credential")
    );
    Ok(crate::hashing::hex(Sha256::digest(secret.as_bytes())))
}
fn platform(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 32
            && value
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
        invalid("invalid platform identifier")
    );
    Ok(())
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateEnrollment {
    pub project_id: String,
    pub name: String,
    pub os: String,
    pub arch: String,
    /// 1..600 seconds, measured by server time.
    pub ttl_seconds: u32,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub project_id: String,
    pub after: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Revoke {
    pub generation: u64,
    pub project_id: String,
    pub worker_id: String,
    pub actor: String,
    pub reason: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enroll {
    pub protocol: u32,
    pub enrollment_token: String,
    /// Generated and persisted by the worker before enrolling; never logged.
    pub credential: String,
    pub os: String,
    pub arch: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    #[serde(default)]
    pub job_protocol: Option<u32>,
    pub protocol: u32,
    pub sequence: u64,
}
impl Db {
    pub fn worker_enrollment(&self, r: &CreateEnrollment) -> Result<Value> {
        self.project(&r.project_id)?;
        crate::control::validate_name(&r.name)?;
        platform(&r.os)?;
        platform(&r.arch)?;
        ensure!(
            (1..=600).contains(&r.ttl_seconds),
            invalid("enrollment lifetime must be 1..600 seconds")
        );
        let token = secret()?;
        let id = uuid::Uuid::now_v7().to_string();
        let expiry = now() + i64::from(r.ttl_seconds);
        self.connection()?.execute("INSERT INTO worker_enrollments(id,project_id,name,expected_os,expected_arch,token_hash,expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![id,r.project_id,r.name,r.os,r.arch,hash(&token)?,expiry])?;
        Ok(
            json!({"enrollment_id":id,"enrollment_token":token,"expires_at":expiry,"worker_protocol":PROTOCOL,"secret_handling":"send over authenticated encrypted transport; never put in pipeline definitions or logs"}),
        )
    }
    pub fn worker_enroll(&self, r: &Enroll) -> Result<Value> {
        ensure!(
            r.protocol == PROTOCOL,
            invalid("unsupported worker protocol")
        );
        platform(&r.os)?;
        platform(&r.arch)?;
        let token = hash(&r.enrollment_token)?;
        let credential = hash(&r.credential)?;
        ensure!(
            token != credential,
            invalid("worker credential must differ from enrollment token")
        );
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row:Option<EnrollmentRow>=tx.query_row("SELECT id,project_id,name,expected_os,expected_arch,expires_at,used_by,credential_hash FROM worker_enrollments WHERE token_hash=?1",[token],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
        let (enrollment, project, name, os, arch, expires, used, previous) =
            row.ok_or(Fault::NotFound)?;
        ensure!(
            os == r.os && arch == r.arch,
            invalid("worker platform does not match enrollment")
        );
        let id = if let Some(id) = used {
            ensure!(
                previous.as_deref() == Some(&credential),
                invalid("enrollment already consumed")
            );
            let current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM workers WHERE id=?1 AND state='registered' AND credential_hash=?2)",params![id,credential],|r|r.get(0))?;
            ensure!(
                current,
                invalid("worker credential was revoked or replaced")
            );
            id
        } else {
            ensure!(now() < expires, invalid("enrollment expired"));
            let old: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT id,state,credential_hash FROM workers WHERE project_id=?1 AND name=?2",
                    params![project, name],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let id = if let Some((id, state, old_credential)) = old {
                ensure!(
                    old_credential != credential,
                    invalid("replacement must use a new worker credential")
                );
                ensure!(
                    state == "revoked",
                    invalid(
                        "worker name already registered; reconnect with existing credential or revoke first"
                    )
                );
                tx.execute("UPDATE workers SET os=?2,arch=?3,credential_hash=?4,state='registered',heartbeat_seq=0,last_seen=NULL,job_protocol=NULL,generation=generation+1 WHERE id=?1",params![id,os,arch,credential])?;
                id
            } else {
                let id = uuid::Uuid::now_v7().to_string();
                tx.execute("INSERT INTO workers(id,project_id,name,os,arch,credential_hash,state) VALUES (?1,?2,?3,?4,?5,?6,'registered')",params![id,project,name,os,arch,credential])?;
                id
            };
            tx.execute(
                "UPDATE worker_enrollments SET used_by=?2,credential_hash=?3 WHERE id=?1",
                params![enrollment, id, credential],
            )?;
            tx.execute("INSERT INTO worker_audit(worker_id,generation,action,actor,reason,created_at) SELECT id,generation,'enrolled','enrollment',?2,?3 FROM workers WHERE id=?1",params![id,enrollment,now()])?;
            id
        };
        let generation: u64 =
            tx.query_row("SELECT generation FROM workers WHERE id=?1", [&id], |r| {
                r.get(0)
            })?;
        tx.commit()?;
        Ok(
            json!({"worker_id":id,"project_id":project,"generation":generation,"worker_protocol":PROTOCOL,"heartbeat_interval_seconds":10,"execution_ready":false}),
        )
    }
    pub fn worker_heartbeat(&self, credential: &str, r: &Heartbeat) -> Result<Value> {
        ensure!(
            r.protocol == PROTOCOL,
            invalid("unsupported worker protocol")
        );
        ensure!(
            r.sequence > 0 && r.sequence <= i64::MAX as u64,
            invalid("invalid heartbeat sequence")
        );
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row:Option<(String,u64)>=tx.query_row("SELECT id,heartbeat_seq FROM workers WHERE credential_hash=?1 AND state='registered'",[hash(credential)?],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (id, seq) = row.ok_or(Fault::NotFound)?;
        ensure!(r.sequence >= seq, invalid("stale heartbeat"));
        if r.sequence > seq {
            tx.execute(
                "UPDATE workers SET heartbeat_seq=?2,last_seen=?3,job_protocol=?4 WHERE id=?1",
                params![id, r.sequence, now(), r.job_protocol],
            )?;
        }
        let accepted: Option<u32> = tx.query_row(
            "SELECT job_protocol FROM workers WHERE id=?1",
            [&id],
            |row| row.get(0),
        )?;
        let blocked = blocked_operations(&tx, &id)?;
        tx.commit()?;
        Ok(
            json!({"worker_id":id,"accepted_sequence":r.sequence,"job_protocol":accepted,"execution_ready":crate::worker_jobs::compatible(accepted)&&blocked==0,"requires_recovery":blocked>0,"blocked_operations":blocked}),
        )
    }
    pub fn workers(&self, r: &List) -> Result<Value> {
        self.project(&r.project_id)?;
        let c = self.connection()?;
        let timestamp = now();
        let mut rows=c.prepare("SELECT id,name,os,arch,state,last_seen,generation,job_protocol,(SELECT count(*) FROM worker_jobs j JOIN operation_leases l ON l.id=j.operation_id WHERE j.worker_id=workers.id AND l.state='abandoned') FROM workers WHERE project_id=?1 AND (?2 IS NULL OR id>?2) ORDER BY id LIMIT 101")?.query_map(params![r.project_id,r.after],|r|{
            let state:String=r.get(4)?;
            let last:Option<i64>=r.get(5)?;let blocked:u64=r.get(8)?;
            Ok(json!({"worker_id":r.get::<_,String>(0)?,"generation":r.get::<_,u64>(6)?,"name":r.get::<_,String>(1)?,"os":r.get::<_,String>(2)?,"arch":r.get::<_,String>(3)?,"online":state=="registered" && last.is_some_and(|t|t<=timestamp && timestamp-t<HEARTBEAT_TTL),"state":state,"last_seen":last,"job_protocol":r.get::<_,Option<u32>>(7)?,"container_jobs":r.get::<_,Option<u32>>(7)?==Some(crate::worker_jobs::CONTAINER_PROTOCOL),"execution_ready":state=="registered" && last.is_some_and(|t|t<=timestamp && timestamp-t<HEARTBEAT_TTL) && crate::worker_jobs::compatible(r.get::<_,Option<u32>>(7)?)&&blocked==0,"requires_recovery":blocked>0,"blocked_operations":blocked}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > 100;
        rows.truncate(100);
        let next = if more {
            rows.last().map(|r| r["worker_id"].clone())
        } else {
            None
        };
        Ok(
            json!({"project_id":r.project_id,"workers":rows,"next_cursor":next,"worker_protocol":PROTOCOL,"liveness_seconds":HEARTBEAT_TTL}),
        )
    }
    pub fn worker_revoke(&self, r: &Revoke) -> Result<Value> {
        ensure!(
            !r.actor.trim().is_empty() && r.actor.len() <= 128,
            invalid("actor must contain 1..128 bytes")
        );
        ensure!(
            !r.reason.trim().is_empty() && r.reason.len() <= 1024,
            invalid("reason must contain 1..1024 bytes")
        );
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<(String, u64)> = tx
            .query_row(
                "SELECT state,generation FROM workers WHERE id=?1 AND project_id=?2",
                params![r.worker_id, r.project_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (state, generation) = state.ok_or(Fault::NotFound)?;
        ensure!(generation == r.generation, Fault::Conflict);
        if state != "revoked" {
            tx.execute(
                "UPDATE workers SET state='revoked' WHERE id=?1",
                [&r.worker_id],
            )?;
            tx.execute("INSERT INTO worker_audit(worker_id,generation,action,actor,reason,created_at) SELECT id,generation,'revoked',?2,?3,?4 FROM workers WHERE id=?1",params![r.worker_id,r.actor,r.reason,now()])?;
        }
        tx.commit()?;
        Ok(json!({"worker_id":r.worker_id,"generation":r.generation,"state":"revoked"}))
    }
}

/// Worker-only gateway: never mount administrative routes behind a remote proxy.
pub fn router(engine: std::sync::Arc<crate::engine::Engine>) -> axum::Router {
    use axum::{Router, extract::DefaultBodyLimit, routing::post};
    Router::new()
        .route("/worker/v1/enroll", post(enroll_http))
        .route("/worker/v1/heartbeat", post(heartbeat_http))
        .route(
            "/worker/v1/jobs/recovery",
            post(crate::worker_recovery::proof_http),
        )
        .route(
            "/worker/v1/jobs/claim",
            post(crate::worker_jobs::claim_http),
        )
        .route(
            "/worker/v1/jobs/renew",
            post(crate::worker_jobs::renew_http),
        )
        .route(
            "/worker/v1/jobs/{id}/outputs",
            axum::routing::put(crate::worker_outputs::upload),
        )
        .route(
            "/worker/v1/jobs/logs",
            post(crate::worker_logs::ingest_http).layer(DefaultBodyLimit::max(512 * 1024)),
        )
        .route(
            "/worker/v1/jobs/finish",
            post(crate::worker_jobs::finish_http).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/worker/v1/transfers/{id}",
            axum::routing::get(crate::worker_transfers::download),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(engine)
}
async fn enroll_http(
    axum::extract::State(e): axum::extract::State<std::sync::Arc<crate::engine::Engine>>,
    headers: axum::http::HeaderMap,
    axum::Json(r): axum::Json<Enroll>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    if headers.contains_key(axum::http::header::ORIGIN) {
        return Err(axum::http::StatusCode::FORBIDDEN);
    }
    e.db.worker_enroll(&r)
        .map(axum::Json)
        .map_err(gateway_error)
}
async fn heartbeat_http(
    axum::extract::State(e): axum::extract::State<std::sync::Arc<crate::engine::Engine>>,
    headers: axum::http::HeaderMap,
    axum::Json(r): axum::Json<Heartbeat>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    use axum::http::{StatusCode, header};
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    e.db.worker_heartbeat(token, &r)
        .map(axum::Json)
        .map_err(gateway_error)
}

pub(crate) fn gateway_error(error: anyhow::Error) -> axum::http::StatusCode {
    if error.downcast_ref::<Fault>().is_some() {
        axum::http::StatusCode::UNAUTHORIZED
    } else {
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ticket(db: &Db, project: &str) -> Value {
        db.worker_enrollment(&CreateEnrollment {
            project_id: project.into(),
            name: "linux-builder".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            ttl_seconds: 60,
        })
        .unwrap()
    }
    #[test]
    fn readiness_requires_explicit_matching_job_protocol_and_fresh_sequence() {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::new(&temp.path().join("db")).unwrap();
        let project = db.register("p", temp.path()).unwrap();
        let issued = ticket(&db, &project.id);
        let credential = secret().unwrap();
        db.worker_enroll(&Enroll {
            protocol: PROTOCOL,
            enrollment_token: issued["enrollment_token"].as_str().unwrap().into(),
            credential: credential.clone(),
            os: "linux".into(),
            arch: "x86_64".into(),
        })
        .unwrap();
        for (sequence, job_protocol, ready) in [
            (1, None, false),
            (2, Some(999), false),
            (2, Some(crate::worker_jobs::PROTOCOL), false),
            (3, Some(crate::worker_jobs::PROTOCOL), true),
        ] {
            let reply = db
                .worker_heartbeat(
                    &credential,
                    &Heartbeat {
                        protocol: PROTOCOL,
                        sequence,
                        job_protocol,
                    },
                )
                .unwrap();
            assert_eq!(reply["execution_ready"], ready);
            let listed = db
                .workers(&List {
                    project_id: project.id.clone(),
                    after: None,
                })
                .unwrap();
            assert_eq!(listed["workers"][0]["execution_ready"], ready);
        }
    }
    #[test]
    fn enrollment_replay_revocation_rotation_and_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        let db = Db::new(&path).unwrap();
        let project = db.register("p", temp.path()).unwrap();
        let issued = ticket(&db, &project.id);
        let mut r = Enroll {
            protocol: PROTOCOL,
            enrollment_token: issued["enrollment_token"].as_str().unwrap().into(),
            credential: secret().unwrap(),
            os: "linux".into(),
            arch: "aarch64".into(),
        };
        assert!(db.worker_enroll(&r).is_err());
        r.arch = "x86_64".into();
        let first = db.worker_enroll(&r).unwrap();
        assert_eq!(first, db.worker_enroll(&r).unwrap());
        let original = r.credential.clone();
        r.credential = secret().unwrap();
        assert!(db.worker_enroll(&r).is_err());
        r.credential = original.clone();
        assert!(
            db.worker_heartbeat(
                &secret().unwrap(),
                &Heartbeat {
                    job_protocol: Some(crate::worker_jobs::PROTOCOL),
                    protocol: PROTOCOL,
                    sequence: 1
                }
            )
            .is_err()
        );
        db.worker_heartbeat(
            &original,
            &Heartbeat {
                job_protocol: Some(crate::worker_jobs::PROTOCOL),
                protocol: PROTOCOL,
                sequence: 2,
            },
        )
        .unwrap();
        assert!(
            db.worker_heartbeat(
                &original,
                &Heartbeat {
                    job_protocol: Some(crate::worker_jobs::PROTOCOL),
                    protocol: PROTOCOL,
                    sequence: 1
                }
            )
            .is_err()
        );
        db.connection()
            .unwrap()
            .execute("UPDATE workers SET last_seen=1", [])
            .unwrap();
        db.worker_heartbeat(
            &original,
            &Heartbeat {
                job_protocol: Some(crate::worker_jobs::PROTOCOL),
                protocol: PROTOCOL,
                sequence: 2,
            },
        )
        .unwrap();
        let listed = db
            .workers(&List {
                project_id: project.id.clone(),
                after: None,
            })
            .unwrap();
        assert_eq!(listed["workers"][0]["online"], false);
        assert!(!listed.to_string().contains(&original));
        assert!(!listed.to_string().contains(&r.enrollment_token));
        let revoke = Revoke {
            generation: 1,
            project_id: project.id.clone(),
            worker_id: first["worker_id"].as_str().unwrap().into(),
            actor: "operator".into(),
            reason: "replace credential".into(),
        };
        let result = db.worker_revoke(&revoke).unwrap();
        assert_eq!(result, db.worker_revoke(&revoke).unwrap());
        assert!(
            db.worker_heartbeat(
                &original,
                &Heartbeat {
                    job_protocol: Some(crate::worker_jobs::PROTOCOL),
                    protocol: PROTOCOL,
                    sequence: 3
                }
            )
            .is_err()
        );
        assert!(db.worker_enroll(&r).is_err());
        let replacement = ticket(&db, &project.id);
        r.enrollment_token = replacement["enrollment_token"].as_str().unwrap().into();
        assert!(db.worker_enroll(&r).is_err());
        r.credential = secret().unwrap();
        assert_eq!(
            first["worker_id"],
            db.worker_enroll(&r).unwrap()["worker_id"]
        );
        assert!(
            db.worker_heartbeat(
                &original,
                &Heartbeat {
                    job_protocol: Some(crate::worker_jobs::PROTOCOL),
                    protocol: PROTOCOL,
                    sequence: 3
                }
            )
            .is_err()
        );
        assert!(db.worker_revoke(&revoke).is_err());
        db.worker_heartbeat(
            &r.credential,
            &Heartbeat {
                job_protocol: Some(crate::worker_jobs::PROTOCOL),
                protocol: PROTOCOL,
                sequence: 1,
            },
        )
        .unwrap();
        let c = db.connection().unwrap();
        let hashes: String = c
            .query_row("SELECT credential_hash FROM workers", [], |r| r.get(0))
            .unwrap();
        assert_ne!(hashes, r.credential);
        assert_eq!(
            c.query_row("SELECT count(*) FROM workers", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM worker_audit", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        drop(c);
        drop(db);
        let db = Db::new(&path).unwrap();
        assert_eq!(
            first["worker_id"],
            db.worker_enroll(&r).unwrap()["worker_id"]
        );
    }
    #[test]
    fn expiry_protocol_and_project_scope_are_enforced() {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::new(&temp.path().join("db")).unwrap();
        let project = db.register("p", temp.path()).unwrap();
        let issued = ticket(&db, &project.id);
        let mut r = Enroll {
            protocol: PROTOCOL,
            enrollment_token: issued["enrollment_token"].as_str().unwrap().into(),
            credential: secret().unwrap(),
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        db.connection()
            .unwrap()
            .execute("UPDATE worker_enrollments SET expires_at=0", [])
            .unwrap();
        assert!(db.worker_enroll(&r).is_err());
        let issued = ticket(&db, &project.id);
        r.enrollment_token = issued["enrollment_token"].as_str().unwrap().into();
        r.protocol = PROTOCOL + 1;
        assert!(db.worker_enroll(&r).is_err());
        r.protocol = PROTOCOL;
        let first = db.worker_enroll(&r).unwrap();
        assert!(
            db.worker_revoke(&Revoke {
                generation: 1,
                project_id: "other".into(),
                worker_id: first["worker_id"].as_str().unwrap().into(),
                actor: "operator".into(),
                reason: "test".into()
            })
            .is_err()
        );
        let duplicate = ticket(&db, &project.id);
        r.enrollment_token = duplicate["enrollment_token"].as_str().unwrap().into();
        r.credential = secret().unwrap();
        assert!(db.worker_enroll(&r).is_err());
    }
}
