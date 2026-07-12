use crate::api_types::ModelToolExecuteResponse;
use coder_core::RunId;
use coder_events::LargePayloadRef;
use coder_harness::HarnessRunEventRef;
use coder_store::{ContentReplacementRecord, DurableJsonlPageOptions, RunStore, StoreError};
use coder_workflow::ModelToolResultBlock;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

const MODEL_TOOL_RESULT_STORAGE_CONTRACT: &str = "coder.model_tool_result_storage.v1";
const MODEL_TOOL_CONTENT_REPLACEMENT_CLEANUP_CONTRACT: &str =
    "coder.model_tool_content_replacement_cleanup.v1";
const CLAUDE_DEFAULT_MAX_RESULT_SIZE_CHARS: usize = 50_000;
const CLAUDE_MAX_TOOL_RESULT_TOKENS: usize = 100_000;
const CLAUDE_BYTES_PER_TOKEN: usize = 4;
const CLAUDE_MAX_TOOL_RESULT_BYTES: usize = CLAUDE_MAX_TOOL_RESULT_TOKENS * CLAUDE_BYTES_PER_TOKEN;
const CLAUDE_MAX_TOOL_RESULTS_PER_MESSAGE_CHARS: usize = 200_000;
const CLAUDE_TOOL_RESULT_PREVIEW_SIZE_BYTES: usize = 2_000;
const PERSISTED_OUTPUT_TAG: &str = "<persisted-output>";
const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";

#[derive(Default)]
pub(crate) struct ModelToolContentReplacementState {
    seen_ids: BTreeSet<String>,
    replacements: BTreeMap<String, StoredToolResultReplacement>,
    tool_run_ids: BTreeMap<String, RunId>,
    loaded_run_ids: BTreeSet<String>,
}

impl ModelToolContentReplacementState {
    pub(crate) fn record_tool_run_id(&mut self, tool_use_id: String, run_id: RunId) {
        self.tool_run_ids.insert(tool_use_id, run_id);
    }
}

pub(crate) fn clear_content_replacement_state_for_run(
    state: &Arc<Mutex<ModelToolContentReplacementState>>,
    run_id: &RunId,
) -> Value {
    let mut payload = json!({
        "contract": MODEL_TOOL_CONTENT_REPLACEMENT_CLEANUP_CONTRACT,
        "source": "coder-server",
        "policy": "post_compact_release_run_scoped_replacement_state",
        "run_id": run_id.as_str(),
        "status": "not_started",
        "removed_tool_run_ids": 0,
        "removed_seen_ids": 0,
        "removed_replacements": 0,
        "removed_loaded_run_id": false
    });

    let Ok(mut state) = state.lock() else {
        if let Value::Object(object) = &mut payload {
            object.insert("status".to_owned(), json!("lock_unavailable"));
        }
        return payload;
    };

    let tool_ids = state
        .tool_run_ids
        .iter()
        .filter(|(_, candidate_run_id)| candidate_run_id.as_str() == run_id.as_str())
        .map(|(tool_use_id, _)| tool_use_id.clone())
        .collect::<Vec<_>>();
    let removed_loaded_run_id = state.loaded_run_ids.remove(run_id.as_str());
    let mut removed_seen_ids = 0usize;
    let mut removed_replacements = 0usize;
    for tool_use_id in &tool_ids {
        if state.seen_ids.remove(tool_use_id) {
            removed_seen_ids += 1;
        }
        if state.replacements.remove(tool_use_id).is_some() {
            removed_replacements += 1;
        }
    }
    for tool_use_id in &tool_ids {
        state.tool_run_ids.remove(tool_use_id);
    }

    if let Value::Object(object) = &mut payload {
        object.insert("status".to_owned(), json!("completed"));
        object.insert("removed_tool_run_ids".to_owned(), json!(tool_ids.len()));
        object.insert("removed_seen_ids".to_owned(), json!(removed_seen_ids));
        object.insert(
            "removed_replacements".to_owned(),
            json!(removed_replacements),
        );
        object.insert(
            "removed_loaded_run_id".to_owned(),
            json!(removed_loaded_run_id),
        );
    }
    payload
}

#[derive(Clone)]
struct StoredToolResultReplacement {
    replacement: String,
    blob_ref: Option<String>,
}

