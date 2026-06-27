from __future__ import annotations

import os
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING, Any

from pydantic import Field

from coder_workbench.memory.bm25_index import BM25Index
from coder_workbench.memory.chroma_index import ChromaVectorIndex
from coder_workbench.memory.hybrid_retriever import HybridRagRetriever
from coder_workbench.memory.models import AgentMemoryRole, MemoryAllowedContext, SECRET_MARKERS
from coder_workbench.memory.policy import policy_for_role
from coder_workbench.memory.rag_models import HybridRagRequest
from coder_workbench.memory.store import AgentScopedMemoryStore, KnowledgeStore

os.environ.setdefault("OPENHANDS_SUPPRESS_BANNER", "1")
os.environ.setdefault("LITELLM_LOCAL_MODEL_COST_MAP", "True")

from openhands.sdk.llm import TextContent
from openhands.sdk.tool import (
    Action,
    Observation,
    ToolAnnotations,
    ToolDefinition,
    ToolExecutor,
    register_tool,
)

if TYPE_CHECKING:
    from openhands.sdk.conversation import LocalConversation
    from openhands.sdk.conversation.state import ConversationState


class CoderHybridRagSearchAction(Action):
    query: str = Field(description="Search query for project memory and knowledge.")
    top_k: int = Field(default=6, ge=1, le=12)
    tags: list[str] = Field(default_factory=list)
    include_content: bool = False


class CoderHybridRagSearchObservation(Observation):
    query: str
    results: list[dict[str, Any]]
    returned: int
    token_estimate: int
    cold_refs: list[str] = Field(default_factory=list)

    @property
    def to_llm_content(self) -> Sequence[TextContent]:
        return [TextContent(text=_format_observation(self))]


class CoderHybridRagSearchExecutor(ToolExecutor[CoderHybridRagSearchAction, CoderHybridRagSearchObservation]):
    def __init__(
        self,
        *,
        memory_root: str,
        role: AgentMemoryRole,
        requested_context: MemoryAllowedContext,
        project_id: str | None,
        session_id: str | None,
        run_id: str | None,
        scope_paths: list[str],
        max_tokens: int,
    ) -> None:
        root = Path(memory_root)
        self.store_root = root.parent if root.name == "memory" else root
        self.role = role
        self.requested_context = requested_context
        self.project_id = project_id
        self.session_id = session_id
        self.run_id = run_id
        self.scope_paths = list(scope_paths)
        self.max_tokens = max(0, int(max_tokens))

    def __call__(
        self,
        action: CoderHybridRagSearchAction,
        conversation: "LocalConversation | None" = None,  # noqa: ARG002
    ) -> CoderHybridRagSearchObservation:
        policy = policy_for_role(self.role).model_copy(update={"max_tokens": self.max_tokens})
        retriever = HybridRagRetriever(
            memory_store=AgentScopedMemoryStore(self.store_root),
            knowledge_store=KnowledgeStore(self.store_root),
            bm25_index=_bm25_index_if_present(self.store_root),
            chroma_index=_chroma_index_if_present(self.store_root),
            policies={self.role: policy},
        )
        results = retriever.retrieve(
            HybridRagRequest(
                role=self.role,
                requested_context=self.requested_context,
                query=action.query,
                project_id=self.project_id,
                session_id=self.session_id,
                run_id=self.run_id,
                scope_paths=self.scope_paths,
                tags=action.tags,
                top_k=action.top_k,
                include_content=action.include_content,
                content_preview_chars=500,
            )
        )
        dumped = [result.model_dump(mode="json", exclude_none=True) for result in results]
        cold_refs = _cold_refs(dumped)
        token_estimate = sum(int(result.get("token_estimate") or 0) for result in dumped)
        return CoderHybridRagSearchObservation(
            query=action.query,
            results=dumped,
            returned=len(dumped),
            token_estimate=token_estimate,
            cold_refs=cold_refs,
        )


