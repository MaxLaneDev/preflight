//! Portable verified run snapshots; never mutates live evidence or restores over it.
use crate::{
    engine::Engine,
    model::{Fault, invalid},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
pub const FORMAT: &str = "preflight-run-archive-v1";
pub(crate) const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_METADATA: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub project_id: String,
    pub run_id: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_recipient: Option<String>,
    #[serde(default = "yes")]
    pub logs: bool,
    #[serde(default = "yes")]
    pub artifacts: bool,
    #[serde(default = "yes")]
    pub source: bool,
    #[serde(default)]
    pub environment: bool,
    pub actor: crate::model::Actor,
    pub reason: String,
}
fn yes() -> bool {
    true
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Id {
    pub project_id: String,
    pub archive_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unlock {
    #[serde(flatten)]
    pub id: Id,
    pub identity_file_env: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub project_id: String,
    pub run_id: Option<String>,
    pub pipeline_key: Option<String>,
    pub before: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Restore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file_env: Option<String>,
    pub project_id: String,
    pub archive_id: String,
    pub request_id: String,
    pub actor: crate::model::Actor,
    pub reason: String,
}
#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Import {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file_env: Option<String>,
    pub project_id: String,
    pub request_id: String,
    pub sha256: String,
    pub actor: crate::model::Actor,
    pub reason: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportStaged {
    #[serde(flatten)]
    pub import: Import,
    /// One file name inside data_dir/archives/inbox; arbitrary server paths are refused.
    pub file_name: String,
}
pub(crate) fn inbox(e: &Engine) -> Result<PathBuf> {
    let root = archive_root(e)?.join("inbox");
    private_dir(&root)?;
    Ok(root)
}
pub fn inbox_location(e: &Engine, project: crate::control::ProjectId) -> Result<Value> {
    e.db.project(&project.project_id)?;
    Ok(
        json!({"path":inbox(e)?,"max_file_bytes":MAX_BYTES,"format":FORMAT,
        "effect":"stage a regular archive file here, then call archives_import with its file_name and SHA256; staging files remain operator-owned"}),
    )
}
pub async fn import_staged(e: Arc<Engine>, r: ImportStaged) -> Result<Value> {
    ensure!(
        crate::trees::portable_component(&r.file_name),
        invalid("invalid inbox file name")
    );
    if let Some(replay) = import_replay(&e, &r.import)? {
        return Ok(replay);
    }
    let permit = e
        .transfer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Fault::Busy)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let file = open_read(&inbox(&e)?.join(&r.file_name))?;
        import_file(&e, r.import, file)
    })
    .await?
}
pub(crate) fn import_file(e: &Engine, r: Import, mut source: File) -> Result<Value> {
    ensure!(!e.is_stopping(), Fault::Busy);
    e.db.project(&r.project_id)?;
    ensure!(
        !r.request_id.is_empty()
            && r.request_id.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 512,
        invalid("request_id and reason required")
    );
    ensure!(
        r.sha256.len() == 64
            && r.sha256
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
        invalid("sha256 must be 64 lowercase hexadecimal characters")
    );
    if let Some(replay) = import_replay(e, &r)? {
        return Ok(replay);
    }
    let request = serde_json::to_string(&r)?;
    // Copy first: the operator may replace or modify the inbox file during verification.
    let root = archive_root(e)?;
    let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
    let bytes = std::io::copy(
        &mut Read::by_ref(&mut source).take(MAX_BYTES + 1),
        temporary.as_file_mut(),
    )?;
    ensure!(
        bytes <= MAX_BYTES,
        invalid("compressed archive exceeds 1 GiB")
    );
    ensure!(
        file_hash(temporary.as_file_mut())? == r.sha256,
        invalid("archive digest mismatch")
    );
    let encrypted = crate::archive_crypto::is_encrypted(temporary.as_file_mut())?;
    let mut plain = decode(
        temporary.as_file_mut(),
        r.identity_file_env.as_deref(),
        &root,
        MAX_BYTES,
    )?;
    let manifest = inspect(
        plain
            .as_mut()
            .map(|v| v.as_file_mut())
            .unwrap_or(temporary.as_file_mut()),
        None,
    )
    .map_err(|error| invalid(format!("invalid archive: {error}")))?;
    uuid::Uuid::parse_str(&manifest.project_id)
        .map_err(|_| invalid("invalid origin project id"))?;
    uuid::Uuid::parse_str(&manifest.run_id).map_err(|_| invalid("invalid origin run id"))?;
    let id = uuid::Uuid::now_v7().to_string();
    let path = root.join(format!("{id}.tar.zst"));
    let mut connection = e.db.connection()?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if let Some((old_id, old_request)) = tx
        .query_row(
            "SELECT id,request_json FROM archive_imports WHERE project_id=?1 AND request_id=?2",
            params![r.project_id, r.request_id],
            |v| Ok((v.get::<_, String>(0)?, v.get::<_, String>(1)?)),
        )
        .optional()?
    {
        ensure!(old_request == request, Fault::IdempotencyConflict);
        drop(tx);
        drop(connection);
        return get(
            e,
            &Id {
                project_id: r.project_id,
                archive_id: old_id,
            },
        );
    }
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(&path)?;
    let inserted = (|| -> Result<()> {
        let now = crate::db::now();
        tx.execute("INSERT INTO archive_imports(id,project_id,origin_project_id,run_id,request_id,request_json,state,sha256,file_bytes,manifest_json,created_at,finished_at) VALUES (?1,?2,?3,?4,?5,?6,'ready',?7,?8,?9,?10,?10)",
            params![id,r.project_id,manifest.project_id,manifest.run_id,r.request_id,request,r.sha256,bytes,serde_json::to_string(&manifest)?,now])?;
        if encrypted {
            tx.execute("INSERT INTO archive_encryption(archive_id,format) VALUES (?1,'age-encryption.org/v1')",[&id])?;
        }
        tx.commit()?;
        Ok(())
    })();
    if inserted.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    inserted?;
    drop(connection);
    get(
        e,
        &Id {
            project_id: r.project_id,
            archive_id: id,
        },
    )
}
fn decode(
    file: &mut File,
    reference: Option<&str>,
    directory: &Path,
    max: u64,
) -> Result<Option<tempfile::NamedTempFile>> {
    crate::archive_crypto::decoded(file, reference, directory, max)
        .map_err(|error| invalid(format!("archive decryption failed: {error}")))
}
pub(crate) fn import_replay(e: &Engine, request: &Import) -> Result<Option<Value>> {
    e.db.project(&request.project_id)?;
    let old =
        e.db.connection()?
            .query_row(
                "SELECT id,request_json FROM archive_imports WHERE project_id=?1 AND request_id=?2",
                params![request.project_id, request.request_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
    if let Some((id, previous)) = old {
        ensure!(
            previous == serde_json::to_string(request)?,
            Fault::IdempotencyConflict
        );
        return Ok(Some(get(
            e,
            &Id {
                project_id: request.project_id.clone(),
                archive_id: id,
            },
        )?));
    }
    Ok(None)
}
fn matches_owner(e: &Engine, id: &Id, manifest: &Manifest) -> Result<bool> {
    let record = row(e, id)?;
    Ok(record["origin_project_id"] == manifest.project_id && record["run_id"] == manifest.run_id)
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Entry {
    kind: String,
    size: u64,
    sha256: Option<String>,
    executable: bool,
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    project_id: String,
    run_id: String,
    created_at: i64,
    components: Value,
    missing: Vec<String>,
    entries: BTreeMap<String, Entry>,
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    if !path.try_exists()? {
        let b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let mut b = b;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(path)?;
    }
    crate::storage_gc::directory(path)
}
pub(crate) fn archive_root(e: &Engine) -> Result<PathBuf> {
    crate::storage_gc::directory(&e.config.data_dir)?;
    let root = e.config.data_dir.join("archives");
    private_dir(&root)?;
    Ok(root)
}
pub(crate) fn open_read(path: &Path) -> Result<File> {
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && !m.is_symlink(),
        invalid("archive files must be regular files")
    );
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    Ok(o.open(path)?)
}
fn relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && path.split('/').all(crate::trees::portable_component)
            && !path.contains('\\'),
        invalid("unsafe archive path")
    );
    Ok(())
}
pub(crate) fn file_hash(f: &mut File) -> Result<String> {
    f.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    f.seek(SeekFrom::Start(0))?;
    Ok(crate::hashing::hex(hash.finalize()))
}
fn row(e: &Engine, r: &Id) -> Result<Value> {
    uuid::Uuid::parse_str(&r.archive_id).map_err(|_| invalid("invalid archive id"))?;
    let c = e.db.connection()?;
    let mut value=c.query_row("SELECT id,project_id,run_id,CASE WHEN EXISTS(SELECT 1 FROM archive_disposals d WHERE d.archive_id=a.id) THEN 'expired' ELSE state END,sha256,file_bytes,manifest_json,error,created_at,finished_at,origin_project_id,imported,COALESCE((SELECT pinned FROM archive_pins p WHERE p.archive_id=a.id),0),COALESCE((SELECT revision FROM archive_pins p WHERE p.archive_id=a.id),0) FROM archive_inventory a WHERE id=?1 AND project_id=?2",params![r.archive_id,r.project_id],|v|Ok(json!({"archive_id":v.get::<_,String>(0)?,"project_id":v.get::<_,String>(1)?,"run_id":v.get::<_,String>(2)?,"state":v.get::<_,String>(3)?,"sha256":v.get::<_,Option<String>>(4)?,"file_bytes":v.get::<_,Option<u64>>(5)?,"manifest":v.get::<_,Option<String>>(6)?,"error":v.get::<_,Option<String>>(7)?,"created_at":v.get::<_,i64>(8)?,"finished_at":v.get::<_,Option<i64>>(9)?,"origin_project_id":v.get::<_,String>(10)?,"imported":v.get::<_,bool>(11)?,"pinned":v.get::<_,bool>(12)?,"pin_revision":v.get::<_,u64>(13)?}))).optional()?.ok_or(Fault::NotFound)?;
    let envelope=c.query_row("SELECT format,recipient FROM archive_encryption WHERE archive_id=?1",[&r.archive_id],|v|Ok(json!({"format":v.get::<_,String>(0)?,"recipient":v.get::<_,Option<String>>(1)?}))).optional()?;
    value["encryption"] = envelope.unwrap_or(Value::Null);
    Ok(value)
}
pub fn get(e: &Engine, r: &Id) -> Result<Value> {
    let mut v = row(e, r)?;
    if let Some(raw) = v["manifest"].as_str() {
        let m: Manifest = serde_json::from_str(raw)?;
        v["summary"] = json!({"format":m.format,"entries":m.entries.len(),"uncompressed_file_bytes":m.entries.values().map(|v|v.size).sum::<u64>(),"components":m.components,"missing":m.missing,"environment":"run workspace and recorded toolchain/SDK identities; not an OS image or installed toolchain backup"});
    }
    v.as_object_mut().unwrap().remove("manifest");
    Ok(v)
}
pub fn list(e: &Engine, r: List) -> Result<Value> {
    e.db.project(&r.project_id)?;
    ensure!(
        r.pipeline_key.as_ref().is_none_or(|s| s.len() <= 512),
        invalid("invalid pipeline key")
    );
    if let Some(id) = &r.before {
        uuid::Uuid::parse_str(id).map_err(|_| invalid("invalid cursor"))?;
    }
    let ids = {
        let c = e.db.connection()?;
        let mut s=c.prepare("SELECT a.id FROM archive_inventory a WHERE a.project_id=?1 AND (?2 IS NULL OR a.run_id=?2) AND (?3 IS NULL OR EXISTS(SELECT 1 FROM run_pipeline_identity i WHERE i.run_id=a.run_id AND i.pipeline_key=?3)) AND (?4 IS NULL OR a.id<?4) ORDER BY a.id DESC LIMIT 26")?;
        s.query_map(
            params![r.project_id, r.run_id, r.pipeline_key, r.before],
            |v| v.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = ids.len() > 25;
    let mut values = Vec::new();
    for id in ids.into_iter().take(25) {
        values.push(get(
            e,
            &Id {
                project_id: r.project_id.clone(),
                archive_id: id,
            },
        )?);
    }
    let cursor = more.then(|| values.last().unwrap()["archive_id"].clone());
    Ok(json!({"items":values,"next_cursor":cursor}))
}
pub fn create(e: &Arc<Engine>, r: Create) -> Result<Value> {
    ensure!(!e.is_stopping(), Fault::Busy);
    ensure!(
        !r.request_id.is_empty()
            && r.request_id.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 512,
        invalid("request_id and reason required")
    );
    if let Some(public) = &r.encryption_recipient {
        crate::archive_crypto::recipient(public)
            .map_err(|_| invalid("invalid age X25519 recipient"))?;
    }
    let run = e.db.run(&r.run_id)?;
    ensure!(run.project_id == r.project_id, Fault::NotFound);
    ensure!(
        run.status.terminal(),
        invalid("archive only completed runs")
    );
    let request = serde_json::to_string(&r)?;
    let id = uuid::Uuid::now_v7().to_string();
    {
        let mut c = e.db.connection()?;
        let tx = c.transaction()?;
        if let Some((id, old)) = tx
            .query_row(
                "SELECT id,request_json FROM run_archives WHERE project_id=?1 AND request_id=?2",
                params![r.project_id, r.request_id],
                |v| Ok((v.get::<_, String>(0)?, v.get::<_, String>(1)?)),
            )
            .optional()?
        {
            ensure!(old == request, Fault::IdempotencyConflict);
            drop(tx);
            drop(c);
            return get(
                e,
                &Id {
                    project_id: r.project_id,
                    archive_id: id,
                },
            );
        }
        let busy: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM run_archives WHERE state IN ('queued','creating'))",
            [],
            |v| v.get(0),
        )?;
        ensure!(
            !busy,
            invalid("another archive is being created; retry after it settles")
        );
        tx.execute("INSERT INTO run_archives(id,project_id,run_id,request_id,request_json,state,created_at) VALUES (?1,?2,?3,?4,?5,'queued',?6)",params![id,r.project_id,r.run_id,r.request_id,request,crate::db::now()])?;
        if let Some(public) = &r.encryption_recipient {
            tx.execute("INSERT INTO archive_encryption(archive_id,format,recipient) VALUES (?1,'age-encryption.org/v1',?2)",params![id,public])?;
        }
        tx.commit()?;
    }
    let engine = e.clone();
    let task = id.clone();
    tokio::task::spawn_blocking(move || {
        let result = build(&engine, &task, &r);
        if let Err(error) = result
            && let Ok(c) = engine.db.connection()
        {
            let _ = c.execute(
                "UPDATE run_archives SET state='failed',error=?2,finished_at=?3 WHERE id=?1",
                params![task, error.to_string(), crate::db::now()],
            );
        }
    });
    get(
        e,
        &Id {
            project_id: run.project_id,
            archive_id: id,
        },
    )
}
fn add_file(
    source: &Path,
    stage: &Path,
    name: &str,
    m: &mut Manifest,
    bytes: &mut u64,
    e: &Engine,
) -> Result<()> {
    ensure!(!e.is_stopping(), invalid("server stopping"));
    relative(name)?;
    ensure!(
        m.entries.len() < MAX_ENTRIES,
        invalid("archive entry limit exceeded")
    );
    let meta = std::fs::symlink_metadata(source)?;
    ensure!(
        !meta.is_symlink(),
        invalid("archive refuses symbolic links")
    );
    let target = stage.join(name);
    if meta.is_dir() {
        std::fs::create_dir(&target)?;
        m.entries.insert(
            name.into(),
            Entry {
                kind: "directory".into(),
                size: 0,
                sha256: None,
                executable: false,
            },
        );
        ensure!(
            name.split('/').count() <= 64,
            invalid("archive directory depth exceeded")
        );
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let child = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("non UTF-8 archive filename"))?;
            add_file(
                &entry.path(),
                stage,
                &format!("{name}/{child}"),
                m,
                bytes,
                e,
            )?;
        }
    } else {
        ensure!(
            meta.is_file() && meta.len() <= MAX_BYTES.saturating_sub(*bytes),
            invalid("archive file/size limit exceeded")
        );
        let mut input = open_read(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            ensure!(size <= meta.len(), invalid("file changed during archive"));
            hash.update(&buf[..n]);
            output.write_all(&buf[..n])?;
        }
        ensure!(size == meta.len(), invalid("file changed during archive"));
        *bytes += size;
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        m.entries.insert(
            name.into(),
            Entry {
                kind: "file".into(),
                size,
                sha256: Some(crate::hashing::hex(hash.finalize())),
                executable,
            },
        );
    }
    Ok(())
}
fn generated(
    stage: &Path,
    name: &str,
    body: &[u8],
    m: &mut Manifest,
    bytes: &mut u64,
) -> Result<()> {
    ensure!(
        body.len() <= MAX_METADATA && body.len() as u64 <= MAX_BYTES.saturating_sub(*bytes),
        invalid("archive metadata exceeds limit")
    );
    std::fs::write(stage.join(name), body)?;
    *bytes += body.len() as u64;
    m.entries.insert(
        name.into(),
        Entry {
            kind: "file".into(),
            size: body.len() as u64,
            sha256: Some(crate::hashing::hex(Sha256::digest(body))),
            executable: false,
        },
    );
    Ok(())
}
fn build(e: &Engine, id: &str, r: &Create) -> Result<()> {
    e.db.connection()?
        .execute("UPDATE run_archives SET state='creating' WHERE id=?1", [id])?;
    let _lease =
        e.db.begin_operation(&r.run_id, "run_archive", BTreeSet::new())?;
    let root = archive_root(e)?;
    let stage = tempfile::Builder::new()
        .prefix(".snapshot-")
        .tempdir_in(&root)?;
    let run = e.db.run(&r.run_id)?;
    let workspace = e.config.data_dir.join("workspaces").join(&r.run_id);
    ensure!(
        workspace == run.workspace,
        invalid("noncanonical run workspace")
    );
    let mut m = Manifest {
        format: FORMAT.into(),
        project_id: r.project_id.clone(),
        run_id: r.run_id.clone(),
        created_at: crate::db::now(),
        components: json!({"data":true,"logs":r.logs,"artifacts":r.artifacts,"source":r.source,"environment":r.environment}),
        missing: Vec::new(),
        entries: BTreeMap::new(),
    };
    let mut bytes = 0;
    let mut tests = Vec::new();
    for s in &run.stages {
        tests.push(json!({"stage_id":s.definition.id,"kind":crate::testing::kind(&s.definition),"observed":crate::testing::observed(&e.db,&r.run_id,s)?}));
    }
    let acceptance: Option<String> =
        e.db.connection()?
            .query_row(
                "SELECT report_json FROM acceptance_results WHERE run_id=?1",
                [&r.run_id],
                |v| v.get(0),
            )
            .optional()?;
    let metadata = json!({"run":run,"inputs":e.db.inputs(&r.run_id)?,"toolchain":e.db.toolchain(&r.run_id)?,"artifacts":e.db.artifacts_report(&r.run_id)?,"tests":tests,"acceptance":acceptance.map(|s|serde_json::from_str::<Value>(&s)).transpose()?,"capture_host":{"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"execution":"local_native"},"schema_version":crate::migrations::VERSION});
    generated(
        stage.path(),
        "run.json",
        &serde_json::to_vec(&metadata)?,
        &mut m,
        &mut bytes,
    )?;
    if r.logs {
        let path = stage.path().join("logs.jsonl");
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut after = 0;
        let mut hash = Sha256::new();
        let mut size = 0;
        loop {
            ensure!(!e.is_stopping(), invalid("server stopping"));
            let page = e.db.logs(&r.run_id, after)?;
            for log in page.events {
                after = log.id;
                let test: Option<String> = e.db.connection()?.query_row(
                    "SELECT test_id FROM log_events WHERE id=?1",
                    [log.id],
                    |v| v.get(0),
                )?;
                let mut value = serde_json::to_value(log)?;
                value["test_id"] = json!(test);
                let mut line = serde_json::to_vec(&value)?;
                line.push(b'\n');
                ensure!(
                    line.len() as u64 <= MAX_BYTES.saturating_sub(bytes + size),
                    invalid("archive log size exceeded")
                );
                f.write_all(&line)?;
                hash.update(&line);
                size += line.len() as u64;
            }
            if page.next_cursor.is_none() {
                break;
            }
        }
        bytes += size;
        m.entries.insert(
            "logs.jsonl".into(),
            Entry {
                kind: "file".into(),
                size,
                sha256: Some(crate::hashing::hex(hash.finalize())),
                executable: false,
            },
        );
    }
    if workspace.try_exists()? {
        crate::storage_gc::directory(&workspace)?;
        std::fs::create_dir(stage.path().join("workspace"))?;
        m.entries.insert(
            "workspace".into(),
            Entry {
                kind: "directory".into(),
                size: 0,
                sha256: None,
                executable: false,
            },
        );
        for entry in std::fs::read_dir(&workspace)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("non UTF-8 workspace path"))?;
            let selected = match name.as_str() {
                "source" => r.source,
                "artifacts" => r.artifacts,
                _ => r.environment,
            };
            if selected {
                add_file(
                    &entry.path(),
                    stage.path(),
                    &format!("workspace/{name}"),
                    &mut m,
                    &mut bytes,
                    e,
                )?;
            }
        }
    }
    if r.source && !workspace.join("source").exists() {
        m.missing.push("source snapshot unavailable".into());
    }
    if r.environment {
        for s in &run.stages {
            if !workspace.join("stages").join(&s.definition.id).exists() {
                m.missing.push(format!(
                    "environment/stages/{} unavailable (remote or cleaned)",
                    s.definition.id
                ));
            }
        }
    }
    if r.artifacts {
        for a in e.db.artifacts(&r.run_id)? {
            let key = format!("workspace/artifacts/{}/{}", a.stage_id, a.name);
            if !m.entries.contains_key(&key) {
                m.missing.push(format!("{key} unavailable"));
            } else if !matches!(a.kind, crate::pipeline::ArtifactKind::Directory) {
                ensure!(
                    m.entries[&key].sha256.as_deref() == Some(a.sha256.as_str()),
                    invalid("artifact digest no longer matches run evidence")
                );
            }
        }
    }
    let manifest = serde_json::to_vec(&m)?;
    ensure!(
        manifest.len() <= MAX_METADATA,
        invalid("archive manifest too large")
    );
    let mut tmp = tempfile::NamedTempFile::new_in(&root)?;
    {
        let encoder = zstd::stream::write::Encoder::new(tmp.as_file_mut(), 3)?;
        let mut tar = tar::Builder::new(encoder);
        append(
            &mut tar,
            "manifest.json",
            &manifest[..],
            manifest.len() as u64,
            false,
            false,
        )?;
        for (name, item) in &m.entries {
            if item.kind == "directory" {
                append(&mut tar, name, std::io::empty(), 0, false, true)?;
            } else {
                let file = open_read(&stage.path().join(name))?;
                append(&mut tar, name, file, item.size, item.executable, false)?;
            }
        }
        tar.into_inner()?.finish()?;
    }
    tmp.as_file().sync_all()?;
    let sha = file_hash(tmp.as_file_mut())?;
    let size = tmp.as_file().metadata()?.len();
    let mut read = open_read(tmp.path())?;
    inspect(&mut read, None)?;
    let (tmp, sha, size) = if let Some(public) = &r.encryption_recipient {
        let mut encrypted = tempfile::NamedTempFile::new_in(&root)?;
        crate::archive_crypto::seal_stream(
            tmp.as_file_mut(),
            encrypted.as_file_mut(),
            public,
            MAX_BYTES,
        )?;
        encrypted.as_file().sync_all()?;
        let size = encrypted.as_file().metadata()?.len();
        ensure!(
            size <= MAX_BYTES,
            invalid("encrypted archive exceeds 1 GiB")
        );
        let sha = file_hash(encrypted.as_file_mut())?;
        (encrypted, sha, size)
    } else {
        (tmp, sha, size)
    };
    tmp.persist_noclobber(root.join(format!("{id}.tar.zst")))
        .map_err(|v| v.error)?;
    // Ready means the independent file is published and original run payloads are no longer read.
    drop(read);
    drop(stage);
    drop(_lease);
    e.db.connection()?.execute("UPDATE run_archives SET state='ready',sha256=?2,file_bytes=?3,manifest_json=?4,finished_at=?5 WHERE id=?1",params![id,sha,size,String::from_utf8(manifest)?,crate::db::now()])?;
    Ok(())
}
fn append<W: Write, R: Read>(
    tar: &mut tar::Builder<W>,
    name: &str,
    reader: R,
    size: u64,
    executable: bool,
    directory: bool,
) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(size);
    h.set_mode(if directory || executable {
        0o700
    } else {
        0o600
    });
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h.set_entry_type(if directory {
        tar::EntryType::Directory
    } else {
        tar::EntryType::Regular
    });
    h.set_cksum();
    tar.append_data(&mut h, name, reader)?;
    Ok(())
}
fn inspect(file: &mut File, destination: Option<&Path>) -> Result<Manifest> {
    file.seek(SeekFrom::Start(0))?;
    let mut decoder = zstd::stream::read::Decoder::new(file)?;
    decoder.window_log_max(23)?;
    let bounded = decoder.take(MAX_BYTES + MAX_METADATA as u64 + MAX_ENTRIES as u64 * 8192);
    let mut tar = tar::Archive::new(bounded);
    let mut entries = tar.entries()?;
    let mut first = entries.next().context("archive is empty")??;
    ensure!(
        first.path()?.as_ref() == Path::new("manifest.json")
            && first.header().entry_type().is_file()
            && first.size() <= MAX_METADATA as u64,
        invalid("manifest must be first regular entry")
    );
    let mut body = Vec::new();
    first.read_to_end(&mut body)?;
    let manifest: Manifest = serde_json::from_slice(&body)?;
    drop(first);
    ensure!(
        manifest.format == FORMAT && manifest.entries.len() <= MAX_ENTRIES,
        invalid("unsupported archive manifest")
    );
    let mut total = 0u64;
    let mut names = BTreeSet::new();
    for (name, item) in &manifest.entries {
        relative(name)?;
        ensure!(
            !matches!(name.as_str(), "manifest.json" | "restore-audit.json"),
            invalid("reserved archive path")
        );
        ensure!(
            names.insert(name.to_lowercase()),
            invalid("case-ambiguous archive paths")
        );
        total = total
            .checked_add(item.size)
            .context("archive size overflow")?;
        ensure!(
            total <= MAX_BYTES && matches!(item.kind.as_str(), "file" | "directory"),
            invalid("invalid archive entry")
        );
    }
    let mut seen = BTreeSet::new();
    for entry in entries {
        let mut entry = entry?;
        let name = entry
            .path()?
            .to_str()
            .context("archive path not UTF-8")?
            .trim_end_matches('/')
            .to_string();
        relative(&name)?;
        if let Some(parent) = name.rsplit_once('/').map(|v| v.0) {
            ensure!(
                seen.contains(parent)
                    && manifest
                        .entries
                        .get(parent)
                        .is_some_and(|v| v.kind == "directory"),
                invalid("archive parent directory must precede its children")
            );
        }
        ensure!(seen.insert(name.clone()), invalid("duplicate archive path"));
        let expected = manifest
            .entries
            .get(&name)
            .context("entry missing from manifest")?;
        ensure!(
            entry.size() == expected.size,
            invalid("archive size mismatch")
        );
        if expected.kind == "directory" {
            ensure!(
                entry.header().entry_type().is_dir()
                    && expected.size == 0
                    && expected.sha256.is_none(),
                invalid("invalid archive directory")
            );
            if let Some(root) = destination {
                std::fs::create_dir(root.join(&name))?;
            }
            continue;
        }
        ensure!(
            entry.header().entry_type().is_file(),
            invalid("archive refuses links and special entries")
        );
        let mut output = if let Some(root) = destination {
            Some(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(root.join(&name))?,
            )
        } else {
            None
        };
        let mut hash = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            if let Some(f) = &mut output {
                f.write_all(&buf[..n])?;
            }
        }
        ensure!(
            expected.sha256.as_deref() == Some(crate::hashing::hex(hash.finalize()).as_str()),
            invalid("archive entry digest mismatch")
        );
        #[cfg(unix)]
        if let Some(root) = destination {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join(&name),
                std::fs::Permissions::from_mode(if expected.executable { 0o700 } else { 0o600 }),
            )?;
        }
    }
    ensure!(
        seen.len() == manifest.entries.len(),
        invalid("archive entries missing")
    );
    // Consume the decoder to validate the zstd frame checksum and bounded trailing padding.
    let mut tail = Vec::new();
    tar.into_inner().take(4097).read_to_end(&mut tail)?;
    ensure!(
        tail.len() <= 4096 && tail.iter().all(|v| *v == 0),
        invalid("unexpected archive trailing data")
    );
    Ok(manifest)
}
pub(crate) fn download_file(e: &Engine, r: &Id) -> Result<File> {
    let v = row(e, r)?;
    ensure!(v["state"] == "ready", invalid("archive is not ready"));
    let root = archive_root(e)?;
    let mut file = open_read(&root.join(format!("{}.tar.zst", r.archive_id)))?;
    fs2::FileExt::try_lock_shared(&file).map_err(|_| Fault::Busy)?;
    ensure!(
        row(e, r)?["state"] == "ready",
        invalid("archive has expired")
    );
    ensure!(
        Some(file.metadata()?.len()) == v["file_bytes"].as_u64(),
        invalid("archive length mismatch")
    );
    ensure!(
        Some(file_hash(&mut file)?.as_str()) == v["sha256"].as_str(),
        invalid("archive digest mismatch")
    );
    Ok(file)
}
pub async fn verify(e: Arc<Engine>, r: Id) -> Result<Value> {
    verify_unlocked(
        e,
        Unlock {
            id: r,
            identity_file_env: None,
        },
    )
    .await
}
pub async fn verify_unlocked(e: Arc<Engine>, r: Unlock) -> Result<Value> {
    let permit = e
        .transfer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Fault::Busy)?;
    tokio::task::spawn_blocking(move||{
        let _permit=permit;let mut file=download_file(&e,&r.id)?;let mut plain=decode(&mut file,r.identity_file_env.as_deref(),&archive_root(&e)?,MAX_BYTES)?;
        let manifest=inspect(plain.as_mut().map(|v|v.as_file_mut()).unwrap_or(&mut file),None)?;
        ensure!(matches_owner(&e,&r.id,&manifest)?,invalid("archive ownership mismatch"));
        Ok(json!({"archive_id":r.id.archive_id,"verified":true,"entries":manifest.entries.len(),"missing":manifest.missing,"format":FORMAT,"encrypted":plain.is_some()}))
    }).await?
}
pub async fn restore(e: Arc<Engine>, r: Restore) -> Result<Value> {
    ensure!(
        !r.request_id.is_empty()
            && r.request_id.len() <= 128
            && !r.reason.trim().is_empty()
            && r.reason.len() <= 512,
        invalid("request_id and reason required")
    );
    let permit = e
        .transfer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Fault::Busy)?;
    tokio::task::spawn_blocking(move||{
 let _permit=permit;
 let id=Id{project_id:r.project_id.clone(),archive_id:r.archive_id.clone()};let mut file=download_file(&e,&id)?;let root=archive_root(&e)?;let restored=root.join("restored");private_dir(&restored)?;let parent=restored.join(&r.archive_id);private_dir(&parent)?;
 let digest=crate::hashing::hex(Sha256::digest(r.request_id.as_bytes()));let target=parent.join(digest);
 ensure!(!target.try_exists()?,invalid("restore request already used; existing restoration is never overwritten"));let tmp=tempfile::Builder::new().prefix(".restore-").tempdir_in(&parent)?;let mut plain=decode(&mut file,r.identity_file_env.as_deref(),&root,MAX_BYTES)?;let m=inspect(plain.as_mut().map(|v|v.as_file_mut()).unwrap_or(&mut file),Some(tmp.path()))?;ensure!(matches_owner(&e,&id,&m)?,invalid("archive ownership mismatch"));std::fs::write(tmp.path().join("manifest.json"),serde_json::to_vec_pretty(&m)?)?;
 std::fs::write(tmp.path().join("restore-audit.json"),serde_json::to_vec_pretty(&json!({"project_id":r.project_id,"archive_id":r.archive_id,"request_id":r.request_id,"actor":r.actor,"reason":r.reason,"restored_at":crate::db::now()}))?)?;
 std::fs::rename(tmp.path(),&target)?;Ok(json!({"archive_id":r.archive_id,"restored":true,"path":target,"entries":m.entries.len(),"missing":m.missing,"effect":"isolated files only; live database, run history, cache and project remain unchanged"}))
 }).await?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_encryption_preserves_legacy_requests_and_rejects_secret_fields() {
        let body = json!({"project_id":"p","request_id":"r","sha256":"0".repeat(64),"actor":"user","reason":"archive import"});
        let request: Import = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), body);
        let unlocked:Unlock=serde_json::from_value(json!({"project_id":"p","archive_id":"a","identity_file_env":"PREFLIGHT_ARCHIVE_IDENTITY_FILE_TEST"})).unwrap();
        assert!(unlocked.identity_file_env.is_some());
        assert!(serde_json::from_value::<Unlock>(json!({"project_id":"p","archive_id":"a","private_identity":"never accept key material"})).is_err());
    }
    fn hostile(name: &str, link: bool) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let m = Manifest {
            format: FORMAT.into(),
            project_id: "p".into(),
            run_id: "r".into(),
            created_at: 0,
            components: json!({}),
            missing: vec![],
            entries: BTreeMap::from([(
                name.into(),
                Entry {
                    kind: "file".into(),
                    size: 0,
                    sha256: Some(crate::hashing::hex(Sha256::digest([]))),
                    executable: false,
                },
            )]),
        };
        {
            let encoder = zstd::stream::write::Encoder::new(f.as_file_mut(), 3).unwrap();
            let mut tar = tar::Builder::new(encoder);
            let data = serde_json::to_vec(&m).unwrap();
            append(
                &mut tar,
                "manifest.json",
                &data[..],
                data.len() as u64,
                false,
                false,
            )
            .unwrap();
            if link {
                let mut header = tar::Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o600);
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name("/etc/passwd").unwrap();
                header.set_cksum();
                tar.append_data(&mut header, "run.json", std::io::empty())
                    .unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        f
    }
    #[test]
    fn verification_refuses_children_before_their_directory() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let manifest = Manifest {
            format: FORMAT.into(),
            project_id: "p".into(),
            run_id: "r".into(),
            created_at: 0,
            components: json!({}),
            missing: vec![],
            entries: BTreeMap::from([
                (
                    "parent".into(),
                    Entry {
                        kind: "directory".into(),
                        size: 0,
                        sha256: None,
                        executable: false,
                    },
                ),
                (
                    "parent/child".into(),
                    Entry {
                        kind: "file".into(),
                        size: 0,
                        sha256: Some(crate::hashing::hex(Sha256::digest([]))),
                        executable: false,
                    },
                ),
            ]),
        };
        let encoder = zstd::stream::write::Encoder::new(file.as_file_mut(), 3).unwrap();
        let mut tar = tar::Builder::new(encoder);
        let json = serde_json::to_vec(&manifest).unwrap();
        append(
            &mut tar,
            "manifest.json",
            &json[..],
            json.len() as u64,
            false,
            false,
        )
        .unwrap();
        append(&mut tar, "parent/child", std::io::empty(), 0, false, false).unwrap();
        append(&mut tar, "parent", std::io::empty(), 0, true, false).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let error = inspect(file.as_file_mut(), None).unwrap_err().to_string();
        assert!(error.contains("parent directory"), "{error}");
    }
    #[test]
    fn extraction_rejects_traversal_reserved_names_and_link_entries() {
        for (name, link) in [
            ("../escape", false),
            ("manifest.json", false),
            ("run.json", true),
        ] {
            let mut file = hostile(name, link);
            let dest = tempfile::tempdir().unwrap();
            assert!(inspect(file.as_file_mut(), Some(dest.path())).is_err());
            assert!(!dest.path().join("run.json").exists());
        }
    }
}
