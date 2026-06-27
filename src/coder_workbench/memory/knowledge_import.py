from __future__ import annotations

import hashlib
import re
from datetime import datetime, timezone
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.memory.models import (
    AgentMemoryRole,
    KnowledgeChunk,
    KnowledgeSource,
    MemoryAcl,
    MemoryAllowedContext,
    MemoryPurpose,
    MemorySensitivity,
)
from coder_workbench.memory.store import KnowledgeStore


MAX_CHUNK_CHARS = 3200


class KnowledgeTextImportRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    title: str
    text: str
    owner_scope: Literal["user", "project", "team"] = "project"
    tags: list[str] = Field(default_factory=list)
    allowed_agents: list[AgentMemoryRole]
    purpose: list[MemoryPurpose]
    allowed_contexts: list[MemoryAllowedContext] = Field(default_factory=list)
    sensitivity: MemorySensitivity = "project"


class KnowledgeImportResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    source: KnowledgeSource
    chunks: list[KnowledgeChunk]


def import_text_knowledge_source(
    store: KnowledgeStore,
    request: KnowledgeTextImportRequest | dict,
) -> KnowledgeImportResult:
    parsed = request if isinstance(request, KnowledgeTextImportRequest) else KnowledgeTextImportRequest.model_validate(request)
    if not parsed.title.strip():
        raise ValueError("knowledge source title is required")
    if not parsed.text.strip():
        raise ValueError("knowledge source text is required")
    if not parsed.allowed_agents:
        raise ValueError("knowledge source allowed_agents is required")
    if not parsed.purpose:
        raise ValueError("knowledge source purpose is required")

    source_hash = _hash_text(parsed.text)
    source_id = f"knowledge-source-{source_hash[:16]}"
    source = KnowledgeSource(
        source_id=source_id,
        kind="manual_note",
        uri=f"manual:{source_id}",
        title=parsed.title.strip(),
        owner_scope=parsed.owner_scope,
        content_hash=f"sha256:{source_hash}",
        imported_at=_now(),
        metadata={
            "tags": list(parsed.tags),
            "chunker": "heading_paragraph_v1",
        },
    )
    contexts = parsed.allowed_contexts or _contexts_for_agents(parsed.allowed_agents, parsed.purpose)
    chunks = [
        KnowledgeChunk(
            chunk_id=f"knowledge-chunk-{source_hash[:16]}-{index}",
            source_id=source_id,
            title=chunk_title,
            text=chunk_text,
            summary=_summary(chunk_text),
            tags=list(parsed.tags),
            purpose=list(parsed.purpose),
            acl=MemoryAcl(
                allowed_agents=list(parsed.allowed_agents),
                allowed_contexts=contexts,
                sensitivity=parsed.sensitivity,
            ),
            sensitivity=parsed.sensitivity,
            trust_level="source",
            content_hash=f"sha256:{_hash_text(source_hash + chunk_text + str(index))}",
            embedding_id=None,
            token_estimate=max(1, len(chunk_text) // 4),
        )
        for index, (chunk_title, chunk_text) in enumerate(_chunk_markdown(parsed.title, parsed.text), start=1)
    ]
    stored_source = store.append_source(source)
    stored_chunks = [store.append_chunk(chunk) for chunk in chunks]
    return KnowledgeImportResult(source=stored_source, chunks=stored_chunks)


def _chunk_markdown(default_title: str, text: str) -> list[tuple[str, str]]:
    sections = _heading_sections(default_title, text)
    if not sections:
        sections = [(default_title, paragraph) for paragraph in _paragraph_chunks(text)]
    chunks: list[tuple[str, str]] = []
    for title, section in sections:
        if len(section) <= MAX_CHUNK_CHARS:
            chunks.append((title, section.strip()))
            continue
        for index, chunk in enumerate(_paragraph_chunks(section), start=1):
            chunks.append((f"{title} ({index})", chunk))
    return [(title, chunk) for title, chunk in chunks if chunk.strip()]


def _heading_sections(default_title: str, text: str) -> list[tuple[str, str]]:
    matches = list(re.finditer(r"(?m)^(#{1,6})\s+(.+?)\s*$", text))
    if not matches:
        return []
    sections: list[tuple[str, str]] = []
    prefix = text[: matches[0].start()].strip()
    if prefix:
        sections.append((default_title, prefix))
    for index, match in enumerate(matches):
        start = match.end()
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        title = match.group(2).strip()
        body = text[start:end].strip()
        section_text = f"{match.group(0).strip()}\n\n{body}".strip()
        if section_text:
            sections.append((title, section_text))
    return sections


def _paragraph_chunks(text: str) -> list[str]:
    paragraphs = [item.strip() for item in re.split(r"\n\s*\n", text) if item.strip()]
    chunks: list[str] = []
    current = ""
    for paragraph in paragraphs:
        candidate = f"{current}\n\n{paragraph}".strip() if current else paragraph
        if len(candidate) <= MAX_CHUNK_CHARS:
            current = candidate
            continue
        if current:
            chunks.append(current)
        if len(paragraph) <= MAX_CHUNK_CHARS:
            current = paragraph
        else:
            chunks.extend(paragraph[index : index + MAX_CHUNK_CHARS] for index in range(0, len(paragraph), MAX_CHUNK_CHARS))
            current = ""
    if current:
        chunks.append(current)
    return chunks


def _contexts_for_agents(agents: list[AgentMemoryRole], purpose: list[MemoryPurpose]) -> list[MemoryAllowedContext]:
    if "persona_style" in purpose:
        return ["assistant_message"]
    contexts: list[MemoryAllowedContext] = []
    if "planning_chat" in agents:
        contexts.extend(["assistant_message", "planner_task_state"])
    if "workflow_supervisor" in agents:
        contexts.extend(["workflow_supervision", "planner_order", "final_report"])
    if "task_execution" in agents:
        contexts.append("execution_prompt")
    return list(dict.fromkeys(contexts))


def _summary(text: str) -> str:
    compact = " ".join(text.strip().split())
    if len(compact) <= 240:
        return compact
    return compact[:237].rstrip() + "..."


def _hash_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()
