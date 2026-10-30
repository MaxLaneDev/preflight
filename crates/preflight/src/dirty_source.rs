//! Bounded, checked working-tree capture. No Git writes or filesystem atomicity claim.
use crate::{git_source::git, model::invalid, sources::DirtySource};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::Path,
    time::SystemTime,
};
use tokio_util::sync::CancellationToken;

const LIMIT: u64 = 128 * 1024 * 1024;
const EXCLUDED: [&str; 4] = [".git", ".preflight", "target", "node_modules"];

#[derive(Debug, PartialEq, Eq)]
struct Inventory {
    head: String,
    index: Vec<u8>,
    paths: BTreeSet<String>,
}
async fn inventory(root: &Path, data: &Path, request: &DirtySource) -> Result<Inventory> {
    let head =
        String::from_utf8(git(root, &["rev-parse", "--verify", "HEAD^{commit}"], &[], 128).await?)?
            .trim()
            .to_owned();
    if let Some(expected) = &request.expected_head {
        ensure!(
            *expected == head,
            invalid("dirty source HEAD does not match expected_head")
        );
    }
    let index = git(root, &["ls-files", "--stage", "-z"], &[], 8 * 1024 * 1024).await?;
    // HEAD paths also retain deletion/recreation semantics relative to the chosen base.
    let tree = git(
        root,
        &["ls-tree", "-r", "-z", "--full-tree", &head],
        &[],
        8 * 1024 * 1024,
    )
    .await?;
    let mut paths = BTreeSet::new();
    for (records, is_index) in [(&index, true), (&tree, false)] {
        for record in records.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let record = std::str::from_utf8(record).context("Git paths must be UTF-8")?;
            let (header, path) = record.split_once('\t').context("invalid Git inventory")?;
            let fields: Vec<_> = header.split(' ').collect();
            ensure!(
                fields.len() == 3 && matches!(fields[0], "100644" | "100755"),
                invalid("dirty sources refuse symlinks and submodules")
            );
            ensure!(
                !is_index || fields[2] == "0",
                invalid("dirty sources refuse an unmerged index")
            );
            if path.split('/').any(|p| EXCLUDED.contains(&p)) || root.join(path).starts_with(data) {
                continue;
            }
            crate::pipeline::relative(path)?;
            ensure!(
                path.len() <= 4096
                    && path.split('/').count() <= 64
                    && path
                        .split('/')
                        .all(|p| !p.is_empty() && p != "." && p != ".."),
                invalid("invalid dirty source path")
            );
            if !path.split('/').any(|p| EXCLUDED.contains(&p)) && !root.join(path).starts_with(data)
            {
                paths.insert(path.to_owned());
            }
            ensure!(
                paths.len() <= 20_000,
                invalid("dirty source exceeds entry limit")
            );
        }
    }
    // Sparse checkouts cannot distinguish intentional omission from a deletion.
    let flags = git(root, &["ls-files", "-v", "-z"], &[], 8 * 1024 * 1024).await?;
    ensure!(
        !flags
            .split(|b| *b == 0)
            .any(|r| matches!(r.first(), Some(b'S' | b's'))),
        invalid("dirty sources do not support skip-worktree entries")
    );
    for path in &request.untracked {
        ensure!(
            !paths.contains(path),
            invalid("untracked allowlist contains a tracked path")
        );
        ensure!(
            !root.join(path).starts_with(data),
            invalid("untracked file belongs to server data")
        );
        paths.insert(path.clone());
    }
    ensure!(
        paths.len() <= 20_000,
        invalid("dirty source exceeds entry limit")
    );
    let mut entries = BTreeSet::new();
    for path in &paths {
        for parent in Path::new(path)
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
        {
            entries.insert(parent);
            ensure!(
                entries.len() <= 20_000,
                invalid("dirty source exceeds file/directory entry limit")
            );
        }
    }
    Ok(Inventory { head, index, paths })
}

