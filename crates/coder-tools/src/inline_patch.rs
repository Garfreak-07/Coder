use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use super::{
    canonical_repo_root, patch_approval_key, resolve_repo_write_path, PatchApplyEvidence,
    PatchApplyTextRequest, PatchFilePreview, PatchPreviewEvidence, RepoToolError,
    DEFAULT_MAX_PATCH_BYTES, DEFAULT_MAX_WRITE_FILE_BYTES,
};

pub const APPLY_PATCH_LARK_GRAMMAR: &str = r#"start: begin_patch hunk+ end_patch
begin_patch: "*** Begin Patch" LF
end_patch: "*** End Patch" LF?

hunk: add_hunk | delete_hunk | update_hunk
add_hunk: "*** Add File: " filename LF add_line+
delete_hunk: "*** Delete File: " filename LF
update_hunk: "*** Update File: " filename LF change_move? change?

filename: /(.+)/
add_line: "+" /(.*)/ LF -> line

change_move: "*** Move to: " filename LF
change: (change_context | change_line)+ eof_line?
change_context: ("@@" | "@@ " /(.+)/) LF
change_line: ("+" | "-" | " ") /(.*)/ LF
eof_line: "*** End of File" LF

%import common.LF
"#;

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";

#[derive(Debug)]
enum PatchHunk {
    Add {
        path: PathBuf,
        content: String,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        move_to: Option<PathBuf>,
        chunks: Vec<UpdateChunk>,
    },
}

#[derive(Debug, Default)]
struct UpdateChunk {
    context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    end_of_file: bool,
}

#[derive(Debug)]
struct PreparedPatch {
    root: PathBuf,
    desired: BTreeMap<PathBuf, Option<Vec<u8>>>,
    preview: PatchPreviewEvidence,
}

pub fn apply_patch_text(
    repo_root: impl AsRef<Path>,
    request: PatchApplyTextRequest,
) -> Result<PatchApplyEvidence, RepoToolError> {
    let limit = request.max_patch_bytes.clamp(1, DEFAULT_MAX_PATCH_BYTES);
    if request.patch.len() > limit {
        return Err(RepoToolError::PatchInvalid(format!(
            "patch is {} bytes, over limit {limit}",
            request.patch.len()
        )));
    }
    let prepared = prepare_patch(repo_root, &request.patch)?;
    let approval_key = patch_approval_key(&request.patch, &prepared.preview);
    if request.source == "model" && !request.approved {
        return Ok(PatchApplyEvidence {
            repo_root: prepared.root.display().to_string(),
            patch_file: "inline:apply_patch".to_owned(),
            status: "blocked".to_owned(),
            applied: false,
            requires_approval: true,
            approval_key,
            reason: "Model-generated patch requires explicit approval.".to_owned(),
            preview: prepared.preview,
            evidence_kind: "patch_apply".to_owned(),
        });
    }

    commit_patch(&prepared)?;
    Ok(PatchApplyEvidence {
        repo_root: prepared.root.display().to_string(),
        patch_file: "inline:apply_patch".to_owned(),
        status: "applied".to_owned(),
        applied: true,
        requires_approval: false,
        approval_key,
        reason: String::new(),
        preview: prepared.preview,
        evidence_kind: "patch_apply".to_owned(),
    })
}

