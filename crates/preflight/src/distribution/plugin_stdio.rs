//! Hook-free, single-platform plugin built from a verified server release.
use super::*;
use std::{fs, io::Write};

pub(super) async fn package(
    root: &Path,
    source: &str,
    sha: &str,
    destination: &Path,
) -> Result<Value> {
    ensure!(!destination.exists(), "destination already exists");
    let source = snapshot(source, sha).await?;
    let payload = tempfile::tempdir()?;
    let manifest = verify(source.path(), Some(payload.path()))?;
    ensure!(manifest.component == "preflight", "server package required");
    let version = manifest.metadata["version"]
        .as_str()
        .context("missing version")?;
    ensure!(
        version == env!("CARGO_PKG_VERSION"),
        "package and plugin builder versions differ"
    );
    ensure!(
        matches!(
            (
                manifest.metadata["os"].as_str(),
                manifest.metadata["arch"].as_str()
            ),
            (Some("macos" | "linux"), Some("aarch64"))
                | (Some("macos" | "linux" | "windows"), Some("x86_64"))
        ),
        "unsupported stdio plugin platform"
    );
    ensure!(
        manifest.files[&manifest.binary].executable,
        "server binary must be executable"
    );
    for name in ["LICENSE", "THIRD_PARTY_NOTICES.json"] {
        ensure!(
            manifest.files.contains_key(name),
            "server package missing {name}"
        );
    }
    let plugin: Value =
        serde_json::from_slice(&fs::read(root.join("integrations/codex/submission.json"))?)?;
    ensure!(
        plugin["version"] == version && plugin["name"] == "preflight",
        "submission manifest version or identity differs"
    );
    let plugin_binary = format!("bin/{}", manifest.binary);
    let mcp = json!({
        "$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
        "mcpServers":{"preflight":{"type":"stdio","command":format!("./{plugin_binary}"),"args":["mcp-proxy"],"cwd":"${PLUGIN_ROOT}"}}
    });
    let mut files = BTreeMap::new();
    for name in manifest.files.keys().filter(|n| {
        n.as_str() == "LICENSE"
            || n.as_str() == "THIRD_PARTY_NOTICES.json"
            || n.starts_with("licenses/")
    }) {
        files.insert(name.clone(), fs::read(payload.path().join(name))?);
    }
    files.insert(
        plugin_binary.clone(),
        fs::read(payload.path().join(&manifest.binary))?,
    );
    files.insert("plugin.json".into(), serde_json::to_vec_pretty(&plugin)?);
    files.insert("mcp.json".into(), serde_json::to_vec_pretty(&mcp)?);
    for name in ["README.md", "setup.sh", "main.c", "pipeline.json"] {
        files.insert(
            format!("review/{name}"),
            fs::read(root.join("integrations/codex/review").join(name))?,
        );
    }
    files.insert(
        "assets/icon.png".into(),
        fs::read(root.join("assets/branding/preflight-icon.png"))?,
    );
    // Copy only the maintained skill and references, never local plugin/runtime state.
    let skills = root.join("skills/preflight-mcp");
    ensure!(
        !fs::symlink_metadata(&skills)?.is_symlink(),
        "symlink skill root"
    );
    for path in super::tooling::walk(&skills)? {
        if path.extension().is_some_and(|e| e == "md") {
            let name = format!(
                "skills/preflight-mcp/{}",
                crate::pipeline::portable_relative(path.strip_prefix(&skills)?)?
            );
            ensure!(safe_name(&name), "unsafe skill path");
            files.insert(name, fs::read(path)?);
        }
    }
    ensure!(
        files.contains_key("skills/preflight-mcp/SKILL.md"),
        "missing MCP skill"
    );
    files.insert(
        "server-package.json".into(),
        serde_json::to_vec_pretty(&json!({"sha256":sha,"metadata":manifest.metadata}))?,
    );
    files.insert("README.md".into(), format!("# Preflight plugin\n\nPlatform: {} {}. Version: {version}.\n\nInstall and start the server first: https://github.com/MaxLaneDev/preflight/blob/main/docs/server-setup.md\nThe bundled binary runs only `mcp-proxy`, connecting to http://127.0.0.1:4170/mcp. It does not start a second server or open its database. Node and Python are not required.\n\nFor another port, edit mcp.json args to include --server and a loopback /mcp URL. For authentication, add --token-env PREFLIGHT_API_TOKEN and supply it through your client's private environment. Never put credentials in this package.\n\nThis archive contains no lifecycle hooks. Completion delivery is configured separately. Directory submission still requires OpenAI approval for local MCP; this archive does not prove eligibility or review acceptance.\n", manifest.metadata["os"].as_str().unwrap(), manifest.metadata["arch"].as_str().unwrap()).into_bytes());
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut zip = zip::ZipWriter::new(temporary);
    for (name, bytes) in files {
        zip.start_file(
            &name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(if name == plugin_binary { 0o755 } else { 0o644 }),
        )?;
        zip.write_all(&bytes)?;
    }
    let temporary = zip.finish()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(destination)
        .map_err(|e| e.error)?;
    Ok(
        json!({"archive":destination,"sha256":hash(destination)?,"server_sha256":sha,"metadata":manifest.metadata,"directory_submission":"not performed"}),
    )
}