#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    digest: String,
    size: u64,
    executable: bool,
    modified: Option<SystemTime>,
}
// Inspect every ancestor: canonicalizing only a leaf would follow a symlink outside root.
fn metadata(root: &Path, relative: &str) -> Result<Option<fs::Metadata>> {
    let parts: Vec<_> = relative.split('/').collect();
    let mut path = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        path.push(part);
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        ensure!(!meta.is_symlink(), invalid("dirty sources refuse symlinks"));
        if i + 1 == parts.len() {
            ensure!(
                meta.is_file(),
                invalid("dirty source path is not a regular file")
            );
            return Ok(Some(meta));
        }
        ensure!(
            meta.is_dir(),
            invalid("dirty source ancestor is not a directory")
        );
    }
    unreachable!()
}
fn read_file(
    root: &Path,
    path: &str,
    output: Option<&Path>,
    remaining: &mut u64,
    cancel: &CancellationToken,
) -> Result<Option<Fingerprint>> {
    ensure!(!cancel.is_cancelled(), "dirty snapshot cancelled");
    let Some(meta) = metadata(root, path)? else {
        return Ok(None);
    };
    ensure!(
        meta.len() <= *remaining,
        invalid("dirty source exceeds 128 MiB")
    );
    let mut input = fs::File::open(root.join(path))?;
    ensure!(
        input.metadata()?.is_file(),
        invalid("dirty source changed file type")
    );
    let mut output = output
        .map(|destination| -> Result<fs::File> {
            fs::create_dir_all(destination.parent().context("destination parent")?)?;
            Ok(fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?)
        })
        .transpose()?;
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = [0u8; 65536];
    loop {
        ensure!(!cancel.is_cancelled(), "dirty snapshot cancelled");
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        *remaining = remaining
            .checked_sub(n as u64)
            .context("dirty source exceeds 128 MiB")?;
        size += n as u64;
        hash.update(&buffer[..n]);
        if let Some(output) = &mut output {
            output.write_all(&buffer[..n])?;
        }
    }
    #[cfg(not(unix))]
    let executable = false;
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(output) = &output {
            output.set_permissions(fs::Permissions::from_mode(if executable {
                0o755
            } else {
                0o644
            }))?;
        }
    }
    ensure!(
        size == meta.len(),
        invalid("dirty source changed during read")
    );
    Ok(Some(Fingerprint {
        digest: crate::hashing::hex(hash.finalize()),
        size,
        executable,
        modified: meta.modified().ok(),
    }))
}
fn copy_checked(
    root: &Path,
    destination: &Path,
    paths: &BTreeSet<String>,
    required: &[String],
    cancel: &CancellationToken,
    after_copy: impl FnOnce(),
) -> Result<()> {
    let mut remaining = LIMIT;
    let before = paths
        .iter()
        .map(|path| {
            Ok((
                path.clone(),
                read_file(root, path, None, &mut remaining, cancel)?,
            ))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
    for path in required {
        ensure!(
            before.get(path).is_some_and(Option::is_some),
            invalid("selected untracked file is missing")
        );
    }
    let mut remaining = LIMIT;
    for (path, expected) in &before {
        let actual = read_file(
            root,
            path,
            Some(&destination.join(path)),
            &mut remaining,
            cancel,
        )?;
        ensure!(
            actual == *expected,
            invalid("dirty source changed during copy")
        );
    }
    let canonical_destination = fs::canonicalize(destination)?;
    for (path, expected) in &before {
        if expected.is_some() {
            ensure!(
                fs::canonicalize(destination.join(path))?.strip_prefix(&canonical_destination)?
                    == Path::new(path),
                invalid("dirty snapshot path alias")
            );
        }
    }
    after_copy();
    let mut remaining = LIMIT;
    for (path, expected) in &before {
        ensure!(
            read_file(root, path, None, &mut remaining, cancel)? == *expected,
            invalid("dirty source changed after copy")
        );
    }
    Ok(())
}
pub async fn prepare(
    root: &Path,
    destination: &Path,
    data: &Path,
    request: &DirtySource,
    cancel: &CancellationToken,
) -> Result<serde_json::Value> {
    request.validate()?;
    let token = cancel.child_token();
    // A timed-out/dropped join cannot stop blocking I/O immediately; the guard requests
    // cancellation and the worker owns its temporary directory until it exits.
    let _guard = token.clone().drop_guard();
    let work = async {
        let top = git(root, &["rev-parse", "--show-toplevel"], &[], 8192).await?;
        ensure!(
            fs::canonicalize(std::str::from_utf8(&top)?.trim_end())? == root,
            invalid("registered project must be the Git repository root")
        );
        let before = inventory(root, data, request).await?;
        let parent = destination.parent().context("snapshot parent")?;
        fs::create_dir_all(parent)?;
        let pending = tempfile::tempdir_in(parent)?;
        let candidate = pending.path().join("tree");
        fs::create_dir(&candidate)?;
        let copy_root = root.to_path_buf();
        let paths = before.paths.clone();
        let required = request.untracked.clone();
        let copy_token = token.clone();
        let pending = tokio::task::spawn_blocking(move || {
            copy_checked(
                &copy_root,
                &candidate,
                &paths,
                &required,
                &copy_token,
                || {},
            )?;
            Ok::<_, anyhow::Error>(pending)
        })
        .await??;
        ensure!(
            inventory(root, data, request).await? == before,
            invalid("Git HEAD or index changed during dirty snapshot")
        );
        ensure!(!token.is_cancelled(), "dirty snapshot cancelled");
        ensure!(!destination.exists(), "source destination already exists");
        fs::rename(pending.path().join("tree"), destination)?;
        Ok(
            serde_json::json!({"format":"preflight-dirty-source-v1","resolved_commit":before.head,"index_inventory_sha256":crate::hashing::hex(Sha256::digest(&before.index)),"snapshot_path":destination,"repository_path":root,"untracked":request.untracked,"inclusion_policy":"head-and-index-paths-plus-explicit-untracked-v1","excluded_components":EXCLUDED,"excluded_server_data_path":data,"consistency":"checked-copy; not an atomic filesystem snapshot"}),
        )
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("dirty snapshot cancelled"),
        result = tokio::time::timeout(std::time::Duration::from_secs(60), work) => result.context("dirty snapshot timed out")?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_mutation_and_preserves_missing_files() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(root.path().join("tracked"), "before").unwrap();
        let paths = BTreeSet::from(["tracked".to_owned(), "deleted".to_owned()]);
        let token = CancellationToken::new();
        let result = copy_checked(root.path(), target.path(), &paths, &[], &token, || {
            fs::write(root.path().join("tracked"), "after!").unwrap();
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("changed after copy")
        );
        assert!(!target.path().join("deleted").exists());
        assert_eq!(fs::read(target.path().join("tracked")).unwrap(), b"before");
    }
    #[test]
    fn cancellation_budget_and_missing_required_file_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "content").unwrap();
        let token = CancellationToken::new();
        let mut remaining = 2;
        assert!(read_file(root.path(), "file", None, &mut remaining, &token).is_err());
        assert!(
            copy_checked(
                root.path(),
                target.path(),
                &BTreeSet::from(["missing".into()]),
                &["missing".into()],
                &token,
                || {}
            )
            .is_err()
        );
        token.cancel();
        let mut remaining = LIMIT;
        assert!(
            read_file(
                root.path(),
                "file",
                Some(&target.path().join("file")),
                &mut remaining,
                &token
            )
            .is_err()
        );
        assert!(!target.path().join("file").exists());
    }
}
