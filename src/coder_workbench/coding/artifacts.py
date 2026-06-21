from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


ConfidenceLevel = Literal["low", "medium", "high"]
CheckStatus = Literal["pass", "fail", "blocked"]


class CodingArtifact(BaseModel):
    model_config = ConfigDict(extra="forbid")

    artifact_id: str | None = None
    artifact_type: str


class RiskFile(BaseModel):
    model_config = ConfigDict(extra="forbid")

    path: str
    risk_level: Literal["low", "medium", "high"] = "high"
    reason: str


class RiskMapArtifact(CodingArtifact):
    artifact_type: Literal["risk_map"] = "risk_map"
    risk_files: list[str] = Field(default_factory=list)
    items: list[RiskFile] = Field(default_factory=list)
    confidence: ConfidenceLevel = "high"


class RepoIndexArtifact(CodingArtifact):
    artifact_type: Literal["repo_index"] = "repo_index"
    languages: list[str] = Field(default_factory=list)
    frameworks: list[str] = Field(default_factory=list)
    source_dirs: list[str] = Field(default_factory=list)
    test_dirs: list[str] = Field(default_factory=list)
    important_files: list[str] = Field(default_factory=list)
    risk_files: list[str] = Field(default_factory=list)
    package_managers: list[str] = Field(default_factory=list)
    file_count: int = 0
    confidence: ConfidenceLevel = "medium"


class CheckCommand(BaseModel):
    model_config = ConfigDict(extra="forbid")

    command: str
    cwd: str = "."
    confidence: ConfidenceLevel = "medium"


class CommandDiscoveryArtifact(CodingArtifact):
    artifact_type: Literal["command_discovery"] = "command_discovery"
    test_commands: list[CheckCommand] = Field(default_factory=list)
    build_commands: list[CheckCommand] = Field(default_factory=list)
    lint_commands: list[CheckCommand] = Field(default_factory=list)
    confidence: ConfidenceLevel = "medium"


class SymbolRecord(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str
    kind: str
    line: int = Field(ge=1)


class SymbolFile(BaseModel):
    model_config = ConfigDict(extra="forbid")

    path: str
    symbols: list[SymbolRecord] = Field(default_factory=list)


class SymbolIndexArtifact(CodingArtifact):
    artifact_type: Literal["symbol_index"] = "symbol_index"
    files: list[SymbolFile] = Field(default_factory=list)
    parser: Literal["tree_sitter", "regex_fallback"] = "regex_fallback"
    languages: list[str] = Field(default_factory=list)
    confidence: ConfidenceLevel = "medium"


class IncludedSnippet(BaseModel):
    model_config = ConfigDict(extra="forbid")

    path: str
    start_line: int = Field(default=1, ge=1)
    end_line: int = Field(default=1, ge=1)
    content: str


class CodingContextPacketArtifact(CodingArtifact):
    artifact_type: Literal["coding_context_packet"] = "coding_context_packet"
    work_item_id: str
    included_files: list[str] = Field(default_factory=list)
    included_snippets: list[IncludedSnippet] = Field(default_factory=list)
    included_artifacts: list[dict[str, Any]] = Field(default_factory=list)
    included_skills: list[dict[str, Any]] = Field(default_factory=list)
    omitted_files: list[str] = Field(default_factory=list)
    estimated_input_tokens: int = 0
    estimated_omitted_tokens: int = 0
    selection_reason: list[str] = Field(default_factory=list)


class PatchPreviewArtifact(CodingArtifact):
    artifact_type: Literal["patch_preview"] = "patch_preview"
    status: Literal["patch_preview_created", "blocked", "rejected"] = "patch_preview_created"
    patch_ref: str | None = None
    change_count: int = 0
    files: list[dict[str, Any]] = Field(default_factory=list)
    errors: list[dict[str, Any]] = Field(default_factory=list)
    requires_approval: bool = True


class CheckResultArtifact(CodingArtifact):
    artifact_type: Literal["check_result"] = "check_result"
    command: str
    cwd: str = "."
    status: CheckStatus
    returncode: int | None = None
    output_ref: str = ""
    summary: str = ""
    output: str = ""


class DebugFindingArtifact(CodingArtifact):
    artifact_type: Literal["debug_finding"] = "debug_finding"
    work_item_id: str = ""
    command: str = ""
    status: Literal["failed", "blocked"] = "failed"
    failure_summary: str = ""
    likely_files: list[str] = Field(default_factory=list)
    error_patterns: list[str] = Field(default_factory=list)
    raw_output_ref: str = ""


class CodingTaskAcceptance(BaseModel):
    model_config = ConfigDict(extra="forbid")

    tests_pass: bool = True
    no_forbidden_files_changed: bool = True


class CodingTaskSpec(BaseModel):
    model_config = ConfigDict(extra="forbid")

    task_id: str
    repo_fixture: str
    request: str
    check_commands: list[str] = Field(default_factory=list)
    expected_changed_files: list[str] = Field(default_factory=list)
    forbidden_paths: list[str] = Field(default_factory=list)
    acceptance: CodingTaskAcceptance = Field(default_factory=CodingTaskAcceptance)


class CodingEvaluationReportArtifact(CodingArtifact):
    artifact_type: Literal["coding_evaluation_report"] = "coding_evaluation_report"
    task_id: str = ""
    task_pass_rate: float = 0.0
    patch_created_rate: float = 0.0
    patch_apply_rate: float = 0.0
    tests_pass_rate: float = 0.0
    forbidden_change_rate: float = 0.0
    planner_rounds: int = 0
    worker_interrupt_rate: float = 0.0
    human_prompt_rate: float = 0.0
    estimated_tokens: int = 0
    repair_count: int = 0
    details: dict[str, Any] = Field(default_factory=dict)