pub(crate) fn maybe_persist_large_model_tool_result(
    store: &RunStore,
    response: &mut ModelToolExecuteResponse,
) -> Result<(), StoreError> {
    let original_size_chars = response.content.chars().count();
    if original_size_chars <= CLAUDE_DEFAULT_MAX_RESULT_SIZE_CHARS {
        return Ok(());
    }

    let original_content = std::mem::take(&mut response.content);
    let original_size_bytes = original_content.len();
    let large_ref = match store
        .write_large_text_ref_with_limit(&original_content, CLAUDE_TOOL_RESULT_PREVIEW_SIZE_BYTES)
    {
        Ok(large_ref) => large_ref,
        Err(error) => {
            response.content = original_content;
            return Err(error);
        }
    };
    let replacement = large_model_tool_result_message(original_size_bytes, &large_ref);
    let persisted_size_bytes = replacement.len();

    response.content = replacement;
    response.content_truncated = true;
    add_model_tool_result_blob_ref(&mut response.refs, &large_ref.blob_ref);

    let metadata = json!({
        "contract": MODEL_TOOL_RESULT_STORAGE_CONTRACT,
        "source": "coder-server",
        "policy": "persist_large_tool_result",
        "threshold_chars": CLAUDE_DEFAULT_MAX_RESULT_SIZE_CHARS,
        "max_tool_result_tokens": CLAUDE_MAX_TOOL_RESULT_TOKENS,
        "bytes_per_token": CLAUDE_BYTES_PER_TOKEN,
        "max_tool_result_bytes": CLAUDE_MAX_TOOL_RESULT_BYTES,
        "max_tool_results_per_message_chars": CLAUDE_MAX_TOOL_RESULTS_PER_MESSAGE_CHARS,
        "preview_size_bytes": CLAUDE_TOOL_RESULT_PREVIEW_SIZE_BYTES,
        "original_size_chars": original_size_chars,
        "original_size_bytes": original_size_bytes,
        "persisted_size_bytes": persisted_size_bytes,
        "estimated_original_tokens": estimated_tokens(original_size_bytes),
        "estimated_persisted_tokens": estimated_tokens(persisted_size_bytes),
        "truncated": large_ref.truncated,
        "preview": large_ref.preview,
        "blob_ref": large_ref.blob_ref
    });
    insert_model_tool_result_storage_metadata(&mut response.payload, metadata);
    Ok(())
}

pub(crate) fn enforce_aggregate_model_tool_result_budget(
    store: &RunStore,
    state: &Arc<Mutex<ModelToolContentReplacementState>>,
    mut results: Vec<ModelToolResultBlock>,
) -> Vec<ModelToolResultBlock> {
    let candidates = aggregate_tool_result_candidates(&results);
    if candidates.is_empty() {
        return results;
    }
    load_persisted_content_replacements_for_candidates(store, state, &candidates);

    let mut replacement_map = BTreeMap::new();
    let mut reapplied_count = 0usize;
    let selected = {
        let Ok(mut state) = state.lock() else {
            return results;
        };
        let mut frozen_size = 0usize;
        let mut fresh = Vec::new();
        for candidate in &candidates {
            if let Some(stored) = state.replacements.get(&candidate.tool_use_id) {
                replacement_map.insert(candidate.tool_use_id.clone(), stored.clone());
                reapplied_count += 1;
            } else if state.seen_ids.contains(&candidate.tool_use_id) {
                frozen_size = frozen_size.saturating_add(candidate.size_chars);
            } else {
                fresh.push(candidate.clone());
            }
        }

        let fresh_size = fresh
            .iter()
            .map(|candidate| candidate.size_chars)
            .sum::<usize>();
        let selected =
            if frozen_size.saturating_add(fresh_size) > CLAUDE_MAX_TOOL_RESULTS_PER_MESSAGE_CHARS {
                select_fresh_aggregate_tool_results_to_replace(
                    fresh,
                    frozen_size,
                    CLAUDE_MAX_TOOL_RESULTS_PER_MESSAGE_CHARS,
                )
            } else {
                Vec::new()
            };
        let selected_ids = selected
            .iter()
            .map(|candidate| candidate.tool_use_id.as_str())
            .collect::<BTreeSet<_>>();
        for candidate in &candidates {
            if !selected_ids.contains(candidate.tool_use_id.as_str()) {
                state.seen_ids.insert(candidate.tool_use_id.clone());
            }
        }
        selected
    };

    let mut fresh_replacements = BTreeMap::new();
    for candidate in selected {
        let original_size_bytes = candidate.content.len();
        let Ok(large_ref) = store.write_large_text_ref_with_limit(
            &candidate.content,
            CLAUDE_TOOL_RESULT_PREVIEW_SIZE_BYTES,
        ) else {
            if let Ok(mut state) = state.lock() {
                state.seen_ids.insert(candidate.tool_use_id);
            }
            continue;
        };
        let replacement = large_model_tool_result_message(original_size_bytes, &large_ref);
        let stored = StoredToolResultReplacement {
            replacement: replacement.clone(),
            blob_ref: Some(large_ref.blob_ref.clone()),
        };
        let persistence = persist_content_replacement_record(
            store,
            state,
            &candidate.tool_use_id,
            replacement.clone(),
        );
        if let Ok(mut state) = state.lock() {
            state.seen_ids.insert(candidate.tool_use_id.clone());
            state
                .replacements
                .insert(candidate.tool_use_id.clone(), stored.clone());
        }
        replacement_map.insert(candidate.tool_use_id.clone(), stored.clone());
        fresh_replacements.insert(
            candidate.tool_use_id.clone(),
            aggregate_tool_result_storage_metadata(
                &candidate.tool_use_id,
                &replacement,
                &large_ref,
                original_size_bytes,
                candidate.size_chars,
                reapplied_count,
                persistence,
            ),
        );
    }

    if replacement_map.is_empty() {
        return results;
    }

    for result in &mut results {
        let Some(stored) = replacement_map.get(&result.tool_use_id) else {
            continue;
        };
        result.content = stored.replacement.clone();
        result.content_truncated = true;
        if let Some(blob_ref) = stored
            .blob_ref
            .clone()
            .or_else(|| blob_ref_from_persisted_output(&stored.replacement))
        {
            add_model_tool_result_blob_ref(&mut result.refs, &blob_ref);
        }
        let metadata = fresh_replacements
            .remove(&result.tool_use_id)
            .unwrap_or_else(|| {
                aggregate_tool_result_reapplied_metadata(
                    &result.tool_use_id,
                    &stored.replacement,
                    reapplied_count,
                )
            });
        insert_model_tool_result_storage_metadata(&mut result.payload, metadata);
    }

    results
}

