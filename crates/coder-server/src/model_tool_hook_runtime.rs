use std::sync::Mutex;

use coder_core::RunId;
use coder_events::CoderEvent;
use coder_store::{RunStore, StoreError};
use serde_json::Value;

use crate::model_tool_hook_output::ModelToolHookEffects;

static MODEL_TOOL_EVENT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub(crate) enum ModelToolEventWriteError {
    LockPoisoned,
    Store(StoreError),
}

impl std::fmt::Display for ModelToolEventWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockPoisoned => formatter.write_str("model tool event lock poisoned"),
            Self::Store(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ModelToolEventWriteError {}

pub(crate) fn with_model_tool_event_lock<T>(body: impl FnOnce() -> T) -> Option<T> {
    let Ok(_guard) = MODEL_TOOL_EVENT_LOCK.lock() else {
        return None;
    };
    Some(body())
}

pub(crate) fn append_model_tool_event(
    store: &RunStore,
    run_id: &RunId,
    kind: impl Into<String>,
    payload: Value,
) -> bool {
    append_model_tool_event_checked(store, run_id, kind, payload).is_ok()
}

pub(crate) fn append_model_tool_event_checked(
    store: &RunStore,
    run_id: &RunId,
    kind: impl Into<String>,
    payload: Value,
) -> Result<u64, ModelToolEventWriteError> {
    let _guard = MODEL_TOOL_EVENT_LOCK
        .lock()
        .map_err(|_| ModelToolEventWriteError::LockPoisoned)?;
    let sequence = store
        .event_count(run_id)
        .map(|count| count as u64 + 1)
        .map_err(ModelToolEventWriteError::Store)?;
    let event = CoderEvent::new(run_id.clone(), sequence, kind, payload);
    store
        .append_event(run_id, &event)
        .map_err(ModelToolEventWriteError::Store)?;
    Ok(sequence)
}

pub(crate) struct ModelToolHookExecution {
    pub(crate) payload: Value,
    pub(crate) blocking_error: Option<String>,
    pub(crate) effects: ModelToolHookEffects,
}

impl ModelToolHookExecution {
    pub(crate) fn with_output_preview(
        mut self,
        output_preview: String,
        output_truncated: bool,
    ) -> Self {
        if let Value::Object(payload) = &mut self.payload {
            payload.insert("output_preview".to_owned(), Value::String(output_preview));
            payload.insert("output_truncated".to_owned(), Value::Bool(output_truncated));
        }
        self
    }

    pub(crate) fn with_hook_json(mut self, hook_json: Value) -> Self {
        if let Value::Object(payload) = &mut self.payload {
            payload.insert("hook_json_output".to_owned(), hook_json);
        }
        self
    }

    pub(crate) fn with_processed_prompt_preview(
        mut self,
        prompt_preview: String,
        prompt_truncated: bool,
    ) -> Self {
        if let Value::Object(payload) = &mut self.payload {
            payload.insert(
                "processed_prompt_preview".to_owned(),
                Value::String(prompt_preview),
            );
            payload.insert(
                "processed_prompt_truncated".to_owned(),
                Value::Bool(prompt_truncated),
            );
        }
        self
    }
}
