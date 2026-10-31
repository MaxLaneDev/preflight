use super::*;
use std::{fs, io::Write};

#[derive(Clone, Serialize, Deserialize)]
struct State {
    format: String,
    component: String,
    binary: String,
    current: String,
    previous: Option<String>,
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn lock(root: &Path) -> Result<Lock> {
    let path = root.join(".preflight-install.lock");
    write_new(&path, format!("{}\n", std::process::id()).as_bytes(), 0o600).context(
        "installer busy or interrupted; inspect .preflight-install.lock before recovery",
    )?;
    Ok(Lock(path))
}
fn state(root: &Path) -> Result<State> {
    no_symlink(&root.join(".preflight-install.json"))?;
    let file = crate::archives::open_read(&root.join(".preflight-install.json"))?;
    ensure!(
        file.metadata()?.len() <= 65536,
        "installation record too large"
    );
    let mut value: State = serde_json::from_reader(file)?;
    ensure!(
        value.format == "preflight-install-v1"
            && digest(&value.current)
            && value.previous.as_ref().is_none_or(|v| digest(v)),
        "invalid installation record"
    );
    let target = fs::read_link(root.join("current"))?;
    let name = target
        .file_name()
        .and_then(|v| v.to_str())
        .context("invalid activation pointer")?;
    ensure!(
        digest(name) && target == Path::new("versions").join(name),
        "invalid activation pointer"
    );
    if name != value.current {
        value.previous = Some(value.current);
        value.current = name.into();
    }
    Ok(value)
}
fn activate(root: &Path, value: &State) -> Result<Value> {
    ensure!(digest(&value.current), "invalid version ID");
    let current = root.join("current");
    if let Ok(meta) = fs::symlink_metadata(&current) {
        ensure!(
            meta.is_symlink(),
            "current must be an owned activation symlink"
        );
    }
    let temporary = root.join(format!(".activate-{}", uuid::Uuid::now_v7()));
    #[cfg(unix)]
    std::os::unix::fs::symlink(Path::new("versions").join(&value.current), &temporary)?;
    #[cfg(not(unix))]
    bail!("package activation currently supports Unix hosts only");
    fs::rename(&temporary, current)?;
    let mut record = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer_pretty(&mut record, value)?;
    record.write_all(b"\n")?;
    record.as_file().sync_all()?;
    record
        .persist(root.join(".preflight-install.json"))
        .map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(root)?.sync_all()?;
    Ok(
        json!({"component":value.component,"current":value.current,"binary":root.join("versions").join(&value.current).join(&value.binary),"restart":"not performed; existing processes and data are untouched"}),
    )
}
fn all_files(root: &Path) -> Result<std::collections::BTreeSet<String>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = std::collections::BTreeSet::new();
    let mut count = 0;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            ensure!(!meta.is_symlink(), "installed version contains a symlink");
            count += 1;
            ensure!(
                count <= 10000,
                "installed version contains too many entries"
            );
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                ensure!(meta.is_file(), "installed version contains a special file");
                files.insert(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_str()
                        .context("non-UTF-8 package path")?
                        .replace('\\', "/"),
                );
            }
        }
    }
    Ok(files)
}
fn verify_installed(version: &Path, expected: Option<&Manifest>) -> Result<Manifest> {
    no_symlink(version)?;
    let manifest_file = crate::archives::open_read(&version.join("manifest.json"))?;
    ensure!(
        manifest_file.metadata()?.len() <= 1024 * 1024,
        "installed manifest exceeds 1 MiB"
    );
    let manifest: Manifest = serde_json::from_reader(manifest_file)?;
    ensure!(
        manifest.format == "preflight-package-v1" && (1..=4000).contains(&manifest.files.len()),
        "invalid installed manifest"
    );
    if let Some(expected) = expected {
        ensure!(
            serde_json::to_value(&manifest)? == serde_json::to_value(expected)?,
            "installed manifest was modified"
        );
    }
    let mut names = manifest
        .files
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    names.insert("manifest.json".into());
    ensure!(
        all_files(version)? == names,
        "installed version contains unrelated or missing files"
    );
    for (name, entry) in &manifest.files {
        ensure!(safe_name(name), "unsafe installed path");
        let file = version.join(name);
        ensure!(
            fs::metadata(&file)?.len() == entry.size && hash(&file)? == entry.sha256,
            "installed version was modified"
        );
    }
    Ok(manifest)
}
pub(super) async fn install(
    source: &str,
    expected: &str,
    prefix: &Path,
    configuration: &Path,
    data: Option<&Path>,
) -> Result<Value> {
    no_symlink(prefix)?;
    no_symlink(configuration)?;
    let root = absolute(prefix)?;
    let config = absolute(configuration)?;
    let data = absolute(
        data.unwrap_or(
            &config
                .parent()
                .context("config parent missing")?
                .join("data"),
        ),
    )?;
    ensure!(
        !config.starts_with(&root) && !data.starts_with(&root),
        "configuration/data must be outside the binary installation"
    );
    let archive = snapshot(source, expected).await?;
    let manifest = verify(archive.path(), None)?;
    ensure!(
        manifest.component == "preflight",
        "installer requires the unified server package"
    );
    ensure!(
        manifest.metadata["os"] == std::env::consts::OS
            && manifest.metadata["arch"] == std::env::consts::ARCH,
        "package platform does not match this host"
    );
    if root.exists() {
        ensure!(
            root.join(".preflight-install.json").is_file() || fs::read_dir(&root)?.next().is_none(),
            "destination contains unrelated data"
        );
    }
    fs::create_dir_all(&root)?;
    let _lock = lock(&root)?;
    let old = if root.join(".preflight-install.json").exists() {
        Some(state(&root)?)
    } else {
        None
    };
    if let Some(old) = &old {
        ensure!(
            old.component == manifest.component,
            "cannot replace a different component"
        );
    }
    let versions = root.join("versions");
    no_symlink(&versions)?;
    fs::create_dir_all(&versions)?;
    let temporary = tempfile::tempdir_in(&versions)?;
    verify(archive.path(), Some(temporary.path()))?;
    write_new(
        &temporary.path().join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
        0o644,
    )?;
    let version = versions.join(expected);
    if version.exists() {
        verify_installed(&version, Some(&manifest))?;
    } else {
        fs::rename(temporary.path(), &version)?;
    }
    let previous = old.as_ref().and_then(|v| {
        if v.current != expected {
            Some(v.current.clone())
        } else {
            v.previous.clone()
        }
    });
    let value = State {
        format: "preflight-install-v1".into(),
        component: manifest.component,
        binary: manifest.binary,
        current: expected.into(),
        previous,
    };
    let mut result = activate(&root, &value)?;
    if !config.exists() {
        fs::create_dir_all(config.parent().context("config parent missing")?)?;
        let credential = config.with_file_name(format!(
            "{}.admin-token",
            config
                .file_stem()
                .and_then(|s| s.to_str())
                .context("invalid config filename")?
        ));
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|_| anyhow::anyhow!("credential entropy unavailable"))?;
        write_new(
            &credential,
            format!("{}\n", crate::hashing::hex(random)).as_bytes(),
            0o600,
        )?;
        let contents = json!({"version":1,"bind":"127.0.0.1:4170","data_dir":data,"allowed_roots":[],"admin_token_file":credential});
        write_new(&config, toml::to_string(&contents)?.as_bytes(), 0o600)?;
        result["admin_token_file"] = json!(credential);
    }
    result["configuration"] = json!(config);
    result["start"] = json!([
        root.join("current").join(&value.binary).to_string_lossy(),
        "--config",
        config.to_string_lossy(),
        "serve"
    ]);
    result["url"] = json!("http://127.0.0.1:4170");
    result["mcp"] = json!("http://127.0.0.1:4170/mcp");
    Ok(result)
}
pub(super) fn manage(operation: &str, prefix: &Path) -> Result<Value> {
    no_symlink(prefix)?;
    let root = absolute(prefix)?;
    if operation == "status" {
        return Ok(serde_json::to_value(state(&root)?)?);
    }
    let _lock = lock(&root)?;
    let mut value = state(&root)?;
    if operation == "rollback" {
        let previous = value.previous.clone().context("no previous installation")?;
        let manifest = verify_installed(&root.join("versions").join(&previous), None)?;
        ensure!(
            manifest.component == value.component && manifest.binary == value.binary,
            "previous component mismatch"
        );
        value.previous = Some(value.current);
        value.current = previous;
        return activate(&root, &value);
    }
    ensure!(
        operation == "uninstall" && value.component == "preflight",
        "uninstall requires a unified server installation"
    );
    let children = fs::read_dir(&root)?
        .map(|p| p.map(|p| p.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
    ensure!(
        children
            == std::collections::BTreeSet::from([
                "versions".into(),
                "current".into(),
                ".preflight-install.json".into(),
                ".preflight-install.lock".into()
            ]),
        "installation contains unrelated files; refusing uninstall"
    );
    let processes = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()?;
    ensure!(
        processes.status.success(),
        "cannot inspect active processes"
    );
    for line in std::str::from_utf8(&processes.stdout)?.lines() {
        let line = line.trim();
        let Some((pid, command)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if pid.parse::<u32>()? != std::process::id() {
            let command = command.trim_start();
            ensure!(
                !command.starts_with(&root.join("current").to_string_lossy().into_owned())
                    && !command.starts_with(&root.join("versions").to_string_lossy().into_owned()),
                "stop the installed service before uninstalling"
            );
        }
    }
    let versions = root.join("versions");
    no_symlink(&versions)?;
    for version in fs::read_dir(&versions)? {
        let version = version?;
        ensure!(
            digest(
                version
                    .file_name()
                    .to_str()
                    .context("invalid installed version")?
            ),
            "unrecognized installed version"
        );
        verify_installed(&version.path(), None)?;
    }
    fs::remove_dir_all(&root)?;
    Ok(
        json!({"uninstalled":true,"configuration_and_data":"preserved","service":"not stopped or removed by installer"}),
    )
}
