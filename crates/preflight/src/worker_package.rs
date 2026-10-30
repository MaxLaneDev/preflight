//! Offline, versioned worker distribution. No compiler or external service on the worker.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
pub const FORMAT: &str = "preflight-worker-package-v2";
pub const CLI_PROTOCOL: u32 = 2;
fn legacy_cli_protocol() -> u32 {
    1
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub version: String,
    pub job_protocol: u32,
    #[serde(default = "legacy_cli_protocol")]
    pub cli_protocol: u32,
    pub os: String,
    pub arch: String,
    pub sha256: String,
    pub bytes: u64,
}
fn hash(path: &Path) -> Result<(String, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.is_symlink() && meta.len() <= 128 * 1024 * 1024,
        "worker binary must be a regular file of at most 128 MiB"
    );
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        ensure!(
            count <= 128 * 1024 * 1024,
            "worker binary grew beyond limit"
        );
        hash.update(&buffer[..n]);
    }
    ensure!(
        count == meta.len(),
        "worker binary changed during packaging"
    );
    Ok((crate::hashing::hex(hash.finalize()), count))
}
pub fn package(destination: &Path) -> Result<Manifest> {
    ensure!(!destination.exists(), "package destination already exists");
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = tempfile::tempdir_in(parent)?;
    let binary = temp.path().join("preflight");
    std::fs::copy(std::env::current_exe()?, &binary)?;
    let (sha256, bytes) = hash(&binary)?;
    let manifest = Manifest {
        format: FORMAT.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        job_protocol: crate::worker_jobs::CONTAINER_PROTOCOL,
        cli_protocol: CLI_PROTOCOL,
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        sha256,
        bytes,
    };
    std::fs::write(
        temp.path().join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    // Bootstrap verifies the executable before allowing it to run its installer.
    let script = format!(
        r#"#!/bin/sh
set -eu
pf_bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$(uname -s)" in Linux) pf_os=linux;; Darwin) pf_os=macos;; *) echo 'unsupported bootstrap OS' >&2; exit 1;; esac
case "$(uname -m)" in x86_64|amd64) pf_arch=x86_64;; aarch64|arm64) pf_arch=aarch64;; *) echo 'unsupported bootstrap architecture' >&2; exit 1;; esac
[ "$pf_os/$pf_arch" = '{os}/{arch}' ] || {{ echo 'package does not match this computer' >&2; exit 1; }}
[ ! -L "$pf_bundle/preflight" ] || {{ echo 'worker binary is a symlink' >&2; exit 1; }}
if command -v sha256sum >/dev/null 2>&1; then pf_hash=$(sha256sum "$pf_bundle/preflight");
elif command -v shasum >/dev/null 2>&1; then pf_hash=$(shasum -a 256 "$pf_bundle/preflight");
else echo 'sha256sum or shasum is required' >&2; exit 1; fi
[ "${{pf_hash%% *}}" = '{digest}' ] || {{ echo 'worker package SHA-256 mismatch' >&2; exit 1; }}
exec "$pf_bundle/preflight" worker bootstrap "$pf_bundle" "$@"
"#,
        os = manifest.os,
        arch = manifest.arch,
        digest = manifest.sha256
    );
    std::fs::write(temp.path().join("bootstrap.sh"), script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::rename(temp.path(), destination)?;
    Ok(manifest)
}
pub fn verify(bundle: &Path, os: &str, arch: &str) -> Result<(Manifest, PathBuf)> {
    let path = bundle.join("manifest.json");
    let meta = std::fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file() && !meta.is_symlink() && meta.len() <= 8192,
        "invalid worker manifest"
    );
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        (manifest.format == FORMAT && manifest.cli_protocol == CLI_PROTOCOL
            || manifest.format == "preflight-worker-package-v1" && manifest.cli_protocol == 1)
            && manifest.version == env!("CARGO_PKG_VERSION")
            && crate::worker_jobs::compatible(Some(manifest.job_protocol)),
        "worker package version/protocol mismatch"
    );
    ensure!(
        manifest.os == os && manifest.arch == arch,
        "worker package platform mismatch"
    );
    crate::modules::validate_digest(&manifest.sha256)?;
    let binary = bundle.join("preflight");
    let (digest, bytes) = hash(&binary)?;
    ensure!(
        digest == manifest.sha256 && bytes == manifest.bytes,
        "worker package SHA-256/size mismatch"
    );
    Ok((manifest, binary))
}
/// Install by content digest while retaining stable enrollment state across upgrades.
pub fn install(bundle: &Path, root: &Path) -> Result<PathBuf> {
    let (manifest, source) = verify(bundle, std::env::consts::OS, std::env::consts::ARCH)?;
    for ancestor in root.ancestors() {
        if let Ok(meta) = std::fs::symlink_metadata(ancestor) {
            ensure!(
                !meta.is_symlink(),
                "installation path must not contain symlinks"
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        ensure!(
            std::fs::metadata(root)?.permissions().mode() & 0o077 == 0,
            "installation root must be private (0700)"
        );
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(root)?;
    let versions = root.join("versions");
    if let Ok(meta) = std::fs::symlink_metadata(&versions) {
        ensure!(
            meta.is_dir() && !meta.is_symlink(),
            "invalid versions directory"
        );
    }
    std::fs::create_dir_all(&versions)?;
    let destination = versions.join(&manifest.sha256);
    let valid = match std::fs::symlink_metadata(&destination) {
        Ok(_) => hash(&destination)? == (manifest.sha256.clone(), manifest.bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !valid {
        let mut temp = tempfile::NamedTempFile::new_in(&versions)?;
        let mut input = std::fs::File::open(source)?;
        std::io::copy(&mut input, temp.as_file_mut())?;
        let (digest, bytes) = hash(temp.path())?;
        ensure!(
            digest == manifest.sha256 && bytes == manifest.bytes,
            "worker package changed during installation"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file_mut()
                .set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        temp.as_file_mut().sync_all()?;
        temp.persist(&destination).map_err(|error| error.error)?;
    }
    ensure!(
        hash(&destination)? == (manifest.sha256, manifest.bytes),
        "installed binary differs from package"
    );
    #[cfg(unix)]
    {
        std::fs::File::open(&versions)?.sync_all()?;
        std::fs::File::open(root)?.sync_all()?;
    }
    Ok(destination)
}
