use super::*;
use std::{collections::BTreeSet, fs, process::Command};

const ROOT_FILES: &[&str] = &[
    ".gitignore",
    "Cargo.toml",
    "Cargo.lock",
    "README.md",
    "LICENSE",
    "SECURITY.md",
    "CONTRIBUTING.md",
    "PRIVACY.md",
    "preflight.example.toml",
    "install.sh",
];
const DIRECTORIES: &[&str] = &[
    ".github",
    "assets",
    "crates",
    "docs",
    "examples",
    "integrations",
    "skills",
    "packaging",
    "migrations",
    "Formula",
];
const SCRIPTS: &[&str] = &[
    "release.sh",
    "release-files.sh",
    "install-preflight.sh",
    "smoke-package.sh",
    "test-release.sh",
    "test-client-notices.sh",
    "test-without-node.sh",
    "test-script-entrypoints.sh",
    "client-notices.sh",
    "mcp-call.sh",
    "package-codex-plugin.sh",
    "test-codex-plugin.sh",
    "test-install-sh.sh",
    "test-client.sh",
    "test-client.cjs",
    "test-launchd.sh",
    "accept-homebrew.sh",
    "accept-storage-volume.sh",
    "export-public-source.sh",
    "test-public-export.sh",
    "homebrew-release.sh",
];
const LIBRARIES: &[&str] = &[
    "common.sh",
    "hash.sh",
    "acceptance.sh",
    "fixture.mjs",
    "public-acceptance.mjs",
    "run-public.mjs",
    "extract-release-binary.sh",
];
fn selected(relative: &str) -> bool {
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.first() == Some(&"examples")
        && !parts
            .get(1)
            .is_some_and(|p| ["go-basic", "c-basic", "cpp-basic"].contains(p))
    {
        return false;
    }
    ROOT_FILES.contains(&relative)
        || DIRECTORIES.contains(&parts[0])
        || (parts[0] == "scripts"
            && (parts.len() == 2 && SCRIPTS.contains(&parts[1])
                || parts.len() == 3 && parts[1] == "lib" && LIBRARIES.contains(&parts[2])))
}
pub(super) fn export(root: &Path, destination: &Path) -> Result<Value> {
    ensure!(!destination.exists(), "destination already exists");
    let listed = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()?;
    ensure!(listed.status.success(), "git source inventory failed");
    let private = regex::Regex::new(
        r"gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|-----BEGIN (?:OPENSSH |RSA |EC )?PRIVATE KEY-----",
    )?;
    let home = std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| format!("{s}/"));
    let mut paths = BTreeSet::new();
    for bytes in listed.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let relative = std::str::from_utf8(bytes)?;
        if !selected(relative) {
            continue;
        }
        ensure!(safe_name(relative), "unsafe source path: {relative}");
        let source = root.join(relative);
        let metadata = match fs::symlink_metadata(&source) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        ensure!(
            !metadata.is_symlink(),
            "publication refuses symlink: {relative}"
        );
        if !metadata.is_file() {
            continue;
        }
        ensure!(
            metadata.len() <= MAX_FILE,
            "source file exceeds limit: {relative}"
        );
        let data = fs::read(&source)?;
        let text = String::from_utf8_lossy(&data);
        ensure!(
            !home.as_ref().is_some_and(|h| text.contains(h)) && !private.is_match(&text),
            "review private data before export: {relative}"
        );
        paths.insert(relative.to_owned());
    }
    for required in [
        "Cargo.toml",
        ".github/workflows/ci.yml",
        ".github/workflows/release.yml",
        "LICENSE",
        "README.md",
        "install.sh",
    ] {
        ensure!(
            paths.contains(required),
            "missing public source: {required}"
        );
    }
    fs::create_dir_all(
        destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    fs::create_dir(destination)?;
    for relative in &paths {
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().context("source parent")?)?;
        fs::copy(root.join(relative), target)?;
    }
    for relative in paths
        .iter()
        .filter(|p| p.starts_with("skills/preflight-mcp/"))
    {
        let target = destination.join("integrations/codex").join(relative);
        fs::create_dir_all(target.parent().context("skill parent")?)?;
        fs::copy(root.join(relative), target)?;
    }
    fs::create_dir_all(destination.join("integrations/codex/assets"))?;
    fs::copy(
        root.join("assets/branding/preflight-icon.png"),
        destination.join("integrations/codex/assets/icon.png"),
    )?;
    fs::create_dir_all(destination.join(".agents/plugins"))?;
    write_new(
        &destination.join(".agents/plugins/marketplace.json"),
        &serde_json::to_vec_pretty(
            &json!({"name":"preflight","interface":{"displayName":"Preflight"},"plugins":[{"name":"preflight","source":{"source":"local","path":"./integrations/codex"},"policy":{"installation":"AVAILABLE","authentication":"ON_INSTALL"},"category":"Developer Tools"}]}),
        )?,
        0o644,
    )?;
    let includes = regex::Regex::new(r#"include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)"#)?;
    for relative in paths
        .iter()
        .filter(|s| s.starts_with("crates/") && s.ends_with(".rs"))
    {
        let rust = destination.join(relative);
        for found in includes.captures_iter(&fs::read_to_string(&rust)?) {
            ensure!(
                rust.parent()
                    .context("Rust parent")?
                    .join(&found[1])
                    .is_file(),
                "missing compile-time input: {relative} -> {}",
                &found[1]
            );
        }
    }
    Ok(json!({"files":paths.len(),"destination":absolute(destination)?,"history_included":false}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_boundary_excludes_runtime_and_internal_fixtures() {
        for name in [
            "data/preflight.sqlite3",
            "preflight.toml",
            ".git/config",
            ".validation/receipt.json",
            "target/release/preflight",
            "scripts/lib/acceptance.mjs",
            "scripts/accept-console.sh",
            "examples/private/input",
        ] {
            assert!(!selected(name), "{name}")
        }
        for name in [
            "scripts/client-notices.sh",
            "scripts/mcp-call.sh",
            "scripts/lib/fixture.mjs",
            "examples/cpp-basic/build.sh",
            "migrations/001.sql",
        ] {
            assert!(selected(name), "{name}")
        }
    }
}
