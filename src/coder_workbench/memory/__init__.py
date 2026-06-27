from .schema import MemoryCommitResult, MemoryDelta, StagedMemoryWrite
from .models import (
    AgentMemoryRole,
    KnowledgeChunk,
    KnowledgeSource,
    MemoryAcl,
    MemoryAllowedContext,
    MemoryPurpose,
    MemoryRecord,
    MemoryScope,
    MemorySourceRef,
    validate_knowledge_chunk,
    validate_memory_record,
)
from .knowledge_import import KnowledgeImportResult, KnowledgeTextImportRequest, import_text_knowledge_source
from .policy import AgentMemoryPolicy, policy_for_role
from .planner_file_memory import (
    PlannerFileMemoryCommitter,
    PlannerMemoryWriteProposal,
    validate_planner_memory_write_proposal,
)
from .retriever import MemoryCard, MemoryRetrievalRequest, MemoryRetriever
from .store import AgentScopedMemoryStore, KnowledgeStore, WorkflowMemoryStore

__all__ = [
    "AgentMemoryPolicy",
    "AgentMemoryRole",
    "AgentScopedMemoryStore",
    "KnowledgeChunk",
    "KnowledgeImportResult",
    "KnowledgeSource",
    "KnowledgeStore",
    "KnowledgeTextImportRequest",
    "MemoryAcl",
    "MemoryAllowedContext",
    "MemoryCard",
    "MemoryCommitResult",
    "MemoryDelta",
    "MemoryPurpose",
    "MemoryRecord",
    "MemoryRetrievalRequest",
    "MemoryRetriever",
    "MemoryScope",
    "MemoryService",
    "MemorySourceRef",
    "PlannerFileMemoryCommitter",
    "PlannerMemoryWriteProposal",
    "StagedMemoryWrite",
    "WorkflowMemoryStore",
    "policy_for_role",
    "import_text_knowledge_source",
    "validate_knowledge_chunk",
    "validate_memory_record",
    "validate_planner_memory_write_proposal",
]


def __getattr__(name: str):
    if name == "MemoryService":
        from .service import MemoryService

        return MemoryService
    raise AttributeError(name)
