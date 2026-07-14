use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use coder_events::{
    redact_payload, OutputEvent, OutputPriority, OutputStreamBehavior, SpeechTokenKind,
};
use coder_store::{DurableJsonlPageOptions, MAX_DURABLE_JSONL_PAGE_LIMIT};
use serde_json::json;
use tokio::sync::watch;
use uuid::Uuid;

use crate::api_types::{
    conversation_assistant_turn, conversation_user_turn, ConversationSession,
    ConversationSessionCreateRequest, ConversationSessionResponse, ConversationSteerRequest,
    ConversationTurn, ConversationTurnControlResponse, ConversationTurnRequest,
    ConversationTurnResponse,
};
use crate::conversation_provider::conversation_reply;
use crate::{ApiError, ApiState};

const SESSION_CACHE_LIMIT: usize = 200;
const SESSION_MAX_TURNS: usize = 64;

#[derive(Debug)]
struct ActiveConversationTurn {
    turn_id: String,
    cancel: watch::Sender<bool>,
    pending_input: VecDeque<String>,
}

#[derive(Debug)]
struct StoredConversationSession {
    session: ConversationSession,
    last_accessed: SystemTime,
    active_turn: Option<ActiveConversationTurn>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ConversationRuntime {
    sessions: Arc<Mutex<BTreeMap<String, StoredConversationSession>>>,
}

impl ConversationRuntime {
    pub(crate) fn create_session(
        &self,
        state: &ApiState,
        request: ConversationSessionCreateRequest,
    ) -> Result<(ConversationSessionResponse, Vec<String>), ApiError> {
        let session = ConversationSession {
            session_id: Uuid::new_v4().to_string(),
            repo_root: normalized_repo_root(request.repo),
            turns: Vec::new(),
        };
        append_session_lifecycle_record(state, &session, "session.created")?;
        let evicted_session_ids = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sessions.insert(session.session_id.clone(), stored_session(session.clone()));
            prune_sessions(&mut sessions, Some(&session.session_id))
        };
        Ok((ConversationSessionResponse { session }, evicted_session_ids))
    }

