//! Standard age envelopes for portable archive files. Keys never enter requests or logs.
use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

pub const MAX_BYTES: u64 = 100 * 1024 * 1024 * 1024;
const ENVELOPE_OVERHEAD: u64 = 64 * 1024 * 1024;

fn output(path: &Path) -> Result<tempfile::NamedTempFile> {
    ensure!(!path.try_exists()?, "destination already exists");
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    crate::storage_gc::directory(parent)?;
    Ok(tempfile::NamedTempFile::new_in(parent)?)
}
fn publish(mut temporary: tempfile::NamedTempFile, target: &Path) -> Result<Value> {
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    let sha256 = crate::archives::file_hash(temporary.as_file_mut())?;
    let bytes = temporary.as_file().metadata()?.len();
    temporary.persist_noclobber(target).map_err(|v| v.error)?;
    #[cfg(unix)]
    File::open(
        target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(json!({"path":target,"sha256":sha256,"file_bytes":bytes}))
}
pub fn keygen(target: &Path) -> Result<Value> {
    let identity = age::x25519::Identity::generate();
    let mut temporary = output(target)?;
    writeln!(temporary, "{}", identity.to_string().expose_secret())?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(target).map_err(|v| v.error)?;
    Ok(
        json!({"identity_file":target,"recipient":identity.to_public().to_string(),"effect":"private key stored only in the new identity file; keep a separate protected copy"}),
    )
}
pub fn seal(source: &Path, destination: &Path, recipient: &str) -> Result<Value> {
    let recipient: age::x25519::Recipient = recipient
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid age X25519 recipient"))?;
    let mut input = crate::archives::open_read(source)?;
    ensure!(
        input.metadata()?.len() <= MAX_BYTES,
        "archive exceeds 100 GiB"
    );
    let mut temporary = output(destination)?;
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?;
    let mut writer = encryptor.wrap_output(temporary.as_file_mut())?;
    let size = std::io::copy(&mut (&mut input).take(MAX_BYTES + 1), &mut writer)?;
    ensure!(size <= MAX_BYTES, "archive exceeds 100 GiB");
    writer.finish()?;
    let mut result = publish(temporary, destination)?;
    result["format"] = "age-encryption.org/v1".into();
    result["recipient"] = recipient.to_string().into();
    result["effect"] = "new encrypted copy; source is unchanged".into();
    Ok(result)
}
pub fn unseal(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
    identity_file: &Path,
) -> Result<Value> {
    let identity = read_identity(identity_file)?;
    unseal_with_identity(source, destination, expected_sha256, &identity)
}
pub(crate) fn read_identity(identity_file: &Path) -> Result<age::x25519::Identity> {
    let identity = crate::archives::open_read(identity_file)?;
    ensure!(
        identity.metadata()?.len() <= 1024,
        "identity file too large"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            identity.metadata()?.permissions().mode() & 0o077 == 0,
            "identity file must be private (chmod 600)"
        );
    }
    let mut text = String::new();
    identity.take(1025).read_to_string(&mut text)?;
    let secret = age::secrecy::SecretString::from(text);
    let mut lines = secret
        .expose_secret()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let key = lines.next().context("empty age identity file")?;
    ensure!(
        lines.next().is_none(),
        "identity file must contain one X25519 identity"
    );
    let identity: age::x25519::Identity = key
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid age X25519 identity file"))?;
    Ok(identity)
}
pub(crate) fn identity_reference(name: &str) -> Result<()> {
    ensure!(
        name.starts_with("PREFLIGHT_ARCHIVE_IDENTITY_")
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
        "invalid archive identity environment reference"
    );
    Ok(())
}
pub(crate) fn identity_references() -> Vec<String> {
    let mut names: Vec<_> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| identity_reference(name).is_ok())
        .collect();
    names.sort();
    names.truncate(64);
    names
}
pub(crate) fn recipient(value: &str) -> Result<age::x25519::Recipient> {
    ensure!(value.len() <= 128, "invalid age recipient");
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid age X25519 recipient"))
}
pub(crate) fn seal_stream(
    input: &mut File,
    output: &mut File,
    public: &str,
    max: u64,
) -> Result<()> {
    input.seek(SeekFrom::Start(0))?;
    let recipient = recipient(public)?;
    let mut encrypted =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?
            .wrap_output(output)?;
    let bytes = std::io::copy(&mut input.take(max + 1), &mut encrypted)?;
    ensure!(bytes <= max, "archive exceeds encryption limit");
    encrypted.finish()?;
    Ok(())
}
pub(crate) fn is_encrypted(file: &mut File) -> Result<bool> {
    const MAGIC: &[u8] = b"age-encryption.org/v1\n";
    file.seek(SeekFrom::Start(0))?;
    let mut prefix = Vec::new();
    Read::by_ref(file)
        .take(MAGIC.len() as u64)
        .read_to_end(&mut prefix)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(prefix == MAGIC)
}
fn check_envelope_header(file: &mut File) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(Read::by_ref(file).take(65537));
    let mut used = 0;
    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line)?;
        used += n;
        ensure!(
            n > 0 && used <= 65536,
            "age header exceeds 64 KiB or is incomplete"
        );
        if line.starts_with(b"--- ") {
            break;
        }
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}
pub(crate) fn decoded(
    file: &mut File,
    reference: Option<&str>,
    directory: &Path,
    max: u64,
) -> Result<Option<tempfile::NamedTempFile>> {
    if !is_encrypted(file)? {
        return Ok(None);
    }
    let name = reference.context("encrypted archive requires identity_file_env")?;
    identity_reference(name)?;
    let path = std::env::var_os(name).context("archive identity reference is not configured")?;
    ensure!(
        Path::new(&path).is_absolute(),
        "configured archive identity file must use an absolute path"
    );
    let identity = read_identity(Path::new(&path))?;
    check_envelope_header(file)?;
    let decryptor = age::Decryptor::new(file)?;
    ensure!(
        !decryptor.is_scrypt(),
        "only X25519 age identities are supported"
    );
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .context("age identity cannot decrypt archive")?;
    let mut plain = tempfile::NamedTempFile::new_in(directory)?;
    let bytes = std::io::copy(&mut (&mut reader).take(max + 1), plain.as_file_mut())
        .context("age payload authentication failed")?;
    ensure!(bytes <= max, "decrypted archive exceeds limit");
    ensure!(
        reader.read(&mut [0u8; 1])? == 0,
        "decrypted archive exceeds limit"
    );
    plain.as_file_mut().seek(SeekFrom::Start(0))?;
    Ok(Some(plain))
}
pub(crate) fn verified_input(
    source: &Path,
    parent: &Path,
    expected_sha256: &str,
    max_bytes: u64,
) -> Result<tempfile::NamedTempFile> {
    ensure!(
        expected_sha256.len() == 64
            && expected_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "sha256 must be 64 lowercase hexadecimal characters"
    );
    let mut input = crate::archives::open_read(source)?;
    ensure!(
        input.metadata()?.len() <= max_bytes,
        "input file exceeds limit"
    );
    let mut copy = tempfile::NamedTempFile::new_in(parent)?;
    let bytes = std::io::copy(&mut (&mut input).take(max_bytes + 1), copy.as_file_mut())?;
    ensure!(bytes <= max_bytes, "input file exceeds limit");
    ensure!(
        crate::archives::file_hash(copy.as_file_mut())? == expected_sha256,
        "input file digest mismatch"
    );
    Ok(copy)
}
fn unseal_with_identity(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
    identity: &age::x25519::Identity,
) -> Result<Value> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut input = verified_input(
        source,
        parent,
        expected_sha256,
        MAX_BYTES + ENVELOPE_OVERHEAD,
    )?;
    check_envelope_header(input.as_file_mut())?;
    let decryptor = age::Decryptor::new(input.as_file_mut()).context("invalid age envelope")?;
    ensure!(
        !decryptor.is_scrypt(),
        "only X25519 age identities are supported"
    );
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .context("age identity cannot decrypt archive")?;
    let mut temporary = output(destination)?;
    let bytes = std::io::copy(
        &mut (&mut reader).take(MAX_BYTES + 1),
        temporary.as_file_mut(),
    )
    .context("age payload authentication failed")?;
    ensure!(bytes <= MAX_BYTES, "decrypted archive exceeds 100 GiB");
    // Consume EOF: publication requires authentication of the final chunk too.
    ensure!(
        reader.read(&mut [0u8; 1])? == 0,
        "decrypted archive exceeds limit"
    );
    let mut result = publish(temporary, destination)?;
    result["effect"] =
        "new decrypted file only; verify/import its archive format separately".into();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn server_envelope_reference_boundaries() {
        let mut oversized = tempfile::tempfile().unwrap();
        oversized.write_all(&vec![b'x'; 65537]).unwrap();
        assert!(check_envelope_header(&mut oversized).is_err());

        assert!(identity_reference("PREFLIGHT_ARCHIVE_IDENTITY_FILE_TEST").is_ok());
        for name in [
            "HOME",
            "PREFLIGHT_ARCHIVE_IDENTITY_path",
            "PREFLIGHT_ARCHIVE_IDENTITY_../key",
        ] {
            assert!(identity_reference(name).is_err());
        }
        let d = tempfile::tempdir().unwrap();
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(b"server archive stream").unwrap();
        let identity = age::x25519::Identity::generate();
        let mut sealed = tempfile::tempfile().unwrap();
        seal_stream(
            &mut input,
            &mut sealed,
            &identity.to_public().to_string(),
            1024,
        )
        .unwrap();
        assert!(is_encrypted(&mut sealed).unwrap());
        assert!(
            decoded(&mut sealed, None, d.path(), 1024)
                .unwrap_err()
                .to_string()
                .contains("identity_file_env")
        );
        assert!(decoded(&mut sealed, Some("HOME"), d.path(), 1024).is_err());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }
    fn digest(path: &Path) -> String {
        crate::archives::file_hash(&mut File::open(path).unwrap()).unwrap()
    }
    #[test]
    fn authenticated_roundtrip_and_no_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let plain = d.path().join("run.tar.zst");
        let cipher = d.path().join("run.age");
        let restored = d.path().join("restored");
        let identity = age::x25519::Identity::generate();
        let bytes = vec![b'x'; 131073];
        fs::write(&plain, &bytes).unwrap();
        seal(&plain, &cipher, &identity.to_public().to_string()).unwrap();
        assert!(
            fs::read(&cipher)
                .unwrap()
                .starts_with(b"age-encryption.org/v1\n")
        );
        assert!(seal(&plain, &cipher, &identity.to_public().to_string()).is_err());
        unseal_with_identity(&cipher, &restored, &digest(&cipher), &identity).unwrap();
        assert_eq!(fs::read(&restored).unwrap(), bytes);
        assert!(unseal_with_identity(&cipher, &restored, &digest(&cipher), &identity).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(restored).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
    #[test]
    fn wrong_identity_tamper_truncation_and_digest_fail_without_publication() {
        let d = tempfile::tempdir().unwrap();
        let plain = d.path().join("plain");
        let cipher = d.path().join("sealed");
        let target = d.path().join("target");
        let identity = age::x25519::Identity::generate();
        fs::write(&plain, vec![7; 131073]).unwrap();
        seal(&plain, &cipher, &identity.to_public().to_string()).unwrap();
        assert!(unseal_with_identity(&cipher, &target, &"0".repeat(64), &identity).is_err());
        assert!(
            unseal_with_identity(
                &cipher,
                &target,
                &digest(&cipher),
                &age::x25519::Identity::generate()
            )
            .is_err()
        );
        let original = fs::read(&cipher).unwrap();
        for n in [0, 20, original.len() - 1, original.len() - 17] {
            fs::write(&cipher, &original[..n]).unwrap();
            assert!(unseal_with_identity(&cipher, &target, &digest(&cipher), &identity).is_err());
            assert!(!target.exists());
        }
        let mut changed = original;
        let last = changed.len() - 1;
        changed[last] ^= 1;
        fs::write(&cipher, changed).unwrap();
        assert!(unseal_with_identity(&cipher, &target, &digest(&cipher), &identity).is_err());
        assert!(!target.exists());
    }
    #[test]
    fn private_identity_file_and_symlink_refusal() {
        let d = tempfile::tempdir().unwrap();
        let key = d.path().join("identity");
        let public = keygen(&key).unwrap();
        assert!(public["recipient"].as_str().unwrap().starts_with("age1"));
        assert!(keygen(&key).is_err());
        let secret = fs::read_to_string(&key).unwrap();
        fs::write(
            &key,
            format!("# age-keygen comment\n\n{secret}\n# trailing comment\n"),
        )
        .unwrap();
        let plain = d.path().join("plain");
        let cipher = d.path().join("cipher");
        let restored = d.path().join("restored");
        fs::write(&plain, []).unwrap();
        seal(&plain, &cipher, public["recipient"].as_str().unwrap()).unwrap();
        unseal(&cipher, &restored, &digest(&cipher), &key).unwrap();
        assert_eq!(fs::read(&restored).unwrap(), b"");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(unseal(&cipher, &d.path().join("other"), &digest(&cipher), &key).is_err());
            let link = d.path().join("link");
            std::os::unix::fs::symlink(plain, &link).unwrap();
            assert!(
                seal(
                    &link,
                    &d.path().join("bad"),
                    public["recipient"].as_str().unwrap()
                )
                .is_err()
            );
        }
    }
}