class CoderHybridRagSearchTool(ToolDefinition[CoderHybridRagSearchAction, CoderHybridRagSearchObservation]):
    name = "coder_hybrid_rag_search"

    @classmethod
    def create(
        cls,
        conv_state: "ConversationState | None" = None,  # noqa: ARG003
        **params: Any,
    ) -> Sequence["CoderHybridRagSearchTool"]:
        executor = CoderHybridRagSearchExecutor(
            memory_root=params["memory_root"],
            role=params["role"],
            requested_context=params["requested_context"],
            project_id=params.get("project_id"),
            session_id=params.get("session_id"),
            run_id=params.get("run_id"),
            scope_paths=params.get("scope_paths") or [],
            max_tokens=params.get("max_tokens") or 2000,
        )
        return [
            cls(
                description=(
                    "Read-only hybrid RAG search over Coder's ACL-scoped memory "
                    "and knowledge library. Use it to retrieve compact project, "
                    "code, documentation, and workflow context. It returns refs "
                    "and summaries, not raw full documents."
                ),
                action_type=CoderHybridRagSearchAction,
                observation_type=CoderHybridRagSearchObservation,
                executor=executor,
                annotations=ToolAnnotations(
                    title="Coder Hybrid RAG Search",
                    readOnlyHint=True,
                    destructiveHint=False,
                    idempotentHint=True,
                    openWorldHint=False,
                ),
            )
        ]


def _bm25_index_if_present(root: Path) -> BM25Index | None:
    index = BM25Index(root)
    return index if index.documents_path.exists() else None


def _chroma_index_if_present(root: Path) -> ChromaVectorIndex | None:
    chroma_root = root / "indexes" / "chroma"
    if not chroma_root.exists() or not ChromaVectorIndex.is_available():
        return None
    return ChromaVectorIndex(root)


def _format_observation(observation: CoderHybridRagSearchObservation) -> str:
    lines = [f"Hybrid RAG returned {observation.returned} results.", ""]
    for index, result in enumerate(observation.results, start=1):
        lines.append(f"{index}. {_redact(str(result.get('title') or result.get('id') or 'Untitled'))}")
        lines.append(f"   Summary: {_redact(str(result.get('summary') or ''))}")
        lines.append(f"   Ref: {_redact(str(result.get('id') or ''))}")
        source_refs = result.get("source_refs") or []
        if source_refs:
            refs = ", ".join(_redact(str(ref.get("ref") or "")) for ref in source_refs if isinstance(ref, dict))
            if refs:
                lines.append(f"   Source refs: {refs}")
        lines.append(
            "   Channels: "
            f"dense_rank={result.get('dense_rank')}, "
            f"bm25_rank={result.get('bm25_rank')}, "
            f"score={result.get('fusion_score')}"
        )
        preview = result.get("text_preview")
        if preview:
            lines.append(f"   Preview: {_redact(str(preview))}")
    if observation.cold_refs:
        lines.append("")
        lines.append("Cold refs: " + ", ".join(_redact(ref) for ref in observation.cold_refs[:10]))
    return "\n".join(lines).strip()


def _cold_refs(results: list[dict[str, Any]]) -> list[str]:
    refs: list[str] = []
    for result in results:
        for source_ref in result.get("source_refs") or []:
            if isinstance(source_ref, dict) and source_ref.get("ref"):
                refs.append(str(source_ref["ref"]))
        refs.extend(str(item) for item in result.get("evidence_refs") or [] if str(item))
    return list(dict.fromkeys(refs))


def _redact(text: str) -> str:
    redacted = text
    for marker in (*SECRET_MARKERS, "DEEPSEEK_API_KEY", "LLM_API_KEY", "BEGIN RSA"):
        redacted = redacted.replace(marker, "[redacted]")
        redacted = redacted.replace(marker.lower(), "[redacted]")
    return redacted


register_tool(CoderHybridRagSearchTool.name, CoderHybridRagSearchTool)