fn prepare_patch(repo_root: impl AsRef<Path>, patch: &str) -> Result<PreparedPatch, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let hunks = parse_patch(patch)?;
    if hunks.is_empty() {
        return Err(RepoToolError::PatchInvalid(
            "patch must contain at least one file hunk".to_owned(),
        ));
    }

    let mut desired = BTreeMap::new();
    let mut touched = BTreeSet::new();
    let mut files = Vec::new();
    let mut hunk_count = 0;
    let mut additions = 0;
    let mut deletions = 0;

    for hunk in hunks {
        match hunk {
            PatchHunk::Add { path, content } => {
                let (target, relative) = resolve_repo_write_path(&root, &path)?;
                ensure_unique_path(&mut touched, &target, &relative)?;
                if target.exists() {
                    return Err(RepoToolError::PatchInvalid(format!(
                        "cannot add existing file: {relative}"
                    )));
                }
                if content.len() > DEFAULT_MAX_WRITE_FILE_BYTES {
                    return Err(RepoToolError::PatchInvalid(format!(
                        "new file is over the {} byte limit: {relative}",
                        DEFAULT_MAX_WRITE_FILE_BYTES
                    )));
                }
                let added = content.lines().count();
                additions += added;
                hunk_count += 1;
                desired.insert(target, Some(content.into_bytes()));
                files.push(PatchFilePreview {
                    old_path: None,
                    new_path: Some(relative),
                    status: "added".to_owned(),
                    hunks: 1,
                    additions: added,
                    deletions: 0,
                    target_exists: false,
                });
            }
            PatchHunk::Delete { path } => {
                let (target, relative) = resolve_repo_write_path(&root, &path)?;
                ensure_unique_path(&mut touched, &target, &relative)?;
                let old = read_patch_text_file(&target, &relative)?;
                let deleted = old.lines().count();
                deletions += deleted;
                hunk_count += 1;
                desired.insert(target, None);
                files.push(PatchFilePreview {
                    old_path: Some(relative),
                    new_path: None,
                    status: "deleted".to_owned(),
                    hunks: 1,
                    additions: 0,
                    deletions: deleted,
                    target_exists: true,
                });
            }
            PatchHunk::Update {
                path,
                move_to,
                chunks,
            } => {
                let (source, old_relative) = resolve_repo_write_path(&root, &path)?;
                ensure_unique_path(&mut touched, &source, &old_relative)?;
                let old = read_patch_text_file(&source, &old_relative)?;
                let new = apply_update_chunks(&old, &old_relative, &chunks)?;
                if new.len() > DEFAULT_MAX_WRITE_FILE_BYTES {
                    return Err(RepoToolError::PatchInvalid(format!(
                        "updated file is over the {} byte limit: {old_relative}",
                        DEFAULT_MAX_WRITE_FILE_BYTES
                    )));
                }
                let (destination, new_relative, status) = if let Some(move_to) = move_to {
                    let (destination, relative) = resolve_repo_write_path(&root, &move_to)?;
                    ensure_unique_path(&mut touched, &destination, &relative)?;
                    if destination.exists() {
                        return Err(RepoToolError::PatchInvalid(format!(
                            "move destination already exists: {relative}"
                        )));
                    }
                    desired.insert(source, None);
                    (destination, relative, "renamed")
                } else {
                    (source, old_relative.clone(), "modified")
                };
                let chunk_additions = chunks.iter().map(|chunk| chunk.new_lines.len()).sum();
                let chunk_deletions = chunks.iter().map(|chunk| chunk.old_lines.len()).sum();
                additions += chunk_additions;
                deletions += chunk_deletions;
                hunk_count += chunks.len().max(1);
                desired.insert(destination, Some(new.into_bytes()));
                files.push(PatchFilePreview {
                    old_path: Some(old_relative),
                    new_path: Some(new_relative),
                    status: status.to_owned(),
                    hunks: chunks.len().max(1),
                    additions: chunk_additions,
                    deletions: chunk_deletions,
                    target_exists: true,
                });
            }
        }
    }

    Ok(PreparedPatch {
        root: root.clone(),
        desired,
        preview: PatchPreviewEvidence {
            repo_root: root.display().to_string(),
            file_count: files.len(),
            files,
            hunk_count,
            additions,
            deletions,
            truncated: false,
            evidence_kind: "repo_evidence".to_owned(),
        },
    })
}

