use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024;
pub const DEFAULT_MAX_SEARCH_MATCHES: usize = 50;

const SKIPPED_DIRS: &[&str] = &[
    ".git",
    ".coder",
    ".venv",
    "node_modules",
    "target",
    "dist",
    "__pycache__",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoToolConfig {
    pub max_file_bytes: u64,
    pub max_search_matches: usize,
}

impl Default for RepoToolConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_search_matches: DEFAULT_MAX_SEARCH_MATCHES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoFileEvidence {
    pub path: String,
    pub size_bytes: u64,
    pub content: String,
    pub evidence_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSearchMatch {
    pub path: String,
    pub line: usize,
    pub preview: String,
    pub evidence_kind: String,
}

pub fn read_file(
    repo_root: impl AsRef<Path>,
    requested_path: impl AsRef<Path>,
    config: &RepoToolConfig,
) -> Result<RepoFileEvidence, RepoToolError> {
    let root = canonical_repo_root(repo_root)?;
    let path = resolve_existing_repo_path(&root, requested_path)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(RepoToolError::NotAFile(relative_display(&root, &path)));
    }
    if metadata.len() > config.max_file_bytes {
        return Err(RepoToolError::FileTooLarge {
            path: relative_display(&root, &path),
            size_bytes: metadata.len(),
            max_bytes: config.max_file_bytes,
        });
    }
    let content = fs::read_to_string(&path).map_err(|source| RepoToolError::ReadText {
        path: relative_display(&root, &path),
        source,
    })?;
    Ok(RepoFileEvidence {
        path: relative_display(&root, &path),
        size_bytes: metadata.len(),
        content,
        evidence_kind: "repo_evidence".to_owned(),
    })
}

pub fn search_text(
    repo_root: impl AsRef<Path>,
    query: &str,
    config: &RepoToolConfig,
) -> Result<Vec<RepoSearchMatch>, RepoToolError> {
    if query.trim().is_empty() {
        return Err(RepoToolError::EmptyQuery);
    }
    let root = canonical_repo_root(repo_root)?;
    let mut matches = Vec::new();
    search_dir(&root, &root, query, config, &mut matches)?;
    Ok(matches)
}

fn search_dir(
    root: &Path,
    dir: &Path,
    query: &str,
    config: &RepoToolConfig,
    matches: &mut Vec<RepoSearchMatch>,
) -> Result<(), RepoToolError> {
    if matches.len() >= config.max_search_matches {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if matches.len() >= config.max_search_matches {
            break;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            search_dir(root, &path, query, config, matches)?;
            continue;
        }
        if file_type.is_file() {
            search_file(root, &path, query, config, matches)?;
        }
    }
    Ok(())
}

fn search_file(
    root: &Path,
    path: &Path,
    query: &str,
    config: &RepoToolConfig,
    matches: &mut Vec<RepoSearchMatch>,
) -> Result<(), RepoToolError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > config.max_file_bytes {
        return Ok(());
    }
    let Ok(content) = fs::read_to_string(path) else {
        return Ok(());
    };
    for (index, line) in content.lines().enumerate() {
        if matches.len() >= config.max_search_matches {
            break;
        }
        if line.contains(query) {
            matches.push(RepoSearchMatch {
                path: relative_display(root, path),
                line: index + 1,
                preview: line.trim().to_owned(),
                evidence_kind: "repo_evidence".to_owned(),
            });
        }
    }
    Ok(())
}

fn canonical_repo_root(repo_root: impl AsRef<Path>) -> Result<PathBuf, RepoToolError> {
    let root =
        fs::canonicalize(repo_root.as_ref()).map_err(|source| RepoToolError::InvalidRoot {
            path: repo_root.as_ref().display().to_string(),
            source,
        })?;
    if !root.is_dir() {
        return Err(RepoToolError::InvalidRootKind(root.display().to_string()));
    }
    Ok(root)
}

fn resolve_existing_repo_path(
    root: &Path,
    requested_path: impl AsRef<Path>,
) -> Result<PathBuf, RepoToolError> {
    let requested = requested_path.as_ref();
    if requested.is_absolute() {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }
    let resolved =
        fs::canonicalize(root.join(requested)).map_err(|source| RepoToolError::PathNotFound {
            path: requested.display().to_string(),
            source,
        })?;
    if !resolved.starts_with(root) {
        return Err(RepoToolError::PathOutsideRepo(
            requested.display().to_string(),
        ));
    }
    Ok(resolved)
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| SKIPPED_DIRS.contains(&name))
        .unwrap_or(false)
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[derive(Debug, Error)]
pub enum RepoToolError {
    #[error("invalid repo root {path}: {source}")]
    InvalidRoot {
        path: String,
        source: std::io::Error,
    },
    #[error("repo root is not a directory: {0}")]
    InvalidRootKind(String),
    #[error("path not found in repo {path}: {source}")]
    PathNotFound {
        path: String,
        source: std::io::Error,
    },
    #[error("path escapes repo root: {0}")]
    PathOutsideRepo(String),
    #[error("path is not a file: {0}")]
    NotAFile(String),
    #[error("file {path} is {size_bytes} bytes, over limit {max_bytes}")]
    FileTooLarge {
        path: String,
        size_bytes: u64,
        max_bytes: u64,
    },
    #[error("failed to read text from {path}: {source}")]
    ReadText {
        path: String,
        source: std::io::Error,
    },
    #[error("search query must not be empty")]
    EmptyQuery,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
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
    fn search_text_rejects_empty_query() {
        let root = temp_repo();

        let error = search_text(&root, "  ", &RepoToolConfig::default()).unwrap_err();

        assert!(matches!(error, RepoToolError::EmptyQuery));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_repo() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "coder-tools-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }
}
