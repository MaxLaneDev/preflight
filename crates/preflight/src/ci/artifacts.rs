use super::{
    http::{FetchError, Http},
    providers::RemoteArtifact,
    store, *,
};
use crate::model::{Artifact, TestReport};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
const LIMIT: usize = 32 * 1024 * 1024;
pub(super) async fn download(
    e: &Engine,
    s: &Source,
    h: &mut Http,
    run: &str,
    a: &RemoteArtifact,
) -> Result<PathBuf> {
    e.db.require_artifact_available(run, &a.stage)?;
    let old:Option<(String,String,u64,String)>=e.db.connection()?.query_row("SELECT path,sha256,size,state FROM ci_artifacts WHERE run_id=?1 AND artifact_id=?2 AND state IN ('downloaded','publishing') AND path IS NOT NULL AND sha256 IS NOT NULL",params![run,a.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((path, sha, size, _)) = &old {
        let path = PathBuf::from(path);
        checked(e, &path)?;
        if path.try_exists()? {
            let bytes = read(&path)?;
            ensure!(
                bytes.len() as u64 == *size && crate::hashing::hex(Sha256::digest(&bytes)) == *sha,
                "stored CI artifact changed; original registration retained"
            );
            commit(e, run, a, &path, sha, *size)?;
            return Ok(path);
        }
    }
    ensure!(!a.expired, "provider artifact expired; metadata retained");
    let url = match s.provider {
        Provider::Github => {
            super::providers::number(&a.id)?;
            h.github(&["actions", "artifacts", &a.id, "zip"])?
        }
        Provider::Gitlab => {
            let job =
                a.id.strip_prefix("job-")
                    .ok_or_else(|| invalid("invalid GitLab artifact ID"))?;
            super::providers::number(job)?;
            h.gitlab(&["jobs", job, "artifacts"])?
        }
    };
    let body = match h.get(url, LIMIT, None, None).await {
        Ok(body) => body,
        Err(error) => {
            if matches!(
                error.downcast_ref::<FetchError>(),
                Some(FetchError::Http(404) | FetchError::Http(410))
            ) {
                e.db.connection()?.execute("UPDATE ci_artifacts SET state='provider_expired' WHERE run_id=?1 AND artifact_id=?2 AND state NOT IN ('downloaded','publishing')",params![run,a.id])?;
            }
            return Err(error);
        }
    };
    ensure!(body.complete, FetchError::BodyLimit);
    let sha = crate::hashing::hex(Sha256::digest(&body.bytes));
    if let Some(digest) = a.metadata["digest"]
        .as_str()
        .and_then(|v| v.strip_prefix("sha256:"))
    {
        ensure!(
            digest.eq_ignore_ascii_case(&sha),
            "provider artifact digest mismatch; no bytes published"
        )
    }
    if let Some((_, expected, size, _)) = &old {
        ensure!(
            *expected == sha && *size == body.bytes.len() as u64,
            "provider artifact changed during publication recovery"
        )
    }
    e.current_config().check_disk_admission().await?;
    let relative = PathBuf::from("workspaces")
        .join(run)
        .join("artifacts")
        .join(&a.stage);
    let mut parent = e.config.data_dir.clone();
    crate::storage_gc::directory(&parent)?;
    for part in relative.components() {
        let std::path::Component::Normal(part) = part else {
            return Err(invalid("invalid CI artifact path"));
        };
        parent.push(part);
        match std::fs::create_dir(&parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        };
        crate::storage_gc::directory(&parent)?;
    }
    let target = parent.join(format!("ci-{}.zip", a.id));
    checked(e, &target)?;
    ensure!(
        !target.try_exists()?,
        "unregistered CI artifact path already exists; no overwrite"
    );
    // Durable identity precedes atomic file publication; a crash can resume without trusting an orphan.
    e.db.connection()?.execute("UPDATE ci_artifacts SET state='publishing',sha256=?3,path=?4,size=?5 WHERE run_id=?1 AND artifact_id=?2",params![run,a.id,sha,target.to_string_lossy(),body.bytes.len()])?;
    let mut temp = tempfile::NamedTempFile::new_in(&parent)?;
    std::io::Write::write_all(&mut temp, &body.bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&target)
        .map_err(|_| invalid("CI artifact publication failed; no file overwritten"))?;
    #[cfg(unix)]
    std::fs::File::open(&parent)?.sync_all()?;
    commit(e, run, a, &target, &sha, body.bytes.len() as u64)?;
    Ok(target)
}
fn checked(e: &Engine, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(&e.config.data_dir)
        .map_err(|_| invalid("CI artifact path escapes storage"))?;
    crate::pipeline::contained(
        &e.config.data_dir,
        relative
            .to_str()
            .ok_or_else(|| invalid("CI artifact path is not UTF-8"))?,
    )?;
    Ok(())
}
fn commit(
    e: &Engine,
    run: &str,
    a: &RemoteArtifact,
    path: &Path,
    sha: &str,
    size: u64,
) -> Result<()> {
    let mut c = e.db.connection()?;
    let tx = c.transaction()?;
    ensure!(
        !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifact_expiry WHERE run_id=?1 AND stage_id=?2)",
            params![run, a.stage],
            |r| r.get::<_, bool>(0)
        )?,
        invalid("artifact_expired: payload cannot be silently repopulated")
    );
    let name = format!("ci-{}", a.id);
    let existing: Option<(String, String, u64)> = tx
        .query_row(
            "SELECT path,sha256,size FROM artifacts WHERE run_id=?1 AND stage_id=?2 AND name=?3",
            params![run, a.stage, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((original, digest, bytes)) = existing {
        ensure!(
            Path::new(&original) == path && digest == sha && bytes == size,
            Fault::IdempotencyConflict
        );
    }
    tx.execute("INSERT OR IGNORE INTO artifacts(run_id,stage_id,name,kind,sha256,size,path) VALUES (?1,?2,?3,json_quote('file'),?4,?5,?6)",params![run,a.stage,name,sha,size,path.to_string_lossy()])?;
    tx.execute("UPDATE ci_artifacts SET state='downloaded',sha256=?3,path=?4,size=?5 WHERE run_id=?1 AND artifact_id=?2",params![run,a.id,sha,path.to_string_lossy(),size])?;
    tx.commit()?;
    Ok(())
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && !m.is_symlink() && m.len() <= LIMIT as u64,
        "CI artifact is not a bounded regular file"
    );
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut data)?;
    ensure!(data.len() <= LIMIT, "CI artifact exceeds 32 MiB");
    Ok(data)
}
pub(super) fn junit(h: &Http, path: &Path, artifact: &str) -> Result<Vec<TestReport>> {
    let bytes = read(path)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| invalid("invalid CI ZIP archive"))?;
    ensure!(archive.len() <= 20000, "CI archive exceeds 20000 entries");
    let mut reports = Vec::new();
    let mut total = 0usize;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|_| invalid("unsupported CI ZIP entry"))?;
        ensure!(
            file.enclosed_name().is_some(),
            "CI ZIP entry escapes archive root"
        );
        ensure!(
            file.unix_mode().is_none_or(|m| m & 0o170000 != 0o120000),
            "CI ZIP symlink is not a test report"
        );
        if file.is_dir() || !file.name()?.to_ascii_lowercase().ends_with(".xml") {
            continue;
        }
        ensure!(
            reports.len() < 100 && file.size() <= 4 * 1024 * 1024,
            "CI JUnit entry or report count exceeds limit"
        );
        let mut data = Vec::new();
        file.by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|_| invalid("CI ZIP decompression failed"))?;
        total += data.len();
        ensure!(
            data.len() <= 4 * 1024 * 1024 && total <= 16 * 1024 * 1024,
            "CI report decompressed bytes exceed limit"
        );
        let xml =
            std::str::from_utf8(&data).map_err(|_| invalid("CI JUnit report is not UTF-8"))?;
        let mut report = crate::junit::parse(xml)?;
        for case in &mut report.cases {
            case.id = format!(
                "{}:{}",
                crate::hashing::hex(Sha256::digest(serde_json::to_vec(&(
                    artifact,
                    file.name()?
                ))?)),
                case.id
            );
            case.name = h.redact(&case.name);
            case.class_name = h.redact(&case.class_name);
            case.suite = h.redact(&case.suite);
            case.message = h.redact(&case.message);
            case.file = case.file.as_ref().map(|v| h.redact(v));
            if let Some(output) = &mut case.output {
                output.stdout = h.redact(&output.stdout);
                output.stderr = h.redact(&output.stderr)
            }
        }
        for failure in &mut report.failures {
            failure.name = h.redact(&failure.name);
            failure.class_name = h.redact(&failure.class_name);
            failure.message = h.redact(&failure.message)
        }
        reports.push(report);
    }
    ensure!(
        !reports.is_empty(),
        "selected CI test artifact contains no XML reports"
    );
    Ok(reports)
}
pub(super) async fn fetch(
    e: &Engine,
    s: &Source,
    h: &mut Http,
    r: &FetchArtifact,
) -> Result<Value> {
    let run = e.db.run(&r.run_id)?;
    ensure!(
        run.project_id == r.project_id && run.project_id == s.project_id,
        Fault::NotFound
    );
    let _lease = store::lease(e, &r.run_id)?;
    let artifact = store::selected_artifacts(e, &r.run_id)?
        .into_iter()
        .find(|a| a.id == r.artifact_id)
        .ok_or(Fault::NotFound)?;
    let path = download(e, s, h, &r.run_id, &artifact).await?;
    let files = e.db.artifacts(&r.run_id)?;
    let record: &Artifact = files
        .iter()
        .find(|a| a.path == path)
        .ok_or(Fault::NotFound)?;
    Ok(
        json!({"run_id":r.run_id,"artifact_id":r.artifact_id,"sha256":record.sha256,"bytes":record.size,"path":record.path,"partial":false,"provider_mutations":0}),
    )
}
pub fn file(e: &Engine, r: &ArtifactFile) -> Result<(Vec<u8>, String)> {
    let run = e.db.run(&r.run_id)?;
    ensure!(run.project_id == r.project_id, Fault::NotFound);
    let _lease = store::lease(e, &r.run_id)?;
    let c = e.db.connection()?;
    let(stage,path,sha):(String,String,String)=c.query_row("SELECT stage_id,path,sha256 FROM ci_artifacts WHERE run_id=?1 AND artifact_id=?2 AND state='downloaded'",params![r.run_id,r.artifact_id],|v|Ok((v.get(0)?,v.get(1)?,v.get(2)?))).optional()?.ok_or(Fault::NotFound)?;
    drop(c);
    e.db.require_artifact_available(&r.run_id, &stage)?;
    let path = PathBuf::from(path);
    let relative = path
        .strip_prefix(&e.config.data_dir)
        .map_err(|_| invalid("CI artifact path escapes storage"))?;
    crate::pipeline::contained(
        &e.config.data_dir,
        relative
            .to_str()
            .ok_or_else(|| invalid("CI artifact path is not UTF-8"))?,
    )?;
    let bytes = read(&path)?;
    ensure!(
        crate::hashing::hex(Sha256::digest(&bytes)) == sha,
        "CI artifact digest mismatch"
    );
    Ok((bytes, sha))
}
