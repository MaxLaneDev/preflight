//! Authenticated launch observations; these do not authorize remote cache reuse.
use crate::{model::invalid, worker_jobs::Spec};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path};

pub const FORMAT: &str = "preflight-worker-runtime-v1";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub format: String,
    pub os: String,
    pub arch: String,
    pub user: Option<String>,
    pub spec_sha256: String,
    pub source_tree_sha256: String,
    pub environment_sha256: String,
    pub launcher_sha256: Option<String>,
    pub container: Option<Container>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub image_id: String,
    pub platform: String,
    pub user: Option<String>,
}
pub(crate) fn digest(value: &impl Serialize) -> Result<String> {
    Ok(crate::hashing::hex(Sha256::digest(serde_json::to_vec(
        value,
    )?)))
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn user() -> Option<String> {
    #[cfg(unix)]
    {
        Some(format!("{}:{}", unsafe { libc::geteuid() }, unsafe {
            libc::getegid()
        }))
    }
    #[cfg(not(unix))]
    {
        None
    }
}
fn executable(command: &str, cwd: &Path, env: &BTreeMap<String, String>) -> Option<String> {
    let path = if command.contains(['/', '\\']) {
        cwd.join(command)
    } else {
        let search = env
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"))?;
        std::env::split_paths(&search)
            .map(|p| cwd.join(p).join(command))
            .find(|p| {
                if !p.is_file() {
                    return false;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    if unsafe {
                        libc::geteuid() != libc::getuid() || libc::getegid() != libc::getgid()
                    } {
                        return false;
                    }
                    std::ffi::CString::new(p.as_os_str().as_bytes())
                        .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 })
                }
                #[cfg(not(unix))]
                {
                    true
                }
            })?
    };
    // Execute-only binaries can run without exposing their bytes; leave identity unknown.
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 * 1024 {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > 1024 * 1024 * 1024 {
            return None;
        }
        digest.update(&buffer[..n]);
    }
    Some(crate::hashing::hex(digest.finalize()))
}
pub(crate) fn observe(
    spec: &Spec,
    tree: &str,
    command: &str,
    workspace: &Path,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Receipt> {
    let mut environment = BTreeMap::new();
    if spec.container.is_none() {
        for name in crate::executor::INHERITED_ENV {
            if let Some(value) = std::env::var_os(name) {
                environment.insert((*name).to_owned(), value.as_encoded_bytes().to_vec());
            }
        }
    } else {
        environment.insert("HOME".into(), b"/tmp".to_vec());
    }
    for (name, value) in env {
        let value = if spec.container.is_some() {
            crate::docker::mapped(value, workspace)?
        } else {
            value.clone()
        };
        environment.insert(name.clone(), value.into_bytes());
    }
    environment.insert(
        "PREFLIGHT_WORKSPACE".into(),
        if spec.container.is_some() {
            b"/workspace".to_vec()
        } else {
            workspace.as_os_str().as_encoded_bytes().to_vec()
        },
    );
    Ok(Receipt {
        format: FORMAT.into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        user: user(),
        spec_sha256: digest(spec)?,
        source_tree_sha256: tree.into(),
        environment_sha256: digest(&environment)?,
        launcher_sha256: if spec.container.is_none() {
            executable(command, cwd, env)
        } else {
            None
        },
        container: None,
    })
}
pub(crate) fn accept(tx: &Transaction<'_>, job: &str, receipt: &Receipt) -> Result<()> {
    let (spec, tree, os, arch, worker, generation, run, stage): (String,String,String,String,String,u64,String,String) = tx.query_row("SELECT j.spec_json,t.tree_sha256,w.os,w.arch,j.worker_id,j.generation,j.run_id,j.stage_id FROM worker_jobs j JOIN worker_transfers t ON t.id=j.transfer_id JOIN workers w ON w.id=j.worker_id WHERE j.id=?1",[job],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))?;
    let spec: Spec = serde_json::from_str(&spec)?;
    validate(receipt, &spec, &tree, &os, &arch)?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT evidence_json FROM stage_keys WHERE run_id=?1 AND stage_id=?2",
            params![run, stage],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        let mut evidence: crate::stage_keys::Evidence = serde_json::from_str(&previous)?;
        evidence.format = "preflight-stage-evidence-v2".into();
        evidence.components.remove("inherited_environment");
        evidence.components.remove("container_runtime");
        evidence.components.insert(
            "host".into(),
            digest(&(&receipt.os, &receipt.arch, &receipt.user))?,
        );
        evidence
            .components
            .insert("execution_worker".into(), digest(&(worker, generation))?);
        evidence
            .components
            .insert("remote_runtime".into(), digest(receipt)?);
        evidence.components.insert(
            "transferred_inputs".into(),
            receipt.source_tree_sha256.clone(),
        );
        evidence.sha256 = digest(&(&evidence.format, &evidence.components))?;
        evidence.reusable = false;
        evidence.reason =
            "authenticated worker launch observation; remote cache reuse remains disabled".into();
        tx.execute(
            "UPDATE stage_keys SET evidence_json=?3 WHERE run_id=?1 AND stage_id=?2",
            params![run, stage, serde_json::to_string(&evidence)?],
        )?;
    }
    Ok(())
}
pub(crate) fn validate(
    receipt: &Receipt,
    spec: &Spec,
    tree: &str,
    os: &str,
    arch: &str,
) -> Result<()> {
    ensure!(
        receipt.format == FORMAT && receipt.os == os && receipt.arch == arch,
        invalid("worker runtime host/format mismatch")
    );
    ensure!(
        receipt.spec_sha256 == digest(spec)? && receipt.source_tree_sha256 == tree,
        invalid("worker runtime inputs/spec mismatch")
    );
    ensure!(
        hash(&receipt.spec_sha256)
            && hash(&receipt.source_tree_sha256)
            && hash(&receipt.environment_sha256)
            && receipt.launcher_sha256.as_deref().is_none_or(hash),
        invalid("invalid worker runtime digest")
    );
    ensure!(
        receipt
            .user
            .as_ref()
            .is_none_or(|v| v.len() <= 64 && v.bytes().all(|b| b.is_ascii_digit() || b == b':')),
        invalid("invalid worker runtime user")
    );
    match (&spec.container, &receipt.container) {
        (None, None) => {}
        (Some(expected), Some(actual)) => {
            ensure!(
                actual.image_id.strip_prefix("sha256:").is_some_and(hash)
                    && actual.platform == expected.spec.platform
                    && actual
                        .user
                        .as_ref()
                        .is_none_or(|v| v.len() <= 128 && !v.contains(['\0', '\n', '\r'])),
                invalid("invalid worker container receipt")
            );
            if expected.spec.image.starts_with("sha256:") {
                ensure!(
                    expected.spec.image == actual.image_id,
                    invalid("worker container image mismatch")
                );
            }
            if let Some(user) = &expected.spec.user {
                ensure!(
                    actual.user.as_ref() == Some(user),
                    invalid("worker container user mismatch")
                );
            } else {
                ensure!(
                    actual.user == receipt.user,
                    invalid("worker default container user mismatch")
                );
            }
        }
        _ => anyhow::bail!(invalid("worker runtime executor mismatch")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn spec() -> Spec {
        serde_json::from_value(
            json!({"command":{"program":"tool","args":[],"timeout_seconds":5},"cwd":".","env":{}}),
        )
        .unwrap()
    }
    #[cfg(unix)]
    #[test]
    fn launcher_hash_tracks_the_executable_allowed_for_this_user() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        for (dir, mode, text) in [(&first, 0o410, "wrong"), (&second, 0o700, "selected")] {
            let tool = dir.join("tool");
            std::fs::write(&tool, format!("#!/bin/sh\nprintf {text}")).unwrap();
            std::fs::set_permissions(tool, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let search = std::env::join_paths([first, second.clone()]).unwrap();
        let actual = std::process::Command::new("tool")
            .env("PATH", &search)
            .output()
            .unwrap();
        assert!(actual.status.success());
        let selected = if actual.stdout == b"wrong" {
            root.path().join("first/tool")
        } else {
            assert_eq!(actual.stdout, b"selected");
            second.join("tool")
        };
        let expected = crate::hashing::hex(Sha256::digest(std::fs::read(selected).unwrap()));
        let env = BTreeMap::from([("PATH".into(), search.to_str().unwrap().into())]);
        assert_eq!(
            observe(
                &spec(),
                &"a".repeat(64),
                "tool",
                root.path(),
                root.path(),
                &env
            )
            .unwrap()
            .launcher_sha256,
            Some(expected)
        );
    }
    #[test]
    fn observations_hash_environment_and_launcher_without_exporting_values() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let cwd = root.join("sub");
        std::fs::create_dir(&cwd).unwrap();
        let tool = root.join("tool");
        std::fs::write(&tool, b"first tool").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let spec = spec();
        let tree = "a".repeat(64);
        let mut env = BTreeMap::from([
            ("PATH".into(), root.to_str().unwrap().into()),
            ("PRIVATE_VALUE".into(), "private-receipt-value".into()),
        ]);
        let first = observe(&spec, &tree, "tool", root, &cwd, &env).unwrap();
        validate(
            &first,
            &spec,
            &tree,
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
        .unwrap();
        assert!(first.launcher_sha256.is_some());
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(!encoded.contains("private-receipt-value"));
        assert!(!encoded.contains(root.to_str().unwrap()));
        assert_eq!(
            first,
            observe(&spec, &tree, "tool", root, &cwd, &env).unwrap()
        );
        std::fs::write(&tool, b"changed tool").unwrap();
        let second = observe(&spec, &tree, "tool", root, &cwd, &env).unwrap();
        assert_ne!(first.launcher_sha256, second.launcher_sha256);
        env.insert("PRIVATE_VALUE".into(), "changed private value".into());
        let third = observe(&spec, &tree, "tool", root, &cwd, &env).unwrap();
        assert_ne!(second.environment_sha256, third.environment_sha256);
        let mut bad = third.clone();
        bad.source_tree_sha256 = "b".repeat(64);
        assert!(
            validate(
                &bad,
                &spec,
                &tree,
                std::env::consts::OS,
                std::env::consts::ARCH
            )
            .is_err()
        );
        bad = third;
        bad.spec_sha256 = "c".repeat(64);
        assert!(
            validate(
                &bad,
                &spec,
                &tree,
                std::env::consts::OS,
                std::env::consts::ARCH
            )
            .is_err()
        );
    }
    #[test]
    fn container_receipts_require_matching_image_platform_and_user() {
        let mut spec = spec();
        spec.container=Some(serde_json::from_value(json!({"memory_mib":256,"spec":{"image":format!("sha256:{}","a".repeat(64)),"platform":"linux/arm64","user":"1001:1001"}})).unwrap());
        let root = tempfile::tempdir().unwrap();
        let tree = "b".repeat(64);
        let mut receipt = observe(
            &spec,
            &tree,
            "tool",
            root.path(),
            root.path(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(receipt.launcher_sha256.is_none());
        assert!(
            validate(
                &receipt,
                &spec,
                &tree,
                std::env::consts::OS,
                std::env::consts::ARCH
            )
            .is_err()
        );
        receipt.container = Some(Container {
            image_id: spec.container.as_ref().unwrap().spec.image.clone(),
            platform: "linux/arm64".into(),
            user: Some("1001:1001".into()),
        });
        validate(
            &receipt,
            &spec,
            &tree,
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
        .unwrap();
        for field in ["image_id", "platform", "user"] {
            let mut bad = receipt.clone();
            let container = bad.container.as_mut().unwrap();
            match field {
                "image_id" => container.image_id = format!("sha256:{}", "c".repeat(64)),
                "platform" => container.platform = "linux/amd64".into(),
                _ => container.user = Some("0:0".into()),
            };
            assert!(
                validate(
                    &bad,
                    &spec,
                    &tree,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )
                .is_err()
            );
        }
    }
    #[test]
    fn legacy_outcome_omits_receipt_and_unknown_fields_are_rejected() {
        let old = json!({"status":"succeeded","exit":0,"stdout":"","stderr":"","truncated":false});
        let value: crate::worker_jobs::Outcome = serde_json::from_value(old.clone()).unwrap();
        assert!(value.runtime_receipt.is_none());
        assert_eq!(serde_json::to_value(value).unwrap(), old);
        let mut unknown = old;
        unknown["runtime_receipt"] = json!({"unexpected":true});
        assert!(serde_json::from_value::<crate::worker_jobs::Outcome>(unknown).is_err());
    }
}