    pub(crate) fn get_session(
        &self,
        state: &ApiState,
        session_id: &str,
    ) -> Result<(ConversationSessionResponse, Vec<String>), ApiError> {
        if let Some(session) = self.cached_session(session_id) {
            return Ok((ConversationSessionResponse { session }, Vec::new()));
        }

        let recovered = recover_session(state, session_id)?;
        let (session, evicted_session_ids) = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let session = {
                let stored = sessions
                    .entry(session_id.to_owned())
                    .or_insert_with(|| stored_session(recovered));
                stored.last_accessed = SystemTime::now();
                stored.session.clone()
            };
            let evicted_session_ids = prune_sessions(&mut sessions, Some(session_id));
            (session, evicted_session_ids)
        };
        Ok((ConversationSessionResponse { session }, evicted_session_ids))
    }

    pub(crate) async fn turn(
        &self,
        state: &ApiState,
        session_id: &str,
        request: ConversationTurnRequest,
    ) -> Result<ConversationTurnResponse, ApiError> {
        let message = normalized_message(request.message)?;
        let turn_id = Uuid::new_v4().to_string();
        let (mut cancel, mut session, user_turn) =
            self.begin_turn(session_id, &turn_id, request.repo, message)?;
        if let Err(error) = append_message_record(state, session_id, &user_turn) {
            self.clear_active_turn(session_id, &turn_id);
            return Err(error);
        }
        append_session_lifecycle_record(state, &session, "session.turn.started")?;
        publish_conversation_output(state, session_id, &turn_id, OutputEvent::TurnStarted);
        publish_conversation_output(state, session_id, &turn_id, OutputEvent::TextStarted);

        let mut assistant_messages = Vec::new();
        loop {
            let reply = tokio::select! {
                result = conversation_reply(state, &session, |delta| {
                    publish_conversation_output(
                        state,
                        session_id,
                        &turn_id,
                        OutputEvent::TextDelta { delta: delta.to_owned() },
                    );
                }) => result,
                changed = cancel.changed() => {
                    if changed.is_err() || *cancel.borrow() {
                        let session = self.finish_cancelled_turn(session_id, &turn_id)?;
                        publish_conversation_output(
                            state,
                            session_id,
                            &turn_id,
                            OutputEvent::TurnCancelled { reason: "user_interrupted".to_owned() },
                        );
                        append_session_lifecycle_record(state, &session, "session.turn.cancelled")?;
                        return Ok(ConversationTurnResponse {
                            session,
                            turn_id,
                            status: "cancelled".to_owned(),
                            assistant_message: assistant_messages.join("\n\n"),
                        });
                    }
                    continue;
                }
            };

            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    let session = self.finish_failed_turn(session_id, &turn_id)?;
                    publish_conversation_output(
                        state,
                        session_id,
                        &turn_id,
                        OutputEvent::Error {
                            message: error.message.clone(),
                            recoverable: true,
                        },
                    );
                    publish_conversation_output(
                        state,
                        session_id,
                        &turn_id,
                        OutputEvent::TurnCancelled {
                            reason: "provider_error".to_owned(),
                        },
                    );
                    append_session_lifecycle_record(state, &session, "session.turn.failed")?;
                    return Err(error);
                }
            };

            publish_speech_intent(state, session_id, &turn_id, &reply);
            assistant_messages.push(reply.clone());
            let outcome = self.commit_reply(session_id, &turn_id, reply)?;
            append_message_record(state, session_id, &outcome.assistant_turn)?;
            for pending_turn in &outcome.pending_turns {
                append_message_record(state, session_id, pending_turn)?;
            }
            session = outcome.session;
            if outcome.finished {
                break;
            }
        }

        let assistant_message = assistant_messages.join("\n\n");
        publish_conversation_output(
            state,
            session_id,
            &turn_id,
            OutputEvent::TextCompleted {
                text: assistant_message.clone(),
            },
        );
        append_session_lifecycle_record(state, &session, "session.turn.completed")?;
        publish_conversation_output(state, session_id, &turn_id, OutputEvent::TurnCompleted);
        Ok(ConversationTurnResponse {
            session,
            turn_id,
            status: "completed".to_owned(),
            assistant_message,
        })
    }

    pub(crate) fn interrupt(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<ConversationTurnControlResponse, ApiError> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = sessions
            .get(session_id)
            .ok_or_else(|| session_not_found(session_id))?;
        let active = expected_active_turn(stored, turn_id)?;
        let _ = active.cancel.send(true);
        Ok(turn_control_response(
            session_id,
            turn_id,
            "interrupt_requested",
        ))
    }

    pub(crate) fn steer(
        &self,
        session_id: &str,
        turn_id: &str,
        request: ConversationSteerRequest,
    ) -> Result<ConversationTurnControlResponse, ApiError> {
        let message = normalized_message(request.message)?;
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = sessions
            .get_mut(session_id)
            .ok_or_else(|| session_not_found(session_id))?;
        let active = expected_active_turn_mut(stored, turn_id)?;
        active.pending_input.push_back(message);
        Ok(turn_control_response(session_id, turn_id, "accepted"))
    }

    fn cached_session(&self, session_id: &str) -> Option<ConversationSession> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = sessions.get_mut(session_id)?;
        stored.last_accessed = SystemTime::now();
        Some(stored.session.clone())
    }

    fn begin_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        repo: Option<String>,
        message: String,
    ) -> Result<(watch::Receiver<bool>, ConversationSession, ConversationTurn), ApiError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = sessions
            .get_mut(session_id)
            .ok_or_else(|| session_not_found(session_id))?;
        if let Some(active) = &stored.active_turn {
            return Err(ApiError::conflict(format!(
                "session '{session_id}' already has active turn '{}'",
                active.turn_id
            )));
        }
        if let Some(repo_root) = normalized_repo_root(repo) {
            stored.session.repo_root = Some(repo_root);
        }
        let user_turn = conversation_user_turn(message);
        stored.session.turns.push(user_turn.clone());
        trim_turns(&mut stored.session);
        stored.last_accessed = SystemTime::now();
        let (cancel, receiver) = watch::channel(false);
        stored.active_turn = Some(ActiveConversationTurn {
            turn_id: turn_id.to_owned(),
            cancel,
            pending_input: VecDeque::new(),
        });
        Ok((receiver, stored.session.clone(), user_turn))
    }

    fn commit_reply(
        &self,
        session_id: &str,
        turn_id: &str,
        reply: String,
    ) -> Result<CommittedReply, ApiError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = sessions
            .get_mut(session_id)
            .ok_or_else(|| session_not_found(session_id))?;
        let pending_input = {
            let active = expected_active_turn_mut(stored, turn_id)?;
            std::mem::take(&mut active.pending_input)
        };
        let assistant_turn = conversation_assistant_turn(reply);
        stored.session.turns.push(assistant_turn.clone());
        let pending_turns = pending_input
            .into_iter()
            .map(conversation_user_turn)
            .collect::<Vec<_>>();
        stored.session.turns.extend(pending_turns.iter().cloned());
        let finished = pending_turns.is_empty();
        if finished {
            stored.active_turn = None;
        }
        trim_turns(&mut stored.session);
        stored.last_accessed = SystemTime::now();
        Ok(CommittedReply {
            session: stored.session.clone(),
            assistant_turn,
            pending_turns,
            finished,
        })
    }

    fn finish_cancelled_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<ConversationSession, ApiError> {
        self.finish_terminal_turn(session_id, turn_id)
    }

    fn finish_failed_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<ConversationSession, ApiError> {
        self.finish_terminal_turn(session_id, turn_id)
    }

    fn finish_terminal_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<ConversationSession, ApiError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = sessions
            .get_mut(session_id)
            .ok_or_else(|| session_not_found(session_id))?;
        expected_active_turn(stored, turn_id)?;
        stored.active_turn = None;
        stored.last_accessed = SystemTime::now();
        Ok(stored.session.clone())
    }

    fn clear_active_turn(&self, session_id: &str, turn_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(stored) = sessions.get_mut(session_id) {
                if stored
                    .active_turn
                    .as_ref()
                    .is_some_and(|active| active.turn_id == turn_id)
                {
                    stored.active_turn = None;
                }
            }
        }
    }
}