pub(crate) fn model_tool_turn_attachment_run_ids(
    host_context_run_id: Option<&str>,
    results: &[ModelToolResultBlock],
    state: &Arc<Mutex<ModelToolContentReplacementState>>,
) -> Vec<RunId> {
    let mut run_ids = BTreeSet::new();
    if let Some(run_id) = host_context_run_id {
        run_ids.insert(run_id.to_owned());
    }
    if let Ok(state) = state.lock() {
        for result in results {
            if let Some(run_id) = state.tool_run_ids.get(&result.tool_use_id) {
                run_ids.insert(run_id.as_str().to_owned());
            }
        }
    }
    run_ids.into_iter().map(RunId::from_string).collect()
}

#[derive(Clone)]
struct AggregateToolResultCandidate {
    index: usize,
    tool_use_id: String,
    size_chars: usize,
    content: String,
}

#[derive(Clone)]
struct ContentReplacementPersistence {
    persisted: bool,
    run_id: Option<String>,
    sequence: Option<u64>,
    record_ref: Option<String>,
    error: Option<String>,
}

fn load_persisted_content_replacements_for_candidates(
    store: &RunStore,
    state: &Arc<Mutex<ModelToolContentReplacementState>>,
    candidates: &[AggregateToolResultCandidate],
) {
    let run_ids = {
        let Ok(mut state) = state.lock() else {
            return;
        };
        let mut run_ids = Vec::new();
        for candidate in candidates {
            let Some(run_id) = state.tool_run_ids.get(&candidate.tool_use_id).cloned() else {
                continue;
            };
            if state.loaded_run_ids.insert(run_id.as_str().to_owned()) {
                run_ids.push(run_id);
            }
        }
        run_ids
    };

    for run_id in run_ids {
        let Ok(options) =
            DurableJsonlPageOptions::tail(crate::RUN_RESUME_CONTENT_REPLACEMENT_RECORD_LIMIT)
        else {
            continue;
        };
        let Ok(page) = store.read_run_content_replacement_records_page(&run_id, options) else {
            continue;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        for entry in page.records {
            for replacement in entry.replacements {
                if replacement.kind != "tool-result" {
                    continue;
                }
                state.seen_ids.insert(replacement.tool_use_id.clone());
                state.replacements.insert(
                    replacement.tool_use_id,
                    StoredToolResultReplacement {
                        blob_ref: blob_ref_from_persisted_output(&replacement.replacement),
                        replacement: replacement.replacement,
                    },
                );
            }
        }
    }
}

fn persist_content_replacement_record(
    store: &RunStore,
    state: &Arc<Mutex<ModelToolContentReplacementState>>,
    tool_use_id: &str,
    replacement: String,
) -> ContentReplacementPersistence {
    let run_id = {
        let Ok(state) = state.lock() else {
            return ContentReplacementPersistence {
                persisted: false,
                run_id: None,
                sequence: None,
                record_ref: None,
                error: Some("content replacement state lock unavailable".to_owned()),
            };
        };
        state.tool_run_ids.get(tool_use_id).cloned()
    };
    let Some(run_id) = run_id else {
        return ContentReplacementPersistence {
            persisted: false,
            run_id: None,
            sequence: None,
            record_ref: None,
            error: Some("run_id unavailable for tool result".to_owned()),
        };
    };

    match store.append_run_content_replacement_record_next(
        &run_id,
        vec![ContentReplacementRecord {
            kind: "tool-result".to_owned(),
            tool_use_id: tool_use_id.to_owned(),
            replacement,
        }],
    ) {
        Ok(sequence) => ContentReplacementPersistence {
            persisted: true,
            run_id: Some(run_id.as_str().to_owned()),
            sequence: Some(sequence),
            record_ref: Some(format!(
                "content-replacements://runs/{}/records/{sequence}",
                run_id.as_str()
            )),
            error: None,
        },
        Err(error) => ContentReplacementPersistence {
            persisted: false,
            run_id: Some(run_id.as_str().to_owned()),
            sequence: None,
            record_ref: None,
            error: Some(error.to_string()),
        },
    }
}

fn aggregate_tool_result_candidates(
    results: &[ModelToolResultBlock],
) -> Vec<AggregateToolResultCandidate> {
    results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| {
            if result.content.trim().is_empty() || result.content.starts_with(PERSISTED_OUTPUT_TAG)
            {
                return None;
            }
            Some(AggregateToolResultCandidate {
                index,
                tool_use_id: result.tool_use_id.clone(),
                size_chars: result.content.chars().count(),
                content: result.content.clone(),
            })
        })
        .collect()
}

