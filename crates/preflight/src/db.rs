use crate::{
    model::*,
    pipeline::{PipelineFile, StageDefinition},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Mutex};

pub fn open(path: &Path) -> Result<Connection> {
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    crate::migrations::apply(&mut connection)?;
    connection.create_scalar_function(
        "preflight_contains",
        2,
        rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC
            | rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |ctx| {
            Ok(crate::search::matches_query(
                &ctx.get::<String>(0)?,
                &ctx.get::<String>(1)?,
            ))
        },
    )?;
    Ok(connection)
}
pub(crate) fn insert_stage_log(
    c: &Connection,
    run: &str,
    stage: &str,
    stream: &str,
    sequence: i64,
    timestamp: i64,
    text: &str,
) -> Result<()> {
    c.execute("INSERT INTO log_events(run_id,stage_id,stream,sequence,timestamp,text,ingested_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![run,stage,stream,sequence,timestamp,text,now()])?;
    c.execute(
        "INSERT INTO log_search(rowid,text) VALUES (?1,?2)",
        params![c.last_insert_rowid(), crate::log_search::redact(text)],
    )?;
    Ok(())
}
pub struct Db(Mutex<Connection>, std::sync::RwLock<Vec<String>>);
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
impl Db {
    pub(crate) fn cache_candidate(
        &self,
        project: &str,
        pipeline: &str,
        key: &str,
    ) -> Result<Option<crate::cache::Candidate>> {
        Ok(self.connection()?.query_row("SELECT c.id,c.producer_run,c.producer_stage FROM stage_cache c JOIN runs r ON r.id=c.producer_run JOIN stage_runs s ON s.run_id=c.producer_run AND s.name=c.producer_stage WHERE c.project_id=?1 AND c.pipeline_id=?2 AND c.cache_key=?3 AND c.state='active' AND r.status='succeeded' AND s.status='succeeded' ORDER BY c.id DESC LIMIT 1",params![project,pipeline,key],|r|Ok(crate::cache::Candidate{id:r.get(0)?,run_id:r.get(1)?,stage_id:r.get(2)?})).optional()?)
    }
    pub(crate) fn cache_publish(
        &self,
        run: &str,
        stage: &str,
        pipeline: &str,
        key: &str,
    ) -> Result<()> {
        let project = self.run(run)?.project_id;
        let entry = uuid::Uuid::now_v7().to_string();
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO stage_cache VALUES (?1,?2,?3,?4,?5,?6,'active')",
            params![entry, project, pipeline, key, run, stage],
        )?;
        tx.execute("INSERT INTO cache_access(cache_id,hits,last_used,rebuild_ms) SELECT ?1,0,?2,MAX(COALESCE(finished_at,?2)-COALESCE(started_at,?2),0) FROM stage_runs WHERE run_id=?3 AND name=?4",params![entry,now(),run,stage])?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn cache_invalidate(&self, id: &str) -> Result<()> {
        self.connection()?.execute(
            "UPDATE stage_cache SET state='invalidated' WHERE id=?1",
            [id],
        )?;
        Ok(())
    }
    pub(crate) fn cache_record(
        &self,
        run: &str,
        stage: &str,
        decision: serde_json::Value,
    ) -> Result<()> {
        if decision["status"] == "hit"
            && let Some(entry) = decision["entry_id"].as_str()
        {
            self.connection()?.execute("INSERT INTO cache_access(cache_id,hits,last_used,rebuild_ms) VALUES (?1,1,?2,0) ON CONFLICT(cache_id) DO UPDATE SET hits=hits+1,last_used=excluded.last_used",params![entry,now()])?;
        }
        self.connection()?.execute(
            "INSERT INTO stage_cache_decisions VALUES (?1,?2,?3)",
            params![run, stage, serde_json::to_string(&decision)?],
        )?;
        Ok(())
    }
    pub fn cache_decisions(&self, run: &str) -> Result<serde_json::Value> {
        self.run(run)?;
        let c = self.connection()?;
        let mut statement = c.prepare("SELECT stage_id,decision_json FROM stage_cache_decisions WHERE run_id=?1 ORDER BY stage_id")?;
        let rows = statement
            .query_map([run], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let decisions = rows.into_iter().map(|(stage,json)|Ok(serde_json::json!({"stage_id":stage,"decision":serde_json::from_str::<serde_json::Value>(&json)?}))).collect::<Result<Vec<_>>>()?;
        Ok(serde_json::json!({"run_id":run,"stages":decisions}))
    }
    pub(crate) fn begin_operation(
        &self,
        run_id: &str,
        kind: &str,
        mut resources: std::collections::BTreeSet<String>,
    ) -> Result<crate::leases::Lease<'_>> {
        self.run(run_id)?;
        resources.insert(format!("run:{run_id}"));
        let id = uuid::Uuid::now_v7().to_string();
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO operation_leases VALUES (?1,?2,?3,'active',?4,NULL)",
            params![id, run_id, kind, now()],
        )?;
        for resource in resources {
            if let Some(entry) = resource.strip_prefix("cache:") {
                let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM stage_cache WHERE id=?1 AND state='active' AND producer_run=?2)",params![entry,run_id],|r|r.get(0))?;
                ensure!(
                    active,
                    invalid("cache entry was invalidated before acquisition")
                );
            }
            tx.execute(
                "INSERT INTO operation_resources VALUES (?1,?2)",
                params![id, resource],
            )?;
        }
        tx.commit()?;
        Ok(crate::leases::Lease {
            db: self,
            id,
            release_on_drop: true,
        })
    }
    pub(crate) fn release_operation(&self, id: &str) -> Result<()> {
        self.connection()?.execute("UPDATE operation_leases SET state='released',finished_at=?2 WHERE id=?1 AND state='active'", params![id,now()])?;
        Ok(())
    }
    pub fn operations(&self, run_id: &str) -> Result<serde_json::Value> {
        self.run(run_id)?;
        let connection = self.connection()?;
        let mut stmt = connection.prepare("SELECT id,kind,state,started_at,finished_at FROM operation_leases WHERE run_id=?1 ORDER BY started_at DESC,id DESC LIMIT 100")?;
        let rows = stmt
            .query_map([run_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let total: i64 = connection.query_row(
            "SELECT COUNT(*) FROM operation_leases WHERE run_id=?1",
            [run_id],
            |r| r.get(0),
        )?;
        let mut values = Vec::new();
        for (id, kind, state, started, finished) in rows {
            let resources = connection
                .prepare(
                    "SELECT resource FROM operation_resources WHERE lease_id=?1 ORDER BY resource",
                )?
                .query_map([&id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let resolution: Option<String> = connection
                .query_row(
                    "SELECT result_json FROM operation_resolutions WHERE lease_id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .optional()?;
            let mut value = serde_json::json!({"id":id,"kind":kind,"state":state,"started_at":started,"finished_at":finished,"resources":resources});
            if let Some(resolution) = resolution {
                value["resolution"] = serde_json::from_str(&resolution)?;
            }
            values.push(value);
        }
        Ok(
            serde_json::json!({"run_id":run_id,"operations":values,"total":total,"truncated":total>100,"policy":"local exclusive-owner leases; no clock-based expiry"}),
        )
    }
    pub fn save_stage_key(
        &self,
        id: &str,
        stage: &str,
        evidence: &crate::stage_keys::Evidence,
    ) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO stage_keys VALUES (?1,?2,?3)",
            params![id, stage, serde_json::to_string(evidence)?],
        )?;
        Ok(())
    }
    pub fn stage_key(&self, id: &str, stage: &str) -> Result<Option<crate::stage_keys::Evidence>> {
        ensure!(
            self.run(id)?
                .stages
                .iter()
                .any(|s| s.definition.id == stage),
            Fault::NotFound
        );
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT evidence_json FROM stage_keys WHERE run_id=?1 AND stage_id=?2",
                params![id, stage],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(Into::into)
    }

    pub fn toolchain(&self, id: &str) -> Result<Option<crate::pins::Verified>> {
        self.run(id)?;
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT verified_json FROM run_toolchains WHERE run_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(Into::into)
    }
    pub fn save_toolchain(&self, id: &str, verified: &crate::pins::Verified) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO run_toolchains VALUES (?1,?2)",
            params![id, serde_json::to_string(verified)?],
        )?;
        Ok(())
    }
    pub fn compatible_module(&self, run_id: &str, digest: &str) -> Result<Artifact> {
        self.compatible_module_inner(run_id, digest, None)
    }
    pub fn compatible_module_for_stage(
        &self,
        run_id: &str,
        digest: &str,
        stage: &StageDefinition,
    ) -> Result<Artifact> {
        self.compatible_module_inner(run_id, digest, Some(stage))
    }
    fn compatible_module_inner(
        &self,
        run_id: &str,
        digest: &str,
        stage: Option<&StageDefinition>,
    ) -> Result<Artifact> {
        let run = self.run(run_id)?;
        self.module(&run.project_id, digest)?;
        let verified = crate::modules::execution_contract(self, run_id, stage)?;
        let key = verified.as_ref().map_or("", |v| v.0.as_str());
        let value: Option<String> = self.connection()?.query_row("SELECT artifact_json FROM module_contracts WHERE project_id=?1 AND sha256=?2 AND contract_sha256=?3", params![run.project_id,digest,key], |r|r.get(0)).optional()?;
        let value = value.ok_or_else(|| {
            invalid(if verified.is_some() {
                "module has no matching verified toolchain/host/target/ABI or container contract"
            } else {
                "module requires a matching pinned-profile or container contract; no legacy publication exists"
            })
        })?;
        Ok(serde_json::from_str(&value)?)
    }
    pub fn module_contracts(&self, r: &crate::modules::ManifestQuery) -> Result<serde_json::Value> {
        self.module(&r.project_id, &r.sha256)?;
        ensure!(
            r.offset <= 100_000,
            invalid("module contract cursor out of range")
        );
        let c = self.connection()?;
        let values = c.prepare("SELECT contract_sha256,contract_json,artifact_json FROM module_contracts WHERE project_id=?1 AND sha256=?2 ORDER BY contract_sha256 LIMIT 101 OFFSET ?3")?.query_map(params![r.project_id,r.sha256,r.offset], |row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = values.len() > 100;
        let entries = values.into_iter().take(100).map(|(sha,contract,artifact)|Ok(serde_json::json!({"status":if sha.is_empty(){"legacy_unverified"}else{"verified_declared_contract"},"contract_sha256":sha,"contract":contract.map(|c|serde_json::from_str::<serde_json::Value>(&c)).transpose()?,"producer":serde_json::from_str::<Artifact>(&artifact)?}))).collect::<Result<Vec<_>>>()?;
        Ok(
            serde_json::json!({"project_id":r.project_id,"sha256":r.sha256,"entries":entries,"next_cursor":more.then_some(r.offset+100)}),
        )
    }
    pub fn source_request(&self, id: &str) -> Result<Option<crate::sources::SourceRequest>> {
        self.run(id)?;
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT request_json FROM run_sources WHERE run_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(Into::into)
    }
    pub fn git_source(&self, id: &str) -> Result<Option<crate::sources::GitSource>> {
        Ok(match self.source_request(id)? {
            Some(crate::sources::SourceRequest::Git(s)) => Some(s),
            _ => None,
        })
    }
    pub fn source_evidence(&self, id: &str) -> Result<serde_json::Value> {
        let request = self.source_request(id)?;
        let manifest = self.inputs(id)?;
        Ok(
            serde_json::json!({"run_id":id,"mode":request.as_ref().map_or("directory", |s|s.mode()),"requested":request.as_ref().map(|s|s.identity()).transpose()?,"prepared":manifest.as_ref().and_then(|m|m.source.clone()).map(crate::multi_source::summary),"tree_sha256":manifest.as_ref().map(|m|&m.tree_sha256),"configuration_sha256":manifest.as_ref().map(|m|&m.configuration_sha256)}),
        )
    }
    pub fn source_root_inputs(
        &self,
        request: &crate::multi_source::RootInputsQuery,
    ) -> Result<serde_json::Value> {
        self.run(&request.run_id)?;
        let manifest = self.inputs(&request.run_id)?.ok_or(Fault::NotFound)?;
        let source = manifest.source.ok_or(Fault::NotFound)?;
        let root = source
            .get("roots")
            .and_then(|r| r.as_array())
            .and_then(|roots| roots.iter().find(|r| r["name"] == request.root_name))
            .ok_or(Fault::NotFound)?;
        let manifest: crate::inputs::Manifest = serde_json::from_value(root["manifest"].clone())?;
        let mut page = crate::inputs::page(&manifest, request.offset)?;
        page["root_name"] = serde_json::json!(request.root_name);
        page["path"] = root["path"].clone();
        Ok(page)
    }
    pub fn source_patch(&self, r: &crate::sources::PatchQuery) -> Result<serde_json::Value> {
        let source = match (self.source_request(&r.run_id)?, &r.root_name) {
            (Some(crate::sources::SourceRequest::Git(s)), None) => s,
            (Some(crate::sources::SourceRequest::Multi(s)), Some(name)) => {
                match s
                    .roots
                    .into_iter()
                    .find(|root| root.name == *name)
                    .map(|root| root.revision)
                {
                    Some(crate::multi_source::RootMode::Git(s)) => s,
                    _ => return Err(Fault::NotFound.into()),
                }
            }
            _ => return Err(Fault::NotFound.into()),
        };
        let patch = source.patches.get(r.ordinal).ok_or(Fault::NotFound)?;
        Ok(serde_json::json!({"run_id":r.run_id,"ordinal":r.ordinal,"patch":patch}))
    }

    pub fn module_manifest(&self, r: &crate::modules::ManifestQuery) -> Result<serde_json::Value> {
        let artifact = self.module(&r.project_id, &r.sha256)?;
        ensure!(r.offset <= 20_000, invalid("manifest cursor out of range"));
        let value: Option<String> = self.connection()?.query_row(
            "SELECT manifest_json FROM modules WHERE project_id=?1 AND sha256=?2",
            params![r.project_id, r.sha256],
            |row| row.get(0),
        )?;
        let Some(value) = value else {
            return Ok(serde_json::json!({"kind":artifact.kind,"sha256":r.sha256,"manifest":null}));
        };
        let manifest: crate::trees::Manifest = serde_json::from_str(&value)?;
        ensure!(
            manifest.format == crate::trees::FORMAT
                && manifest.sha256()? == r.sha256
                && manifest.size() == artifact.size,
            "stored module manifest mismatch"
        );
        Ok(
            serde_json::json!({"kind":artifact.kind,"sha256":r.sha256,"format":manifest.format,"total":manifest.entries.len(),"entries":manifest.entries.iter().skip(r.offset).take(100).map(|(path,metadata)|serde_json::json!({"path":path,"metadata":metadata})).collect::<Vec<_>>(),"next_cursor":(r.offset+100<manifest.entries.len()).then_some(r.offset+100)}),
        )
    }

    pub fn record_module_input(
        &self,
        run: &str,
        stage: &str,
        path: &Path,
        artifact: &Artifact,
    ) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO module_inputs VALUES (?1,?2,?3,?4)",
            params![
                run,
                stage,
                path.to_str()
                    .ok_or_else(|| invalid("module path is not UTF-8"))?,
                serde_json::to_string(artifact)?
            ],
        )?;
        Ok(())
    }
    pub fn module_inputs(&self, r: &crate::inputs::InputQuery) -> Result<serde_json::Value> {
        self.run(&r.run_id)?;
        ensure!(
            r.offset <= 20_000,
            invalid("module input cursor out of range")
        );
        let c = self.connection()?;
        let mut stmt=c.prepare("SELECT stage_id,path,artifact_json FROM module_inputs WHERE run_id=?1 ORDER BY stage_id,path LIMIT 101 OFFSET ?2")?;
        let rows = stmt
            .query_map(params![r.run_id, r.offset], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > 100;
        let entries=rows.into_iter().take(100).map(|(stage,path,value)|Ok(serde_json::json!({"stage_id":stage,"materialized_path":path,"producer":serde_json::from_str::<serde_json::Value>(&value)?,"verified_at":"stage_preparation","host":"local"}))).collect::<Result<Vec<_>>>()?;
        Ok(
            serde_json::json!({"run_id":r.run_id,"entries":entries,"next_cursor":more.then_some(r.offset+100)}),
        )
    }

    pub fn publish_module(
        &self,
        project: &str,
        artifact: &Artifact,
        manifest: Option<&crate::trees::Manifest>,
    ) -> Result<()> {
        let producer = self.run(&artifact.run_id)?;
        let stage = producer
            .stages
            .iter()
            .find(|s| s.definition.id == artifact.stage_id)
            .map(|s| &s.definition);
        let verified = crate::modules::execution_contract(self, &artifact.run_id, stage)?;
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT artifact_json FROM modules WHERE project_id=?1 AND sha256=?2",
                params![project, artifact.sha256],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(value) = existing {
            let old: Artifact = serde_json::from_str(&value)?;
            ensure!(
                old.kind == artifact.kind && old.size == artifact.size,
                invalid("module digest already registered with different kind or size")
            );
        } else {
            tx.execute(
                "INSERT INTO modules(project_id,sha256,artifact_json,manifest_json) VALUES (?1,?2,?3,?4)",
                params![project, artifact.sha256, serde_json::to_string(artifact)?,manifest.map(serde_json::to_string).transpose()?],
            )?;
        }
        tx.execute(
            "DELETE FROM module_evictions WHERE project_id=?1 AND sha256=?2",
            params![project, artifact.sha256],
        )?;
        tx.execute("INSERT INTO module_contracts(project_id,sha256,contract_sha256,contract_json,artifact_json) VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",params![project,artifact.sha256,verified.as_ref().map_or("",|v|v.0.as_str()),verified.as_ref().map(|v|serde_json::to_string(&v.1)).transpose()?,serde_json::to_string(artifact)?])?;
        tx.execute("INSERT INTO module_publications VALUES (?1,?2,?3) ON CONFLICT(project_id,sha256) DO UPDATE SET published_at=excluded.published_at",params![project,artifact.sha256,now()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn module(&self, project: &str, digest: &str) -> Result<Artifact> {
        crate::modules::validate_digest(digest)?;
        self.project(project)?;
        let evicted: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM module_evictions WHERE project_id=?1 AND sha256=?2)",
            params![project, digest],
            |r| r.get(0),
        )?;
        ensure!(
            !evicted,
            invalid("module copy was evicted; republish its retained producer artifact")
        );
        let value: String = self
            .connection()?
            .query_row(
                "SELECT artifact_json FROM modules WHERE project_id=?1 AND sha256=?2",
                params![project, digest],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Fault::NotFound)?;
        Ok(serde_json::from_str(&value)?)
    }

    pub fn save_inputs(&self, id: &str, manifest: &crate::inputs::Manifest) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO run_inputs VALUES (?1,?2)",
            params![id, serde_json::to_string(manifest)?],
        )?;
        Ok(())
    }
    pub fn inputs(&self, id: &str) -> Result<Option<crate::inputs::Manifest>> {
        self.run(id)?;
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT manifest_json FROM run_inputs WHERE run_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(Into::into)
    }
    pub fn input_page(&self, r: &crate::inputs::InputQuery) -> Result<serde_json::Value> {
        match self.inputs(&r.run_id)? {
            Some(m) => Ok(
                serde_json::json!({"run_id":r.run_id,"available":true,"manifest":crate::inputs::page(&m,r.offset)?}),
            ),
            None => Ok(
                serde_json::json!({"run_id":r.run_id,"available":false,"reason":"snapshot not captured (pending, failed preparation, or historical run)"}),
            ),
        }
    }
    pub fn compare_inputs(&self, r: &crate::inputs::CompareQuery) -> Result<serde_json::Value> {
        ensure!(
            self.run(&r.run_id)?.project_id == self.run(&r.baseline_id)?.project_id,
            invalid("runs must belong to the same project")
        );
        let current = self
            .inputs(&r.run_id)?
            .ok_or_else(|| invalid("run has no input manifest"))?;
        let baseline = self
            .inputs(&r.baseline_id)?
            .ok_or_else(|| invalid("baseline has no input manifest"))?;
        Ok(
            serde_json::json!({"run_id":r.run_id,"baseline_id":r.baseline_id,"diff":crate::inputs::compare(&current,&baseline,r.offset)?}),
        )
    }

    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self(Mutex::new(open(path)?), Default::default()))
    }
    pub(crate) fn remember_secrets(&self, values: Vec<String>) {
        let mut current = self.1.write().expect("secret roster lock");
        let mut combined = current.clone();
        combined.extend(values);
        *current = crate::secrets::variants(combined);
    }
    pub(crate) fn secret_values(&self) -> Vec<String> {
        self.1.read().expect("secret roster lock").clone()
    }
    pub(crate) fn redact(&self, text: &str) -> String {
        let mut filter = crate::secrets::Filter::new(self.secret_values());
        crate::log_search::redact(&String::from_utf8_lossy(
            &filter.push(text.as_bytes(), true),
        ))
    }
    pub(crate) fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("database mutex poisoned"))
    }
    pub fn save_pipeline(&self, r: &crate::control::SavePipeline) -> Result<serde_json::Value> {
        crate::control::save(&self.project(&r.project_id)?.root, r)
    }
    pub fn pipeline_config(&self, r: &crate::control::ConfigRef) -> Result<serde_json::Value> {
        crate::control::get(&self.project(&r.project_id)?.root, r)
    }
    pub fn pipeline_list(&self, id: &str) -> Result<serde_json::Value> {
        crate::control::list(&self.project(id)?.root, id)
    }
    pub fn query_runs(&self, q: &crate::control::RunQuery) -> Result<serde_json::Value> {
        let limit = q.limit.unwrap_or(20);
        ensure!((1..=100).contains(&limit), invalid("limit must be 1..100"));
        if let Some(id) = &q.project_id {
            self.project(id)?;
        }
        if let Some(id) = &q.before {
            ensure!(id.parse::<uuid::Uuid>().is_ok(), invalid("invalid cursor"));
        }
        if let Some(status) = &q.status {
            let s: RunState = serde_json::from_value(serde_json::json!(status))
                .map_err(|_| invalid("invalid run status"))?;
            ensure!(
                !matches!(s, RunState::Blocked | RunState::Skipped),
                invalid("stage-only status")
            );
        }
        let c = self.connection()?;
        let mut stmt=c.prepare("SELECT id,project_id,status,created_at,finished_at,(SELECT p.name FROM run_pipeline_identity p WHERE p.run_id=runs.id),(SELECT x.name FROM experiment_runs x WHERE x.run_id=runs.id) FROM runs WHERE NOT EXISTS(SELECT 1 FROM management_trash t WHERE t.project_id=runs.project_id AND t.kind='run' AND t.target_id=runs.id AND t.restored_at IS NULL) AND NOT EXISTS(SELECT 1 FROM shared_executions x WHERE x.run_id=runs.id) AND (?1 IS NULL OR project_id=?1) AND (?2 IS NULL OR status=?2) AND (?3 IS NULL OR id<?3) AND (?5 IS NULL OR EXISTS(SELECT 1 FROM run_pipeline_identity p WHERE p.run_id=runs.id AND p.pipeline_key=?5)) AND ((?6 IS NULL AND (?7 OR NOT EXISTS(SELECT 1 FROM experiment_runs x WHERE x.run_id=runs.id))) OR (?6 IS NOT NULL AND EXISTS(SELECT 1 FROM experiment_runs x WHERE x.run_id=runs.id AND x.name=?6))) ORDER BY id DESC LIMIT ?4")?;
        let mut rows=stmt.query_map(params![q.project_id,q.status,q.before,limit+1,q.pipeline_key,q.experiment_name,q.include_experiments],|r|Ok(serde_json::json!({"run_id":r.get::<_,String>(0)?,"project_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?,"finished_at":r.get::<_,Option<i64>>(4)?,"pipeline_name":r.get::<_,Option<String>>(5)?,"experiment_name":r.get::<_,Option<String>>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let cursor = if more {
            rows.last().map(|r| r["run_id"].clone())
        } else {
            None
        };
        Ok(serde_json::json!({"runs":rows,"next_cursor":cursor}))
    }
    pub(crate) fn reconcile(&self) -> Result<()> {
        self.connection()?.execute("UPDATE run_archives SET state='failed',error='server restarted before archive completion',finished_at=?1 WHERE state IN ('queued','creating')",[now()])?;
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        tx.execute("UPDATE stage_runs SET status='infrastructure_error',finished_at=?1,cancel_actor=COALESCE(cancel_actor,'worker_lost'),reason=COALESCE(reason,'server restarted before completion') WHERE status IN ('queued','preparing','running','collecting')",[now()])?;
        tx.execute("UPDATE runs SET status='infrastructure_error',finished_at=?1,cancel_actor=COALESCE(cancel_actor,'worker_lost'),reason=COALESCE(reason,'server restarted before completion') WHERE finished_at IS NULL",[now()])?;
        tx.execute("UPDATE worker_connections SET state='failed',finished_at=?1,next_retry_at=NULL,reason='server restarted before provisioning started' WHERE state='queued'",[now()])?;
        tx.execute("UPDATE worker_connections SET state='interrupted',launcher_pid=NULL,finished_at=?1,reason='server restarted; verify previous SSH process has stopped before recovery' WHERE state IN ('starting','connected','stopping')",[now()])?;
        tx.execute("UPDATE worker_jobs SET state='lost',reason='server restarted before worker completion' WHERE state IN ('queued','leased','cancelling')",[])?;
        tx.execute("UPDATE operation_leases SET state='released',finished_at=?1 WHERE state='active' AND id IN (SELECT operation_id FROM worker_jobs WHERE state='lost' AND claim_hash IS NULL)",[now()])?;
        tx.execute(
            "UPDATE operation_leases SET state='abandoned',finished_at=?1 WHERE state='active'",
            [now()],
        )?;
        tx.execute(
            "UPDATE shared_subscribers SET state='abandoned' WHERE state='waiting'",
            [],
        )?;
        tx.execute("UPDATE ci_syncs SET state='queued',error='resuming read-only import after restart' WHERE state='running'",[])?;
        tx.execute("UPDATE operation_leases SET state='released',finished_at=?1 WHERE kind='ci_import' AND state='abandoned'",[now()])?;
        tx.execute("UPDATE fixture_results SET status='infrastructure_error' WHERE status IN ('queued','running')",[])?;
        tx.execute("UPDATE codex_bindings SET state='uncertain',last_error='server restarted during delivery; do not resend automatically',updated_at=CURRENT_TIMESTAMP WHERE state='sending'",[])?;

        tx.execute("INSERT OR IGNORE INTO completion_events(run_id,project_id,status,finished_at) SELECT id,project_id,status,finished_at FROM runs WHERE finished_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM shared_executions x WHERE x.run_id=runs.id) ORDER BY finished_at,id",[])?;
        tx.commit()?;
        Ok(())
    }
    pub fn register(&self, name: &str, root: &Path) -> Result<Project> {
        let c = self.connection()?;
        let path = root
            .to_str()
            .ok_or_else(|| invalid("project path must be UTF-8"))?;
        c.execute(
            "INSERT INTO projects(id,name,root) VALUES (?1,?2,?3) ON CONFLICT(root) DO NOTHING",
            params![uuid::Uuid::now_v7().to_string(), name, path],
        )?;
        Ok(c.query_row(
            "SELECT id,name,root FROM projects WHERE root=?1",
            [path],
            project_row,
        )?)
    }
    pub fn projects(&self) -> Result<Vec<Project>> {
        let c = self.connection()?;
        let mut stmt = c.prepare("SELECT id,name,root FROM projects ORDER BY name,id LIMIT 100")?;
        Ok(stmt
            .query_map([], project_row)?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn project(&self, id: &str) -> Result<Project> {
        self.connection()?
            .query_row(
                "SELECT id,name,root FROM projects WHERE id=?1",
                [id],
                project_row,
            )
            .optional()?
            .ok_or_else(|| Fault::NotFound.into())
    }
    pub fn insert_run(&self, id: &str, request: &StartBuild, workspace: &Path) -> Result<()> {
        self.insert_plan(
            id,
            &request.project_id,
            workspace,
            None,
            &[StageDefinition::native(request.command.clone())],
        )
    }
    pub fn insert_plan(
        &self,
        id: &str,
        project_id: &str,
        workspace: &Path,
        pipeline: Option<&PipelineFile>,
        stages: &[StageDefinition],
    ) -> Result<()> {
        self.insert_submission(id, project_id, workspace, pipeline, stages, None, None)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn insert_submission(
        &self,
        id: &str,
        project_id: &str,
        workspace: &Path,
        pipeline: Option<&PipelineFile>,
        stages: &[StageDefinition],
        submission: Option<&crate::profiles::Submission>,
        source: Option<&crate::sources::SourceRequest>,
    ) -> Result<()> {
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        let first = stages.first().ok_or_else(|| invalid("empty pipeline"))?;
        tx.execute("INSERT INTO runs(id,project_id,status,command_json,created_at,workspace,pipeline_json) VALUES (?1,?2,'queued',?3,?4,?5,?6)",params![id,project_id,serde_json::to_string(&first.command)?,now(),workspace.to_string_lossy(),pipeline.map(serde_json::to_string).transpose()?])?;
        let pipeline_json = pipeline.map(serde_json::to_string).transpose()?;
        let (key, name, saved) = crate::inspection::identity(
            pipeline_json.as_deref(),
            &serde_json::to_string(&first.command)?,
            submission.map(|s| &s.request),
        );
        tx.execute(
            "INSERT INTO run_pipeline_identity VALUES (?1,?2,?3,?4)",
            params![id, key, name, saved],
        )?;
        if let Some(s) = submission {
            tx.execute(
                "INSERT INTO submissions VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    project_id,
                    s.key,
                    s.fingerprint,
                    serde_json::to_string(&s.request)?
                ],
            )?;
        }
        if let Some(s) = submission
            && s.request["operation"] == "experiment_start"
        {
            if let Some(baseline) = s.request["experiment_definition"]["baseline_run"].as_str() {
                let expired: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM artifact_expiry WHERE run_id=?1)",
                    [baseline],
                    |r| r.get(0),
                )?;
                ensure!(
                    !expired,
                    invalid("artifact_expired: choose a baseline with retained output bytes")
                );
            }
            tx.execute(
                "INSERT INTO experiment_runs VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    project_id,
                    s.request["request"]["name"].as_str(),
                    s.request["request"]["revision"].as_u64(),
                    s.request["experiment_definition"].to_string(),
                    s.request["experiment_definition"]["baseline_run"].as_str()
                ],
            )?;
        }
        if let Some(s) = submission
            && s.request["operation"] == "pipeline_retry"
        {
            tx.execute("INSERT INTO experiment_runs SELECT ?1,project_id,name,revision,definition_json,baseline_run FROM experiment_runs WHERE run_id=?2 AND project_id=?3",params![id,s.request["retry"]["parent_run_id"].as_str(),project_id])?;
        }
        if let Some(source) = source {
            tx.execute(
                "INSERT INTO run_sources VALUES (?1,?2)",
                params![id, serde_json::to_string(source)?],
            )?;
        }
        for (ordinal, stage) in stages.iter().enumerate() {
            tx.execute("INSERT INTO stage_runs(id,run_id,name,ordinal,definition_json,status) VALUES (?1,?2,?3,?4,?5,'queued')",params![format!("{id}:{}",stage.id),id,stage.id,ordinal,serde_json::to_string(stage)?])?;
            if let Some(action) = &stage.fixtures {
                for (ordinal, case) in action.suite.cases.iter().enumerate() {
                    tx.execute("INSERT INTO fixture_results(run_id,stage_id,case_id,ordinal) VALUES (?1,?2,?3,?4)",params![id,stage.id,case.id,ordinal])?;
                }
            }
        }
        if let Some(s) = submission
            && s.request["operation"] == "single_flight"
        {
            tx.execute(
                "INSERT INTO shared_executions VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    s.request["owner_run"].as_str(),
                    s.request["owner_stage"].as_str(),
                    s.request["pipeline_id"].as_str(),
                    s.request["cache_key"].as_str()
                ],
            )?;
            tx.execute(
                "INSERT INTO shared_subscribers VALUES (?1,?2,?3,'waiting')",
                params![
                    s.request["owner_run"].as_str(),
                    s.request["owner_stage"].as_str(),
                    id
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn submitted(&self, project: &str, s: &crate::profiles::Submission) -> Result<Option<Run>> {
        let Some(key) = &s.key else { return Ok(None) };
        let c = self.connection()?;
        let row = c
            .query_row(
                "SELECT run_id,fingerprint FROM submissions WHERE project_id=?1 AND request_key=?2",
                params![project, key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((id, fingerprint)) = row {
            ensure!(fingerprint == s.fingerprint, Fault::IdempotencyConflict);
            Ok(Some(load_run(&c, &id)?))
        } else {
            Ok(None)
        }
    }
    pub fn submission(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let c = self.connection()?;
        let value = c
            .query_row(
                "SELECT request_json FROM submissions WHERE run_id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.map(|v| serde_json::from_str(&v)).transpose()?)
    }
    pub fn run(&self, id: &str) -> Result<Run> {
        let c = self.connection()?;
        load_run(&c, id)
    }
    pub fn runs(&self) -> Result<Vec<Run>> {
        let c = self.connection()?;
        let mut query =
            c.prepare("SELECT id FROM runs WHERE NOT EXISTS(SELECT 1 FROM shared_executions x WHERE x.run_id=runs.id) ORDER BY created_at DESC,id DESC LIMIT 100")?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter().map(|id| load_run(&c, id)).collect()
    }
    pub fn transition(
        &self,
        id: &str,
        state: RunState,
        exit: Option<i32>,
        actor: Option<&str>,
        reason: Option<&str>,
    ) -> Result<()> {
        ensure!(
            !matches!(state, RunState::Blocked | RunState::Skipped),
            "invalid overall run state"
        );
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        let old: RunState = tx.query_row("SELECT status FROM runs WHERE id=?1", [id], |r| {
            decode(r, 0, true)
        })?;
        check_transition(old, state)?;
        tx.execute("UPDATE runs SET status=?2,exit_code=?3,finished_at=?4,cancel_actor=COALESCE(cancel_actor,?5),reason=COALESCE(reason,?6) WHERE id=?1",params![id,state.as_str(),exit,state.terminal().then(now),actor,reason])?;
        if state.terminal() {
            tx.execute("INSERT INTO completion_events(run_id,project_id,status,finished_at) SELECT id,project_id,status,finished_at FROM runs WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM shared_executions x WHERE x.run_id=runs.id)",[id])?;
            tx.execute("UPDATE fixture_results SET status=?2 WHERE run_id=?1 AND status IN ('queued','running')",params![id,state.as_str()])?;
            tx.execute("UPDATE stage_runs SET status=?2,finished_at=?3,cancel_actor=COALESCE(cancel_actor,?4),reason=COALESCE(reason,?5) WHERE run_id=?1 AND status IN ('queued','preparing','running','collecting')",params![id,state.as_str(),now(),actor,reason])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn stage_transition(
        &self,
        run_id: &str,
        name: &str,
        state: RunState,
        exit: Option<i32>,
        reason: Option<&str>,
    ) -> Result<()> {
        self.finish_stage(run_id, name, state, exit, reason, None, &[])
    }
    #[allow(clippy::too_many_arguments)]
    pub fn finish_stage(
        &self,
        run_id: &str,
        name: &str,
        state: RunState,
        exit: Option<i32>,
        reason: Option<&str>,
        tests: Option<&TestReport>,
        artifacts: &[Artifact],
    ) -> Result<()> {
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        let old: RunState = tx.query_row(
            "SELECT status FROM stage_runs WHERE run_id=?1 AND name=?2",
            params![run_id, name],
            |r| decode(r, 0, true),
        )?;
        check_transition(old, state)?;
        let actor = (state == RunState::TimedOut).then_some("timeout");
        if state.terminal() {
            tx.execute("UPDATE fixture_results SET status=?3 WHERE run_id=?1 AND stage_id=?2 AND status IN ('queued','running')",params![run_id,name,state.as_str()])?;
        }
        let mut stored = tests.cloned();
        if let Some(report) = stored.as_mut() {
            for case in &mut report.cases {
                case.output = None;
            }
        }
        tx.execute("UPDATE stage_runs SET status=?3,exit_code=?4,started_at=COALESCE(started_at,?5),finished_at=?6,cancel_actor=COALESCE(cancel_actor,?7),reason=COALESCE(reason,?8),tests_json=COALESCE(?9,tests_json) WHERE run_id=?1 AND name=?2",params![run_id,name,state.as_str(),exit,(state==RunState::Running).then(now),state.terminal().then(now),actor,reason,stored.as_ref().map(serde_json::to_string).transpose()?])?;
        if let Some(report) = tests {
            for case in &report.cases {
                let event = format!("Test {} reported {}\n", case.name, case.status);
                let mut values = vec![
                    ("test_event", event.as_str()),
                    ("diagnostic", case.message.as_str()),
                ];
                if let Some(output) = &case.output {
                    values.push(("stdout", &output.stdout));
                    values.push(("stderr", &output.stderr));
                }
                for (stream, text) in values {
                    if text.is_empty() {
                        continue;
                    }
                    let mut text = crate::log_search::redact(text);
                    if case.output.as_ref().is_some_and(|o| o.truncated) {
                        text.push_str("\n[reported test output truncated]");
                    }
                    tx.execute("INSERT INTO log_events(run_id,stage_id,stream,sequence,timestamp,text,test_id,ingested_at) VALUES (?1,?2,?3,(SELECT COALESCE(MAX(sequence),-1)+1 FROM log_events WHERE run_id=?1 AND stage_id=?2 AND stream=?3),?4,?5,?6,?4)",params![run_id,name,stream,now(),text,case.id])?;
                    tx.execute(
                        "INSERT INTO log_search(rowid,text) VALUES (?1,?2)",
                        params![tx.last_insert_rowid(), text],
                    )?;
                }
            }
        }

        ensure!(
            artifacts.is_empty() || state == RunState::Succeeded,
            "only successful stages publish artifacts"
        );
        for a in artifacts {
            tx.execute("INSERT INTO artifacts(run_id,stage_id,name,kind,sha256,size,path) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![run_id,name,a.name,serde_json::to_string(&a.kind)?,a.sha256,a.size,a.path.to_string_lossy()])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn compare_batches(&self, id: &str, baseline: &str) -> Result<serde_json::Value> {
        ensure!(
            self.run(id)?.project_id == self.run(baseline)?.project_id,
            invalid("batch comparison requires the same project")
        );
        fn states(
            c: &Connection,
            id: &str,
        ) -> Result<std::collections::BTreeMap<(String, String), String>> {
            let mut stmt=c.prepare("SELECT stage_id,case_id,status FROM fixture_results WHERE run_id=?1 ORDER BY stage_id,ordinal")?;
            Ok(stmt
                .query_map([id], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?)
        }
        let c = self.connection()?;
        let current = states(&c, id)?;
        let previous = states(&c, baseline)?;
        let keys = current
            .keys()
            .chain(previous.keys())
            .collect::<std::collections::BTreeSet<_>>();
        let differences=keys.into_iter().filter(|k|current.get(*k)!=previous.get(*k)).map(|(stage,case)|serde_json::json!({"stage_id":stage,"case_id":case,"before":previous.get(&(stage.clone(),case.clone())),"after":current.get(&(stage.clone(),case.clone()))})).collect::<Vec<_>>();
        Ok(
            serde_json::json!({"batch_id":id,"baseline_id":baseline,"changed":differences.len(),"differences":differences.iter().take(20).collect::<Vec<_>>(),"truncated":differences.len()>20}),
        )
    }
    pub fn save_fixture(
        &self,
        id: &str,
        stage: &str,
        result: &crate::fixtures::CaseResult,
    ) -> Result<()> {
        let changed=self.connection()?.execute("UPDATE fixture_results SET status=?4,result_json=?5 WHERE run_id=?1 AND stage_id=?2 AND case_id=?3",params![id,stage,result.case_id,result.status.as_str(),serde_json::to_string(result)?])?;
        ensure!(changed == 1, "fixture case missing");
        Ok(())
    }
    pub fn fixture_results(&self, id: &str, offset: usize) -> Result<serde_json::Value> {
        self.run(id)?;
        ensure!(offset <= 3200, invalid("fixture cursor out of range"));
        let c = self.connection()?;
        let mut stmt=c.prepare("SELECT stage_id,case_id,status FROM fixture_results WHERE run_id=?1 ORDER BY stage_id,ordinal LIMIT 21 OFFSET ?2")?;
        let mut rows=stmt.query_map(params![id,offset],|r| Ok(serde_json::json!({"stage_id":r.get::<_,String>(0)?,"case_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let next = (rows.len() > 20).then_some(offset + 20);
        rows.truncate(20);
        let mut counts = c.prepare(
            "SELECT status,count(*) FROM fixture_results WHERE run_id=?1 GROUP BY status",
        )?;
        let counts = counts
            .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?
            .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()?;
        Ok(serde_json::json!({"batch_id":id,"cases":rows,"counts":counts,"next_cursor":next}))
    }
    pub fn fixture_diff(&self, id: &str, stage: &str, case: &str) -> Result<serde_json::Value> {
        let c = self.connection()?;
        let (status,json): (String,Option<String>)=c.query_row("SELECT status,result_json FROM fixture_results WHERE run_id=?1 AND stage_id=?2 AND case_id=?3",params![id,stage,case],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Fault::NotFound)?;
        let mut value = json
            .map(|s| serde_json::from_str::<serde_json::Value>(&s))
            .transpose()?
            .unwrap_or_else(|| serde_json::json!({"case_id":case}));
        value["status"] = status.into();
        value["stage_id"] = stage.into();
        value["batch_id"] = id.into();
        Ok(value)
    }
    pub fn request_cancel(&self, id: &str, actor: &str, reason: &str) -> Result<()> {
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        tx.execute("UPDATE runs SET cancel_actor=COALESCE(cancel_actor,?2),reason=COALESCE(reason,?3) WHERE id=?1 AND finished_at IS NULL",params![id,actor,reason])?;
        tx.execute("UPDATE stage_runs SET cancel_actor=COALESCE(cancel_actor,?2),reason=COALESCE(reason,?3) WHERE run_id=?1 AND status IN ('queued','preparing','running','collecting')",params![id,actor,reason])?;
        tx.commit()?;
        Ok(())
    }
    pub fn append_stage_log(
        &self,
        run_id: &str,
        stage_id: &str,
        stream: &str,
        sequence: i64,
        text: &str,
    ) -> Result<()> {
        let text = self.redact(text);
        let captured_at = now();
        let mut c = self.connection()?;
        let tx = c.transaction()?;
        insert_stage_log(&tx, run_id, stage_id, stream, sequence, captured_at, &text)?;
        tx.commit()?;
        Ok(())
    }
    pub fn logs(&self, run_id: &str, after: i64) -> Result<LogPage> {
        self.stage_logs(run_id, None, after)
    }
    pub fn stage_logs(&self, run_id: &str, stage: Option<&str>, after: i64) -> Result<LogPage> {
        self.run(run_id)?;
        let c = self.connection()?;
        crate::evidence_expiry::available(&c, run_id, crate::evidence_expiry::Category::Logs)?;
        let mut stmt=c.prepare("SELECT id,stage_id,stream,sequence,timestamp,text,ingested_at FROM log_events WHERE run_id=?1 AND id>?2 AND (?3 IS NULL OR stage_id=?3) ORDER BY id LIMIT 17")?;
        let mut events = stmt
            .query_map(params![run_id, after, stage], |r| {
                Ok(LogEvent {
                    id: r.get(0)?,
                    stage_id: r.get(1)?,
                    stream: r.get(2)?,
                    sequence: r.get(3)?,
                    timestamp: r.get(4)?,
                    text: r.get(5)?,
                    ingested_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = events.len() > 16;
        events.truncate(16);
        crate::log_archive::hydrate(&c, run_id, &mut events)?;
        let next_cursor = if more {
            events.last().map(|e| e.id)
        } else {
            None
        };
        Ok(LogPage {
            events,
            next_cursor,
        })
    }
    pub fn artifacts(&self, run_id: &str) -> Result<Vec<Artifact>> {
        let c = self.connection()?;
        let mut stmt=c.prepare("SELECT run_id,stage_id,name,kind,sha256,size,path FROM artifacts WHERE run_id=?1 ORDER BY stage_id,name")?;
        Ok(stmt
            .query_map([run_id], |r| {
                Ok(Artifact {
                    run_id: r.get(0)?,
                    stage_id: r.get(1)?,
                    name: r.get(2)?,
                    kind: decode(r, 3, false)?,
                    sha256: r.get(4)?,
                    size: r.get(5)?,
                    path: std::path::PathBuf::from(r.get::<_, String>(6)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}
fn check_transition(old: RunState, state: RunState) -> Result<()> {
    ensure!(
        !old.terminal()
            && (state.terminal()
                || matches!(
                    (old, state),
                    (RunState::Queued, RunState::Preparing)
                        | (RunState::Preparing, RunState::Running)
                        | (RunState::Running, RunState::Collecting)
                )),
        "invalid state transition: {old:?} -> {state:?}"
    );
    Ok(())
}
fn project_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        root: std::path::PathBuf::from(r.get::<_, String>(2)?),
    })
}
fn decode<T: serde::de::DeserializeOwned>(
    r: &rusqlite::Row<'_>,
    i: usize,
    quoted: bool,
) -> rusqlite::Result<T> {
    let text: String = r.get(i)?;
    let result = if quoted {
        serde_json::from_value(serde_json::Value::String(text))
    } else {
        serde_json::from_str(&text)
    };
    result.map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn optional_json<T: serde::de::DeserializeOwned>(
    r: &rusqlite::Row<'_>,
    i: usize,
) -> rusqlite::Result<Option<T>> {
    if r.get::<_, Option<String>>(i)?.is_none() {
        Ok(None)
    } else {
        decode(r, i, false).map(Some)
    }
}
fn load_run(c: &Connection, id: &str) -> Result<Run> {
    let mut stmt=c.prepare("SELECT id,definition_json,status,exit_code,started_at,finished_at,cancel_actor,reason,tests_json FROM stage_runs WHERE run_id=?1 ORDER BY ordinal")?;
    let stages = stmt
        .query_map([id], |r| {
            Ok(StageRun {
                id: r.get(0)?,
                definition: decode(r, 1, false)?,
                status: decode(r, 2, true)?,
                exit_code: r.get(3)?,
                started_at: r.get(4)?,
                finished_at: r.get(5)?,
                cancel_actor: r.get(6)?,
                reason: r.get(7)?,
                tests: optional_json(r, 8)?,
                external: c
                    .query_row(
                        "SELECT metadata_json FROM ci_jobs WHERE run_id=?1 AND stage_id=?2",
                        params![
                            id,
                            decode::<crate::pipeline::StageDefinition>(r, 1, false)?.id
                        ],
                        |v| v.get::<_, String>(0),
                    )
                    .optional()?
                    .and_then(|v| serde_json::from_str(&v).ok()),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let stage = stages.first().ok_or(Fault::NotFound)?.clone();
    let expired_payloads = c
        .prepare("SELECT category FROM evidence_expiry WHERE run_id=?1 ORDER BY category")?
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(c.query_row("SELECT id,project_id,status,command_json,created_at,finished_at,exit_code,cancel_actor,reason,workspace,pipeline_json,(SELECT json_extract(request_json,'$.retry') FROM submissions WHERE run_id=runs.id) FROM runs WHERE id=?1",[id],|r|Ok(Run{external:c.query_row("SELECT json_set(metadata_json,'$.provider',provider,'$.import_state',state,'$.read_only',json('true')) FROM ci_runs WHERE run_id=?1",[id],|v|v.get::<_,String>(0)).optional()?.and_then(|v|serde_json::from_str(&v).ok()),expired_payloads,retry:optional_json(r,11)?,id:r.get(0)?,project_id:r.get(1)?,status:decode(r,2,true)?,command:decode(r,3,false)?,created_at:r.get(4)?,finished_at:r.get(5)?,exit_code:r.get(6)?,cancel_actor:r.get(7)?,reason:r.get(8)?,workspace:std::path::PathBuf::from(r.get::<_,String>(9)?),pipeline:optional_json(r,10)?,stage,stages}))?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_is_repeatable_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        open(&path).unwrap();
        let c = open(&path).unwrap();
        assert_eq!(
            c.query_row("SELECT MAX(version) FROM schema_version", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            crate::migrations::VERSION as u32
        );
    }
    #[test]
    fn future_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.sqlite3");
        let c = open(&path).unwrap();
        c.execute(
            "INSERT INTO schema_version VALUES (?1)",
            [crate::migrations::VERSION + 1],
        )
        .unwrap();
        drop(c);
        assert!(open(&path).is_err());
    }
    #[test]
    fn m1_migration_keeps_completed_runs_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sqlite3");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(include_str!("../../../migrations/0001_initial.sql"))
            .unwrap();
        c.execute_batch(r#"INSERT INTO projects VALUES ('p','old','/old');
        INSERT INTO runs VALUES ('r','p','succeeded','{"program":"cargo","args":["build"]}',1,2,0,NULL,NULL,'/workspace');
        INSERT INTO stage_runs VALUES ('r:native','r','succeeded',0);
        INSERT INTO log_events(run_id,stream,sequence,timestamp,text) VALUES ('r','stdout',0,1,'kept');"#).unwrap();
        drop(c);
        let db = Db::new(&path).unwrap();
        db.reconcile().unwrap();
        let run = db.run("r").unwrap();
        assert_eq!(run.status, RunState::Succeeded);
        assert_eq!(run.stage.status, RunState::Succeeded);
        assert_eq!(run.stage.definition.command.program, "cargo");
        assert!(run.pipeline.is_none());
        let events = db
            .completion_events(&crate::events::Query {
                project_id: "p".into(),
                after: 0,
                limit: 100,
            })
            .unwrap();
        assert_eq!(events["events"].as_array().unwrap().len(), 1);
        assert_eq!(events["events"][0]["historical"], true);

        assert!(
            db.begin_operation("unknown", "test", Default::default())
                .is_err()
        );
        let lease = db
            .begin_operation(
                "r",
                "test_release",
                std::collections::BTreeSet::from(["module:example".into()]),
            )
            .unwrap();
        let released_id = lease.id.clone();
        assert_eq!(
            db.operations("r").unwrap()["operations"][0]["state"],
            "active"
        );
        drop(lease);
        db.release_operation(&released_id).unwrap();
        assert_eq!(
            db.operations("r").unwrap()["operations"][0]["state"],
            "released"
        );
        let abandoned = db
            .begin_operation("r", "test_crash", Default::default())
            .unwrap();
        // Simulate process loss: guard cannot execute its release.
        std::mem::forget(abandoned);
        drop(db);
        let db = Db::new(&path).unwrap();
        assert_eq!(
            db.operations("r").unwrap()["operations"][0]["state"],
            "active"
        );
        db.reconcile().unwrap();
        db.reconcile().unwrap();
        let operations = db.operations("r").unwrap();
        assert_eq!(operations["operations"][0]["state"], "abandoned");
        assert_eq!(operations["operations"][1]["state"], "released");
        assert_eq!(
            operations["operations"][1]["resources"],
            serde_json::json!(["module:example", "run:r"])
        );
        assert_eq!(db.run("r").unwrap().status, RunState::Succeeded);

        assert_eq!(db.logs("r", 0).unwrap().events[0].text, "kept");
    }
}
