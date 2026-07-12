use super::*;

#[test]
fn read_file_returns_repo_evidence() {
    let root = temp_repo();
    fs::write(root.join("src.txt"), "hello repo").unwrap();

    let evidence = read_file(&root, "src.txt", &RepoToolConfig::default()).unwrap();

    assert_eq!(evidence.path, "src.txt");
    assert_eq!(evidence.content, "hello repo");
    assert_eq!(evidence.evidence_kind, "repo_evidence");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_rejects_path_escape() {
    let root = temp_repo();
    let outside = root.parent().unwrap().join("outside.txt");
    fs::write(&outside, "outside").unwrap();

    let error = read_file(&root, "../outside.txt", &RepoToolConfig::default()).unwrap_err();

    assert!(matches!(error, RepoToolError::PathOutsideRepo(_)));
    let _ = fs::remove_file(outside);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn write_text_file_creates_repo_file() {
    let root = temp_repo();

    let evidence = write_text_file(
        &root,
        FileWriteRequest {
            path: PathBuf::from("docs/readme.md"),
            content: "hello\n".to_owned(),
            max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
            source: "test".to_owned(),
        },
    )
    .unwrap();

    assert_eq!(evidence.path, "docs/readme.md");
    assert!(evidence.created);
    assert_eq!(evidence.status, "written");
    assert_eq!(
        fs::read_to_string(root.join("docs").join("readme.md")).unwrap(),
        "hello\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn write_text_file_rejects_escape_and_sensitive_paths() {
    let root = temp_repo();

    let escaped = write_text_file(
        &root,
        FileWriteRequest {
            path: PathBuf::from("../outside.txt"),
            content: "nope".to_owned(),
            max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
            source: "test".to_owned(),
        },
    )
    .unwrap_err();
    let sensitive = write_text_file(
        &root,
        FileWriteRequest {
            path: PathBuf::from(".env"),
            content: "SECRET=value".to_owned(),
            max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
            source: "test".to_owned(),
        },
    )
    .unwrap_err();

    assert!(matches!(escaped, RepoToolError::PathOutsideRepo(_)));
    assert!(matches!(sensitive, RepoToolError::SensitivePath(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn edit_text_file_replaces_one_exact_unique_string() {
    let root = temp_repo();
    fs::write(
        root.join("main.js"),
        "const before = true;\nconst plantType = 'sunflower';\nconst after = true;\n",
    )
    .unwrap();

    let evidence = edit_text_file(
        &root,
        FileEditRequest {
            path: PathBuf::from("main.js"),
            old_string: "const plantType = 'sunflower';".to_owned(),
            new_string: "const plantType = 's';".to_owned(),
            replace_all: false,
            max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
            source: "test".to_owned(),
        },
    )
    .unwrap();

    assert!(!evidence.created);
    assert_eq!(evidence.evidence_kind, "file_edit");
    assert_eq!(
        fs::read_to_string(root.join("main.js")).unwrap(),
        "const before = true;\nconst plantType = 's';\nconst after = true;\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn edit_text_file_requires_unique_context_unless_replace_all_is_set() {
    let root = temp_repo();
    fs::write(root.join("main.js"), "plant();\nplant();\n").unwrap();
    let request = || FileEditRequest {
        path: PathBuf::from("main.js"),
        old_string: "plant();".to_owned(),
        new_string: "grow();".to_owned(),
        replace_all: false,
        max_bytes: DEFAULT_MAX_WRITE_FILE_BYTES,
        source: "test".to_owned(),
    };

    let error = edit_text_file(&root, request()).unwrap_err();
    assert!(matches!(
        error,
        RepoToolError::EditStringNotUnique { matches: 2, .. }
    ));

    let mut replace_all = request();
    replace_all.replace_all = true;
    edit_text_file(&root, replace_all).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("main.js")).unwrap(),
        "grow();\ngrow();\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn edit_text_file_batch_applies_sequential_edits_with_one_write() {
    let root = temp_repo();
    fs::write(root.join("main.js"), "const value = 1;\nshow(value);\n").unwrap();

    let evidence = edit_text_file_batch(
        &root,
        FileEditBatchRequest {
            path: PathBuf::from("main.js"),
            edits: vec![
                FileEditReplacement {
                    old_string: "const value = 1;".to_owned(),
                    new_string: "const score = 2;".to_owned(),
                    replace_all: false,
                },
                FileEditReplacement {
                    old_string: "show(value);".to_owned(),
                    new_string: "show(score);".to_owned(),
                    replace_all: false,
                },
            ],
            max_bytes: 1024,
            source: "test".to_owned(),
        },
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(root.join("main.js")).unwrap(),
        "const score = 2;\nshow(score);\n"
    );
    assert_eq!(evidence.evidence_kind, "file_edit");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn edit_text_file_batch_is_atomic_when_a_later_edit_fails() {
    let root = temp_repo();
    let original = "const value = 1;\nshow(value);\n";
    fs::write(root.join("main.js"), original).unwrap();

    let error = edit_text_file_batch(
        &root,
        FileEditBatchRequest {
            path: PathBuf::from("main.js"),
            edits: vec![
                FileEditReplacement {
                    old_string: "const value = 1;".to_owned(),
                    new_string: "const score = 2;".to_owned(),
                    replace_all: false,
                },
                FileEditReplacement {
                    old_string: "missing();".to_owned(),
                    new_string: "show(score);".to_owned(),
                    replace_all: false,
                },
            ],
            max_bytes: 1024,
            source: "test".to_owned(),
        },
    )
    .unwrap_err();

    assert!(matches!(error, RepoToolError::EditStringNotFound(_)));
    assert_eq!(fs::read_to_string(root.join("main.js")).unwrap(), original);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_rejects_large_files() {
    let root = temp_repo();
    fs::write(root.join("large.txt"), "123456").unwrap();
    let config = RepoToolConfig {
        max_file_bytes: 3,
        max_search_matches: 10,
    };

    let error = read_file(&root, "large.txt", &config).unwrap_err();

    assert!(matches!(error, RepoToolError::FileTooLarge { .. }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_returns_line_refs() {
    let root = temp_repo();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("app.py"), "one\ntwo\nthree\n").unwrap();

    let snippet = read_file_range(&root, "src/app.py", 2, 1, 16_000).unwrap();

    assert_eq!(snippet.path, "src/app.py");
    assert_eq!(snippet.start_line, 2);
    assert_eq!(snippet.end_line, 2);
    assert_eq!(snippet.text, "two\n");
    assert!(snippet.truncated);
    assert_eq!(snippet.evidence_kind, "repo_evidence");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_reports_line_and_char_truncation() {
    let root = temp_repo();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("app.py"), "one\ntwo\nthree\n").unwrap();
    fs::write(root.join("src").join("unicode.txt"), "abc\u{e9}def\n").unwrap();

    let by_lines = read_file_range(&root, "src/app.py", 1, 2, 16_000).unwrap();
    let by_chars = read_file_range(&root, "src/unicode.txt", 1, 120, 4).unwrap();

    assert_eq!(by_lines.end_line, 2);
    assert!(by_lines.truncated);
    assert_eq!(by_chars.text, "abc\u{e9}");
    assert!(by_chars.truncated);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn read_file_range_rejects_sensitive_and_binary_files() {
    let root = temp_repo();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join(".env"), "SECRET=value\n").unwrap();
    fs::write(root.join("src").join("bin.dat"), b"abc\0def").unwrap();

    let sensitive = read_file_range(&root, ".env", 1, 120, 16_000).unwrap_err();
    let binary = read_file_range(&root, "src/bin.dat", 1, 120, 16_000).unwrap_err();

    assert!(matches!(sensitive, RepoToolError::SensitivePath(_)));
    assert!(matches!(binary, RepoToolError::BinaryFile(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn find_files_returns_repo_evidence_and_filters_results() {
    let root = temp_repo();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join(".coder")).unwrap();
    fs::write(root.join("src").join("app.py"), "app\n").unwrap();
    fs::write(root.join("src").join("app.rs"), "app\n").unwrap();
    fs::write(root.join("docs").join("app.md"), "app\n").unwrap();
    fs::write(root.join(".coder").join("app.py"), "hidden\n").unwrap();
    fs::write(root.join(".env"), "SECRET=value\n").unwrap();

    let files = find_files(&root, Some("app"), &[String::from("py")], 10).unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "src/app.py");
    assert_eq!(files[0].normalized_path, "src/app.py");
    assert_eq!(files[0].language.as_deref(), Some("python"));
    assert_eq!(files[0].evidence_kind, "repo_evidence");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn find_files_skips_sensitive_paths_and_bounds_results() {
    let root = temp_repo();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join(".ssh")).unwrap();
    fs::write(root.join(".env"), "SECRET=value\n").unwrap();
    fs::write(root.join(".ssh").join("config"), "secret\n").unwrap();
    fs::write(root.join("src").join("a.txt"), "a\n").unwrap();
    fs::write(root.join("src").join("b.txt"), "b\n").unwrap();

    let files = find_files(&root, None, &[], 1).unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "src/a.txt");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn search_text_returns_matches_and_skips_hidden_runtime_dirs() {
    let root = temp_repo();
    fs::write(root.join("src.txt"), "first\nneedle here\n").unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("ignored.txt"), "needle hidden").unwrap();
    fs::create_dir(root.join("node_modules")).unwrap();
    fs::write(root.join("node_modules").join("ignored.txt"), "needle deps").unwrap();

    let matches = search_text(&root, "needle", &RepoToolConfig::default()).unwrap();

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, "src.txt");
    assert_eq!(matches[0].line, 2);
    assert_eq!(matches[0].evidence_kind, "repo_evidence");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn repo_text_evidence_skips_sensitive_files() {
    let root = temp_repo();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join(".env"), "needle secret\n").unwrap();
    fs::write(root.join("src").join("app.py"), "needle safe\n").unwrap();

    let matches = search_text(&root, "needle", &RepoToolConfig::default()).unwrap();

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, "src/app.py");
    let error = read_file(&root, ".env", &RepoToolConfig::default()).unwrap_err();
    assert!(matches!(error, RepoToolError::SensitivePath(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn search_text_rejects_empty_query() {
    let root = temp_repo();

    let error = search_text(&root, "  ", &RepoToolConfig::default()).unwrap_err();

    assert!(matches!(error, RepoToolError::EmptyQuery));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn git_status_returns_branch_and_worktree_evidence() {
    let root = temp_repo();
    init_git_repo(&root);
    fs::write(root.join("untracked.txt"), "new evidence\n").unwrap();

    let evidence = git_status(&root).unwrap();

    assert!(evidence
        .porcelain_v1
        .lines()
        .any(|line| line.starts_with("## ")));
    assert!(evidence.porcelain_v1.contains("?? untracked.txt"));
    assert!(!evidence.truncated);
    assert_eq!(evidence.evidence_kind, "repo_evidence");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn git_diff_returns_bounded_preview() {
    let root = temp_repo();
    init_git_repo(&root);
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    git(&root, &["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "changed\n").unwrap();

    let evidence = git_diff(&root, 4096).unwrap();

    assert!(evidence.preview.contains("diff --git"));
    assert!(evidence.preview.contains("-base"));
    assert!(evidence.preview.contains("+changed"));
    assert!(!evidence.truncated);

    let truncated = git_diff(&root, 24).unwrap();
    assert!(truncated.truncated);
    assert_eq!(truncated.preview.len(), 24);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn patch_preview_summarizes_unified_diff() {
    let root = temp_repo();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("app.py"), "base\n").unwrap();
    let patch = "\
diff --git a/src/app.py b/src/app.py
--- a/src/app.py
+++ b/src/app.py
@@ -1 +1 @@
-base
+changed
";

    let evidence = preview_patch_text(&root, patch, false).unwrap();

    assert_eq!(evidence.file_count, 1);
    assert_eq!(evidence.hunk_count, 1);
    assert_eq!(evidence.additions, 1);
    assert_eq!(evidence.deletions, 1);
    assert_eq!(evidence.files[0].new_path.as_deref(), Some("src/app.py"));
    assert_eq!(evidence.files[0].status, "modified");
    assert!(evidence.files[0].target_exists);
    assert_eq!(evidence.evidence_kind, "repo_evidence");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn patch_preview_file_reads_repo_patch() {
    let root = temp_repo();
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        root.join("change.patch"),
        "\
diff --git a/tracked.txt b/tracked.txt
index df967b9..5ea2ed4 100644
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
    )
    .unwrap();

    let evidence = preview_patch_file(&root, "change.patch", DEFAULT_MAX_PATCH_BYTES).unwrap();

    assert_eq!(evidence.file_count, 1);
    assert_eq!(evidence.files[0].new_path.as_deref(), Some("tracked.txt"));
    assert!(!evidence.truncated);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn patch_apply_blocks_model_patch_without_approval() {
    let root = temp_repo();
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        root.join("change.patch"),
        "\
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
    )
    .unwrap();

    let evidence = apply_patch_file(
        &root,
        PatchApplyRequest {
            patch_file: PathBuf::from("change.patch"),
            max_patch_bytes: DEFAULT_MAX_PATCH_BYTES,
            source: "model".to_owned(),
            approved: false,
        },
    )
    .unwrap();

    assert_eq!(evidence.status, "blocked");
    assert!(!evidence.applied);
    assert!(evidence.requires_approval);
    assert!(evidence.approval_key.starts_with("patch:"));
    assert_eq!(
        fs::read_to_string(root.join("tracked.txt")).unwrap(),
        "base\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn patch_apply_applies_approved_patch() {
    let root = temp_repo();
    init_git_repo(&root);
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    fs::write(
        root.join("change.patch"),
        "\
diff --git a/tracked.txt b/tracked.txt
index df967b9..5ea2ed4 100644
--- a/tracked.txt
+++ b/tracked.txt
@@ -1 +1 @@
-base
+changed
",
    )
    .unwrap();

    let evidence = apply_patch_file(
        &root,
        PatchApplyRequest {
            patch_file: PathBuf::from("change.patch"),
            max_patch_bytes: DEFAULT_MAX_PATCH_BYTES,
            source: "model".to_owned(),
            approved: true,
        },
    )
    .unwrap();

    assert_eq!(evidence.status, "applied");
    assert!(evidence.applied);
    assert!(!evidence.requires_approval);
    assert_eq!(
        fs::read_to_string(root.join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "changed\n"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn patch_preview_rejects_path_escape_and_sensitive_targets() {
    let root = temp_repo();
    let escaped = "\
diff --git a/src/app.py b/../escape.py
--- a/src/app.py
+++ b/../escape.py
@@ -1 +1 @@
-base
+changed
";
    let sensitive = "\
diff --git a/.env b/.env
--- a/.env
+++ b/.env
@@ -1 +1 @@
-safe
+unsafe
";

    let escaped_error = preview_patch_text(&root, escaped, false).unwrap_err();
    let sensitive_error = preview_patch_text(&root, sensitive, false).unwrap_err();

    assert!(matches!(escaped_error, RepoToolError::PathOutsideRepo(_)));
    assert!(matches!(sensitive_error, RepoToolError::SensitivePath(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_command_executes_discovered_argv_without_shell() {
    let root = temp_repo();

    let evidence = run_command(
        &root,
        CommandRunRequest {
            argv: platform_echo_args("argv-ok"),
            source: "discovered".to_owned(),
            ..CommandRunRequest::default()
        },
    )
    .unwrap();

    assert!(evidence.passed);
    assert_eq!(evidence.status, "completed");
    assert!(!evidence.requires_approval);
    assert!(evidence.output.contains("argv-ok"));
    assert_eq!(evidence.policy.risk, "low");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_command_blocks_model_command_without_approval() {
    let root = temp_repo();

    let evidence = run_command(
        &root,
        CommandRunRequest {
            argv: platform_echo_args("blocked"),
            source: "model".to_owned(),
            ..CommandRunRequest::default()
        },
    )
    .unwrap();

    assert_eq!(evidence.status, "blocked");
    assert!(evidence.blocked);
    assert!(evidence.requires_approval);
    assert_eq!(evidence.policy.risk, "medium");
    assert!(evidence.approval_key.starts_with("cmd:"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn command_timeout_uses_configured_default_and_max_bounds() {
    assert_eq!(
        effective_command_timeout_seconds(DEFAULT_COMMAND_TIMEOUT_SECONDS),
        120
    );
    assert_eq!(effective_command_timeout_seconds(0), 1);
    assert_eq!(
        effective_command_timeout_seconds(MAX_COMMAND_TIMEOUT_SECONDS + 1),
        600
    );
}

#[test]
fn command_preview_reports_approval_key_without_running() {
    let root = temp_repo();

    let preview =
        preview_command(&root, ".", platform_echo_args("preview"), "model", false).unwrap();

    assert_eq!(preview.cwd, ".");
    assert!(preview.requires_approval);
    assert_eq!(preview.policy.risk, "medium");
    assert_eq!(
        preview.approval_key,
        command_approval_key(&preview.command, ".")
    );
    assert_eq!(preview.evidence_kind, "command_preview");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn command_preview_does_not_treat_sandbox_flag_as_capability() {
    let root = temp_repo();

    let preview =
        preview_command(&root, ".", platform_echo_args("preview"), "model", true).unwrap();

    assert!(preview.requires_approval);
    assert_eq!(preview.policy.risk, "medium");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_command_reports_nonzero_exit() {
    let root = temp_repo();

    let evidence = run_command(
        &root,
        CommandRunRequest {
            argv: platform_exit_args(7),
            source: "discovered".to_owned(),
            approved: true,
            ..CommandRunRequest::default()
        },
    )
    .unwrap();

    assert!(!evidence.passed);
    assert_eq!(evidence.status, "failed");
    assert_eq!(evidence.returncode, Some(7));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_command_writes_stdin_to_child_process() {
    let root = temp_repo();

    let evidence = run_command(
        &root,
        CommandRunRequest {
            argv: platform_cat_args(),
            stdin: Some("stdin-ok\n".to_owned()),
            source: "discovered".to_owned(),
            approved: true,
            ..CommandRunRequest::default()
        },
    )
    .unwrap();

    assert!(evidence.passed);
    assert_eq!(evidence.status, "completed");
    assert!(evidence.output.contains("stdin-ok"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn run_command_rejects_cwd_escape() {
    let root = temp_repo();

    let error = run_command(
        &root,
        CommandRunRequest {
            cwd: PathBuf::from(".."),
            argv: platform_echo_args("nope"),
            source: "discovered".to_owned(),
            ..CommandRunRequest::default()
        },
    )
    .unwrap_err();

    assert!(matches!(error, RepoToolError::PathOutsideRepo(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn git_status_rejects_non_git_directory() {
    let root = temp_repo();

    let error = git_status(&root).unwrap_err();

    assert!(matches!(error, RepoToolError::GitFailed { .. }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn model_commands_do_not_inherit_provider_credentials() {
    let mut model_command = Command::new("unused");
    model_command.env("DEEPSEEK_API_KEY", "secret");
    configure_model_command_environment(&mut model_command, "model");
    assert!(model_command.get_envs().any(|(name, value)| {
        name == std::ffi::OsStr::new("DEEPSEEK_API_KEY") && value.is_none()
    }));

    let mut user_command = Command::new("unused");
    user_command.env("DEEPSEEK_API_KEY", "user-selected");
    configure_model_command_environment(&mut user_command, "user");
    assert!(user_command.get_envs().any(|(name, value)| {
        name == std::ffi::OsStr::new("DEEPSEEK_API_KEY")
            && value == Some(std::ffi::OsStr::new("user-selected"))
    }));
}

fn temp_repo() -> PathBuf {
    static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = test_tmp_root().join(format!("coder-tools-{}-{}", std::process::id(), id));
    fs::create_dir_all(&root).unwrap();
    root
}

fn test_tmp_root() -> PathBuf {
    std::env::var_os("CODER_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn init_git_repo(root: &Path) {
    git(root, &["init"]);
}

fn platform_echo_args(text: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd.exe".to_owned(),
            "/C".to_owned(),
            "echo".to_owned(),
            text.to_owned(),
        ]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), format!("printf {text}")]
    }
}

fn platform_exit_args(code: i32) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd.exe".to_owned(),
            "/C".to_owned(),
            "exit".to_owned(),
            "/B".to_owned(),
            code.to_string(),
        ]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), format!("exit {code}")]
    }
}

fn platform_cat_args() -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd.exe".to_owned(), "/C".to_owned(), "more".to_owned()]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), "cat".to_owned()]
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
