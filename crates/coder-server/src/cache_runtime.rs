use std::env;

use axum::{
    extract::{Path, State},
    Json,
};
use coder_store::CacheBucketUsage;
use coder_workflow::{browser_verifier_runtime_status, BrowserVerifierRuntimeStatus};

use crate::{
    ApiError, ApiState, BrowserVerifierCacheStatus, BrowserVerifierRuntimeCandidateStatus,
    CacheActionResponse, CacheBucketStatus, CacheStatusResponse, CacheTaskCancelResponse,
    CacheTaskResponse, CacheTasksResponse,
};

pub(crate) async fn cache_status(
    State(state): State<ApiState>,
) -> Result<Json<CacheStatusResponse>, ApiError> {
    state.store.ensure_local_layout()?;
    let browser_verifier_runtime_root = state
        .store
        .root()
        .join("tmp")
        .join("runtime-cache")
        .join("browser-verifier");
    let browser_verifier_status = browser_verifier_runtime_status(
        &env::current_dir()
            .unwrap_or_else(|_| ".".into())
            .to_string_lossy(),
        &browser_verifier_runtime_root,
    );
    Ok(Json(CacheStatusResponse {
        repo_index: cache_bucket_status(state.store.cache_bucket_usage("repo-index")?),
        plugin_cache: cache_bucket_status(state.store.cache_bucket_usage("plugin-cache")?),
        skill_cache: cache_bucket_status(state.store.cache_bucket_usage("skill-cache")?),
        blob_store: cache_bucket_status(state.store.cache_bucket_usage("blobs")?),
        browser_verifier: browser_verifier_cache_status(
            browser_verifier_status,
            cache_bucket_status(
                state
                    .store
                    .cache_bucket_usage("tmp/runtime-cache/browser-verifier")?,
            ),
        ),
    }))
}

fn cache_bucket_status(usage: CacheBucketUsage) -> CacheBucketStatus {
    CacheBucketStatus {
        entries: usage.entries,
        bytes: usage.bytes,
        stale: false,
        scanned_entries: usage.scanned_entries,
        entry_scan_limit: usage.entry_scan_limit,
        truncated: usage.truncated,
    }
}

fn browser_verifier_cache_status(
    status: BrowserVerifierRuntimeStatus,
    runtime_cache: CacheBucketStatus,
) -> BrowserVerifierCacheStatus {
    let node_path = status
        .node_path
        .as_ref()
        .map(|path| path.display().to_string());
    let resolved_node_modules = status
        .resolved_node_modules
        .as_ref()
        .map(|path| path.display().to_string());
    let state = if node_path.is_none() {
        "missing_node"
    } else if resolved_node_modules.is_none() {
        "missing_playwright"
    } else {
        "ready"
    };
    let message = match state {
        "ready" => "Browser verifier runtime is ready.".to_owned(),
        "missing_node" => {
            "Browser verifier runtime needs Node.js on PATH or CODER_NODE_BIN.".to_owned()
        }
        _ => "Browser verifier runtime needs Playwright in a Coder-owned runtime path.".to_owned(),
    };
    let candidates = status
        .candidates
        .into_iter()
        .map(|candidate| BrowserVerifierRuntimeCandidateStatus {
            source: candidate.source,
            path: candidate.path.display().to_string(),
            path_exists: candidate.path_exists,
            has_playwright_package: candidate.has_playwright_package,
        })
        .collect::<Vec<_>>();
    BrowserVerifierCacheStatus {
        status: state.to_owned(),
        runtime_root: status.runtime_root.display().to_string(),
        browsers_path: status.browsers_path.display().to_string(),
        runtime_cache,
        node_path,
        resolved_node_modules,
        candidate_count: candidates.len(),
        candidates,
        message,
    }
}

pub(crate) async fn cache_clear(
    State(state): State<ApiState>,
) -> Result<Json<CacheActionResponse>, ApiError> {
    let store = state.store.clear_disposable_caches()?;
    Ok(Json(CacheActionResponse {
        status: "completed".to_owned(),
        message: format!("Cleared {} disposable store entries.", store.entries),
        store,
    }))
}

pub(crate) async fn cache_reindex() -> Json<CacheTaskResponse> {
    Json(CacheTaskResponse {
        task_id: "repo-index-noop".to_owned(),
        status: "completed".to_owned(),
    })
}

pub(crate) async fn cache_tasks() -> Json<CacheTasksResponse> {
    Json(CacheTasksResponse { tasks: Vec::new() })
}

pub(crate) async fn cancel_cache_task(
    Path(task_id): Path<String>,
) -> Json<CacheTaskCancelResponse> {
    Json(CacheTaskCancelResponse {
        task_id,
        cancelled: false,
        status: "not_found".to_owned(),
    })
}
