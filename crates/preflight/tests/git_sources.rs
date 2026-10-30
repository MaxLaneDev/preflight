mod common;
use common::*;
use preflight::{
    model::RunState,
    pipeline::PipelineFile,
    sources::{GitSource, Patch, StartGitPipeline},
};
use serde_json::json;
use std::path::Path;
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn setup(f: &Fixture) -> String {
    std::fs::write(
        f.project.root.join("main.rs"),
        "fn main(){println!(\"base\");}\n",
    )
    .unwrap();
    std::fs::write(f.project.root.join("note"), "original\n").unwrap();
    git(&f.project.root, &["init", "-q"]);
    git(&f.project.root, &["config", "user.name", "Preflight test"]);
    git(
        &f.project.root,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&f.project.root, &["add", "."]);
    git(&f.project.root, &["commit", "-qm", "base"]);
    String::from_utf8(git(&f.project.root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .into()
}
fn definition() -> PipelineFile {
    serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"compile","command":{"program":"rustc","args":["main.rs","-o","program.exe"]},"outputs":{"program":{"path":"program.exe","type":"executable"}}},{"id":"execute","needs":["compile"],"command":{"program":"${artifact.compile.program}","args":[]}}]}})).unwrap()
}
fn patch(before: &str, after: &str) -> Patch {
    Patch {
        name: format!("{before}-to-{after}"),
        diff: format!(
            "diff --git a/main.rs b/main.rs\n--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-fn main(){{println!(\"{before}\");}}\n+fn main(){{println!(\"{after}\");}}\n"
        ),
    }
}
#[tokio::test]
async fn exact_commit_patches_and_failure_leave_checkout_and_index_unchanged() {
    let f = Fixture::new();
    assert!(f.temp.path().exists());
    let commit = setup(&f);
    std::fs::write(
        f.project.root.join("main.rs"),
        "compile_error!(\"dirty checkout must not build\");\n",
    )
    .unwrap();
    std::fs::write(f.project.root.join("note"), "staged change\n").unwrap();
    git(&f.project.root, &["add", "note"]);
    std::fs::write(f.project.root.join("untracked"), "private local input").unwrap();
    let status = git(&f.project.root, &["status", "--porcelain=v1", "-z"]);
    let index = std::fs::read(f.project.root.join(".git/index")).unwrap();
    let start = |patches| {
        f.engine
            .start_git_pipeline(StartGitPipeline {
                project_id: f.project.id.clone(),
                definition: definition(),
                source: GitSource {
                    commit: commit.clone(),
                    patches,
                },
            })
            .unwrap()
    };
    let base = start(vec![]);
    assert_eq!(
        finished(&f.engine, &base.id).await.status,
        RunState::Succeeded
    );
    assert!(output(&f.engine, &base.id, "stdout").contains("base"));
    assert!(!base.workspace.join("source/untracked").exists());
    let patches = vec![patch("base", "middle"), patch("middle", "final")];
    let patched = start(patches.clone());
    let done = finished(&f.engine, &patched.id).await;
    assert_eq!(done.status, RunState::Succeeded, "{done:?}");
    assert!(output(&f.engine, &patched.id, "stdout").contains("final"));
    let evidence = f.engine.db.source_evidence(&patched.id).unwrap();
    assert_eq!(evidence["prepared"]["resolved_commit"], commit);
    assert_eq!(evidence["prepared"]["patches"].as_array().unwrap().len(), 2);
    assert_ne!(
        evidence["tree_sha256"],
        f.engine.db.source_evidence(&base.id).unwrap()["tree_sha256"]
    );
    assert_eq!(
        evidence["configuration_sha256"],
        f.engine.db.source_evidence(&base.id).unwrap()["configuration_sha256"]
    );
    let saved = f
        .engine
        .db
        .source_patch(&preflight::sources::PatchQuery {
            run_id: patched.id.clone(),
            ordinal: 1,
            root_name: None,
        })
        .unwrap();
    assert_eq!(saved["patch"]["diff"], patches[1].diff);
    let reversed = start(patches.into_iter().rev().collect());
    assert_eq!(
        finished(&f.engine, &reversed.id).await.status,
        RunState::InfrastructureError
    );
    assert!(!reversed.workspace.join("source").exists());
    assert!(output(&f.engine, &reversed.id, "stdout").is_empty());
    assert!(f.engine.db.source_evidence(&reversed.id).unwrap()["prepared"].is_null());
    assert_eq!(
        git(&f.project.root, &["status", "--porcelain=v1", "-z"]),
        status
    );
    assert_eq!(
        std::fs::read(f.project.root.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        String::from_utf8(git(&f.project.root, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim(),
        commit
    );
    assert!(
        std::fs::read_to_string(f.project.root.join("main.rs"))
            .unwrap()
            .contains("dirty checkout")
    );
    let reopened =
        preflight::db::Db::new(&f.engine.config.data_dir.join("preflight.sqlite3")).unwrap();
    assert_eq!(reopened.source_evidence(&patched.id).unwrap(), evidence);
    let ordinary = f.start(&[], 5);
    assert_eq!(
        finished(&f.engine, &ordinary.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        f.engine.db.source_evidence(&ordinary.id).unwrap()["mode"],
        "directory"
    );
}

#[tokio::test]
async fn invalid_objects_paths_and_submodules_never_launch_stages() {
    let f = Fixture::new();
    let base = setup(&f);
    let outside = f.temp.path().join("outside");
    std::fs::write(&outside, "untouched").unwrap();
    let start = |commit: String, patches| {
        f.engine
            .start_git_pipeline(StartGitPipeline {
                project_id: f.project.id.clone(),
                definition: definition(),
                source: GitSource { commit, patches },
            })
            .unwrap()
    };
    for path in ["../outside", ".git/config", ".preflight/pipeline.yaml"] {
        let diff = format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+forbidden\n"
        );
        let run = start(
            base.clone(),
            vec![Patch {
                name: "invalid path".into(),
                diff,
            }],
        );
        assert_ne!(
            finished(&f.engine, &run.id).await.status,
            RunState::Succeeded
        );
        assert!(!run.workspace.join("source").exists());
        assert!(output(&f.engine, &run.id, "stdout").is_empty());
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
    let missing = start("0".repeat(40), vec![]);
    assert_ne!(
        finished(&f.engine, &missing.id).await.status,
        RunState::Succeeded
    );
    git(
        &f.project.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &base,
            "nested",
        ],
    );
    git(&f.project.root, &["commit", "-qm", "gitlink"]);
    let submodule = String::from_utf8(git(&f.project.root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    let run = start(submodule, vec![]);
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(!run.workspace.join("source").exists());
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_commits_are_refused_without_following_targets() {
    let f = Fixture::new();
    setup(&f);
    std::os::unix::fs::symlink("/etc/passwd", f.project.root.join("link")).unwrap();
    git(&f.project.root, &["add", "link"]);
    git(&f.project.root, &["commit", "-qm", "symlink"]);
    let commit = String::from_utf8(git(&f.project.root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .into();
    let run = f
        .engine
        .start_git_pipeline(StartGitPipeline {
            project_id: f.project.id.clone(),
            definition: definition(),
            source: GitSource {
                commit,
                patches: vec![],
            },
        })
        .unwrap();
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(!run.workspace.join("source").exists());
}

#[tokio::test]
async fn sha256_repository_requires_full_commit_and_cancel_preserves_request() {
    let f = Fixture::new();
    git(&f.project.root, &["init", "-q", "--object-format=sha256"]);
    let commit = setup(&f);
    assert_eq!(commit.len(), 64);
    let start = |commit: String| {
        f.engine
            .start_git_pipeline(StartGitPipeline {
                project_id: f.project.id.clone(),
                definition: definition(),
                source: GitSource {
                    commit,
                    patches: vec![],
                },
            })
            .unwrap()
    };
    let full = start(commit.clone());
    assert_eq!(
        finished(&f.engine, &full.id).await.status,
        RunState::Succeeded
    );
    let short = start(commit[..40].into());
    assert_ne!(
        finished(&f.engine, &short.id).await.status,
        RunState::Succeeded
    );
    assert!(!short.workspace.join("source").exists());
    let cancelled = start(commit.clone());
    f.engine
        .cancel(preflight::model::CancelRun {
            run_id: cancelled.id.clone(),
            actor: preflight::model::Actor::Agent,
            reason: "cancel before preparation".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &cancelled.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        f.engine.db.source_evidence(&cancelled.id).unwrap()["requested"]["commit"],
        commit
    );
    assert!(output(&f.engine, &cancelled.id, "stdout").is_empty());
}

#[tokio::test]
async fn dirty_uses_working_bytes_deletions_and_explicit_untracked_without_git_writes() {
    use preflight::sources::{DirtySource, StartDirtyPipeline};
    let f = Fixture::new();
    let head = setup(&f);
    std::fs::write(
        f.project.root.join("main.rs"),
        "compile_error!(\"staged must not build\");\n",
    )
    .unwrap();
    std::fs::write(f.project.root.join("added"), "staged addition").unwrap();
    git(&f.project.root, &["add", "main.rs", "added"]);
    std::fs::write(
        f.project.root.join("main.rs"),
        "fn main(){println!(\"working bytes\");}\n",
    )
    .unwrap();
    std::fs::write(f.project.root.join("added"), "working addition").unwrap();
    std::fs::remove_file(f.project.root.join("note")).unwrap();
    std::fs::write(f.project.root.join("selected"), "explicit input").unwrap();
    std::fs::write(f.project.root.join("private"), "must not appear").unwrap();
    let index = std::fs::read(f.project.root.join(".git/index")).unwrap();
    let status = git(&f.project.root, &["status", "--porcelain=v1", "-z"]);
    let request = StartDirtyPipeline {
        project_id: f.project.id.clone(),
        definition: definition(),
        source: DirtySource {
            expected_head: Some(head.clone()),
            untracked: vec!["selected".into()],
        },
    };
    let run = f.engine.start_dirty_pipeline(request.clone()).unwrap();
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded, "{done:?}");
    assert!(output(&f.engine, &run.id, "stdout").contains("working bytes"));
    let snapshot = run.workspace.join("source");
    assert_eq!(
        std::fs::read_to_string(snapshot.join("added")).unwrap(),
        "working addition"
    );
    assert!(snapshot.join("selected").exists());
    assert!(!snapshot.join("private").exists());
    assert!(!snapshot.join("note").exists());
    assert!(!snapshot.join(".git").exists());
    let evidence = f.engine.db.source_evidence(&run.id).unwrap();
    assert_eq!(evidence["mode"], "git_dirty");
    assert_eq!(
        evidence["prepared"]["index_inventory_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(evidence["prepared"]["resolved_commit"], head);
    assert_eq!(evidence["requested"]["untracked"], json!(["selected"]));
    let again = f.engine.start_dirty_pipeline(request).unwrap();
    assert_eq!(
        finished(&f.engine, &again.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        evidence["tree_sha256"],
        f.engine.db.source_evidence(&again.id).unwrap()["tree_sha256"]
    );
    assert_eq!(
        std::fs::read(f.project.root.join(".git/index")).unwrap(),
        index
    );
    assert_eq!(
        git(&f.project.root, &["status", "--porcelain=v1", "-z"]),
        status
    );
    assert_eq!(
        String::from_utf8(git(&f.project.root, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim(),
        head
    );
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let reopened = preflight::engine::Engine::open(Fixture::config(f.temp.path())).unwrap();
    assert_eq!(reopened.db.source_evidence(&run.id).unwrap(), evidence);
    assert!(
        reopened
            .db
            .source_patch(&preflight::sources::PatchQuery {
                run_id: run.id,
                ordinal: 0,
                root_name: None,
            })
            .is_err()
    );
}

#[tokio::test]
async fn dirty_refuses_wrong_head_missing_allowlist_and_sparse_entries() {
    use preflight::sources::{DirtySource, StartDirtyPipeline};
    let f = Fixture::new();
    let head = setup(&f);
    let start = |expected_head, untracked| {
        f.engine
            .start_dirty_pipeline(StartDirtyPipeline {
                project_id: f.project.id.clone(),
                definition: definition(),
                source: DirtySource {
                    expected_head,
                    untracked,
                },
            })
            .unwrap()
    };
    for run in [
        start(Some("0".repeat(40)), vec![]),
        start(Some(head.clone()), vec!["absent".into()]),
        start(None, vec!["main.rs".into()]),
    ] {
        assert_ne!(
            finished(&f.engine, &run.id).await.status,
            RunState::Succeeded
        );
        assert!(!run.workspace.join("source").exists());
    }
    git(
        &f.project.root,
        &["update-index", "--skip-worktree", "note"],
    );
    let run = start(None, vec![]);
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(!run.workspace.join("source").exists());
    git(
        &f.project.root,
        &["update-index", "--no-skip-worktree", "note"],
    );
    // Queue cancellation must retain the requested dirty source without publishing it.
    let run = start(Some(head), vec![]);
    f.engine
        .cancel(preflight::model::CancelRun {
            run_id: run.id.clone(),
            actor: preflight::model::Actor::Agent,
            reason: "dirty cancellation regression".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &run.id).await.status,
        RunState::Cancelled
    );
    assert_eq!(
        f.engine.db.source_evidence(&run.id).unwrap()["mode"],
        "git_dirty"
    );
    assert!(!run.workspace.join("source").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn dirty_refuses_symlink_ancestors_and_selected_symlinks() {
    use preflight::sources::{DirtySource, StartDirtyPipeline};
    let f = Fixture::new();
    setup(&f);
    let outside = f.temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), "outside").unwrap();
    std::os::unix::fs::symlink(&outside, f.project.root.join("alias")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), f.project.root.join("link")).unwrap();
    for path in ["alias/secret", "link"] {
        let run = f
            .engine
            .start_dirty_pipeline(StartDirtyPipeline {
                project_id: f.project.id.clone(),
                definition: definition(),
                source: DirtySource {
                    expected_head: None,
                    untracked: vec![path.into()],
                },
            })
            .unwrap();
        assert_ne!(
            finished(&f.engine, &run.id).await.status,
            RunState::Succeeded
        );
        assert!(!run.workspace.join("source").exists());
    }
    assert_eq!(
        std::fs::read_to_string(outside.join("secret")).unwrap(),
        "outside"
    );
}

#[tokio::test]
async fn dirty_refuses_submodule_and_unmerged_index() {
    use preflight::sources::{DirtySource, StartDirtyPipeline};
    use std::io::Write;
    let f = Fixture::new();
    let head = setup(&f);
    let start = || {
        f.engine
            .start_dirty_pipeline(StartDirtyPipeline {
                project_id: f.project.id.clone(),
                definition: definition(),
                source: DirtySource::default(),
            })
            .unwrap()
    };
    git(
        &f.project.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},submodule"),
        ],
    );
    let run = start();
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(!run.workspace.join("source").exists());
    git(
        &f.project.root,
        &["update-index", "--force-remove", "submodule"],
    );
    let blob = String::from_utf8(git(&f.project.root, &["rev-parse", "HEAD:note"])).unwrap();
    let mut command = std::process::Command::new("git")
        .arg("-C")
        .arg(&f.project.root)
        .args(["update-index", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    command
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                "0 {}\tnote\n100644 {} 1\tnote\n",
                "0".repeat(40),
                blob.trim()
            )
            .as_bytes(),
        )
        .unwrap();
    assert!(command.wait().unwrap().success());
    let run = start();
    assert_ne!(
        finished(&f.engine, &run.id).await.status,
        RunState::Succeeded
    );
    assert!(!run.workspace.join("source").exists());
}

fn multi_setup(f: &Fixture) -> preflight::multi_source::StartMultiPipeline {
    setup(f);
    std::fs::write(
        f.project.root.join("main.rs"),
        "#[path = \"../dep/value.rs\"] mod value; fn main(){println!(\"{}\",value::VALUE);}\n",
    )
    .unwrap();
    git(&f.project.root, &["add", "main.rs"]);
    git(&f.project.root, &["commit", "-qm", "use sibling"]);
    let commit = String::from_utf8(git(&f.project.root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    let dependency = f.temp.path().join("dependency");
    std::fs::create_dir(&dependency).unwrap();
    std::fs::write(
        dependency.join("value.rs"),
        "pub const VALUE: &str = \"committed\";\n",
    )
    .unwrap();
    git(&dependency, &["init", "-q"]);
    git(&dependency, &["config", "user.name", "Preflight test"]);
    git(
        &dependency,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&dependency, &["add", "."]);
    git(&dependency, &["commit", "-qm", "dependency"]);
    std::fs::write(
        dependency.join("value.rs"),
        "pub const VALUE: &str = \"dirty dependency\";\n",
    )
    .unwrap();
    let project = f
        .engine
        .register(preflight::model::RegisterProject {
            name: "dependency".into(),
            path: dependency.to_string_lossy().into_owned(),
        })
        .unwrap();
    serde_json::from_value(json!({"project_id":f.project.id,"definition":{"version":1,"pipeline":{"stages":[{"id":"compile","cwd":"app","command":{"program":"rustc","args":["main.rs","-o","program.exe"]},"outputs":{"program":{"path":"program.exe","type":"executable"}}},{"id":"execute","needs":["compile"],"command":{"program":"${artifact.compile.program}","args":[]}}]}},"source":{"roots":[{"name":"app","project_id":f.project.id,"path":"app","revision":{"mode":"git","source":{"commit":commit}}},{"name":"dep","project_id":project.id,"path":"dep","revision":{"mode":"dirty","source":{}}}]}})).unwrap()
}

#[tokio::test]
async fn multi_repository_build_identity_pages_and_restart() {
    let f = Fixture::new();
    let mut request = multi_setup(&f);
    let capabilities = preflight::control::capabilities(&f.engine);
    assert_eq!(capabilities["preflight_protocol"]["major"], 1);
    assert!(
        capabilities["preflight_protocol"]["minor"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(capabilities["features"]["multi_repository_sources"], true);
    let app_index = std::fs::read(f.project.root.join(".git/index")).unwrap();
    let dep = f.temp.path().join("dependency");
    let dep_index = std::fs::read(dep.join(".git/index")).unwrap();
    let run = f.engine.start_multi_pipeline(request.clone()).unwrap();
    let done = finished(&f.engine, &run.id).await;
    assert_eq!(done.status, RunState::Succeeded, "{done:?}");
    assert!(output(&f.engine, &run.id, "stdout").contains("dirty dependency"));
    let evidence = f.engine.db.source_evidence(&run.id).unwrap();
    assert_eq!(evidence["mode"], "multi_repository");
    let roots = evidence["prepared"]["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots[0]["manifest"].get("entries").is_none());
    assert_eq!(
        roots[0]["prepared"]["snapshot_path"],
        json!(run.workspace.join("source/app"))
    );
    let page = f
        .engine
        .db
        .source_root_inputs(&preflight::multi_source::RootInputsQuery {
            run_id: run.id.clone(),
            root_name: "dep".into(),
            offset: 0,
        })
        .unwrap();
    assert_eq!(page["entries"][0]["path"], "value.rs");
    assert_eq!(page["tree_sha256"], roots[1]["manifest"]["tree_sha256"]);
    request.source.roots.reverse();
    request.definition.pipeline.stages[0]
        .command
        .timeout_seconds += 1;
    let again = f.engine.start_multi_pipeline(request.clone()).unwrap();
    assert_eq!(
        finished(&f.engine, &again.id).await.status,
        RunState::Succeeded
    );
    let repeated = f.engine.db.source_evidence(&again.id).unwrap();
    assert_eq!(
        evidence["prepared"]["aggregate_sha256"],
        repeated["prepared"]["aggregate_sha256"]
    );
    assert_eq!(evidence["tree_sha256"], repeated["tree_sha256"]);
    assert_ne!(
        evidence["configuration_sha256"],
        repeated["configuration_sha256"]
    );
    std::fs::write(
        dep.join("value.rs"),
        "pub const VALUE: &str = \"changed dependency\";\n",
    )
    .unwrap();
    let changed = f.engine.start_multi_pipeline(request.clone()).unwrap();
    assert_eq!(
        finished(&f.engine, &changed.id).await.status,
        RunState::Succeeded
    );
    let new = f.engine.db.source_evidence(&changed.id).unwrap();
    assert_ne!(
        evidence["prepared"]["aggregate_sha256"],
        new["prepared"]["aggregate_sha256"]
    );
    assert_ne!(
        roots[1]["manifest"]["tree_sha256"],
        new["prepared"]["roots"][1]["manifest"]["tree_sha256"]
    );
    assert_eq!(
        roots[0]["manifest"]["tree_sha256"],
        new["prepared"]["roots"][0]["manifest"]["tree_sha256"]
    );
    let alternate = f.temp.path().join("alternate-dependency");
    git(
        f.temp.path(),
        &[
            "clone",
            "--quiet",
            "--no-hardlinks",
            dep.to_str().unwrap(),
            alternate.to_str().unwrap(),
        ],
    );
    std::fs::copy(dep.join("value.rs"), alternate.join("value.rs")).unwrap();
    let alternate_project = f
        .engine
        .register(preflight::model::RegisterProject {
            name: "alternate".into(),
            path: alternate.to_string_lossy().into_owned(),
        })
        .unwrap();
    request
        .source
        .roots
        .iter_mut()
        .find(|r| r.name == "dep")
        .unwrap()
        .project_id = alternate_project.id;
    let relocated = f.engine.start_multi_pipeline(request).unwrap();
    assert_eq!(
        finished(&f.engine, &relocated.id).await.status,
        RunState::Succeeded
    );
    assert_eq!(
        new["prepared"]["aggregate_sha256"],
        f.engine.db.source_evidence(&relocated.id).unwrap()["prepared"]["aggregate_sha256"]
    );
    assert!(
        f.engine
            .db
            .source_root_inputs(&preflight::multi_source::RootInputsQuery {
                run_id: run.id.clone(),
                root_name: "absent".into(),
                offset: 0
            })
            .is_err()
    );
    assert!(
        f.engine
            .db
            .source_root_inputs(&preflight::multi_source::RootInputsQuery {
                run_id: run.id.clone(),
                root_name: "dep".into(),
                offset: 20001
            })
            .is_err()
    );
    assert_eq!(
        app_index,
        std::fs::read(f.project.root.join(".git/index")).unwrap()
    );
    assert_eq!(dep_index, std::fs::read(dep.join(".git/index")).unwrap());
    f.engine.shutdown().await.unwrap();
    drop(f.engine);
    let reopened = preflight::engine::Engine::open(Fixture::config(f.temp.path())).unwrap();
    assert_eq!(reopened.db.source_evidence(&run.id).unwrap(), evidence);
    assert_eq!(
        reopened
            .db
            .source_root_inputs(&preflight::multi_source::RootInputsQuery {
                run_id: run.id,
                root_name: "dep".into(),
                offset: 0
            })
            .unwrap(),
        page
    );
}

#[tokio::test]
async fn multi_failure_rolls_back_all_roots_and_validates_authority() {
    let f = Fixture::new();
    let mut request = multi_setup(&f);
    request.source.roots[1].revision = preflight::multi_source::RootMode::Git(GitSource {
        commit: "0".repeat(40),
        patches: vec![],
    });
    let run = f.engine.start_multi_pipeline(request.clone()).unwrap();
    let done = finished(&f.engine, &run.id).await;
    assert_ne!(done.status, RunState::Succeeded);
    assert!(!run.workspace.join("source").exists());
    assert!(output(&f.engine, &run.id, "stdout").is_empty());
    assert!(f.engine.db.source_evidence(&run.id).unwrap()["prepared"].is_null());
    let cancelled = f.engine.start_multi_pipeline(request.clone()).unwrap();
    f.engine
        .cancel(preflight::model::CancelRun {
            run_id: cancelled.id.clone(),
            actor: preflight::model::Actor::Agent,
            reason: "multi cancellation".into(),
        })
        .unwrap();
    assert_eq!(
        finished(&f.engine, &cancelled.id).await.status,
        RunState::Cancelled
    );
    assert!(!cancelled.workspace.join("source").exists());
    request.source.roots[1].project_id = "unknown".into();
    assert!(f.engine.start_multi_pipeline(request.clone()).is_err());
    let outside = tempfile::tempdir().unwrap();
    let outside_project = f.engine.db.register("outside", outside.path()).unwrap();
    request.source.roots[1].project_id = outside_project.id;
    assert!(f.engine.start_multi_pipeline(request).is_err());
}

async fn queue_done(e: &std::sync::Arc<preflight::engine::Engine>, id: &str) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            preflight::queues::tick(e).unwrap();
            let status = preflight::queues::status(e, id, false).unwrap();
            if status["state"] == "finished" || status["state"] == "cancelled" {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn queue_request(
    f: &Fixture,
    commit: &str,
    mode: preflight::queues::Mode,
) -> preflight::queues::Create {
    preflight::queues::Create {
        project_id: f.project.id.clone(),
        name: "changes".into(),
        request_id: "queue-request".into(),
        definition: definition(),
        profile: None,
        mode,
        max_parallel: 1,
        changes: vec![
            preflight::queues::Change {
                id: "one".into(),
                source: GitSource {
                    commit: commit.into(),
                    patches: vec![patch("base", "one")],
                },
            },
            preflight::queues::Change {
                id: "two".into(),
                source: GitSource {
                    commit: commit.into(),
                    patches: vec![patch("one", "two")],
                },
            },
        ],
    }
}
#[tokio::test]
async fn cumulative_queue_builds_cumulative_sources_and_recovers_dispatch_gap() {
    let f = Fixture::new();
    let commit = setup(&f);
    let request = queue_request(&f, &commit, preflight::queues::Mode::Cumulative);
    let created = preflight::queues::create(&f.engine, request.clone()).unwrap();
    assert_eq!(
        preflight::queues::create(&f.engine, request).unwrap(),
        created
    );
    let id = created["queue_id"].as_str().unwrap();
    let status = queue_done(&f.engine, id).await;
    assert_eq!(status["counts"]["succeeded"], 2);
    for (item, expected) in status["items"]
        .as_array()
        .unwrap()
        .iter()
        .zip(["one", "two"])
    {
        assert!(output(&f.engine, item["run_id"].as_str().unwrap(), "stdout").contains(expected));
        assert!(item.get("source").is_none());
    }
    let details = preflight::queues::status(&f.engine, id, true).unwrap();
    assert_eq!(
        details["items"][1]["source"]["patches"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        serde_json::to_vec(&status).unwrap().len() < serde_json::to_vec(&details).unwrap().len()
    );
    assert_eq!(
        std::fs::read_to_string(f.project.root.join("main.rs")).unwrap(),
        "fn main(){println!(\"base\");}\n"
    );
    let config = f.engine.config.clone();
    f.engine.shutdown().await.unwrap();
    // Simulate loss of the queue link after durable run submission, then restart.
    let connection = rusqlite::Connection::open(config.data_dir.join("preflight.sqlite3")).unwrap();
    connection.execute("UPDATE change_queue_items SET run_id=NULL,state='pending' WHERE queue_id=?1 AND ordinal=1",[id]).unwrap();
    connection
        .execute("UPDATE change_queues SET state='active' WHERE id=?1", [id])
        .unwrap();
    drop(connection);
    let id = id.to_owned();
    drop(f.engine);
    let engine = preflight::engine::Engine::open(config).unwrap();
    let recovered = queue_done(&engine, &id).await;
    assert_eq!(status, recovered);
    assert_eq!(engine.db.runs().unwrap().len(), 2);
    engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn independent_queue_parallelism_and_cumulative_failure_gate() {
    let f = Fixture::new();
    let commit = setup(&f);
    let mut request = queue_request(&f, &commit, preflight::queues::Mode::Independent);
    request.max_parallel = 2;
    request.changes[1].source.patches = vec![patch("base", "two")];
    request.definition=serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"wait","command":{"program":"sleep","args":["0.2"]}}]}})).unwrap();
    let created = preflight::queues::create(&f.engine, request).unwrap();
    let id = created["queue_id"].as_str().unwrap();
    preflight::queues::tick(&f.engine).unwrap();
    assert_eq!(
        preflight::queues::status(&f.engine, id, false).unwrap()["counts"]["running"],
        2
    );
    let status = queue_done(&f.engine, id).await;
    assert_eq!(status["counts"]["succeeded"], 2);
    let a = f
        .engine
        .db
        .run(status["items"][0]["run_id"].as_str().unwrap())
        .unwrap();
    let b = f
        .engine
        .db
        .run(status["items"][1]["run_id"].as_str().unwrap())
        .unwrap();
    assert!(
        a.stages[0].started_at.unwrap() < b.stages[0].finished_at.unwrap()
            && b.stages[0].started_at.unwrap() < a.stages[0].finished_at.unwrap()
    );
    let mut request = queue_request(&f, &commit, preflight::queues::Mode::Cumulative);
    request.request_id = "broken".into();
    request.changes[0].source.patches = vec![patch("missing", "one")];
    let created = preflight::queues::create(&f.engine, request).unwrap();
    let failed = queue_done(&f.engine, created["queue_id"].as_str().unwrap()).await;
    assert_eq!(failed["counts"]["failed"], 1);
    assert_eq!(failed["counts"]["blocked"], 1);
    assert!(failed["items"][1].get("run_id").is_none());
    f.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn queue_cancel_is_scoped_and_validation_precedes_creation() {
    use preflight::{model::Actor, queues};
    let f = Fixture::new();
    let commit = setup(&f);
    let mut r = queue_request(&f, &commit, queues::Mode::Cumulative);
    r.max_parallel = 2;
    assert!(queues::create(&f.engine, r.clone()).is_err());
    r.max_parallel = 1;
    r.changes[1].source.commit = "b".repeat(40);
    assert!(queues::create(&f.engine, r).is_err());
    let mut r = queue_request(&f, &commit, queues::Mode::Independent);
    r.max_parallel = 1;
    r.changes[1].source.patches.clear();
    r.definition=serde_json::from_value(json!({"version":1,"pipeline":{"stages":[{"id":"wait","command":{"program":"sleep","args":["2"]}}]}})).unwrap();
    let created = queues::create(&f.engine, r.clone()).unwrap();
    let id = created["queue_id"].as_str().unwrap();
    r.request_id = "other".into();
    let other = queues::create(&f.engine, r.clone()).unwrap();
    queues::tick(&f.engine).unwrap();
    assert!(
        queues::cancel(
            &f.engine,
            queues::Cancel {
                queue_id: id.into(),
                actor: Actor::User,
                reason: "x".repeat(513)
            }
        )
        .is_err()
    );
    queues::cancel(
        &f.engine,
        queues::Cancel {
            queue_id: id.into(),
            actor: Actor::User,
            reason: "cancel only selected queue".into(),
        },
    )
    .unwrap();
    let cancelled = queue_done(&f.engine, id).await;
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["counts"]["cancelled"], 2);
    assert_eq!(
        queues::status(&f.engine, other["queue_id"].as_str().unwrap(), false).unwrap()["state"],
        "active"
    );
    r.name = "changed".into();
    assert!(queues::create(&f.engine, r).is_err());
    let page = queues::list(
        &f.engine,
        &queues::List {
            project_id: f.project.id.clone(),
            before: None,
            limit: 1,
        },
    )
    .unwrap();
    assert!(page["next_cursor"].is_string());
    let next = queues::list(
        &f.engine,
        &queues::List {
            project_id: f.project.id.clone(),
            before: page["next_cursor"].as_str().map(str::to_owned),
            limit: 1,
        },
    )
    .unwrap();
    assert_eq!(next["queues"][0]["queue_id"], id);
    queues::cancel(
        &f.engine,
        queues::Cancel {
            queue_id: other["queue_id"].as_str().unwrap().into(),
            actor: Actor::User,
            reason: "finish test".into(),
        },
    )
    .unwrap();
    queue_done(&f.engine, other["queue_id"].as_str().unwrap()).await;
    f.engine.shutdown().await.unwrap();
}
