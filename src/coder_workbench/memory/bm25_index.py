from __future__ import annotations

import json
import math
import re
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.memory.models import (
    KnowledgeChunk,
    MemoryRecord,
    SECRET_MARKERS,
)

try:  # pragma: no cover - exercised only when the optional dependency is present.
    from rank_bm25 import BM25Okapi
except ImportError:  # pragma: no cover - current base environment intentionally lacks it.
    BM25Okapi = None


class BM25Document(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    item_type: Literal["knowledge_chunk", "memory_record"]
    title: str
    text: str
    summary: str
    tags: list[str] = Field(default_factory=list)
    token_estimate: int = 0
    metadata: dict[str, Any] = Field(default_factory=dict)


class BM25SearchHit(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    item_type: Literal["knowledge_chunk", "memory_record"]
    rank: int
    score: float


class BM25Index:
    def __init__(self, root: str | Path) -> None:
        self.root = _bm25_root(root)
        self.documents_path = self.root / "documents.jsonl"
        self.manifest_path = self.root / "manifest.json"
        self._documents: list[BM25Document] | None = None
        self._tokenized_documents: list[list[str]] | None = None
        self._rank_bm25: Any | None = None

    @classmethod
    def is_available(cls) -> bool:
        return BM25Okapi is not None

    def rebuild(
        self,
        *,
        memory_records: list[MemoryRecord],
        knowledge_chunks: list[KnowledgeChunk],
    ) -> None:
        documents: list[BM25Document] = []
        documents.extend(_chunk_document(chunk) for chunk in knowledge_chunks if _chunk_indexable(chunk))
        documents.extend(_record_document(record) for record in memory_records if _record_indexable(record))

        self.root.mkdir(parents=True, exist_ok=True)
        with self.documents_path.open("w", encoding="utf-8") as handle:
            for document in documents:
                handle.write(json.dumps(document.model_dump(mode="json"), ensure_ascii=False, sort_keys=True))
                handle.write("\n")
        self.manifest_path.write_text(
            json.dumps(
                {
                    "version": "bm25-v1",
                    "document_count": len(documents),
                    "rank_bm25_available": self.is_available(),
                    "updated_at": datetime.now(timezone.utc).isoformat(),
                },
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        self._load(documents)

    def search(self, query: str, *, top_k: int) -> list[BM25SearchHit]:
        documents = self._documents
        if documents is None:
            documents = self._read_documents()
            self._load(documents)
        if not documents or top_k <= 0:
            return []

        query_tokens = tokenize_bm25(query)
        if not query_tokens:
            return []
        tokenized_documents = self._tokenized_documents or []
        if self._rank_bm25 is not None:
            scores = [float(score) for score in self._rank_bm25.get_scores(query_tokens)]
        else:
            scores = _fallback_bm25_scores(query_tokens, tokenized_documents)

        ranked = sorted(
            (
                (document, score)
                for document, score in zip(documents, scores, strict=True)
                if score > 0
            ),
            key=lambda item: (-item[1], item[0].id),
        )
        return [
            BM25SearchHit(
                id=document.id,
                item_type=document.item_type,
                rank=index,
                score=round(score, 6),
            )
            for index, (document, score) in enumerate(ranked[:top_k], start=1)
        ]

    def documents(self) -> list[BM25Document]:
        if self._documents is None:
            self._load(self._read_documents())
        return list(self._documents or [])

    def _load(self, documents: list[BM25Document]) -> None:
        self._documents = documents
        self._tokenized_documents = [tokenize_bm25(_document_search_text(document)) for document in documents]
        if BM25Okapi is not None and self._tokenized_documents:
            self._rank_bm25 = BM25Okapi(self._tokenized_documents)
        else:
            self._rank_bm25 = None

    def _read_documents(self) -> list[BM25Document]:
        if not self.documents_path.exists():
            return []
        documents: list[BM25Document] = []
        for line in self.documents_path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                documents.append(BM25Document.model_validate_json(line))
        return documents


def tokenize_bm25(text: str) -> list[str]:
    normalized = text.replace("\\", "/")
    raw_tokens = re.findall(r"[A-Za-z0-9_./:-]+", normalized)
    tokens: list[str] = []
    for raw in raw_tokens:
        if "/" in raw:
            for part in raw.split("/"):
                tokens.extend(_segment_tokens(part))
        else:
            tokens.extend(_segment_tokens(raw))
    return [token for token in tokens if token]


def _segment_tokens(segment: str) -> list[str]:
    trimmed = segment.strip(" .:-")
    if not trimmed:
        return []
    tokens: list[str] = []
    dot_parts = [part for part in trimmed.split(".") if part]
    parts = dot_parts if len(dot_parts) > 1 else [trimmed]
    for part in parts:
        lowered = part.lower()
        if lowered:
            tokens.append(lowered)
        for subpart in re.split(r"[-_]+", part):
            tokens.extend(_camel_tokens(subpart))
    return list(dict.fromkeys(tokens))


def _camel_tokens(value: str) -> list[str]:
    if not value:
        return []
    split = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", value)
    split = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1 \2", split)
    return [part.lower() for part in split.split() if part]


def _fallback_bm25_scores(query_tokens: list[str], tokenized_documents: list[list[str]]) -> list[float]:
    if not tokenized_documents:
        return []
    doc_count = len(tokenized_documents)
    avg_doc_len = sum(len(tokens) for tokens in tokenized_documents) / max(1, doc_count)
    doc_freq: Counter[str] = Counter()
    for tokens in tokenized_documents:
        doc_freq.update(set(tokens))
    k1 = 1.5
    b = 0.75
    scores: list[float] = []
    for tokens in tokenized_documents:
        frequencies = Counter(tokens)
        doc_len = len(tokens) or 1
        score = 0.0
        for query_token in query_tokens:
            freq = frequencies.get(query_token, 0)
            if freq == 0:
                continue
            idf = math.log(1 + (doc_count - doc_freq[query_token] + 0.5) / (doc_freq[query_token] + 0.5))
            denom = freq + k1 * (1 - b + b * doc_len / max(1.0, avg_doc_len))
            score += idf * (freq * (k1 + 1)) / denom
        scores.append(score)
    return scores


def _chunk_document(chunk: KnowledgeChunk) -> BM25Document:
    text = " ".join(
        [
            chunk.title,
            chunk.summary,
            chunk.text,
            " ".join(chunk.tags),
            " ".join(chunk.purpose),
        ]
    )
    return BM25Document(
        id=chunk.chunk_id,
        item_type="knowledge_chunk",
        title=chunk.title,
        text=text,
        summary=chunk.summary,
        tags=chunk.tags,
        token_estimate=chunk.token_estimate,
        metadata={
            "source_id": chunk.source_id,
            "content_hash": chunk.content_hash,
            "purpose": chunk.purpose,
            "allowed_agents": chunk.acl.allowed_agents,
            "allowed_contexts": chunk.acl.allowed_contexts,
            "sensitivity": chunk.sensitivity,
            "trust_level": chunk.trust_level,
        },
    )


def _record_document(record: MemoryRecord) -> BM25Document:
    content = _safe_record_content(record)
    text = " ".join(
        item
        for item in [
            record.title,
            record.summary,
            content,
            " ".join(record.tags),
            " ".join(record.purpose),
            " ".join(ref.ref for ref in record.source_refs),
        ]
        if item
    )
    return BM25Document(
        id=record.id,
        item_type="memory_record",
        title=record.title,
        text=text,
        summary=record.summary,
        tags=record.tags,
        token_estimate=record.token_estimate,
        metadata={
            "scope": record.scope,
            "purpose": record.purpose,
            "allowed_agents": record.acl.allowed_agents,
            "allowed_contexts": record.acl.allowed_contexts,
            "sensitivity": record.acl.sensitivity,
            "project_id": record.project_id,
            "session_id": record.session_id,
            "run_id": record.run_id,
        },
    )


def _chunk_indexable(chunk: KnowledgeChunk) -> bool:
    if chunk.sensitivity == "secret" or chunk.acl.sensitivity == "secret":
        return False
    return not _contains_secret_like(chunk.text)


def _record_indexable(record: MemoryRecord) -> bool:
    if record.status != "active":
        return False
    if record.acl.sensitivity == "secret":
        return False
    return not _contains_secret_like(record.title) and not _contains_secret_like(record.summary)


def _safe_record_content(record: MemoryRecord) -> str:
    content = record.content or ""
    if not content or len(content) > 1200:
        return ""
    if _contains_secret_like(content):
        return ""
    lowered = content.lower()
    raw_markers = ("raw prompt", "raw model output", "full diff", "diff --git", "traceback", "begin rsa")
    if any(marker in lowered for marker in raw_markers):
        return ""
    return content


def _contains_secret_like(value: str | None) -> bool:
    lowered = (value or "").lower()
    return any(marker in lowered for marker in SECRET_MARKERS)


def _document_search_text(document: BM25Document) -> str:
    return " ".join([document.title, document.summary, document.text, " ".join(document.tags)])


def _bm25_root(root: str | Path) -> Path:
    path = Path(root)
    if path.name == "bm25":
        return path
    if path.name == "indexes":
        return path / "bm25"
    return path / "indexes" / "bm25"
