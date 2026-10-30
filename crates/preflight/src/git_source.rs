use crate::{model::invalid, pipeline, sources::GitSource};
use anyhow::{Context, Result, ensure};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const EXCLUDED: [&str; 4] = [".git", ".preflight", "target", "node_modules"];

pub(crate) async fn git(cwd: &Path, args: &[&str], input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut command = tokio::process::Command::new("git");
    command
        .current_dir(cwd)
        .env_clear()
        .kill_on_drop(true)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in ["PATH", "SystemRoot", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env(
            "GIT_CEILING_DIRECTORIES",
            cwd.parent().context("Git cwd parent")?,
        );
    let mut child = command.spawn().context("start local Git")?;
    let mut stdin = child.stdin.take().context("Git stdin")?;
    let stdout = child.stdout.take().context("Git stdout")?;
    let stderr = child.stderr.take().context("Git stderr")?;
    async fn read(reader: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
        let mut output = vec![];
        reader
            .take(limit as u64 + 1)
            .read_to_end(&mut output)
            .await?;
        ensure!(
            output.len() <= limit,
            invalid("Git output exceeds configured limit")
        );
        Ok(output)
    }
    let write = async {
        stdin.write_all(input).await?;
        drop(stdin);
        Ok::<_, anyhow::Error>(())
    };
    let (output, errors, (), status) =
        tokio::try_join!(read(stdout, limit), read(stderr, 8192), write, async {
            Ok::<_, anyhow::Error>(child.wait().await?)
        })?;
    ensure!(
        status.success(),
        invalid(format!(
            "Git source preparation failed: {}",
            String::from_utf8_lossy(&errors)
                .chars()
                .take(1000)
                .collect::<String>()
        ))
    );
    Ok(output)
}
fn relative(path: &str) -> Result<std::path::PathBuf> {
    ensure!(
        !path.is_empty()
            && path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != "..")
            && path.split('/').count() <= 64,
        invalid("invalid Git source path")
    );
    pipeline::relative(path)
}
fn excluded(path: &str) -> bool {
    path.split('/').any(|p| EXCLUDED.contains(&p))
}
fn validate_tree(root: &Path) -> Result<()> {
    fn visit(root: &Path, depth: usize, count: &mut usize, bytes: &mut u64) -> Result<()> {
        ensure!(depth <= 64, invalid("Git source depth exceeds 64"));
        for item in std::fs::read_dir(root)? {
            let item = item?;
            let path = item.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| invalid("Git source name is not UTF-8"))?;
            ensure!(
                !EXCLUDED.contains(&name.as_str()),
                invalid("patch creates an excluded source path")
            );
            relative(&name)?;
            ensure!(
                !metadata.is_symlink(),
                invalid("Git sources refuse symlinks")
            );
            *count += 1;
            ensure!(
                *count <= MAX_ENTRIES,
                invalid("Git source exceeds 20000 entries")
            );
            if metadata.is_dir() {
                visit(&path, depth + 1, count, bytes)?;
            } else {
                ensure!(
                    metadata.is_file(),
                    invalid("Git sources refuse special files")
                );
                *bytes = bytes
                    .checked_add(metadata.len())
                    .context("Git source size overflow")?;
                ensure!(
                    *bytes <= MAX_BYTES as u64,
                    invalid("Git source exceeds 128 MiB")
                );
            }
        }
        Ok(())
    }
    visit(root, 0, &mut 0, &mut 0)
}