fn select_fresh_aggregate_tool_results_to_replace(
    mut fresh: Vec<AggregateToolResultCandidate>,
    frozen_size: usize,
    limit: usize,
) -> Vec<AggregateToolResultCandidate> {
    fresh.sort_by(|left, right| {
        right
            .size_chars
            .cmp(&left.size_chars)
            .then_with(|| left.index.cmp(&right.index))
    });
    let mut remaining = frozen_size.saturating_add(
        fresh
            .iter()
            .map(|candidate| candidate.size_chars)
            .sum::<usize>(),
    );
    let mut selected = Vec::new();
    for candidate in fresh {
        if remaining <= limit {
            break;
        }
        remaining = remaining.saturating_sub(candidate.size_chars);
        selected.push(candidate);
    }
    selected
}

fn aggregate_tool_result_storage_metadata(
    tool_use_id: &str,
    replacement: &str,
    large_ref: &LargePayloadRef,
    original_size_bytes: usize,
    original_size_chars: usize,
    reapplied_count: usize,
    persistence: ContentReplacementPersistence,
) -> Value {
    json!({
        "contract": MODEL_TOOL_RESULT_STORAGE_CONTRACT,
        "source": "coder-server",
        "policy": "persist_aggregate_tool_result_budget",
        "selection_strategy": "largest_fresh_results",
        "max_tool_results_per_message_chars": CLAUDE_MAX_TOOL_RESULTS_PER_MESSAGE_CHARS,
        "preview_size_bytes": CLAUDE_TOOL_RESULT_PREVIEW_SIZE_BYTES,
        "original_size_chars": original_size_chars,
        "original_size_bytes": original_size_bytes,
        "persisted_size_bytes": replacement.len(),
        "estimated_original_tokens": estimated_tokens(original_size_bytes),
        "estimated_persisted_tokens": estimated_tokens(replacement.len()),
        "reapplied": false,
        "reapplied_count": reapplied_count,
        "truncated": large_ref.truncated,
        "preview": large_ref.preview,
        "blob_ref": large_ref.blob_ref,
        "content_replacement_record": {
            "kind": "tool-result",
            "toolUseId": tool_use_id,
            "replacement": replacement
        },
        "content_replacement_persistence": content_replacement_persistence_json(persistence)
    })
}

