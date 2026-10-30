use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior, params};
use sha2::{Digest, Sha256};

pub const VERSION: usize = 53;
const SQL: [&str; VERSION] = [
    include_str!("../../../migrations/0001_initial.sql"),
    include_str!("../../../migrations/0002_pipelines.sql"),
    include_str!("../../../migrations/0003_fixtures.sql"),
    include_str!("../../../migrations/0004_submissions.sql"),
    include_str!("../../../migrations/0005_inputs.sql"),
    include_str!("../../../migrations/0006_modules.sql"),
    include_str!("../../../migrations/0007_module_inputs.sql"),
    include_str!("../../../migrations/0008_module_manifests.sql"),
    include_str!("../../../migrations/0009_git_sources.sql"),
    include_str!("../../../migrations/0010_toolchain_compatibility.sql"),
    include_str!("../../../migrations/0011_stage_keys.sql"),
    include_str!("../../../migrations/0012_operation_leases.sql"),
    include_str!("../../../migrations/0013_stage_cache.sql"),
    include_str!("../../../migrations/0014_cache_cleanup.sql"),
    include_str!("../../../migrations/0015_completion_events.sql"),
    include_str!("../../../migrations/0016_codex_bridge.sql"),
    include_str!("../../../migrations/0017_module_retention.sql"),
    include_str!("../../../migrations/0018_change_queues.sql"),
    include_str!("../../../migrations/0019_single_flight.sql"),
    include_str!("../../../migrations/0020_operation_resolutions.sql"),
    include_str!("../../../migrations/0021_workers.sql"),
    include_str!("../../../migrations/0022_worker_transfers.sql"),
    include_str!("../../../migrations/0023_worker_jobs.sql"),
    include_str!("../../../migrations/0024_worker_outputs.sql"),
    include_str!("../../../migrations/0025_worker_capabilities.sql"),
    include_str!("../../../migrations/0026_worker_connections.sql"),
    include_str!("../../../migrations/0027_storage_gc.sql"),
    include_str!("../../../migrations/0028_pipeline_identity.sql"),
    include_str!("../../../migrations/0029_experiments.sql"),
    include_str!("../../../migrations/0030_log_search.sql"),
    include_str!("../../../migrations/0031_module_evictions.sql"),
    include_str!("../../../migrations/0032_artifact_expiry.sql"),
    include_str!("../../../migrations/0033_artifact_pins.sql"),
    include_str!("../../../migrations/0034_log_chunks.sql"),
    include_str!("../../../migrations/0035_schedules.sql"),
    include_str!("../../../migrations/0036_schedule_occurrences.sql"),
    include_str!("../../../migrations/0037_schedule_hidden.sql"),
    include_str!("../../../migrations/0038_storage_policies.sql"),
    include_str!("../../../migrations/0039_schedule_original_requests.sql"),
    include_str!("../../../migrations/0040_acceptance_and_test_evidence.sql"),
    include_str!("../../../migrations/0041_execution_progress.sql"),
    include_str!("../../../migrations/0042_management.sql"),
    include_str!("../../../migrations/0043_run_archives.sql"),
    include_str!("../../../migrations/0044_worker_reconnect.sql"),
    include_str!("../../../migrations/0045_storage_policy_quotas.sql"),
    include_str!("../../../migrations/0046_diagnostic_index.sql"),
    include_str!("../../../migrations/0047_evidence_retention.sql"),
    include_str!("../../../migrations/0048_external_ci.sql"),
    include_str!("../../../migrations/0049_access_and_containers.sql"),
    include_str!("../../../migrations/0050_archive_imports.sql"),
    include_str!("../../../migrations/0051_archive_retention.sql"),
    include_str!("../../../migrations/0052_archive_encryption.sql"),
    include_str!("../../../migrations/0053_worker_live_logs.sql"),
];
pub fn apply(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version')",
        [],
        |r| r.get(0),
    )?;
    let history = if exists {
        tx.prepare("SELECT version FROM schema_version ORDER BY version")?
            .query_map([], |r| r.get::<_, usize>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        vec![]
    };
    ensure!(
        history.len() <= VERSION && history.iter().enumerate().all(|(i, v)| *v == i + 1),
        "unsupported or non-contiguous database migration history"
    );
    let ledger: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='migration_history')", [], |r|r.get(0))?;
    if ledger {
        let rows = tx
            .prepare("SELECT version, sha256 FROM migration_history ORDER BY version")?
            .query_map([], |r| Ok((r.get::<_, usize>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            rows.len() == history.len(),
            "migration ledger/history mismatch"
        );
        for (i, (version, hash)) in rows.iter().enumerate() {
            ensure!(
                *version == i + 1
                    && *hash == crate::hashing::hex(Sha256::digest(SQL[i].as_bytes())),
                "migration checksum mismatch at version {}",
                i + 1
            );
        }
    } else {
        ensure!(history.len() <= 4, "missing migration ledger");
        tx.execute_batch("CREATE TABLE migration_history(version INTEGER PRIMARY KEY, sha256 TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, adopted INTEGER NOT NULL);")?;
        // Legacy databases did not store checksums. Adoption is explicitly marked.
        for &version in &history {
            tx.execute(
                "INSERT INTO migration_history(version,sha256,adopted) VALUES (?1,?2,1)",
                params![
                    version,
                    crate::hashing::hex(Sha256::digest(SQL[version - 1].as_bytes()))
                ],
            )?;
        }
    }
    for (i, sql) in SQL.iter().enumerate().skip(history.len()) {
        tx.execute_batch(sql)
            .with_context(|| format!("migration {} failed", i + 1))?;
        if i + 1 == 30 {
            crate::log_search::backfill(&tx)?;
        }
        if i + 1 == 28 {
            crate::inspection::backfill(&tx)?;
        }
        tx.execute(
            "INSERT INTO migration_history(version,sha256,adopted) VALUES (?1,?2,0)",
            params![i + 1, crate::hashing::hex(Sha256::digest(sql.as_bytes()))],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Read-only validation for backups; never adopts, upgrades or changes a source DB.
pub(crate) fn verify_backup_schema(connection: &Connection) -> Result<usize> {
    let history = connection
        .prepare("SELECT version FROM schema_version ORDER BY version")?
        .query_map([], |r| r.get::<_, usize>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        (51..=VERSION).contains(&history.len())
            && history.iter().enumerate().all(|(i, v)| *v == i + 1),
        "backup migration history is unsupported or non-contiguous"
    );
    let ledger = connection
        .prepare("SELECT version,sha256 FROM migration_history ORDER BY version")?
        .query_map([], |r| Ok((r.get::<_, usize>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        ledger.len() == history.len()
            && ledger.iter().enumerate().all(|(i, (v, h))| *v == i + 1
                && *h == crate::hashing::hex(Sha256::digest(SQL[i].as_bytes()))),
        "backup migration checksum mismatch"
    );
    Ok(history.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_reconnect_upgrade_keeps_legacy_connection_and_disables_retries() {
        let mut c = Connection::open_in_memory().unwrap();
        for sql in &SQL[..43] {
            c.execute_batch(sql).unwrap();
        }
        c.execute_batch("CREATE TABLE migration_history(version INTEGER PRIMARY KEY, sha256 TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, adopted INTEGER NOT NULL);").unwrap();
        for (index, sql) in SQL[..43].iter().enumerate() {
            c.execute(
                "INSERT INTO migration_history(version,sha256,adopted) VALUES (?1,?2,0)",
                params![
                    index + 1,
                    crate::hashing::hex(Sha256::digest(sql.as_bytes()))
                ],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO projects(id,name,root) VALUES ('p','project','/project')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO worker_connections(id,project_id,name,request_id,request_json,state,created_at) VALUES ('c','p','worker','request','{}','queued',123)", []).unwrap();
        apply(&mut c).unwrap();
        let row: (String,String,i64,u8,u16,u8,u16,Option<i64>) = c.query_row("SELECT request_json,state,created_at,retry_limit,retry_delay_seconds,retries,attempt,next_retry_at FROM worker_connections WHERE id='c'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).unwrap();
        assert_eq!(row, ("{}".into(), "queued".into(), 123, 0, 10, 0, 0, None));
        assert!(
            c.execute("UPDATE worker_connections SET retry_limit=11", [])
                .is_err()
        );
        apply(&mut c).unwrap();
    }
    #[test]
    fn quota_upgrade_preserves_legacy_cycles_and_observations_on_replay() {
        let mut c = Connection::open_in_memory().unwrap();
        for sql in &SQL[..44] {
            c.execute_batch(sql).unwrap();
        }
        c.execute_batch("CREATE TABLE migration_history(version INTEGER PRIMARY KEY, sha256 TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, adopted INTEGER NOT NULL);").unwrap();
        for (index, sql) in SQL[..44].iter().enumerate() {
            c.execute(
                "INSERT INTO migration_history(version,sha256,adopted) VALUES (?1,?2,0)",
                params![
                    index + 1,
                    crate::hashing::hex(Sha256::digest(sql.as_bytes()))
                ],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO projects VALUES ('p','preserved','/example')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO storage_policy_cycles(id,policy_hash,policy_json,project_id,cutoff,cursor,state,created_at,updated_at) VALUES ('cycle','legacy-hash','{}','p',123,'cursor','active',100,101)",[]).unwrap();
        apply(&mut c).unwrap();
        let row:(String,i64,String,Option<String>) = c.query_row("SELECT policy_hash,cutoff,cursor,observation_json FROM storage_policy_cycles WHERE id='cycle'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(row, ("legacy-hash".into(), 123, "cursor".into(), None));
        c.execute("UPDATE storage_policy_cycles SET observation_json='{\"triggered\":true}' WHERE id='cycle'",[]).unwrap();
        apply(&mut c).unwrap();
        assert_eq!(c.query_row("SELECT json_extract(observation_json,'$.triggered') FROM storage_policy_cycles WHERE id='cycle'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
    #[test]
    fn diagnostic_upgrade_preserves_legacy_logs_and_unknown_ingestion_time() {
        let mut c = Connection::open_in_memory().unwrap();
        for sql in &SQL[..45] {
            c.execute_batch(sql).unwrap();
        }
        c.execute_batch("CREATE TABLE migration_history(version INTEGER PRIMARY KEY,sha256 TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,adopted INTEGER NOT NULL);").unwrap();
        for (index, sql) in SQL[..45].iter().enumerate() {
            c.execute(
                "INSERT INTO migration_history(version,sha256,adopted) VALUES (?1,?2,0)",
                params![
                    index + 1,
                    crate::hashing::hex(Sha256::digest(sql.as_bytes()))
                ],
            )
            .unwrap();
        }
        c.execute("INSERT INTO projects VALUES ('p','old','/old')", [])
            .unwrap();
        c.execute("INSERT INTO runs(id,project_id,status,command_json,created_at,workspace) VALUES ('r','p','succeeded','{}',123,'/old/workspace')",[]).unwrap();
        c.execute("INSERT INTO log_events(run_id,stage_id,stream,sequence,timestamp,text) VALUES ('r','native','stderr',7,124,'old.c:2:1: error: preserved')",[]).unwrap();
        let event = c.last_insert_rowid();
        c.execute(
            "INSERT INTO log_search(rowid,text) VALUES (?1,'old.c:2:1: error: preserved')",
            [event],
        )
        .unwrap();
        apply(&mut c).unwrap();
        apply(&mut c).unwrap();
        let old: (i64, String, Option<i64>) = c
            .query_row(
                "SELECT timestamp,text,ingested_at FROM log_events WHERE id=?1",
                [event],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(old, (124, "old.c:2:1: error: preserved".into(), None));
        assert_eq!(
            c.query_row("SELECT text FROM log_search WHERE rowid=?1", [event], |r| r
                .get::<_, String>(0))
                .unwrap(),
            old.1
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM diagnostic_scans", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn fresh_repeat_and_corruption() {
        let mut c = Connection::open_in_memory().unwrap();
        apply(&mut c).unwrap();
        apply(&mut c).unwrap();
        c.execute(
            "UPDATE migration_history SET sha256='tampered' WHERE version=2",
            [],
        )
        .unwrap();
        assert!(apply(&mut c).unwrap_err().to_string().contains("checksum"));
    }
    #[test]
    fn legacy_upgrade_and_future_refusal() {
        let mut c = Connection::open_in_memory().unwrap();
        for sql in &SQL[..3] {
            c.execute_batch(sql).unwrap();
        }
        c.execute(
            "INSERT INTO projects VALUES ('p','preserved','/example')",
            [],
        )
        .unwrap();
        apply(&mut c).unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM projects", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row("SELECT sum(adopted) FROM migration_history", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            3
        );
        c.execute("INSERT INTO schema_version VALUES (?1)", [VERSION + 1])
            .unwrap();
        assert!(apply(&mut c).is_err());
    }
    #[test]
    fn upgrade_keeps_prepared_file_module_metadata() {
        let mut c = Connection::open_in_memory().unwrap();
        for sql in &SQL[..7] {
            c.execute_batch(sql).unwrap();
        }
        c.execute("INSERT INTO projects VALUES ('p','kept','/kept')", [])
            .unwrap();
        c.execute(
            "INSERT INTO modules VALUES ('p','digest',?1)",
            [r#"{"kind":"file"}"#],
        )
        .unwrap();
        // Schema 7 was already checksummed; emulate its ledger before upgrading.
        c.execute_batch("CREATE TABLE migration_history(version INTEGER PRIMARY KEY, sha256 TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, adopted INTEGER NOT NULL);").unwrap();
        for (i, sql) in SQL[..7].iter().enumerate() {
            c.execute(
                "INSERT INTO migration_history(version,sha256,adopted) VALUES (?1,?2,0)",
                params![i + 1, crate::hashing::hex(Sha256::digest(sql.as_bytes()))],
            )
            .unwrap();
        }
        apply(&mut c).unwrap();
        let (metadata, manifest): (String, Option<String>) = c
            .query_row(
                "SELECT artifact_json,manifest_json FROM modules WHERE project_id='p'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(metadata, r#"{"kind":"file"}"#);
        assert!(manifest.is_none());
        let (contract, metadata): (Option<String>,String) = c.query_row("SELECT contract_json,artifact_json FROM module_contracts WHERE project_id='p' AND sha256='digest' AND contract_sha256=''", [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert!(contract.is_none());
        assert_eq!(metadata, r#"{"kind":"file"}"#);
    }
    #[test]
    fn gap_and_failed_upgrade_are_not_committed() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(SQL[0]).unwrap();
        c.execute("INSERT INTO schema_version VALUES (3)", [])
            .unwrap();
        assert!(apply(&mut c).is_err());
        c.execute("DELETE FROM schema_version WHERE version=3", [])
            .unwrap();
        c.execute("CREATE TABLE submissions(dummy TEXT)", [])
            .unwrap();
        assert!(apply(&mut c).is_err());
        assert_eq!(
            c.query_row("SELECT max(version) FROM schema_version", [], |r| r
                .get::<_, i32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='migration_history'",
                [],
                |r| r.get::<_, i32>(0)
            )
            .unwrap(),
            0
        );
    }
}