fn parse_patch(patch: &str) -> Result<Vec<PatchHunk>, RepoToolError> {
    let lines = patch.trim().lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some(BEGIN_PATCH) {
        return Err(RepoToolError::PatchInvalid(format!(
            "first line must be '{BEGIN_PATCH}'"
        )));
    }
    if lines.last().map(|line| line.trim()) != Some(END_PATCH) {
        return Err(RepoToolError::PatchInvalid(format!(
            "last line must be '{END_PATCH}'"
        )));
    }

    let mut hunks = Vec::new();
    let mut index = 1;
    let end = lines.len() - 1;
    while index < end {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix(ADD_FILE) {
            let path = patch_path(path, index + 1)?;
            index += 1;
            let mut content = Vec::new();
            while index < end && !is_file_marker(lines[index]) {
                let Some(line) = lines[index].strip_prefix('+') else {
                    return invalid_hunk(index + 1, "added file lines must start with '+'");
                };
                content.push(line);
                index += 1;
            }
            if content.is_empty() {
                return invalid_hunk(index + 1, "add file hunk must contain at least one line");
            }
            hunks.push(PatchHunk::Add {
                path,
                content: format!("{}\n", content.join("\n")),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix(DELETE_FILE) {
            hunks.push(PatchHunk::Delete {
                path: patch_path(path, index + 1)?,
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix(UPDATE_FILE) {
            let path = patch_path(path, index + 1)?;
            index += 1;
            let mut move_to = None;
            if index < end {
                if let Some(path) = lines[index].strip_prefix(MOVE_TO) {
                    move_to = Some(patch_path(path, index + 1)?);
                    index += 1;
                }
            }
            let mut chunks = Vec::new();
            let mut current: Option<UpdateChunk> = None;
            while index < end && !is_file_marker(lines[index]) {
                let line = lines[index];
                if line == "@@" || line.starts_with("@@ ") {
                    if let Some(chunk) = current.take() {
                        chunks.push(chunk);
                    }
                    current = Some(UpdateChunk {
                        context: line.strip_prefix("@@ ").map(str::to_owned),
                        ..UpdateChunk::default()
                    });
                } else if line == END_OF_FILE {
                    current.get_or_insert_with(UpdateChunk::default).end_of_file = true;
                } else {
                    let (prefix, text) = line.split_at_checked(1).ok_or_else(|| {
                        RepoToolError::PatchInvalid(format!(
                            "invalid empty change line at {}",
                            index + 1
                        ))
                    })?;
                    let chunk = current.get_or_insert_with(UpdateChunk::default);
                    match prefix {
                        " " => {
                            chunk.old_lines.push(text.to_owned());
                            chunk.new_lines.push(text.to_owned());
                        }
                        "-" => chunk.old_lines.push(text.to_owned()),
                        "+" => chunk.new_lines.push(text.to_owned()),
                        _ if line.trim().is_empty() => {}
                        _ => {
                            return invalid_hunk(
                                index + 1,
                                "change lines must start with ' ', '+', or '-'",
                            )
                        }
                    }
                }
                index += 1;
            }
            if let Some(chunk) = current.take() {
                chunks.push(chunk);
            }
            if chunks.is_empty() && move_to.is_none() {
                return invalid_hunk(index + 1, "update file hunk is empty");
            }
            hunks.push(PatchHunk::Update {
                path,
                move_to,
                chunks,
            });
            continue;
        }
        return invalid_hunk(
            index + 1,
            "expected Add File, Delete File, or Update File marker",
        );
    }
    Ok(hunks)
}

fn patch_path(path: &str, line: usize) -> Result<PathBuf, RepoToolError> {
    let path = path.trim();
    if path.is_empty() {
        return invalid_hunk(line, "file path cannot be empty");
    }
    Ok(PathBuf::from(path))
}

fn invalid_hunk<T>(line: usize, message: &str) -> Result<T, RepoToolError> {
    Err(RepoToolError::PatchInvalid(format!(
        "invalid hunk at line {line}: {message}"
    )))
}

fn is_file_marker(line: &str) -> bool {
    line.starts_with(ADD_FILE) || line.starts_with(DELETE_FILE) || line.starts_with(UPDATE_FILE)
}

fn read_patch_text_file(path: &Path, relative: &str) -> Result<String, RepoToolError> {
    if !path.exists() {
        return Err(RepoToolError::PathNotFound {
            path: relative.to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "file does not exist"),
        });
    }
    let bytes = fs::read(path)?;
    String::from_utf8(bytes).map_err(|_| RepoToolError::BinaryFile(relative.to_owned()))
}

fn ensure_unique_path(
    touched: &mut BTreeSet<PathBuf>,
    path: &Path,
    relative: &str,
) -> Result<(), RepoToolError> {
    if !touched.insert(path.to_path_buf()) {
        return Err(RepoToolError::PatchInvalid(format!(
            "file appears more than once in patch: {relative}"
        )));
    }
    Ok(())
}

fn apply_update_chunks(
    original: &str,
    relative: &str,
    chunks: &[UpdateChunk],
) -> Result<String, RepoToolError> {
    let mut lines = original.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut replacements = Vec::new();
    let mut line_index = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.context {
            let Some(index) =
                seek_sequence(&lines, std::slice::from_ref(context), line_index, false)
            else {
                return Err(RepoToolError::PatchInvalid(format!(
                    "failed to find context '{context}' in {relative}"
                )));
            };
            line_index = index + 1;
        }
        if chunk.old_lines.is_empty() {
            replacements.push((lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }
        let Some(index) = seek_sequence(&lines, &chunk.old_lines, line_index, chunk.end_of_file)
        else {
            return Err(RepoToolError::PatchInvalid(format!(
                "failed to find expected lines in {relative}:\n{}",
                chunk.old_lines.join("\n")
            )));
        };
        replacements.push((index, chunk.old_lines.len(), chunk.new_lines.clone()));
        line_index = index + chunk.old_lines.len();
    }
    for (index, old_len, new_lines) in replacements.into_iter().rev() {
        lines.splice(index..index + old_len, new_lines);
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    end_of_file: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start.min(lines.len()));
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let search_start = if end_of_file {
        lines.len() - pattern.len()
    } else {
        start.min(lines.len() - pattern.len())
    };
    let end = lines.len() - pattern.len();
    for mode in 0..3 {
        for index in search_start..=end {
            let matches = pattern.iter().enumerate().all(|(offset, expected)| {
                let actual = &lines[index + offset];
                match mode {
                    0 => actual == expected,
                    1 => actual.trim_end() == expected.trim_end(),
                    _ => actual.trim() == expected.trim(),
                }
            });
            if matches {
                return Some(index);
            }
        }
    }
    None
}

fn commit_patch(prepared: &PreparedPatch) -> Result<(), RepoToolError> {
    let originals = prepared
        .desired
        .keys()
        .map(|path| {
            let original = if path.exists() {
                Some(fs::read(path)?)
            } else {
                None
            };
            Ok((path.clone(), original))
        })
        .collect::<Result<BTreeMap<_, _>, std::io::Error>>()?;
    let mut created_dirs = Vec::new();

    for (path, desired) in &prepared.desired {
        let result = match desired {
            Some(content) => {
                if let Some(parent) = path.parent() {
                    create_missing_dirs(parent, &prepared.root, &mut created_dirs)?;
                }
                fs::write(path, content)
            }
            None => fs::remove_file(path),
        };
        if let Err(error) = result {
            let rollback = rollback_patch(&originals, &created_dirs);
            return match rollback {
                Ok(()) => Err(RepoToolError::PatchInvalid(format!(
                    "patch commit failed and was rolled back: {error}"
                ))),
                Err(rollback_error) => Err(RepoToolError::PatchInvalid(format!(
                    "patch commit failed: {error}; rollback failed: {rollback_error}"
                ))),
            };
        }
    }
    Ok(())
}

fn create_missing_dirs(
    directory: &Path,
    root: &Path,
    created: &mut Vec<PathBuf>,
) -> Result<(), RepoToolError> {
    let mut missing = Vec::new();
    let mut current = directory;
    while current != root && !current.exists() {
        missing.push(current.to_path_buf());
        current = current.parent().ok_or_else(|| {
            RepoToolError::PatchInvalid("patch directory escaped repository".to_owned())
        })?;
    }
    for directory in missing.into_iter().rev() {
        fs::create_dir(&directory)?;
        created.push(directory);
    }
    Ok(())
}

fn rollback_patch(
    originals: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    created_dirs: &[PathBuf],
) -> Result<(), std::io::Error> {
    for (path, original) in originals {
        match original {
            Some(content) => fs::write(path, content)?,
            None if path.exists() => fs::remove_file(path)?,
            None => {}
        }
    }
    for directory in created_dirs.iter().rev() {
        match fs::remove_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_repo() -> PathBuf {
        let root = std::env::temp_dir().join(format!("coder-inline-patch-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn apply(
        root: &Path,
        patch: &str,
        approved: bool,
    ) -> Result<PatchApplyEvidence, RepoToolError> {
        apply_patch_text(
            root,
            PatchApplyTextRequest {
                patch: patch.to_owned(),
                max_patch_bytes: DEFAULT_MAX_PATCH_BYTES,
                source: "model".to_owned(),
                approved,
            },
        )
    }

    #[test]
    fn patch_applies_add_update_delete_and_move_as_one_transaction() {
        let root = temp_repo();
        fs::write(root.join("update.txt"), "alpha\nbeta\n").unwrap();
        fs::write(root.join("delete.txt"), "delete me\n").unwrap();
        fs::write(root.join("move.txt"), "before\n").unwrap();
        let patch = r#"*** Begin Patch
*** Add File: nested/new.txt
+new
*** Update File: update.txt
@@
-alpha
+ALPHA
 beta
*** Delete File: delete.txt
*** Update File: move.txt
*** Move to: moved.txt
@@
-before
+after
*** End Patch"#;

        let evidence = apply(&root, patch, true).unwrap();

        assert!(evidence.applied);
        assert_eq!(evidence.preview.file_count, 4);
        assert_eq!(
            fs::read_to_string(root.join("nested/new.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("update.txt")).unwrap(),
            "ALPHA\nbeta\n"
        );
        assert!(!root.join("delete.txt").exists());
        assert!(!root.join("move.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("moved.txt")).unwrap(),
            "after\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn model_patch_without_approval_is_preview_only() {
        let root = temp_repo();
        let patch = "*** Begin Patch\n*** Add File: blocked.txt\n+blocked\n*** End Patch";

        let evidence = apply(&root, patch, false).unwrap();

        assert!(evidence.requires_approval);
        assert!(!evidence.applied);
        assert!(!root.join("blocked.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_later_hunk_does_not_mutate_an_earlier_file() {
        let root = temp_repo();
        fs::write(root.join("first.txt"), "one\n").unwrap();
        fs::write(root.join("second.txt"), "two\n").unwrap();
        let patch = r#"*** Begin Patch
*** Update File: first.txt
@@
-one
+ONE
*** Update File: second.txt
@@
-missing
+TWO
*** End Patch"#;

        assert!(apply(&root, patch, true).is_err());
        assert_eq!(fs::read_to_string(root.join("first.txt")).unwrap(), "one\n");
        assert_eq!(
            fs::read_to_string(root.join("second.txt")).unwrap(),
            "two\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_io_failure_rolls_back_files_already_written() {
        let root = temp_repo();
        fs::write(root.join("a-first.txt"), "before\n").unwrap();
        fs::write(root.join("z-parent"), "not a directory\n").unwrap();
        let patch = r#"*** Begin Patch
*** Update File: a-first.txt
@@
-before
+after
*** Add File: z-parent/child.txt
+cannot write
*** End Patch"#;

        assert!(apply(&root, patch, true).is_err());
        assert_eq!(
            fs::read_to_string(root.join("a-first.txt")).unwrap(),
            "before\n"
        );
        assert!(!root.join("z-parent/child.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn patch_rejects_paths_outside_the_repository() {
        let root = temp_repo();
        let patch = "*** Begin Patch\n*** Add File: ../escape.txt\n+no\n*** End Patch";
        assert!(matches!(
            apply(&root, patch, true),
            Err(RepoToolError::PathOutsideRepo(_))
        ));
        let _ = fs::remove_dir_all(root);
    }
}
