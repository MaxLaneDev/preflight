use super::*;
use std::{fs, io::Write, process::Command};

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let result = command.output()?;
    ensure!(
        result.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}
fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = vec![];
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                pending.push(entry.path())
            } else if ty.is_file() {
                result.push(entry.path())
            }
        }
    }
    result.sort();
    Ok(result)
}
fn add(files: &mut BTreeMap<String, Vec<u8>>, name: &str, path: &Path) -> Result<()> {
    ensure!(safe_name(name), "unsafe payload name");
    ensure!(!fs::symlink_metadata(path)?.is_symlink(), "symlink payload");
    ensure!(
        fs::metadata(path)?.len() <= MAX_FILE,
        "payload exceeds limit"
    );
    files.insert(name.into(), fs::read(path)?);
    Ok(())
}
fn licenses(root: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Result<Vec<Value>> {
    let rustc = String::from_utf8(output(Command::new("rustc").arg("-vV"))?)?;
    let host = rustc
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .context("missing Rust target")?;
    let meta: Value =
        serde_json::from_slice(&output(Command::new("cargo").current_dir(root).args([
            "metadata",
            "--locked",
            "--offline",
            "--filter-platform",
            host,
            "--format-version",
            "1",
        ]))?)?;
    let mut selected = std::collections::BTreeSet::new();
    let mut pending = meta["workspace_members"]
        .as_array()
        .context("workspace members")?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if !selected.insert(id.clone()) {
            continue;
        }
        if let Some(node) = meta["resolve"]["nodes"]
            .as_array()
            .and_then(|a| a.iter().find(|n| n["id"] == id))
        {
            for dep in node["deps"].as_array().context("dependency list")? {
                if dep["dep_kinds"]
                    .as_array()
                    .context("dependency kinds")?
                    .iter()
                    .any(|k| k["kind"] != "dev")
                {
                    pending.push(dep["pkg"].as_str().context("dependency ID")?.into());
                }
            }
        }
    }
    let mut rows = vec![];
    for package in meta["packages"].as_array().context("packages")? {
        if package["source"].is_null()
            || !selected.contains(package["id"].as_str().context("package ID")?)
        {
            continue;
        }
        let parent = Path::new(package["manifest_path"].as_str().context("manifest path")?)
            .parent()
            .context("manifest parent")?;
        let mut found = vec![];
        for path in walk(parent)? {
            let filename = path
                .file_name()
                .context("license filename")?
                .to_string_lossy()
                .to_lowercase();
            let relative = path
                .strip_prefix(parent)?
                .to_str()
                .context("license path")?;
            if (["license", "licence", "copying", "notice"]
                .iter()
                .any(|p| filename.starts_with(p))
                || package["license_file"] == relative)
                && fs::metadata(&path)?.len() <= 128 * 1024
            {
                let name = format!(
                    "licenses/cargo/{}-{}/{relative}",
                    package["name"].as_str().context("name")?,
                    package["version"].as_str().context("version")?
                );
                add(files, &name, &path)?;
                found.push(name);
            }
        }
        rows.push(json!({"name":package["name"],"version":package["version"],"license":package["license"],"files":found}));
    }
    Ok(rows)
}
fn append(
    builder: &mut tar::Builder<flate2::write::GzEncoder<fs::File>>,
    name: &str,
    bytes: &[u8],
    executable: bool,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(if executable { 0o755 } else { 0o644 });
    header.set_mtime(0);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)?;
    Ok(())
}
fn package(root: &Path, server: &Path, client: Option<&Path>, destination: &Path) -> Result<Value> {
    ensure!(!destination.exists(), "destination already exists");
    fs::create_dir_all(destination)?;
    let mut common = BTreeMap::new();
    add(&mut common, "LICENSE", &root.join("LICENSE"))?;
    let mut notices = licenses(root, &mut common)?;
    let js: Value = serde_json::from_slice(&fs::read(
        root.join("crates/preflight-client/THIRD_PARTY_NOTICES.json"),
    )?)?;
    ensure!(
        js["lock_sha256"] == hash(&root.join("crates/preflight-client/package-lock.json"))?
            && js["bundle_sha256"]
                == hash(&root.join("crates/preflight-client/src/editor-bundle.mjs"))?,
        "regenerate client notices after changing bundle or lock"
    );
    for (name, text) in js["files"].as_object().context("client license files")? {
        ensure!(safe_name(name), "unsafe license name");
        common.insert(
            name.clone(),
            text.as_str().context("license text")?.as_bytes().to_vec(),
        );
    }
    notices.extend(
        js["packages"]
            .as_array()
            .context("client packages")?
            .iter()
            .cloned(),
    );
    common.insert(
        "THIRD_PARTY_NOTICES.json".into(),
        serde_json::to_vec_pretty(&notices)?,
    );
    let mut result = vec![];
    for (binary, component) in [(Some(server), "preflight"), (client, "preflight-client")] {
        let Some(binary) = binary else { continue };
        let binary = binary.canonicalize()?;
        let info: Value = serde_json::from_slice(&output(Command::new(&binary).arg("--version"))?)?;
        ensure!(info["component"] == component, "component mismatch");
        let version = info["version"].as_str().context("missing version")?;
        semver::Version::parse(version)?;
        let os = info["os"].as_str().context("missing OS")?;
        let arch = info["arch"].as_str().context("missing architecture")?;
        ensure!(
            [os, arch].iter().all(|s| !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')),
            "invalid platform"
        );
        let name = format!("{component}{}", if os == "windows" { ".exe" } else { "" });
        let mut files = common.clone();
        add(&mut files, &name, &binary)?;
        if component == "preflight" {
            add(
                &mut files,
                "install-preflight.sh",
                &root.join("scripts/install-preflight.sh"),
            )?;
        }
        files.insert("README.md".into(),b"Preflight Web UI + MCP\n\nRun: preflight --config /absolute/preflight.toml serve\nWeb: http://127.0.0.1:4170/\nMCP: http://127.0.0.1:4170/mcp\n\nInstall: sh install-preflight.sh install ARCHIVE --sha256 SHA256 --prefix BINARIES --config CONFIG --data DATA\nKeep configuration and data outside BINARIES. Installation never starts or restarts services.\nBinary rollback does not undo database migrations. Back up data before upgrades.\n".to_vec());
        let entries = files
            .iter()
            .map(|(key, bytes)| {
                use sha2::{Digest, Sha256};
                (
                    key.clone(),
                    Entry {
                        sha256: crate::hashing::hex(Sha256::digest(bytes)),
                        size: bytes.len() as u64,
                        executable: key == &name || key == "install-preflight.sh",
                    },
                )
            })
            .collect();
        let manifest = Manifest {
            format: "preflight-package-v1".into(),
            component: component.into(),
            binary: name,
            metadata: info.clone(),
            packaging_lock_sha256: Some(hash(&root.join("Cargo.lock"))?),
            files: entries,
        };
        let path = destination.join(format!("{component}-{version}-{os}-{arch}.tar.gz"));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let gzip = flate2::GzBuilder::new()
            .mtime(0)
            .write(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(gzip);
        append(
            &mut tar,
            "manifest.json",
            &serde_json::to_vec(&manifest)?,
            false,
        )?;
        for (name, bytes) in &files {
            append(&mut tar, name, bytes, manifest.files[name].executable)?;
        }
        tar.into_inner()?.finish()?.sync_all()?;
        verify(&path, None)?;
        let sha = hash(&path)?;
        write_new(
            &path.with_file_name(format!(
                "{}.sha256",
                path.file_name().context("archive name")?.to_string_lossy()
            )),
            format!(
                "{sha}  {}\n",
                path.file_name().context("archive name")?.to_string_lossy()
            )
            .as_bytes(),
            0o644,
        )?;
        result.push(json!({"archive":path,"sha256":sha,"metadata":info}));
    }
    fs::copy(
        root.join("scripts/install-preflight.sh"),
        destination.join("install-preflight.sh"),
    )?;
    Ok(json!(result))
}
fn formula(
    root: &Path,
    manifest: &Manifest,
    sha: &str,
    url: &str,
    homepage: &str,
    destination: &Path,
) -> Result<Value> {
    ensure!(
        manifest.component == "preflight"
            && manifest.metadata["os"] == "macos"
            && manifest.metadata["arch"] == "aarch64",
        "formula requires unified macOS ARM64 package"
    );
    for value in [url, homepage] {
        ensure!(
            value.starts_with("https://")
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"./_%~:+-".contains(&b)),
            "formula URLs must be safe HTTPS URLs"
        );
    }
    let version = manifest.metadata["version"]
        .as_str()
        .context("missing version")?;
    semver::Version::parse(version)?;
    let mut content = fs::read_to_string(root.join("packaging/preflight.rb.in"))?;
    for (key, value) in [
        ("URL", url),
        ("HOMEPAGE", homepage),
        ("VERSION", version),
        ("SHA256", sha),
    ] {
        content = content.replace(&format!("@{key}@"), value);
    }
    write_new(destination, content.as_bytes(), 0o644)?;
    Ok(
        json!({"formula":destination,"sha256":hash(destination)?,"publication":"not performed","platform":"macos-aarch64"}),
    )
}
fn plugin(root: &Path, destination: &Path, zipped: bool) -> Result<Value> {
    ensure!(!destination.exists(), "destination already exists");
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = tempfile::tempdir_in(parent)?;
    let plugin = temp.path().join("plugins/preflight");
    fs::create_dir_all(&plugin)?;
    for relative in [
        ".codex-plugin/plugin.json",
        ".mcp.json",
        "hooks/hooks.json",
        "hooks/preflight.sh",
    ] {
        let target = plugin.join(relative);
        fs::create_dir_all(target.parent().context("plugin parent")?)?;
        fs::copy(root.join("integrations/codex").join(relative), target)?;
    }
    for path in walk(&root.join("skills/preflight-mcp"))? {
        if path.extension().is_some_and(|e| e == "md") {
            let target = plugin
                .join("skills/preflight-mcp")
                .join(path.strip_prefix(root.join("skills/preflight-mcp"))?);
            fs::create_dir_all(target.parent().context("skill parent")?)?;
            fs::copy(path, target)?;
        }
    }
    fs::create_dir_all(plugin.join("assets"))?;
    fs::copy(
        root.join("assets/branding/preflight-icon.png"),
        plugin.join("assets/icon.png"),
    )?;
    if zipped {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        let mut zip = zip::ZipWriter::new(file);
        for path in walk(&plugin)? {
            let name = path.strip_prefix(&plugin)?.to_str().context("ZIP path")?;
            zip.start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(if name.ends_with(".sh") { 0o755 } else { 0o644 }),
            )?;
            zip.write_all(&fs::read(path)?)?;
        }
        zip.finish()?.sync_all()?;
    } else {
        fs::create_dir_all(destination.parent().context("marketplace parent")?)?;
        fs::create_dir(destination)?;
        fs::rename(temp.path().join("plugins"), destination.join("plugins"))?;
        fs::create_dir_all(destination.join(".agents/plugins"))?;
        write_new(
            &destination.join(".agents/plugins/marketplace.json"),
            &serde_json::to_vec_pretty(
                &json!({"name":"preflight-local","interface":{"displayName":"Preflight Local"},"plugins":[{"name":"preflight","source":{"source":"local","path":"./plugins/preflight"},"policy":{"installation":"AVAILABLE","authentication":"ON_INSTALL"},"category":"Productivity"}]}),
            )?,
            0o644,
        )?;
    }
    Ok(json!(destination))
}
pub(super) async fn run(operation: &str, mut args: Args) -> Result<Value> {
    let root = args
        .options
        .remove("--repository")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    match operation {
        "create" => {
            let server = args.path("--server")?;
            let client = args.options.remove("--client").map(PathBuf::from);
            let dest = args.path("--destination")?;
            args.finish()?;
            package(&root, &server, client.as_deref(), &dest)
        }
        "plugin" => {
            let dest = PathBuf::from(args.one()?);
            let zipped = args.zip;
            args.zip = false;
            args.finish()?;
            plugin(&root, &dest, zipped)
        }
        "formula" => {
            let source = args.one()?;
            let sha = args.take("--sha256")?;
            let url = args.take("--url")?;
            let home = args.take("--homepage")?;
            let dest = args.path("--destination")?;
            args.finish()?;
            let file = snapshot(&source, &sha).await?;
            formula(&root, &verify(file.path(), None)?, &sha, &url, &home, &dest)
        }
        _ => bail!("unknown package operation: {operation}"),
    }
}
