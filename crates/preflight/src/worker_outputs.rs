//! Return only declared job paths; commit verified trees under the current claim.
use crate::{model::Fault, pipeline::ArtifactKind, worker_jobs, worker_transfer};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    body::Body,
    extract::{Path as RoutePath, State},
    http::{HeaderMap, StatusCode},
};
use futures_core::Stream;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, pin::Pin, sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt;

pub fn validate_paths(paths: &BTreeMap<String, ArtifactKind>) -> Result<()> {
    ensure!(paths.len() <= 128, "too many returned worker paths");
    for path in paths.keys() {
        ensure!(
            path.len() <= 4096 && path.split('/').all(crate::trees::portable_component),
            "nonportable returned worker path"
        );
        crate::pipeline::relative(path)?;
        ensure!(
            !paths
                .keys()
                .any(|other| path != other && path.starts_with(&format!("{other}/"))),
            "overlapping returned worker paths"
        );
    }
    Ok(())
}
fn authorize(
    tx: &Transaction<'_>,
    credential: &str,
    claim: &str,
    job: &str,
) -> Result<worker_jobs::Spec> {
    worker_jobs::sweep(tx)?;
    let (worker, generation) = worker_jobs::auth(tx, credential)?;
    let spec: Option<String> = tx.query_row(
        "SELECT spec_json FROM worker_jobs WHERE id=?1 AND worker_id=?2 AND generation=?3 AND claim_hash=?4 AND state IN ('leased','cancelling')",
        params![job,worker,generation,crate::workers::hash(claim)?], |r|r.get(0)).optional()?;
    serde_json::from_str(&spec.ok_or(Fault::NotFound)?).map_err(Into::into)
}
fn validate_manifest(spec: &worker_jobs::Spec, manifest: &crate::trees::Manifest) -> Result<()> {
    validate_paths(&spec.returns)?;
    for (path, entry) in &manifest.entries {
        let allowed = spec.returns.iter().any(|(root, kind)| {
            if root == path {
                return match kind {
                    ArtifactKind::Directory => entry.kind == "directory",
                    ArtifactKind::File => entry.kind == "file",
                    ArtifactKind::Executable => entry.kind == "file" && entry.executable,
                };
            }
            (*kind == ArtifactKind::Directory && path.starts_with(&format!("{root}/")))
                || (entry.kind == "directory" && root.starts_with(&format!("{path}/")))
        });
        ensure!(
            allowed,
            "undeclared or incorrectly typed worker output: {path}"
        );
    }
    Ok(())
}

