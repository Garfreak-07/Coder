from __future__ import annotations

import math
import re
from datetime import datetime, timezone
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.memory.models import (
    AgentMemoryRole,
    KnowledgeChunk,
    MemoryAllowedContext,
    MemoryPurpose,
    MemoryRecord,
    MemoryScope,
    MemorySourceRef,
    memory_has_expired,
)
from coder_workbench.memory.policy import AgentMemoryPolicy, policy_for_role
from coder_workbench.memory.store import AgentScopedMemoryStore, KnowledgeStore


class MemoryRetrievalRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    role: AgentMemoryRole
    query: str
    project_id: str | None = None
    session_id: str | None = None
    run_id: str | None = None
    scope_paths: list[str] = Field(default_factory=list)
    tags: list[str] = Field(default_factory=list)
    requested_context: MemoryAllowedContext
    token_budget: int | None = None


class MemoryCard(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    title: str
    summary: str
    scope: MemoryScope
    purpose: list[MemoryPurpose]
    source_refs: list[MemorySourceRef] = Field(default_factory=list)
    evidence_refs: list[str] = Field(default_factory=list)
    tags: list[str] = Field(default_factory=list)
    token_estimate: int
    score: float
    card_type: Literal["memory_record", "knowledge_chunk"] = "memory_record"


class MemoryRetriever:
    def __init__(
        self,
        *,
        memory_store: AgentScopedMemoryStore | None = None,
        knowledge_store: KnowledgeStore | None = None,
        policies: dict[AgentMemoryRole, AgentMemoryPolicy] | None = None,
    ) -> None:
        self.memory_store = memory_store
        self.knowledge_store = knowledge_store
        self.policies = policies or {}

    def retrieve(self, request: MemoryRetrievalRequest | dict[str, Any]) -> list[MemoryCard]:
        parsed = request if isinstance(request, MemoryRetrievalRequest) else MemoryRetrievalRequest.model_validate(request)
        policy = self.policies.get(parsed.role) or policy_for_role(parsed.role)
        if parsed.requested_context not in policy.allowed_contexts:
            return []

        candidates: list[MemoryCard] = []
        if self.memory_store is not None:
            candidates.extend(
                self._record_card(record, parsed, policy)
                for record in self.memory_store.list_records(status="active")
                if self._record_allowed(record, parsed, policy)
            )
        if self.knowledge_store is not None:
            candidates.extend(
                self._chunk_card(chunk, parsed, policy)
                for chunk in self.knowledge_store.list_chunks()
                if self._chunk_allowed(chunk, parsed, policy)
            )

        budget = policy.max_tokens
        if parsed.token_budget is not None:
            budget = min(budget, max(0, parsed.token_budget))
        ranked = sorted(candidates, key=lambda card: (-card.score, card.token_estimate, card.id))
        selected: list[MemoryCard] = []
        used = 0
        for card in ranked:
            if len(selected) >= policy.max_records:
                break
            cost = max(1, card.token_estimate)
            if used + cost > budget:
                continue
            selected.append(card)
            used += cost
        return selected

    def _record_allowed(
        self,
        record: MemoryRecord,
        request: MemoryRetrievalRequest,
        policy: AgentMemoryPolicy,
    ) -> bool:
        if record.status != "active":
            return False
        if memory_has_expired(record):
            return False
        if record.scope not in policy.allowed_scopes:
            return False
        if not set(record.purpose).issubset(set(policy.allowed_purposes)):
            return False
        if request.role not in record.acl.allowed_agents:
            return False
        if request.requested_context not in record.acl.allowed_contexts:
            return False
        if record.acl.sensitivity == "secret":
            return False
        if not _scope_identity_allowed(record.project_id, request.project_id):
            return False
        if not _scope_identity_allowed(record.session_id, request.session_id):
            return False
        if not _scope_identity_allowed(record.run_id, request.run_id):
            return False
        return True

    def _chunk_allowed(
        self,
        chunk: KnowledgeChunk,
        request: MemoryRetrievalRequest,
        policy: AgentMemoryPolicy,
    ) -> bool:
        if "knowledge_source" not in policy.allowed_scopes:
            return False
        if not set(chunk.purpose).issubset(set(policy.allowed_purposes)):
            return False
        if request.role not in chunk.acl.allowed_agents:
            return False
        if request.requested_context not in chunk.acl.allowed_contexts:
            return False
        if chunk.sensitivity == "secret" or chunk.acl.sensitivity == "secret":
            return False
        return True

    def _record_card(
        self,
        record: MemoryRecord,
        request: MemoryRetrievalRequest,
        policy: AgentMemoryPolicy,
    ) -> MemoryCard:
        token_estimate = _token_estimate(record.token_estimate, record.title, record.summary)
        return MemoryCard(
            id=record.id,
            title=record.title,
            summary=record.summary,
            scope=record.scope,
            purpose=record.purpose,
            source_refs=record.source_refs,
            evidence_refs=record.evidence_refs,
            tags=record.tags,
            token_estimate=token_estimate,
            score=_score_record(record, request, token_estimate),
            card_type="memory_record",
        )

    def _chunk_card(
        self,
        chunk: KnowledgeChunk,
        request: MemoryRetrievalRequest,
        policy: AgentMemoryPolicy,
    ) -> MemoryCard:
        token_estimate = _token_estimate(chunk.token_estimate, chunk.title, chunk.summary)
        return MemoryCard(
            id=chunk.chunk_id,
            title=chunk.title,
            summary=chunk.summary,
            scope="knowledge_source",
            purpose=chunk.purpose,
            source_refs=[
                MemorySourceRef(
                    kind="external",
                    ref=chunk.source_id,
                    title=chunk.title,
                    content_hash=chunk.content_hash,
                )
            ],
            evidence_refs=[],
            tags=chunk.tags,
            token_estimate=token_estimate,
            score=_score_chunk(chunk, request, token_estimate),
            card_type="knowledge_chunk",
        )


def _scope_identity_allowed(record_value: str | None, request_value: str | None) -> bool:
    return not record_value or not request_value or record_value == request_value


def _score_record(record: MemoryRecord, request: MemoryRetrievalRequest, token_estimate: int) -> float:
    searchable = " ".join(
        [
            record.title,
            record.summary,
            record.content or "",
            " ".join(record.tags),
        ]
    )
    score = _term_overlap_score(request.query, searchable)
    score += _tag_overlap_score(request.tags, record.tags)
    score += _identity_bonus(record.project_id, request.project_id)
    score += _identity_bonus(record.session_id, request.session_id)
    score += _identity_bonus(record.run_id, request.run_id)
    score += {"high": 1.0, "medium": 0.4, "low": 0.0}.get(record.confidence, 0.0)
    score += {"user_confirmed": 1.0, "system_recorded": 0.7, "source": 0.5, "model_inferred": 0.1}.get(record.trust_level, 0.0)
    score += _recency_bonus(record.updated_at)
    score += _scope_path_score(request.scope_paths, record.source_refs)
    score -= min(1.0, token_estimate / 4000)
    return round(score, 6)


def _score_chunk(chunk: KnowledgeChunk, request: MemoryRetrievalRequest, token_estimate: int) -> float:
    searchable = " ".join([chunk.title, chunk.summary, chunk.text, " ".join(chunk.tags)])
    score = _term_overlap_score(request.query, searchable)
    score += _tag_overlap_score(request.tags, chunk.tags)
    score += {"user_confirmed": 1.0, "system_recorded": 0.7, "source": 0.5, "model_inferred": 0.1}.get(chunk.trust_level, 0.0)
    score -= min(1.0, token_estimate / 4000)
    return round(score, 6)


def _term_overlap_score(query: str, text: str) -> float:
    query_terms = set(_terms(query))
    if not query_terms:
        return 0.0
    text_terms = set(_terms(text))
    overlap = query_terms & text_terms
    return float(len(overlap)) + len(overlap) / max(1.0, math.sqrt(len(query_terms)))


def _tag_overlap_score(request_tags: list[str], record_tags: list[str]) -> float:
    requested = {tag.lower() for tag in request_tags}
    available = {tag.lower() for tag in record_tags}
    return float(len(requested & available) * 2)


def _identity_bonus(record_value: str | None, request_value: str | None) -> float:
    if record_value and request_value and record_value == request_value:
        return 1.5
    return 0.0


def _scope_path_score(scope_paths: list[str], source_refs: list[MemorySourceRef]) -> float:
    if not scope_paths or not source_refs:
        return 0.0
    scope_terms = [path.lower().replace("\\", "/") for path in scope_paths]
    refs = [ref.ref.lower().replace("\\", "/") for ref in source_refs]
    return float(sum(1 for path in scope_terms for ref in refs if path in ref or ref in path))


def _recency_bonus(updated_at: str) -> float:
    try:
        timestamp = datetime.fromisoformat(updated_at.replace("Z", "+00:00"))
    except ValueError:
        return 0.0
    if timestamp.tzinfo is None:
        timestamp = timestamp.replace(tzinfo=timezone.utc)
    age_days = max(0.0, (datetime.now(timezone.utc) - timestamp).total_seconds() / 86400)
    if age_days <= 1:
        return 0.5
    if age_days <= 30:
        return 0.25
    return 0.0


def _token_estimate(value: int, *texts: str) -> int:
    if value > 0:
        return value
    chars = sum(len(text) for text in texts)
    return max(1, math.ceil(chars / 4))


def _terms(value: str) -> list[str]:
    return [term for term in re.findall(r"[a-z0-9_]+", value.lower()) if len(term) > 1]
