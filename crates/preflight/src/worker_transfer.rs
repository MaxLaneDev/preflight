//! Bounded, hash-verified tree transfer. No archive path interpretation or symlinks.
use crate::trees::{Entry, Manifest};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::Path,
};

pub const MAGIC: &[u8] = b"PREFLIGHT-TREE-1\n";
pub const MAX_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_MANIFEST: usize = 4 * 1024 * 1024;
pub const MAX_WIRE: u64 = MAX_BYTES + MAX_MANIFEST as u64 + MAGIC.len() as u64 + 4;

pub fn validate(manifest: &Manifest) -> Result<()> {
    ensure!(
        manifest.format == crate::trees::FORMAT,
        "unsupported transfer tree format"
    );
    ensure!(
        manifest.entries.len() <= 20_000,
        "transfer entry limit exceeded"
    );
    let mut folded = BTreeSet::new();
    let mut total = 0u64;
    for (path, entry) in &manifest.entries {
        let parts: Vec<_> = path.split('/').collect();
        ensure!(
            path.len() <= 4096
                && parts.len() <= 65
                && parts
                    .iter()
                    .all(|p| *p != "." && *p != ".." && crate::trees::portable_component(p)),
            "nonportable transfer path"
        );
        ensure!(
            folded.insert(path.to_lowercase()),
            "case-ambiguous transfer paths"
        );
        if let Some((parent, _)) = path.rsplit_once('/') {
            ensure!(
                manifest
                    .entries
                    .get(parent)
                    .is_some_and(|p| p.kind == "directory"),
                "missing or non-directory transfer parent"
            );
        }
        match entry.kind.as_str() {
            "directory" => ensure!(
                parts.len() <= 64 && entry.size == 0 && entry.sha256.is_none() && !entry.executable,
                "invalid directory transfer entry"
            ),
            "file" => {
                crate::modules::validate_digest(
                    entry
                        .sha256
                        .as_deref()
                        .context("missing transfer file digest")?,
                )?;
                total = total
                    .checked_add(entry.size)
                    .context("transfer byte overflow")?;
                ensure!(total <= MAX_BYTES, "transfer exceeds 128 MiB");
            }
            _ => anyhow::bail!("unsupported transfer entry kind"),
        }
    }
    Ok(())
}
fn copy_exact(reader: &mut impl Read, writer: &mut impl Write, entry: &Entry) -> Result<()> {
    let mut left = entry.size;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    while left > 0 {
        let n = reader.read(&mut buf[..left.min(65536) as usize])?;
        ensure!(n > 0, "truncated transfer file");
        hash.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
        left -= n as u64;
    }
    ensure!(
        Some(crate::hashing::hex(hash.finalize())).as_ref() == entry.sha256.as_ref(),
        "transfer file SHA-256 mismatch"
    );
    Ok(())
}
/// Call with a private frozen tree. Any error leaves the output unpublished.
pub fn encode(root: &Path, manifest: &Manifest, writer: &mut impl Write) -> Result<()> {
    validate(manifest)?;
    let bytes = serde_json::to_vec(manifest)?;
    ensure!(bytes.len() <= MAX_MANIFEST, "transfer manifest too large");
    writer.write_all(MAGIC)?;
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    ensure!(
        !std::fs::symlink_metadata(root)?.is_symlink(),
        "transfer root is a symlink"
    );
    for (name, entry) in &manifest.entries {
        let mut path = root.to_path_buf();
        for part in name.split('/') {
            path.push(part);
            ensure!(
                !std::fs::symlink_metadata(&path)?.is_symlink(),
                "symlink in transfer source"
            );
        }
        if entry.kind == "file" {
            let meta = std::fs::symlink_metadata(&path)?;
            ensure!(
                meta.is_file() && meta.len() == entry.size,
                "transfer source changed"
            );
            let mut file = std::fs::File::open(&path)?;
            copy_exact(&mut file, writer, entry)?;
            let mut extra = [0u8; 1];
            ensure!(file.read(&mut extra)? == 0, "transfer source grew");
        }
    }
    Ok(())
}
pub struct VerifiedTree {
    pub directory: tempfile::TempDir,
    pub manifest: Manifest,
}
/// The result is reachable by the caller only after all hashes and EOF are verified.
/// Dropping a failed/incomplete transfer removes its temporary tree.
pub fn decode(
    reader: &mut impl Read,
    expected_sha256: &str,
    parent: &Path,
) -> Result<VerifiedTree> {
    crate::modules::validate_digest(expected_sha256)?;
    let mut magic = vec![0; MAGIC.len()];
    reader.read_exact(&mut magic)?;
    ensure!(magic == MAGIC, "invalid transfer framing");
    let mut len = [0; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    ensure!(len <= MAX_MANIFEST, "transfer manifest too large");
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    validate(&manifest)?;
    ensure!(
        manifest.sha256()? == expected_sha256,
        "transfer manifest SHA-256 mismatch"
    );
    let directory = tempfile::tempdir_in(parent)?;
    for (name, entry) in &manifest.entries {
        let path = directory.path().join(name);
        if entry.kind == "directory" {
            std::fs::create_dir(&path)?;
        } else {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            copy_exact(reader, &mut file, entry)?;
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode(if entry.executable { 0o700 } else { 0o600 }),
                )?;
            }
        }
    }
    let mut extra = [0u8; 1];
    ensure!(reader.read(&mut extra)? == 0, "trailing transfer bytes");
    Ok(VerifiedTree {
        directory,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, io::Cursor};
    #[test]
    fn roundtrip_checks_content_empty_directories_and_truncated_streams() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("empty")).unwrap();
        std::fs::write(source.join("code"), [0, 1, 2, 255]).unwrap();
        std::fs::write(source.join("zero"), []).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(source.join("code"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let tree = temp.path().join("frozen");
        let manifest = crate::trees::copy(&source, &tree).unwrap();
        let mut wire = Vec::new();
        encode(&tree, &manifest, &mut wire).unwrap();
        let digest = manifest.sha256().unwrap();
        let destination = temp.path().join("received");
        std::fs::create_dir(&destination).unwrap();
        let verified = decode(&mut Cursor::new(&wire), &digest, &destination).unwrap();
        assert_eq!(
            std::fs::read(verified.directory.path().join("code")).unwrap(),
            [0, 1, 2, 255]
        );
        assert!(verified.directory.path().join("empty").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(verified.directory.path().join("code"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        drop(verified);
        let mut corrupt = wire.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        let mut trailing = wire.clone();
        trailing.push(0);
        for bytes in [
            &corrupt[..],
            &trailing[..],
            &wire[..wire.len() - 1],
            &wire[..2],
        ] {
            assert!(decode(&mut Cursor::new(bytes), &digest, &destination).is_err());
            assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        }
        assert!(decode(&mut Cursor::new(&wire), &"0".repeat(64), &destination).is_err());
    }
    fn file() -> Entry {
        Entry {
            kind: "file".into(),
            size: 0,
            sha256: Some(crate::hashing::hex(Sha256::digest([]))),
            executable: false,
        }
    }
    #[test]
    fn rejects_paths_collisions_kinds_and_size_overflow_before_creating_tree() {
        let mut manifest = Manifest {
            format: crate::trees::FORMAT.into(),
            entries: BTreeMap::new(),
        };
        for path in [
            "../escape",
            "/absolute",
            "C:/drive",
            "a\\b",
            "a//b",
            "./dot",
            "CON",
            "tail.",
            "a/b",
            "",
        ] {
            manifest.entries = BTreeMap::from([(path.into(), file())]);
            assert!(validate(&manifest).is_err(), "{path}");
        }
        manifest.entries = BTreeMap::from([("a".into(), file()), ("A".into(), file())]);
        assert!(validate(&manifest).is_err());
        manifest.entries = BTreeMap::from([("file".into(), file())]);
        manifest.entries.get_mut("file").unwrap().size = u64::MAX;
        assert!(validate(&manifest).is_err());
        manifest.entries.get_mut("file").unwrap().kind = "symlink".into();
        assert!(validate(&manifest).is_err());
        let mut bad = MAGIC.to_vec();
        bad.extend_from_slice(&((MAX_MANIFEST + 1) as u32).to_be_bytes());
        let temp = tempfile::tempdir().unwrap();
        assert!(decode(&mut Cursor::new(bad), &"a".repeat(64), temp.path()).is_err());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    #[test]
    fn changed_source_is_not_encoded_as_the_old_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("file"), "before").unwrap();
        let root = temp.path().join("tree");
        let manifest = crate::trees::copy(&source, &root).unwrap();
        // Frozen copies are read-only; simulate replacement by another writer.
        std::fs::remove_file(root.join("file")).unwrap();
        std::fs::write(root.join("file"), "after!").unwrap();
        assert!(encode(&root, &manifest, &mut Vec::new()).is_err());
        #[cfg(unix)]
        {
            std::fs::remove_file(root.join("file")).unwrap();
            std::os::unix::fs::symlink(source.join("file"), root.join("file")).unwrap();
            assert!(encode(&root, &manifest, &mut Vec::new()).is_err());
        }
    }
}