#[derive(Debug)]
struct CommittedReply {
    session: ConversationSession,
    assistant_turn: ConversationTurn,
    pending_turns: Vec<ConversationTurn>,
    finished: bool,
}

fn stored_session(session: ConversationSession) -> StoredConversationSession {
    StoredConversationSession {
        session,
        last_accessed: SystemTime::now(),
        active_turn: None,
    }
}

fn recover_session(state: &ApiState, session_id: &str) -> Result<ConversationSession, ApiError> {
    let records = state
        .store
        .read_session_records_page(
            session_id,
            DurableJsonlPageOptions::tail(MAX_DURABLE_JSONL_PAGE_LIMIT)?,
        )?
        .records;
    if records.is_empty() {
        return Err(session_not_found(session_id));
    }
    let mut session = ConversationSession {
        session_id: session_id.to_owned(),
        repo_root: None,
        turns: Vec::new(),
    };
    for record in records {
        if let Some(repo_root) = record
            .payload
            .get("repo_root")
            .and_then(|value| value.as_str())
        {
            session.repo_root = Some(repo_root.to_owned());
        }
        if record.kind == "session.message.appended" {
            let turn = record
                .payload
                .get("turn")
                .cloned()
                .and_then(|value| serde_json::from_value::<ConversationTurn>(value).ok());
            if let Some(turn) = turn {
                session.turns.push(turn);
            }
        }
    }
    trim_turns(&mut session);
    Ok(session)
}

fn append_message_record(
    state: &ApiState,
    session_id: &str,
    turn: &ConversationTurn,
) -> Result<(), ApiError> {
    state.store.append_session_record_next(
        session_id,
        "session.message.appended",
        redact_payload(json!({ "turn": turn })),
    )?;
    Ok(())
}

fn append_session_lifecycle_record(
    state: &ApiState,
    session: &ConversationSession,
    kind: &str,
) -> Result<(), ApiError> {
    state.store.append_session_record_next(
        &session.session_id,
        kind,
        json!({
            "repo_root": session.repo_root,
            "turn_count": session.turns.len()
        }),
    )?;
    Ok(())
}