pub async fn prepare(
    root: &Path,
    destination: &Path,
    data: &Path,
    source: &GitSource,
    cancel: &CancellationToken,
) -> Result<serde_json::Value> {
    source.validate()?;
    let work = async {
        let top = git(root, &["rev-parse", "--show-toplevel"], &[], 8192).await?;
        let top = std::str::from_utf8(&top)?.trim_end_matches(['\n', '\r']);
        ensure!(
            std::fs::canonicalize(top)? == root,
            invalid("registered project must be the Git worktree root")
        );
        let resolved = git(
            root,
            &["rev-parse", "--verify", "--end-of-options", &source.commit],
            &[],
            128,
        )
        .await?;
        let resolved = std::str::from_utf8(&resolved)?.trim();
        ensure!(
            resolved == source.commit,
            invalid("commit must use the repository's full object ID")
        );
        let kind = git(root, &["cat-file", "-t", &source.commit], &[], 64).await?;
        ensure!(
            kind == b"commit\n",
            invalid("Git source object must be a commit")
        );
        let listing = git(
            root,
            &["ls-tree", "-r", "-z", "--full-tree", &source.commit],
            &[],
            8 * 1024 * 1024,
        )
        .await?;
        let mut files = vec![];
        for row in listing.split(|b| *b == 0).filter(|row| !row.is_empty()) {
            ensure!(
                files.len() < MAX_ENTRIES,
                invalid("Git source has too many files")
            );
            let tab = row
                .iter()
                .position(|b| *b == b'\t')
                .context("Git tree record has no path")?;
            let fields = std::str::from_utf8(&row[..tab])?
                .split(' ')
                .collect::<Vec<_>>();
            ensure!(
                fields.len() == 3
                    && fields[1] == "blob"
                    && matches!(fields[0], "100644" | "100755"),
                invalid("Git symlinks, submodules and unsupported modes are refused")
            );
            let path = std::str::from_utf8(&row[tab + 1..])
                .map_err(|_| invalid("Git source path is not UTF-8"))?;
            if excluded(path) || root.join(path).starts_with(data) {
                continue;
            }
            let path = relative(path)?;
            files.push((path, fields[0].to_owned(), fields[2].to_owned()));
        }
        let ids = files
            .iter()
            .map(|(_, _, id)| format!("{id}\n"))
            .collect::<String>();
        let blobs = git(
            root,
            &["cat-file", "--batch"],
            ids.as_bytes(),
            MAX_BYTES + MAX_ENTRIES * 128,
        )
        .await?;
        std::fs::create_dir_all(destination.parent().context("source parent")?)?;
        let pending = tempfile::tempdir_in(destination.parent().unwrap())?;
        let candidate = pending.path().canonicalize()?.join("tree");
        std::fs::create_dir(&candidate)?;
        let patch_git = pending.path().canonicalize()?.join("patch-git");
        let mut remaining = blobs.as_slice();
        let mut total = 0usize;
        for (path, mode, oid) in files {
            ensure!(
                !cancel.is_cancelled(),
                invalid("source preparation cancelled")
            );
            let newline = remaining
                .iter()
                .position(|b| *b == b'\n')
                .context("missing Git blob header")?;
            let fields = std::str::from_utf8(&remaining[..newline])?
                .split(' ')
                .collect::<Vec<_>>();
            ensure!(
                fields.len() == 3 && fields[0] == oid && fields[1] == "blob",
                invalid("unexpected Git blob response")
            );
            let size = fields[2].parse::<usize>()?;
            ensure!(
                size <= MAX_BYTES.saturating_sub(total),
                invalid("Git source exceeds 128 MiB")
            );
            total += size;
            remaining = &remaining[newline + 1..];
            ensure!(
                remaining.len() > size && remaining[size] == b'\n',
                invalid("truncated Git blob")
            );
            let target = candidate.join(path);
            let parent = target.parent().context("Git file parent")?;
            std::fs::create_dir_all(parent)?;
            ensure!(
                parent.canonicalize()? == parent,
                invalid("Git source path aliases another directory")
            );
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)?;
            file.write_all(&remaining[..size])?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(if mode == "100755" {
                    0o755
                } else {
                    0o644
                }))?;
            }
            #[cfg(not(unix))]
            let _ = mode;
            remaining = &remaining[size + 1..];
            tokio::task::yield_now().await;
        }
        ensure!(
            remaining.is_empty(),
            invalid("unexpected trailing Git blob output")
        );
        validate_tree(&candidate)?;
        if !source.patches.is_empty() {
            git(
                pending.path(),
                &[
                    "init",
                    "--bare",
                    "--quiet",
                    "--template=",
                    patch_git.to_str().context("patch Git path")?,
                ],
                &[],
                8192,
            )
            .await?;
        }
        for (index, patch) in source.patches.iter().enumerate() {
            ensure!(
                !patch
                    .diff
                    .lines()
                    .any(|line| line == "GIT binary patch" || line.starts_with("Binary files ")),
                invalid("binary patches are not supported")
            );
            git(
                &candidate,
                &[
                    "--git-dir",
                    patch_git.to_str().unwrap(),
                    "--work-tree",
                    candidate.to_str().context("patch tree path")?,
                    "-c",
                    "core.bare=false",
                    "apply",
                    "--check",
                    "--whitespace=nowarn",
                    "-",
                ],
                patch.diff.as_bytes(),
                8192,
            )
            .await
            .with_context(|| format!("patch {index} check"))?;
            git(
                &candidate,
                &[
                    "--git-dir",
                    patch_git.to_str().unwrap(),
                    "--work-tree",
                    candidate.to_str().context("patch tree path")?,
                    "-c",
                    "core.bare=false",
                    "apply",
                    "--whitespace=nowarn",
                    "-",
                ],
                patch.diff.as_bytes(),
                8192,
            )
            .await
            .with_context(|| format!("patch {index} apply"))?;
            validate_tree(&candidate)?;
        }
        ensure!(
            !cancel.is_cancelled(),
            invalid("source preparation cancelled")
        );
        ensure!(
            !destination.exists(),
            invalid("source destination already exists")
        );
        // The destination is run-owned. It becomes visible only after all patches validate.
        std::fs::rename(&candidate, destination)?;
        let mut identity = source.identity()?;
        identity["resolved_commit"] = source.commit.clone().into();
        identity["snapshot_path"] = destination.to_string_lossy().into_owned().into();
        identity["repository_path"] = root.to_string_lossy().into_owned().into();
        identity["excluded_components"] = serde_json::json!(EXCLUDED);
        identity["exclusion_policy"] = "preflight-source-exclusions-v1".into();
        identity["excluded_server_data_path"] = data.to_string_lossy().into_owned().into();
        Ok(identity)
    };
    tokio::select! {
        _=cancel.cancelled()=>Err(invalid("source preparation cancelled")),
        result=tokio::time::timeout(Duration::from_secs(60),work)=>result.context("Git preparation exceeded 60 seconds")?,
    }
}