pub async fn upload(
    State(engine): State<Arc<crate::engine::Engine>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>, StatusCode> {
    let credential = worker_jobs::credential(&headers)?.to_owned();
    let header = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or(StatusCode::BAD_REQUEST)
    };
    let claim = header("x-preflight-claim")?;
    let digest = header("x-preflight-tree-sha256")?;
    crate::modules::validate_digest(&digest).map_err(|_| StatusCode::BAD_REQUEST)?;
    let permit = engine
        .transfer_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let spec = (|| -> Result<_> {
        let mut c = engine.db.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let spec = authorize(&tx, &credential, &claim, &id)?;
        tx.commit()?;
        Ok(spec)
    })()
    .map_err(crate::workers::gateway_error)?;
    let root = crate::pipeline::contained(&engine.config.data_dir, "worker-outputs")
        .map_err(crate::workers::gateway_error)?;
    std::fs::create_dir_all(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let spool =
        tempfile::NamedTempFile::new_in(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut file = tokio::fs::File::from_std(
        spool
            .reopen()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    let receive = async {
        let mut stream = body.into_data_stream();
        let mut size = 0u64;
        while let Some(chunk) = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)).await
        {
            let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
            if size > worker_transfer::MAX_WIRE {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            file.write_all(&chunk)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        file.flush()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(size)
    };
    let size = tokio::time::timeout(Duration::from_secs(60), receive)
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)??;
    drop(file);
    tokio::task::spawn_blocking(move || -> Result<Json<Value>> {
        let _permit = permit;
        let tree =
            worker_transfer::decode(&mut std::fs::File::open(spool.path())?, &digest, &root)?;
        validate_manifest(&spec, &tree.manifest)?;
        let mut c = engine.db.connection()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        authorize(&tx, &credential, &claim, &id)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT tree_sha256 FROM worker_outputs WHERE job_id=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            ensure!(old == digest, Fault::IdempotencyConflict);
        } else {
            let directory = tree
                .directory
                .path()
                .file_name()
                .context("output directory")?
                .to_str()
                .context("output path encoding")?;
            // File contents were synced by decode; sync directory metadata before the DB reference.
            #[cfg(unix)]
            {
                for (path, entry) in tree.manifest.entries.iter().rev() {
                    if entry.kind == "directory" {
                        std::fs::File::open(tree.directory.path().join(path))?.sync_all()?;
                    }
                }
                std::fs::File::open(tree.directory.path())?.sync_all()?;
                std::fs::File::open(&root)?.sync_all()?;
            }
            authorize(&tx, &credential, &claim, &id)?;
            tx.execute(
                "INSERT INTO worker_outputs VALUES (?1,?2,?3,?4,?5)",
                params![id, digest, directory, size, crate::workers::now()],
            )?;
            tx.commit()?;
            let _ = tree.directory.keep();
            return Ok(Json(json!({"job_id":id,"tree_sha256":digest})));
        }
        tx.commit()?;
        Ok(Json(json!({"job_id":id,"tree_sha256":digest})))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(crate::workers::gateway_error)
}

/// Snapshot only existing declared results. Missing required outputs are judged by the engine.
pub fn pack(
    workspace: &Path,
    paths: &BTreeMap<String, ArtifactKind>,
    parent: &Path,
) -> Result<(tempfile::NamedTempFile, String)> {
    validate_paths(paths)?;
    let staging = tempfile::tempdir_in(parent)?;
    let mut total = 0u64;
    let mut entries = 0usize;
    for (path, kind) in paths {
        let source = crate::pipeline::contained(workspace, path)?;
        if !source.try_exists()? {
            continue;
        }
        let destination = staging.path().join(path);
        std::fs::create_dir_all(destination.parent().context("output parent")?)?;
        let (_, bytes, manifest) = crate::artifacts::copy_artifact(&source, &destination, *kind)?;
        total = total.checked_add(bytes).context("output byte overflow")?;
        entries += manifest.as_ref().map_or(1, |m| m.entries.len() + 1);
        ensure!(
            total <= worker_transfer::MAX_BYTES && entries <= 20_000,
            "worker output limit exceeded"
        );
    }
    let snapshot = tempfile::tempdir_in(parent)?;
    let tree = snapshot.path().join("tree");
    let manifest = crate::trees::copy(staging.path(), &tree)?;
    #[cfg(windows)]
    let manifest = {
        let mut manifest = manifest;
        // Windows has no execute bit; preserve the declared artifact type on the wire.
        for (path, kind) in paths {
            if *kind == ArtifactKind::Executable
                && let Some(entry) = manifest.entries.get_mut(path)
            {
                entry.executable = true;
            }
        }
        manifest
    };
    let digest = manifest.sha256()?;
    let mut bundle = tempfile::NamedTempFile::new_in(parent)?;
    worker_transfer::encode(&tree, &manifest, bundle.as_file_mut())?;
    bundle.as_file_mut().sync_all()?;
    Ok((bundle, digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_results_roundtrip_and_reject_unsafe_declarations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("out/module/empty")).unwrap();
        std::fs::write(source.join("out/module/data"), b"\0binary\xff").unwrap();
        std::fs::write(source.join("out/report.xml"), "<testsuite/>").unwrap();
        std::fs::write(source.join("private.txt"), "never return").unwrap();
        std::fs::write(source.join("out/tool.exe"), b"test executable").unwrap();
        std::fs::write(source.join("out/data.exe"), b"ordinary file").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                source.join("out/tool.exe"),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let paths = BTreeMap::from([
            ("out/module".into(), ArtifactKind::Directory),
            ("out/report.xml".into(), ArtifactKind::File),
            ("out/tool.exe".into(), ArtifactKind::Executable),
            ("out/data.exe".into(), ArtifactKind::File),
            ("missing.txt".into(), ArtifactKind::File),
        ]);
        let (bundle, digest) = pack(&source, &paths, root.path()).unwrap();
        let mut tree = worker_transfer::decode(
            &mut std::fs::File::open(bundle.path()).unwrap(),
            &digest,
            root.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(tree.directory.path().join("out/module/data")).unwrap(),
            b"\0binary\xff"
        );
        assert!(tree.directory.path().join("out/module/empty").is_dir());
        assert!(!tree.directory.path().join("private.txt").exists());
        assert!(tree.manifest.entries["out/tool.exe"].executable);
        assert!(!tree.manifest.entries["out/data.exe"].executable);
        let spec = worker_jobs::Spec {
            container: None,
            command: crate::model::CommandSpec {
                program: "tool.exe".into(),
                args: vec![],
                timeout_seconds: 5,
            },
            cwd: ".".into(),
            env: BTreeMap::new(),
            returns: paths,
        };
        validate_manifest(&spec, &tree.manifest).unwrap();
        tree.manifest
            .entries
            .get_mut("out/tool.exe")
            .unwrap()
            .executable = false;
        assert!(validate_manifest(&spec, &tree.manifest).is_err());
        assert!(!tree.directory.path().join("missing.txt").exists());
        for path in [
            "../secret",
            "/tmp/secret",
            "a/../b",
            "a//b",
            "a\\b",
            "CON",
            ".",
        ] {
            assert!(
                validate_paths(&BTreeMap::from([(path.into(), ArtifactKind::File)])).is_err(),
                "{path}"
            );
        }
        assert!(
            validate_paths(&BTreeMap::from([
                ("out".into(), ArtifactKind::Directory),
                ("out/file".into(), ArtifactKind::File)
            ]))
            .is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(source.join("private.txt"), source.join("link")).unwrap();
            assert!(
                pack(
                    &source,
                    &BTreeMap::from([("link".into(), ArtifactKind::File)]),
                    root.path()
                )
                .is_err()
            );
        }
    }
}
