use axum::{
    extract::{Path, State},
    Json,
};
use coder_store::CacheBucketUsage;

use crate::{
    ApiError, ApiState, CacheActionResponse, CacheBucketStatus, CacheStatusResponse,
    CacheTaskCancelResponse, CacheTaskResponse, CacheTasksResponse,
};

pub(crate) async fn cache_status(
    State(state): State<ApiState>,
) -> Result<Json<CacheStatusResponse>, ApiError> {
    state.store.ensure_local_layout()?;
    Ok(Json(CacheStatusResponse {
        repo_index: cache_bucket_status(state.store.cache_bucket_usage("repo-index")?),
        plugin_cache: cache_bucket_status(state.store.cache_bucket_usage("plugin-cache")?),
        skill_cache: cache_bucket_status(state.store.cache_bucket_usage("skill-cache")?),
        blob_store: cache_bucket_status(state.store.cache_bucket_usage("blobs")?),
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
