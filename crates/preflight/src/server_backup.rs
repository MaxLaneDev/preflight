//! Offline SQLite + owned payload snapshot; restoration is isolated and never activates itself.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions as FsOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

pub const FORMAT: &str = "preflight-server-backup-v1";
const MAX_BYTES: u64 = 100 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_MANIFEST: u64 = 16 * 1024 * 1024;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    bytes: u64,
    sha256: String,
    executable: bool,
    directory: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    schema: usize,
    original_data_dir: PathBuf,
    created_at: i64,
    project_roots: BTreeMap<String, PathBuf>,
    entries: BTreeMap<String, Entry>,
}
fn private_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 4096
        && !name.contains('\\')
        && name.split('/').all(crate::trees::portable_component)
        && Path::new(name)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn check_db(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "backup SQLite integrity check failed");
    ensure!(
        db.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_none(),
        "backup SQLite foreign keys are inconsistent"
    );
    crate::migrations::verify_backup_schema(&db)?;
    Ok(db)
}
fn quiescent(db: &Connection) -> Result<()> {
    for (name, sql) in [
        (
            "runs",
            "SELECT COUNT(*) FROM runs WHERE finished_at IS NULL",
        ),
        (
            "leases",
            "SELECT COUNT(*) FROM operation_leases WHERE state IN ('active','abandoned')",
        ),
        (
            "queues",
            "SELECT COUNT(*) FROM change_queues WHERE state IN ('active','cancelling')",
        ),
        (
            "workers",
            "SELECT COUNT(*) FROM worker_jobs WHERE state IN ('queued','leased','cancelling')",
        ),
        (
            "connections",
            "SELECT COUNT(*) FROM worker_connections WHERE state IN ('queued','starting','connected','stopping','interrupted')",
        ),
        (
            "archives",
            "SELECT COUNT(*) FROM run_archives WHERE state IN ('queued','creating')",
        ),
        (
            "archive cleanup",
            "SELECT COUNT(*) FROM archive_gc_items WHERE state='deleting'",
        ),
        (
            "storage cleanup",
            "SELECT COUNT(*) FROM storage_gc_items WHERE state IN ('moving','quarantined')",
        ),
        (
            "CI",
            "SELECT COUNT(*) FROM ci_syncs WHERE state IN ('queued','running')",
        ),
        (
            "schedules",
            "SELECT COUNT(*) FROM schedules WHERE state='dispatching'",
        ),
        (
            "hooks",
            "SELECT COUNT(*) FROM codex_bindings WHERE state='sending'",
        ),
    ] {
        let count: i64 = db.query_row(sql, [], |r| r.get(0))?;
        ensure!(
            count == 0,
            "backup refused: unresolved {name}; recover/settle the stopped instance first"
        );
    }
    Ok(())
}
fn visit(
    source: &Path,
    name: &str,
    entries: &mut BTreeMap<String, Entry>,
    files: &mut BTreeMap<String, PathBuf>,
    total: &mut u64,
) -> Result<()> {
    ensure!(safe_name(name), "nonportable backup path");
    let metadata = std::fs::symlink_metadata(source)?;
    ensure!(!metadata.is_symlink(), "backup refuses symbolic links");
    ensure!(entries.len() < MAX_ENTRIES, "backup entry limit exceeded");
    if metadata.is_dir() {
        ensure!(
            entries
                .insert(
                    name.into(),
                    Entry {
                        bytes: 0,
                        sha256: crate::hashing::hex(Sha256::digest([])),
                        executable: false,
                        directory: true
                    }
                )
                .is_none(),
            "duplicate backup directory"
        );
        for child in std::fs::read_dir(source)? {
            let child = child?;
            let leaf = child
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non UTF-8 backup path"))?;
            visit(
                &child.path(),
                &format!("{name}/{leaf}"),
                entries,
                files,
                total,
            )?;
        }
    } else {
        ensure!(
            metadata.is_file() && entries.len() < MAX_ENTRIES,
            "backup file type/count limit exceeded"
        );
        *total = total
            .checked_add(metadata.len())
            .context("backup size overflow")?;
        ensure!(*total <= MAX_BYTES, "backup exceeds 100 GiB");
        let mut file = crate::archives::open_read(source)?;
        let sha256 = crate::archives::file_hash(&mut file)?;
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        ensure!(
            entries
                .insert(
                    name.into(),
                    Entry {
                        bytes: metadata.len(),
                        sha256,
                        executable,
                        directory: false,
                    }
                )
                .is_none(),
            "duplicate backup path"
        );
        files.insert(name.into(), source.into());
    }
    Ok(())
}
fn append<W: Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    size: u64,
    executable: bool,
    input: impl Read,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(if executable { 0o700 } else { 0o600 });
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    tar.append_data(&mut header, name, input)?;
    Ok(())
}
pub fn create(config_file: &Path, target: &Path) -> Result<Value> {
    let mut config = crate::config::Config::load(config_file)?;
    config.data_dir = config.data_dir.canonicalize()?;
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    ensure!(
        !parent.starts_with(&config.data_dir),
        "backup must be outside the server data directory"
    );
    ensure!(!target.try_exists()?, "backup destination already exists");
    let lock_path = config.data_dir.join("server.lock");
    let _check = crate::archives::open_read(&lock_path)?;
    let mut lock_options = FsOptions::new();
    lock_options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        lock_options.custom_flags(libc::O_NOFOLLOW);
    }
    let lock = lock_options.open(lock_path)?;
    lock.try_lock_exclusive()
        .context("server is running; offline backup never stops it")?;
    let db = check_db(&config.data_dir.join("preflight.sqlite3"))?;
    quiescent(&db)?;
    let snapshot = tempfile::tempdir_in(&parent)?;
    let database = snapshot.path().join("preflight.sqlite3");
    db.execute(
        "VACUUM INTO ?1",
        [database.to_str().context("database path must be UTF-8")?],
    )?;
    check_db(&database)?;
    let mut manifest = Manifest {
        format: FORMAT.into(),
        schema: crate::migrations::verify_backup_schema(&db)?,
        original_data_dir: config.data_dir.clone(),
        created_at: crate::db::now(),
        project_roots: BTreeMap::new(),
        entries: BTreeMap::new(),
    };
    let mut files = BTreeMap::new();
    let mut total = 0;
    visit(
        &database,
        "data/preflight.sqlite3",
        &mut manifest.entries,
        &mut files,
        &mut total,
    )?;
    for entry in std::fs::read_dir(&config.data_dir)? {
        let entry = entry?;
        let leaf = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non UTF-8 data path"))?;
        if matches!(
            leaf.as_str(),
            "server.lock" | "preflight.sqlite3" | "preflight.sqlite3-wal" | "preflight.sqlite3-shm"
        ) {
            continue;
        }
        visit(
            &entry.path(),
            &format!("data/{leaf}"),
            &mut manifest.entries,
            &mut files,
            &mut total,
        )?;
    }
    let configuration = snapshot.path().join("server-config.toml");
    std::fs::write(&configuration, toml::to_string(&config)?)?;
    visit(
        &configuration,
        "server-config.toml",
        &mut manifest.entries,
        &mut files,
        &mut total,
    )?;
    let mut projects = db.prepare("SELECT id,root FROM projects ORDER BY id")?;
    for row in projects.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            PathBuf::from(r.get::<_, String>(1)?),
        ))
    })? {
        let (id, root) = row?;
        uuid::Uuid::parse_str(&id)?;
        manifest.project_roots.insert(id.clone(), root.clone());
        let configuration = root.join(".preflight");
        if configuration.try_exists()? {
            ensure!(
                !parent.starts_with(configuration.canonicalize()?),
                "backup destination must be outside project configuration directories"
            );
            visit(
                &configuration,
                &format!("project-configs/{id}/.preflight"),
                &mut manifest.entries,
                &mut files,
                &mut total,
            )?;
        }
    }
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    ensure!(
        manifest_bytes.len() as u64 <= MAX_MANIFEST,
        "backup manifest exceeds limit"
    );
    let mut output = tempfile::NamedTempFile::new_in(&parent)?;
    {
        let mut tar =
            tar::Builder::new(zstd::stream::write::Encoder::new(output.as_file_mut(), 3)?);
        append(
            &mut tar,
            "manifest.json",
            manifest_bytes.len() as u64,
            false,
            &manifest_bytes[..],
        )?;
        for (name, entry) in &manifest.entries {
            if entry.directory {
                let mut header = tar::Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o700);
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mtime(0);
                header.set_cksum();
                tar.append_data(&mut header, name, std::io::empty())?;
                continue;
            }
            append(
                &mut tar,
                name,
                entry.bytes,
                entry.executable,
                crate::archives::open_read(&files[name])?,
            )?;
        }
        tar.into_inner()?.finish()?;
    }
    output.as_file().sync_all()?;
    let digest = crate::archives::file_hash(output.as_file_mut())?;
    // Re-read the published bytes, so source mutation cannot create a silently inconsistent backup.
    inspect(output.as_file_mut(), None)?;
    let file_bytes = output.as_file().metadata()?.len();
    output.persist_noclobber(target).map_err(|v| v.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(
        json!({"format":FORMAT,"path":target,"sha256":digest,"file_bytes":file_bytes,"entries":manifest.entries.len(),"original_data_dir":manifest.original_data_dir,"project_configs":manifest.project_roots.len(),"encrypted":false,"effect":"offline consistent SQLite snapshot and owned data; external project source/toolchains/SDKs are not copied"}),
    )
}
fn inspect(file: &mut File, destination: Option<&Path>) -> Result<Manifest> {
    file.seek(SeekFrom::Start(0))?;
    let mut decoder = zstd::stream::read::Decoder::new(file)?;
    decoder.window_log_max(23)?;
    let mut archive =
        tar::Archive::new(decoder.take(MAX_BYTES + MAX_MANIFEST + MAX_ENTRIES as u64 * 8192));
    let mut entries = archive.entries()?;
    let mut first = entries.next().context("empty server backup")??;
    ensure!(
        first.path()?.as_ref() == Path::new("manifest.json")
            && first.header().entry_type().is_file()
            && first.size() <= MAX_MANIFEST,
        "backup manifest must be first regular entry"
    );
    let manifest: Manifest = serde_json::from_reader((&mut first).take(MAX_MANIFEST + 1))?;
    drop(first);
    ensure!(
        manifest.format == FORMAT
            && (51..=crate::migrations::VERSION).contains(&manifest.schema)
            && manifest.entries.len() <= MAX_ENTRIES
            && manifest.original_data_dir.is_absolute(),
        "unsupported backup format/schema"
    );
    ensure!(
        manifest.entries.contains_key("data/preflight.sqlite3")
            && manifest.entries.contains_key("server-config.toml"),
        "incomplete server backup"
    );
    for (id, root) in &manifest.project_roots {
        uuid::Uuid::parse_str(id)?;
        ensure!(root.is_absolute(), "invalid project root");
    }
    let mut total = 0u64;
    for (name, expected) in &manifest.entries {
        ensure!(
            safe_name(name)
                && (name.starts_with("data/")
                    || name.starts_with("project-configs/")
                    || name == "server-config.toml"),
            "unsafe backup entry"
        );
        ensure!(
            expected.sha256.len() == 64
                && expected
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid backup digest"
        );
        total = total
            .checked_add(expected.bytes)
            .context("backup size overflow")?;
        ensure!(total <= MAX_BYTES, "backup exceeds 100 GiB");
        let mut entry = entries.next().context("missing backup entry")??;
        ensure!(
            (if expected.directory {
                entry.header().entry_type().is_dir()
            } else {
                entry.header().entry_type().is_file()
            }) && entry.path()?.as_ref() == Path::new(name)
                && entry.size() == expected.bytes,
            "backup entry differs from manifest"
        );
        if expected.directory {
            ensure!(
                expected.bytes == 0 && expected.sha256 == crate::hashing::hex(Sha256::digest([])),
                "invalid backup directory"
            );
            if let Some(root) = destination {
                std::fs::create_dir_all(root.join(name))?;
            }
            continue;
        }
        let mut hash = Sha256::new();
        let mut bytes = 0;
        let mut target =
            if let Some(root) = destination {
                let target = root.join(name);
                std::fs::create_dir_all(target.parent().unwrap())?;
                let file = FsOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(
                        if expected.executable { 0o700 } else { 0o600 },
                    ))?;
                }
                Some(file)
            } else {
                None
            };
        let mut buffer = [0u8; 65536];
        loop {
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            hash.update(&buffer[..n]);
            if let Some(output) = target.as_mut() {
                output.write_all(&buffer[..n])?;
            }
        }
        ensure!(
            bytes == expected.bytes && crate::hashing::hex(hash.finalize()) == expected.sha256,
            "backup payload digest mismatch"
        );
        if let Some(output) = target {
            output.sync_all()?;
        }
    }
    ensure!(entries.next().is_none(), "unexpected backup entry");
    if let Some(root) = destination {
        let database = check_db(&root.join("data/preflight.sqlite3"))?;
        ensure!(
            crate::migrations::verify_backup_schema(&database)? == manifest.schema,
            "backup database schema differs from manifest"
        );
    }
    Ok(manifest)
}
pub fn verify(source: &Path, expected_sha256: &str) -> Result<Value> {
    let scratch = tempfile::tempdir()?;
    let mut file = crate::archive_crypto::verified_input(
        source,
        scratch.path(),
        expected_sha256,
        MAX_BYTES + 1024 * 1024 * 1024,
    )?;
    let m = inspect(file.as_file_mut(), Some(scratch.path()))?;
    Ok(
        json!({"format":FORMAT,"verified":true,"entries":m.entries.len(),"schema":m.schema,"original_data_dir":m.original_data_dir,"project_roots":m.project_roots,"activation":"not performed; absolute paths remain bound to the original installation"}),
    )
}
pub fn restore(source: &Path, expected_sha256: &str, destination: &Path) -> Result<Value> {
    ensure!(
        !destination.try_exists()?,
        "restore destination already exists"
    );
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    crate::storage_gc::directory(parent)?;
    let mut file = crate::archive_crypto::verified_input(
        source,
        parent,
        expected_sha256,
        MAX_BYTES + 1024 * 1024 * 1024,
    )?;
    let temporary = tempfile::tempdir_in(parent)?;
    let m = inspect(file.as_file_mut(), Some(temporary.path()))?;
    std::fs::write(
        temporary.path().join("manifest.json"),
        serde_json::to_vec_pretty(&m)?,
    )?;
    std::fs::write(temporary.path().join("RESTORE_NOT_ACTIVATED"),b"Absolute paths still refer to the original installation. Review project-configs and server-config.toml before offline activation.\n")?;
    // Claim the new destination atomically; never rename over a pre-existing directory.
    private_dir(destination)?;
    if let Err(error) = std::fs::rename(temporary.path(), destination) {
        let _ = std::fs::remove_dir(destination);
        return Err(error.into());
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(
        json!({"restored":true,"path":destination,"entries":m.entries.len(),"original_data_dir":m.original_data_dir,"activated":false,"effect":"isolated verified files only; live server, project roots and configuration unchanged"}),
    )
}

fn activation_target(configuration: &Path) -> Result<(PathBuf, crate::config::Config, PathBuf)> {
    let configuration = configuration.canonicalize()?;
    let config = crate::config::Config::load(&configuration)?;
    let parent = config
        .data_dir
        .parent()
        .context("invalid data directory")?
        .canonicalize()?;
    let name = config
        .data_dir
        .file_name()
        .context("invalid data directory")?;
    let target = parent.join(name);
    Ok((configuration, config, target))
}
fn checked_restoration(root: &Path) -> Result<(Manifest, String, Value)> {
    crate::storage_gc::directory(root)?;
    let mut file = crate::archives::open_read(&root.join("manifest.json"))?;
    ensure!(
        file.metadata()?.len() <= MAX_MANIFEST,
        "restore manifest too large"
    );
    let digest = crate::archives::file_hash(&mut file)?;
    let manifest: Manifest = serde_json::from_reader(file)?;
    ensure!(
        manifest.format == FORMAT
            && (51..=crate::migrations::VERSION).contains(&manifest.schema)
            && manifest.entries.len() <= MAX_ENTRIES,
        "unsupported restore manifest"
    );
    ensure!(
        manifest.entries.contains_key("data/preflight.sqlite3")
            && manifest.entries.contains_key("server-config.toml"),
        "incomplete restore manifest"
    );
    ensure!(
        manifest.original_data_dir.is_absolute(),
        "invalid original data directory"
    );
    let mut payload_bytes = 0u64;
    for (name, entry) in &manifest.entries {
        ensure!(safe_name(name), "unsafe restore path");
        payload_bytes = payload_bytes
            .checked_add(entry.bytes)
            .context("restore size overflow")?;
        ensure!(payload_bytes <= MAX_BYTES, "restore exceeds 100 GiB");
        ensure!(
            entry.sha256.len() == 64
                && entry
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid restore digest"
        );
        if entry.directory {
            ensure!(
                entry.bytes == 0
                    && entry.sha256 == crate::hashing::hex(Sha256::digest([]))
                    && !entry.executable,
                "invalid restore directory"
            );
        }
    }
    let mut observed = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut total = 0;
    visit(
        &root.join("data"),
        "data",
        &mut observed,
        &mut files,
        &mut total,
    )?;
    observed.remove("data");
    let expected: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .filter(|(name, _)| name.starts_with("data/"))
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect();
    ensure!(
        observed == expected,
        "restored data differs from the verified snapshot"
    );
    for (id, path) in &manifest.project_roots {
        uuid::Uuid::parse_str(id)?;
        ensure!(path.is_absolute(), "invalid project root");
    }
    for (name, entry) in &manifest.entries {
        ensure!(
            safe_name(name)
                && (name.starts_with("data/")
                    || name.starts_with("project-configs/")
                    || name == "server-config.toml"),
            "unsafe restore entry"
        );
        if name.starts_with("data/") {
            continue;
        }
        let source = root.join(name);
        let meta = std::fs::symlink_metadata(&source)?;
        ensure!(!meta.is_symlink(), "restore refuses symbolic links");
        if entry.directory {
            ensure!(
                meta.is_dir() && entry.bytes == 0,
                "restore directory mismatch"
            );
        } else {
            ensure!(
                meta.is_file()
                    && meta.len() == entry.bytes
                    && crate::archives::file_hash(&mut crate::archives::open_read(&source)?)?
                        == entry.sha256,
                "restore configuration digest mismatch"
            );
        }
        if let Some(relative) = name.strip_prefix("project-configs/") {
            let (id, relative) = relative
                .split_once('/')
                .context("invalid project configuration path")?;
            ensure!(
                relative == ".preflight" || relative.starts_with(".preflight/"),
                "invalid project configuration path"
            );
            let target = manifest
                .project_roots
                .get(id)
                .context("unknown project configuration owner")?
                .join(relative);
            let meta = std::fs::symlink_metadata(&target)
                .context("recover missing project configuration before activation")?;
            ensure!(
                !meta.is_symlink(),
                "project configuration refuses symbolic links"
            );
            if entry.directory {
                ensure!(meta.is_dir(), "project configuration directory differs");
            } else {
                ensure!(
                    meta.is_file()
                        && meta.len() == entry.bytes
                        && crate::archives::file_hash(&mut crate::archives::open_read(&target)?)?
                            == entry.sha256,
                    "project configuration differs; preserve immutable revisions before activation"
                );
            }
        }
    }
    let db = check_db(&root.join("data/preflight.sqlite3"))?;
    ensure!(
        crate::migrations::verify_backup_schema(&db)? == manifest.schema,
        "restore database schema differs from manifest"
    );
    quiescent(&db)?;
    let summary = json!({"projects":db.query_row("SELECT count(*) FROM projects",[],|r|r.get::<_,i64>(0))?,"runs":db.query_row("SELECT count(*) FROM runs",[],|r|r.get::<_,i64>(0))?,"artifacts":db.query_row("SELECT count(*) FROM artifacts",[],|r|r.get::<_,i64>(0))?,"pending_schedules":db.query_row("SELECT count(*) FROM schedules WHERE state='pending'",[],|r|r.get::<_,i64>(0))?,"pending_hooks":db.query_row("SELECT count(*) FROM codex_bindings WHERE state='pending'",[],|r|r.get::<_,i64>(0))?});
    Ok((manifest, digest, summary))
}
pub fn activation_preview(restored: &Path, configuration: &Path) -> Result<Value> {
    let restored = restored.canonicalize()?;
    let (configuration, config, target) = activation_target(configuration)?;
    ensure!(
        !target.try_exists()?,
        "activation target exists; stop the selected server and preserve its old data before activation"
    );
    ensure!(
        config.bind.port() != 0,
        "activation requires a fixed configured server port"
    );
    ensure!(
        std::net::TcpStream::connect_timeout(&config.bind, std::time::Duration::from_millis(300))
            .is_err(),
        "selected server address is active; stop it before activation"
    );
    let (manifest, digest, summary) = checked_restoration(&restored)?;
    ensure!(
        target == manifest.original_data_dir,
        "activation requires the original data path; relocation is not performed"
    );
    let plan = json!({"format":"preflight-activation-plan-v1","restored":restored,"configuration":configuration,"configuration_sha256":crate::archives::file_hash(&mut crate::archives::open_read(&configuration)?)?,"manifest_sha256":digest,"target":target,"snapshot_schema":manifest.schema,"activation_schema":crate::migrations::VERSION,"entries":manifest.entries.len(),"summary":summary,"configured_storage_policies":config.storage_policies.len(),"effect":"create a new data directory only; existing data and external project configurations are never overwritten; server is not started"});
    let plan_id = crate::hashing::hex(Sha256::digest(serde_json::to_vec(&plan)?));
    Ok(json!({"plan_id":plan_id,"plan":plan}))
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn publish_exclusive(source: &Path, target: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let target = std::ffi::CString::new(target.as_os_str().as_bytes())?;
    // Both C strings remain alive and NUL-terminated throughout the OS call.
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn publish_exclusive(_source: &Path, _target: &Path) -> Result<()> {
    anyhow::bail!(
        "activation publication is supported on macOS/Linux; other platforms are not accepted"
    )
}

pub fn activate(restored: &Path, configuration: &Path, plan_id: &str) -> Result<Value> {
    ensure!(
        plan_id.len() == 64
            && plan_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid activation plan_id"
    );
    let (_, _, target) = activation_target(configuration)?;
    if target.try_exists()? {
        crate::storage_gc::directory(&target)?;
        let file = crate::archives::open_read(&target.join("recovery-activation.json"))
            .context("activation target already exists; no data was changed")?;
        ensure!(
            file.metadata()?.len() <= 65536,
            "activation receipt exceeds limit"
        );
        let receipt: Value = serde_json::from_reader(file)?;
        ensure!(
            receipt["format"] == "preflight-activation-receipt-v1"
                && receipt["plan_id"] == plan_id
                && receipt["target"] == json!(target),
            "activation target belongs to a different operation"
        );
        return Ok(
            json!({"activated":true,"already_activated":true,"plan_id":plan_id,"path":target,"server_started":false}),
        );
    }
    let preview = activation_preview(restored, configuration)?;
    ensure!(
        preview["plan_id"] == plan_id,
        "activation plan changed; review a fresh preview"
    );
    let restored = restored.canonicalize()?;
    let (manifest, _, _) = checked_restoration(&restored)?;
    let parent = target.parent().context("invalid activation parent")?;
    let candidate = tempfile::Builder::new()
        .prefix(".preflight-activation-")
        .tempdir_in(parent)?;
    for (name, entry) in &manifest.entries {
        let Some(relative) = name.strip_prefix("data/") else {
            continue;
        };
        let destination = candidate.path().join(relative);
        if relative == "recovery-activation.json" {
            let file = crate::archives::open_read(&restored.join(name))?;
            ensure!(
                file.metadata()?.len() <= 65536,
                "previous activation receipt exceeds limit"
            );
            let old: Value = serde_json::from_reader(file)?;
            ensure!(
                old["format"] == "preflight-activation-receipt-v1",
                "reserved receipt path contains unrecognized data"
            );
            continue;
        }
        if entry.directory {
            std::fs::create_dir_all(&destination)?;
            continue;
        }
        std::fs::create_dir_all(destination.parent().unwrap())?;
        let mut source = crate::archives::open_read(&restored.join(name))?;
        let mut output = FsOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let mut hash = Sha256::new();
        let mut count = 0;
        let mut buffer = [0u8; 65536];
        loop {
            let n = source.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            ensure!(count <= entry.bytes, "restored file grew during activation");
            hash.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
        }
        ensure!(
            count == entry.bytes && crate::hashing::hex(hash.finalize()) == entry.sha256,
            "restored file changed during activation"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(std::fs::Permissions::from_mode(if entry.executable {
                0o700
            } else {
                0o600
            }))?;
        }
        output.sync_all()?;
    }
    {
        let mut db = Connection::open(candidate.path().join("preflight.sqlite3"))?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
        crate::migrations::apply(&mut db)?;
    }
    check_db(&candidate.path().join("preflight.sqlite3"))?;
    ensure!(
        activation_preview(&restored, configuration)?["plan_id"] == plan_id,
        "activation plan changed during preparation"
    );
    let mut options = FsOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(candidate.path().join("server.lock"))?;
    lock.try_lock_exclusive()?;
    let receipt = json!({"format":"preflight-activation-receipt-v1","plan_id":plan_id,"target":target,"restored":restored,"snapshot_schema":manifest.schema,"active_schema":crate::migrations::VERSION,"created_at":crate::db::now(),"configuration":preview["plan"]["configuration"]});
    let mut receipt_file = options.open(candidate.path().join("recovery-activation.json"))?;
    receipt_file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
    receipt_file.sync_all()?;
    File::open(candidate.path().join("preflight.sqlite3"))?.sync_all()?;
    #[cfg(unix)]
    File::open(candidate.path())?.sync_all()?;
    #[cfg(unix)]
    for (name, entry) in manifest.entries.iter().rev() {
        if entry.directory
            && let Some(relative) = name.strip_prefix("data/")
        {
            File::open(candidate.path().join(relative))?.sync_all()?;
        }
    }
    publish_exclusive(candidate.path(), &target)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    drop(lock);
    Ok(
        json!({"activated":true,"already_activated":false,"plan_id":plan_id,"path":target,"server_started":false,"schema":crate::migrations::VERSION}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exclusive_publication_preserves_an_existing_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("payload"), b"owned").unwrap();
        std::fs::create_dir(&target).unwrap();
        assert!(publish_exclusive(&source, &target).is_err());
        assert!(source.join("payload").exists());
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        std::fs::remove_dir(&target).unwrap();
        publish_exclusive(&source, &target).unwrap();
        assert_eq!(std::fs::read(target.join("payload")).unwrap(), b"owned");
    }
    #[test]
    fn hostile_manifest_paths_never_escape_restore() {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "data/../escape",
            "data//double",
            "/absolute",
            "data/C:drive",
            "project-configs/../../escape",
        ] {
            let mut file = tempfile::NamedTempFile::new_in(d.path()).unwrap();
            let empty = || Entry {
                bytes: 0,
                sha256: crate::hashing::hex(Sha256::digest([])),
                executable: false,
                directory: false,
            };
            let m = Manifest {
                format: FORMAT.into(),
                schema: crate::migrations::VERSION,
                original_data_dir: PathBuf::from("/owned/data"),
                created_at: 0,
                project_roots: BTreeMap::new(),
                entries: BTreeMap::from([
                    ("data/preflight.sqlite3".into(), empty()),
                    ("server-config.toml".into(), empty()),
                    (name.into(), empty()),
                ]),
            };
            {
                let mut tar = tar::Builder::new(
                    zstd::stream::write::Encoder::new(file.as_file_mut(), 3).unwrap(),
                );
                let bytes = serde_json::to_vec(&m).unwrap();
                append(
                    &mut tar,
                    "manifest.json",
                    bytes.len() as u64,
                    false,
                    &bytes[..],
                )
                .unwrap();
                tar.into_inner().unwrap().finish().unwrap();
            }
            let target = d.path().join("restored");
            let digest = crate::archives::file_hash(file.as_file_mut()).unwrap();
            assert!(restore(file.path(), &digest, &target).is_err());
            assert!(!target.exists());
            assert!(!d.path().join("escape").exists());
        }
        assert!(!safe_name("data//double"));
        assert!(!safe_name("data/../escape"));
        assert!(safe_name(
            "project-configs/id/.preflight/pipelines/qa/1.json"
        ));
    }
}
