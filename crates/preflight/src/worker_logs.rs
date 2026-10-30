//! Authenticated, leased and idempotent log frames feed the ordinary log/search store.
use crate::{
    db::{Db, insert_stage_log},
    model::{Fault, invalid},
    worker_jobs,
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub(crate) const FRAME_BYTES: usize = 64 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    pub id: i64,
    pub stream: String,
    pub sequence: i64,
    pub timestamp: i64,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Frame {
    pub after: i64,
    pub events: Vec<Event>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub protocol: u32,
    pub job_id: String,
    pub claim_secret: String,
    pub frame: Frame,
}
fn filter(
    c: &Connection,
    db: &Db,
    job: &str,
    stream: &str,
) -> Result<(crate::secrets::Filter, bool)> {
    let checkpoint: Option<String> = c
        .query_row(
            "SELECT checkpoint FROM worker_log_streams WHERE job_id=?1 AND stream=?2",
            params![job, stream],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let checkpoint = checkpoint
        .map(|s| serde_json::from_str::<crate::secrets::Checkpoint>(&s))
        .transpose()?;
    Ok(crate::secrets::Filter::restore(
        db.secret_values(),
        checkpoint.as_ref(),
    ))
}
fn save_filter(
    c: &Connection,
    job: &str,
    stream: &str,
    filter: &crate::secrets::Filter,
) -> Result<()> {
    let checkpoint = filter
        .checkpoint()
        .map(|v| serde_json::to_string(&v))
        .transpose()?;
    c.execute("INSERT INTO worker_log_streams(job_id,stream,checkpoint) VALUES (?1,?2,?3) ON CONFLICT(job_id,stream) DO UPDATE SET checkpoint=excluded.checkpoint",params![job,stream,checkpoint])?;
    Ok(())
}
pub(crate) fn flush(c: &Connection, db: &Db, job: &str) -> Result<()> {
    let (run, stage): (String, String) = c.query_row(
        "SELECT run_id,stage_id FROM worker_jobs WHERE id=?1",
        [job],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let streams = c
        .prepare(
            "SELECT stream FROM worker_log_streams WHERE job_id=?1 AND checkpoint IS NOT NULL",
        )?
        .query_map([job], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for stream in streams {
        let (mut filter, restored) = filter(c, db, job, &stream)?;
        let text = if restored {
            filter.push(&[], true)
        } else {
            b"[REDACTED]".to_vec()
        };
        let sequence:i64=c.query_row("SELECT COALESCE(MAX(sequence),-1)+1 FROM log_events WHERE run_id=?1 AND stage_id=?2 AND stream=?3",params![run,stage,stream],|r|r.get(0))?;
        if !text.is_empty() {
            insert_stage_log(
                c,
                &run,
                &stage,
                &stream,
                sequence,
                crate::db::now(),
                &crate::log_search::redact(&String::from_utf8_lossy(&text)),
            )?;
        }
        c.execute(
            "UPDATE worker_log_streams SET checkpoint=NULL WHERE job_id=?1 AND stream=?2",
            params![job, stream],
        )?;
    }
    Ok(())
}
impl Db {
    pub(crate) fn worker_logs_ingest(&self, credential: &str, r: &Request) -> Result<Value> {
        ensure!(
            worker_jobs::compatible(Some(r.protocol))
                && r.frame.after >= 0
                && !r.frame.events.is_empty()
                && r.frame.events.len() <= 16,
            invalid("invalid worker log frame")
        );
        let mut prior = r.frame.after;
        let mut bytes = 0usize;
        for event in &r.frame.events {
            ensure!(
                event.id > prior
                    && event.sequence >= 0
                    && event.sequence < i64::MAX - 1
                    && event.timestamp >= 0
                    && matches!(event.stream.as_str(), "stdout" | "stderr" | "lifecycle"),
                invalid("invalid worker log event")
            );
            prior = event.id;
            bytes += event.text.len();
        }
        ensure!(
            bytes <= FRAME_BYTES,
            invalid("worker log frame exceeds 64 KiB")
        );
        let digest = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&r.frame)?));
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        worker_jobs::sweep(&tx)?;
        let (worker, generation) = worker_jobs::auth(&tx, credential)?;
        let row:Option<(String,String,String,i64,u64,u64,bool)>=tx.query_row("SELECT run_id,stage_id,state,log_cursor,log_bytes,log_events,log_truncated FROM worker_jobs WHERE id=?1 AND worker_id=?2 AND generation=?3 AND claim_hash=?4",params![r.job_id,worker,generation,crate::workers::hash(&r.claim_secret)?],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).optional()?;
        let (run, stage, state, cursor, stored, count, truncated) = row.ok_or(Fault::NotFound)?;
        let old:Option<(i64,String)>=tx.query_row("SELECT last_cursor,payload_sha256 FROM worker_log_batches WHERE job_id=?1 AND after_cursor=?2",params![r.job_id,r.frame.after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((last, hash)) = old {
            ensure!(hash == digest, Fault::IdempotencyConflict);
            tx.commit()?;
            return Ok(
                json!({"job_id":r.job_id,"cursor":last,"replayed":true,"truncated":truncated}),
            );
        }
        ensure!(
            matches!(state.as_str(), "leased" | "cancelling"),
            invalid("late or lost worker log frame rejected")
        );
        ensure!(
            cursor == r.frame.after,
            invalid("worker log cursor mismatch")
        );
        ensure!(
            !truncated,
            invalid("worker log stream is closed after truncation")
        );
        let truncated =
            stored + bytes as u64 > 4 * 1024 * 1024 || count + r.frame.events.len() as u64 > 4096;
        if truncated {
            let sequence:i64=tx.query_row("SELECT COALESCE(MAX(sequence),-1)+1 FROM log_events WHERE run_id=?1 AND stage_id=?2 AND stream='stderr'",params![run,stage],|r|r.get(0))?;
            insert_stage_log(
                &tx,
                &run,
                &stage,
                "stderr",
                sequence,
                crate::db::now(),
                "\n[Preflight: remote log storage limit reached; remaining output is not forwarded]\n",
            )?;
        }
        if !truncated {
            for event in &r.frame.events {
                let (mut filter, restored) = filter(&tx, self, &r.job_id, &event.stream)?;
                let mut output = if restored {
                    vec![]
                } else {
                    b"[REDACTED]".to_vec()
                };
                output.extend(filter.push(event.text.as_bytes(), false));
                if !output.is_empty() {
                    insert_stage_log(
                        &tx,
                        &run,
                        &stage,
                        &event.stream,
                        event.sequence,
                        event.timestamp,
                        &crate::log_search::redact(&String::from_utf8_lossy(&output)),
                    )?;
                }
                save_filter(&tx, &r.job_id, &event.stream, &filter)?;
            }
        }
        tx.execute(
            "INSERT INTO worker_log_batches VALUES (?1,?2,?3,?4)",
            params![r.job_id, r.frame.after, prior, digest],
        )?;
        tx.execute("UPDATE worker_jobs SET log_cursor=?2,log_bytes=log_bytes+?3,log_events=log_events+?4,log_truncated=?5 WHERE id=?1",params![r.job_id,prior,if truncated{0}else{bytes as u64},if truncated{0}else{r.frame.events.len() as u64},truncated])?;
        tx.commit()?;
        Ok(json!({"job_id":r.job_id,"cursor":prior,"replayed":false,"truncated":truncated}))
    }
}
pub(crate) async fn ingest_http(
    axum::extract::State(e): axum::extract::State<std::sync::Arc<crate::engine::Engine>>,
    headers: axum::http::HeaderMap,
    axum::Json(r): axum::Json<Request>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    e.db.worker_logs_ingest(worker_jobs::credential(&headers)?, &r)
        .map(axum::Json)
        .map_err(crate::workers::gateway_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Db, String, String, String) {
        let temp = tempfile::tempdir().unwrap();
        let db = Db::new(&temp.path().join("db")).unwrap();
        let project = db.register("owned", temp.path()).unwrap();
        for id in ["source", "target"] {
            db.insert_run(
                id,
                &crate::model::StartBuild {
                    project_id: project.id.clone(),
                    command: crate::model::CommandSpec {
                        program: "sh".into(),
                        args: vec![],
                        timeout_seconds: 60,
                    },
                },
                temp.path(),
            )
            .unwrap();
        }
        let enrollment = db
            .worker_enrollment(&crate::workers::CreateEnrollment {
                project_id: project.id,
                name: "worker".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                ttl_seconds: 60,
            })
            .unwrap();
        let credential = crate::workers::secret().unwrap();
        let worker = db
            .worker_enroll(&crate::workers::Enroll {
                protocol: 1,
                enrollment_token: enrollment["enrollment_token"].as_str().unwrap().into(),
                credential: credential.clone(),
                os: "linux".into(),
                arch: "x86_64".into(),
            })
            .unwrap();
        db.connection().unwrap().execute("INSERT INTO worker_transfers VALUES ('transfer','source',?1,1,'fixture',?2,1,?3,0)",params![worker["worker_id"].as_str().unwrap(),"a".repeat(64),crate::workers::now()+600]).unwrap();
        let spec = worker_jobs::Spec {
            container: None,
            command: crate::model::CommandSpec {
                program: "sh".into(),
                args: vec![],
                timeout_seconds: 60,
            },
            cwd: ".".into(),
            env: Default::default(),
            returns: Default::default(),
        };
        let id = db
            .worker_job_enqueue("target", "native", "transfer", &spec)
            .unwrap()["job_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let claim = crate::workers::secret().unwrap();
        db.worker_job_claim(
            &credential,
            &worker_jobs::Claim {
                protocol: 3,
                claim_secret: claim.clone(),
            },
        )
        .unwrap();
        (temp, db, credential, id, claim)
    }
    fn request(job: &str, claim: &str, after: i64, id: i64, text: &str) -> Request {
        Request {
            protocol: 3,
            job_id: job.into(),
            claim_secret: claim.into(),
            frame: Frame {
                after,
                events: vec![Event {
                    id,
                    stream: "stdout".into(),
                    sequence: id,
                    timestamp: crate::db::now(),
                    text: text.into(),
                }],
            },
        }
    }
    fn outcome(cursor: Option<i64>, truncated: bool) -> worker_jobs::Outcome {
        worker_jobs::Outcome {
            runtime_receipt: None,
            log_cursor: cursor,
            status: crate::model::RunState::Succeeded,
            exit: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            truncated,
        }
    }
    fn text(db: &Db) -> String {
        db.logs("target", 0)
            .unwrap()
            .events
            .into_iter()
            .map(|e| e.text)
            .collect()
    }
    #[test]
    fn frames_are_authenticated_ordered_atomic_idempotent_and_fenced() {
        let (_temp, db, credential, id, claim) = setup();
        let mut frame = request(&id, &claim, 0, 1, "live first\n");
        let parsed: Request =
            serde_json::from_value(serde_json::to_value(&frame).unwrap()).unwrap();
        assert_eq!(
            db.worker_logs_ingest(&credential, &parsed).unwrap()["cursor"],
            1
        );
        assert_eq!(
            db.worker_logs_ingest(&credential, &frame).unwrap()["replayed"],
            true
        );
        assert_eq!(text(&db), "live first\n");
        frame.frame.events[0].text = "changed replay".into();
        assert!(db.worker_logs_ingest(&credential, &frame).is_err());
        frame.frame.events[0].text = "live first\n".into();
        let mut bad = request(&id, &claim, 1, 2, "next");
        bad.claim_secret = crate::workers::secret().unwrap();
        assert!(db.worker_logs_ingest(&credential, &bad).is_err());
        assert!(
            db.worker_logs_ingest(&crate::workers::secret().unwrap(), &frame)
                .is_err()
        );
        assert!(
            db.worker_logs_ingest(&credential, &request(&id, &claim, 2, 3, "gap"))
                .is_err()
        );
        assert_eq!(text(&db), "live first\n");
        db.connection()
            .unwrap()
            .execute(
                "UPDATE worker_jobs SET lease_expires_at=?1 WHERE id=?2",
                params![crate::workers::now() - 1, id],
            )
            .unwrap();
        assert!(
            db.worker_logs_ingest(&credential, &request(&id, &claim, 1, 2, "late"))
                .is_err()
        );
        assert_eq!(
            db.worker_logs_ingest(&credential, &frame).unwrap()["cursor"],
            1
        );
        assert_eq!(text(&db), "live first\n");
    }
    #[test]
    fn secret_checkpoints_survive_restart_without_plaintext_and_finish_requires_cursor() {
        let (temp, db, credential, id, claim) = setup();
        let secret = "private-server-credential-long-enough-for-testing";
        db.remember_secrets(vec![secret.into()]);
        db.worker_logs_ingest(
            &credential,
            &request(&id, &claim, 0, 1, &format!("before {}", &secret[..20])),
        )
        .unwrap();
        assert_eq!(text(&db), "before ");
        let checkpoint: String = db
            .connection()
            .unwrap()
            .query_row("SELECT checkpoint FROM worker_log_streams", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!checkpoint.contains(&secret[..20]));
        assert!(checkpoint.len() < 200);
        drop(db);
        let db = Db::new(&temp.path().join("db")).unwrap();
        db.remember_secrets(vec![secret.into()]);
        db.worker_logs_ingest(
            &credential,
            &request(&id, &claim, 1, 2, &format!("{} after", &secret[20..])),
        )
        .unwrap();
        assert_eq!(text(&db), "before [REDACTED] after");
        let mut finish = worker_jobs::Finish {
            protocol: 3,
            job_id: id,
            claim_secret: claim,
            outcome: outcome(Some(1), false),
        };
        assert!(db.worker_job_finish(&credential, &finish).is_err());
        finish.outcome.log_cursor = None;
        assert!(db.worker_job_finish(&credential, &finish).is_err());
        finish.outcome.log_cursor = Some(2);
        db.worker_job_finish(&credential, &finish).unwrap();
        db.worker_job_finish(&credential, &finish).unwrap();
        assert_eq!(text(&db), "before [REDACTED] after");
        assert!(
            db.worker_logs_ingest(
                &credential,
                &request(&finish.job_id, &finish.claim_secret, 2, 3, "after finish")
            )
            .is_err()
        );
    }
    #[test]
    fn budget_closes_stream_with_marker_and_does_not_fail_the_command() {
        let (_temp, db, credential, id, claim) = setup();
        db.connection()
            .unwrap()
            .execute("UPDATE worker_jobs SET log_events=4096 WHERE id=?1", [&id])
            .unwrap();
        let frame = request(&id, &claim, 0, 1, "overflow");
        let ack = db.worker_logs_ingest(&credential, &frame).unwrap();
        assert_eq!(ack["truncated"], true);
        assert!(text(&db).contains("remote log storage limit reached"));
        assert_eq!(
            db.worker_logs_ingest(&credential, &frame).unwrap()["cursor"],
            1
        );
        assert!(
            db.worker_logs_ingest(&credential, &request(&id, &claim, 1, 2, "not stored"))
                .is_err()
        );
        assert!(!text(&db).contains("not stored"));
        db.worker_job_finish(
            &credential,
            &worker_jobs::Finish {
                protocol: 3,
                job_id: id,
                claim_secret: claim,
                outcome: outcome(Some(1), true),
            },
        )
        .unwrap();
    }
    #[test]
    fn malformed_frames_do_not_mutate_logs_or_cursor() {
        let (_temp, db, credential, id, claim) = setup();
        let mut frame = request(&id, &claim, 0, 1, "allowed");
        frame.frame.events[0].stream = "arbitrary".into();
        assert!(db.worker_logs_ingest(&credential, &frame).is_err());
        frame.frame.events[0].stream = "stdout".into();
        frame.frame.events[0].text = "x".repeat(FRAME_BYTES + 1);
        assert!(db.worker_logs_ingest(&credential, &frame).is_err());
        assert!(text(&db).is_empty());
        assert_eq!(
            db.worker_jobs("target").unwrap()["jobs"][0]["log_cursor"],
            0
        );
    }
}