fn aggregate_tool_result_reapplied_metadata(
    tool_use_id: &str,
    replacement: &str,
    reapplied_count: usize,
) -> Value {
    json!({
        "contract": MODEL_TOOL_RESULT_STORAGE_CONTRACT,
        "source": "coder-server",
        "policy": "persist_aggregate_tool_result_budget",
        "selection_strategy": "stable_replacement_reapply",
        "max_tool_results_per_message_chars": CLAUDE_MAX_TOOL_RESULTS_PER_MESSAGE_CHARS,
        "preview_size_bytes": CLAUDE_TOOL_RESULT_PREVIEW_SIZE_BYTES,
        "persisted_size_bytes": replacement.len(),
        "estimated_persisted_tokens": estimated_tokens(replacement.len()),
        "reapplied": true,
        "reapplied_count": reapplied_count,
        "blob_ref": blob_ref_from_persisted_output(replacement),
        "content_replacement_record": {
            "kind": "tool-result",
            "toolUseId": tool_use_id,
            "replacement": replacement
        }
    })
}

fn content_replacement_persistence_json(persistence: ContentReplacementPersistence) -> Value {
    json!({
        "persisted": persistence.persisted,
        "run_id": persistence.run_id,
        "sequence": persistence.sequence,
        "record_ref": persistence.record_ref,
        "error": persistence.error
    })
}

fn blob_ref_from_persisted_output(replacement: &str) -> Option<String> {
    let marker = "Full output saved to: ";
    let start = replacement.find(marker)? + marker.len();
    let rest = &replacement[start..];
    let blob_ref = rest
        .split_whitespace()
        .next()
        .map(str::trim)
        .filter(|value| value.starts_with("blob://sha256/"))?;
    Some(blob_ref.to_owned())
}

fn large_model_tool_result_message(
    original_size_bytes: usize,
    large_ref: &LargePayloadRef,
) -> String {
    let mut message = format!(
        "{PERSISTED_OUTPUT_TAG}\nOutput too large ({}). Full output saved to: {}\n\nPreview (first {}):\n{}",
        format_file_size(original_size_bytes),
        large_ref.blob_ref,
        format_file_size(CLAUDE_TOOL_RESULT_PREVIEW_SIZE_BYTES),
        large_ref.preview
    );
    if large_ref.truncated {
        message.push_str("\n...\n");
    } else {
        message.push('\n');
    }
    message.push_str(PERSISTED_OUTPUT_CLOSING_TAG);
    message
}

fn insert_model_tool_result_storage_metadata(payload: &mut Value, metadata: Value) {
    if let Some(payload) = payload.as_object_mut() {
        payload.insert("model_tool_result_storage".to_owned(), metadata);
        return;
    }

    let original_payload = std::mem::take(payload);
    *payload = json!({
        "result": original_payload,
        "model_tool_result_storage": metadata
    });
}

fn add_model_tool_result_blob_ref(refs: &mut Vec<HarnessRunEventRef>, blob_ref: &str) {
    refs.push(HarnessRunEventRef {
        label: "model_tool_result_blob".to_owned(),
        uri: blob_ref.to_owned(),
    });
    refs.sort_by(|left, right| {
        (left.label.as_str(), left.uri.as_str()).cmp(&(right.label.as_str(), right.uri.as_str()))
    });
    refs.dedup_by(|left, right| left.label == right.label && left.uri == right.uri);
}

fn estimated_tokens(bytes: usize) -> usize {
    bytes.div_ceil(CLAUDE_BYTES_PER_TOKEN)
}

fn format_file_size(size_in_bytes: usize) -> String {
    let kb = size_in_bytes as f64 / 1024.0;
    if kb < 1.0 {
        return format!("{size_in_bytes} bytes");
    }
    if kb < 1024.0 {
        return format!("{}KB", trim_trailing_decimal_zero(kb));
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{}MB", trim_trailing_decimal_zero(mb));
    }
    let gb = mb / 1024.0;
    format!("{}GB", trim_trailing_decimal_zero(gb))
}

fn trim_trailing_decimal_zero(value: f64) -> String {
    let formatted = format!("{value:.1}");
    formatted
        .strip_suffix(".0")
        .unwrap_or(&formatted)
        .to_owned()
}