fn expected_active_turn<'a>(
    stored: &'a StoredConversationSession,
    turn_id: &str,
) -> Result<&'a ActiveConversationTurn, ApiError> {
    let active = stored
        .active_turn
        .as_ref()
        .ok_or_else(|| ApiError::conflict("session has no active turn"))?;
    if active.turn_id != turn_id {
        return Err(ApiError::conflict(format!(
            "expected active turn '{}' but found '{}'",
            turn_id, active.turn_id
        )));
    }
    Ok(active)
}

fn expected_active_turn_mut<'a>(
    stored: &'a mut StoredConversationSession,
    turn_id: &str,
) -> Result<&'a mut ActiveConversationTurn, ApiError> {
    let active = stored
        .active_turn
        .as_mut()
        .ok_or_else(|| ApiError::conflict("session has no active turn"))?;
    if active.turn_id != turn_id {
        return Err(ApiError::conflict(format!(
            "expected active turn '{}' but found '{}'",
            turn_id, active.turn_id
        )));
    }
    Ok(active)
}

fn turn_control_response(
    session_id: &str,
    turn_id: &str,
    status: &str,
) -> ConversationTurnControlResponse {
    ConversationTurnControlResponse {
        session_id: session_id.to_owned(),
        turn_id: turn_id.to_owned(),
        status: status.to_owned(),
    }
}

fn normalized_message(message: String) -> Result<String, ApiError> {
    let message = message.trim().to_owned();
    if message.is_empty() {
        Err(ApiError::bad_request("message must not be empty"))
    } else {
        Ok(message)
    }
}

fn session_not_found(session_id: &str) -> ApiError {
    ApiError::not_found(format!("session '{session_id}' was not found"))
}

fn publish_conversation_output(
    state: &ApiState,
    session_id: &str,
    turn_id: &str,
    output: OutputEvent,
) {
    state.session_host.publish_output(
        session_id,
        Some(turn_id.to_owned()),
        "conversation",
        OutputPriority::Normal,
        output,
    );
}

fn publish_speech_intent(state: &ApiState, session_id: &str, turn_id: &str, text: &str) {
    let intent_id = format!("speech-{turn_id}-{}", Uuid::new_v4());
    let stream_id = format!("text-{turn_id}");
    publish_conversation_output(
        state,
        session_id,
        turn_id,
        OutputEvent::SpeechIntentStarted {
            intent_id: intent_id.clone(),
            stream_id: stream_id.clone(),
            behavior: OutputStreamBehavior::Queue,
            priority: 0,
        },
    );
    publish_conversation_output(
        state,
        session_id,
        turn_id,
        OutputEvent::SpeechIntentToken {
            intent_id: intent_id.clone(),
            stream_id: stream_id.clone(),
            sequence: 0,
            kind: SpeechTokenKind::Literal,
            value: Some(text.to_owned()),
        },
    );
    publish_conversation_output(
        state,
        session_id,
        turn_id,
        OutputEvent::SpeechIntentEnded {
            intent_id,
            stream_id,
        },
    );
}

fn normalized_repo_root(repo: Option<String>) -> Option<String> {
    repo.map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(value).to_string_lossy().trim().to_owned())
}

fn trim_turns(session: &mut ConversationSession) {
    if session.turns.len() > SESSION_MAX_TURNS {
        session
            .turns
            .drain(0..session.turns.len() - SESSION_MAX_TURNS);
    }
}

fn prune_sessions(
    sessions: &mut BTreeMap<String, StoredConversationSession>,
    keep_session_id: Option<&str>,
) -> Vec<String> {
    let mut evicted_session_ids = Vec::new();
    while sessions.len() > SESSION_CACHE_LIMIT {
        let Some(session_id) = sessions
            .iter()
            .filter(|(session_id, stored)| {
                keep_session_id != Some(session_id.as_str()) && stored.active_turn.is_none()
            })
            .min_by_key(|(_, stored)| stored.last_accessed)
            .map(|(session_id, _)| session_id.clone())
        else {
            break;
        };
        sessions.remove(&session_id);
        evicted_session_ids.push(session_id);
    }
    evicted_session_ids
}
