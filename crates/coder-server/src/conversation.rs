use std::{convert::Infallible, time::Duration};

use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures_util::{stream::unfold, Stream};

use crate::api_types::{
    ConversationSessionCreateRequest, ConversationSessionResponse, ConversationSteerRequest,
    ConversationTurnControlResponse, ConversationTurnRequest, ConversationTurnResponse,
};
use crate::{ApiError, ApiState};

pub(crate) async fn create_session(
    State(state): State<ApiState>,
    Json(request): Json<ConversationSessionCreateRequest>,
) -> Result<Json<ConversationSessionResponse>, ApiError> {
    let response = state.session_host.create_conversation(&state, request)?;
    Ok(Json(response))
}

pub(crate) async fn get_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<ConversationSessionResponse>, ApiError> {
    let response = state.session_host.get_conversation(&state, &session_id)?;
    Ok(Json(response))
}

pub(crate) async fn interrupt_turn(
    State(state): State<ApiState>,
    Path((session_id, turn_id)): Path<(String, String)>,
) -> Result<Json<ConversationTurnControlResponse>, ApiError> {
    let response = state
        .session_host
        .interrupt_conversation_turn(&session_id, &turn_id)?;
    Ok(Json(response))
}

pub(crate) async fn steer_turn(
    State(state): State<ApiState>,
    Path((session_id, turn_id)): Path<(String, String)>,
    Json(request): Json<ConversationSteerRequest>,
) -> Result<Json<ConversationTurnControlResponse>, ApiError> {
    let response = state
        .session_host
        .steer_conversation_turn(&session_id, &turn_id, request)?;
    Ok(Json(response))
}

pub(crate) async fn turn(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<ConversationTurnRequest>,
) -> Result<Json<ConversationTurnResponse>, ApiError> {
    let response = state
        .session_host
        .conversation_turn(&state, &session_id, request)
        .await?;
    Ok(Json(response))
}

pub(crate) async fn output_events(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let receiver = state
        .session_host
        .subscribe_output(&session_id)
        .ok_or_else(|| ApiError::not_found(format!("session '{session_id}' was not found")))?;
    let stream = unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Ok(envelope) => {
                let event = match Event::default()
                    .id(envelope.sequence.to_string())
                    .event("output")
                    .json_data(&envelope)
                {
                    Ok(event) => event,
                    Err(error) => Event::default()
                        .event("serialization_error")
                        .data(error.to_string()),
                };
                Some((Ok(event), receiver))
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                let event = Event::default().event("lagged").data(skipped.to_string());
                Some((Ok(event), receiver))
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    });
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}
