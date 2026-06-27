from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any
from uuid import uuid4

from fastapi import FastAPI, HTTPException
from fastapi.responses import StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.core import (
    AgentWorkflowSpec,
    AgentWorkflowValidationError,
    capability_catalog,
    compile_runtime_profiles,
    default_planner_led_agent_workflow,
    role_card_catalog,
    validate_agent_workflow_payload,
)
from coder_workbench.core.artifacts import validate_artifact
from coder_workbench.core.planner_chat_artifacts import PlannerChatTurn, PlannerTaskState
from coder_workbench.context import build_harness_context_packet
from coder_workbench.extensions import builtin_plugin_manifests, extension_search
from coder_workbench.harness_runtime import (
    ArtifactProjectionError,
    ArtifactProjector,
    HarnessRunResult,
    HarnessRuntimeContext,
    HarnessRuntimeManager,
    OpenHandsRuntimeProvider,
)
from coder_workbench.harness_runtime.fallback_provider import InternalFallbackProvider
from coder_workbench.memory import KnowledgeStore, KnowledgeTextImportRequest, import_text_knowledge_source
from coder_workbench.memory.hybrid_index import HybridIndexManager
from coder_workbench.server.agent_manager import AgentGraphRunManager
from coder_workbench.server.library import LibraryStore
from coder_workbench.server.planner_chat_sessions import (
    PlannerChatSessionCreateRequest,
    PlannerChatSessionRecord,
    PlannerChatTurnRequest,
    message_record,
)
from coder_workbench.server.settings import ProviderSettingsStore, provider_status
from coder_workbench.server.storage import RunStore
from coder_workbench.skills import (
    InstalledSkillStore,
    RegistryClient,
    RegistryClientError,
    SkillAutoUpdateResult,
    SkillInstaller,
    SkillVerificationError,
    SkillUpdatePolicy,
    SkillTrustLevel,
    build_skill_index,
    is_auto_update_allowed,
    skill_auto_update_block_reason,
)
from coder_workbench.tools import default_tool_registry
from coder_workbench.tools.filesystem import normalize_scope_paths, resolve_existing_dir
from coder_workbench.tools.patching import rollback_patch

class AgentRunRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    repo: str
    request: str
    agent_workflow: dict[str, Any]
    approved: bool = False
    scopes: list[str] = Field(default_factory=list)
    initial_data: dict[str, Any] = Field(default_factory=dict)


class PlannerChatDraftRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    request: str
    workflow_id: str = "default-planner-led"
    planner_agent_id: str = "planner"
    agent_workflow: dict[str, Any] | None = None
    knowledge_pack_ids: list[str] = Field(default_factory=list)
    skill_pack_ids: list[str] = Field(default_factory=list)
    memory_pack_ids: list[str] = Field(default_factory=list)
    repo: str | None = None
    scopes: list[str] = Field(default_factory=list)


class PlannerChatConfirmRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    draft_id: str
    approved: bool
    edits: dict[str, Any] = Field(default_factory=dict)
    repo: str | None = None
    scopes: list[str] = Field(default_factory=list)
    initial_data: dict[str, Any] = Field(default_factory=dict)


class RollbackRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    repo: str
    snapshot_id: str
    scopes: list[str] = Field(default_factory=list)


class ProviderSettingsRequest(BaseModel):
    model_config = ConfigDict(extra="allow")


class SkillInstallRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    skill_id: str
    registry_url: str | None = None
    allow_untrusted: bool = False


class ExtensionInstallRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    extension_id: str
    extension_type: str = "skill"
    registry_url: str | None = None
    allow_untrusted: bool = False


class SkillUpdateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    registry_url: str | None = None
    allow_untrusted: bool = False
    force: bool = False


class SkillDeveloperImportRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    path: str
    trust_level: SkillTrustLevel = "local"
    enabled: bool = True
    allow_untrusted: bool = False


class SkillPinRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    version: str | None = None


class SkillRollbackRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    version: str | None = None


class SkillUpdatePolicyRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    update_policy: SkillUpdatePolicy


def create_app(store_root: str | Path = ".coder", frontend_dist: str | Path | None = None) -> FastAPI:
    app = FastAPI(title="Coder Runtime API", version="0.1.0")
    store = RunStore(store_root)
    settings_store = ProviderSettingsStore(store_root)
    library = LibraryStore(Path(store_root) / "library")
    skill_store = InstalledSkillStore(store_root)
    agent_manager = AgentGraphRunManager(store, runtime_settings_loader=settings_store.load)
    harness_runtime_manager = HarnessRuntimeManager(
        providers=[
            OpenHandsRuntimeProvider(),
            InternalFallbackProvider(planning_chat_runner=_planning_chat_fallback_runner),
        ]
    )
    artifact_projector = ArtifactProjector()
    planner_chat_drafts: dict[str, dict[str, Any]] = {}
    planner_chat_sessions: dict[str, PlannerChatSessionRecord] = {}

    @app.get("/api/v2/health")
    def health() -> dict[str, Any]:
        return {
            "status": "ok",
            "tools": default_tool_registry().names(),
        }

    @app.get("/api/v2/capabilities")
    def get_capabilities() -> dict[str, Any]:
        return {"capabilities": capability_catalog()}

    @app.get("/api/v2/agent-role-cards")
    def get_agent_role_cards() -> dict[str, Any]:
        return {"role_cards": role_card_catalog()}

    @app.get("/api/v2/extensions/plugins")
    def get_extension_plugins() -> dict[str, Any]:
        return {"plugins": [plugin.model_dump(mode="json") for plugin in builtin_plugin_manifests()]}

    @app.get("/api/v2/extensions/skills")
    def get_extension_skills() -> dict[str, Any]:
        records = skill_store.list_installed()
        return {
            "skills": [extension.model_dump(mode="json") for extension in extension_search(query="", skills=records) if extension.extension_type == "skill"],
            "index": build_skill_index(records).model_dump(mode="json"),
        }

    @app.get("/api/v2/extensions/installed")
    def get_installed_extensions() -> dict[str, Any]:
        records = skill_store.list_installed()
        extensions = extension_search(query="", skills=records)
        return {"extensions": [extension.model_dump(mode="json") for extension in extensions]}

    @app.get("/api/v2/extensions/search")
    def search_extensions(q: str = "") -> dict[str, Any]:
        extensions = extension_search(query=q, skills=skill_store.list_installed())
        return {"extensions": [extension.model_dump(mode="json") for extension in extensions]}

    @app.post("/api/v2/extensions/install")
    def install_extension(body: ExtensionInstallRequest) -> dict[str, Any]:
        if body.extension_type not in {"skill", "skills"}:
            raise HTTPException(status_code=400, detail="Only skill extension installs are supported by this local registry.")
        try:
            url = _resolve_skill_registry_url(body.registry_url)
            result = SkillInstaller(
                client=RegistryClient(url),
                store=skill_store,
            ).install(body.extension_id, allow_untrusted=body.allow_untrusted)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="extension not found") from exc
        except (RegistryClientError, SkillVerificationError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return result.model_dump(mode="json")

    @app.get("/api/v2/skills/installed")
    def get_installed_skills() -> dict[str, Any]:
        records = skill_store.list_installed()
        return {
            "skills": [record.summary().model_dump(mode="json") for record in records],
            "index": build_skill_index(records).model_dump(mode="json"),
        }

    @app.get("/api/v2/skills/discover")
    def discover_skills(registry_url: str | None = None) -> dict[str, Any]:
        try:
            url = _resolve_skill_registry_url(registry_url)
            index = RegistryClient(url).fetch_index()
        except (RegistryClientError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        installed_ids = {record.id for record in skill_store.list_installed()}
        return {
            "registry": index.model_dump(mode="json"),
            "skills": [entry.summary(installed=entry.id in installed_ids) for entry in index.skills],
        }

    @app.post("/api/v2/skills/install")
    def install_skill(body: SkillInstallRequest) -> dict[str, Any]:
        try:
            url = _resolve_skill_registry_url(body.registry_url)
            result = SkillInstaller(
                client=RegistryClient(url),
                store=skill_store,
            ).install(body.skill_id, allow_untrusted=body.allow_untrusted)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        except (RegistryClientError, SkillVerificationError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return result.model_dump(mode="json")

    @app.post("/api/v2/skills/developer-import")
    def developer_import_skill(body: SkillDeveloperImportRequest) -> dict[str, Any]:
        try:
            result = SkillInstaller(
                client=RegistryClient("unused-local-import-registry.json"),
                store=skill_store,
            ).import_local(
                body.path,
                trust_level=body.trust_level,
                enabled=body.enabled,
                allow_untrusted=body.allow_untrusted,
            )
        except (SkillVerificationError, ValueError, OSError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return result.model_dump(mode="json")

    @app.get("/api/v2/skills/updates")
    def get_skill_updates(registry_url: str | None = None) -> dict[str, Any]:
        try:
            url = _resolve_skill_registry_url(registry_url)
            index = RegistryClient(url).fetch_index()
        except (RegistryClientError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return {"updates": _skill_update_report(skill_store, index.skills)}

    @app.post("/api/v2/skills/auto-update")
    def auto_update_skills(body: SkillUpdateRequest) -> dict[str, Any]:
        try:
            url = _resolve_skill_registry_url(body.registry_url)
            result: SkillAutoUpdateResult = SkillInstaller(
                client=RegistryClient(url),
                store=skill_store,
            ).auto_update(allow_untrusted=body.allow_untrusted)
        except (RegistryClientError, SkillVerificationError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return result.model_dump(mode="json")

    @app.post("/api/v2/skills/{skill_id}/update")
    def update_skill(skill_id: str, body: SkillUpdateRequest) -> dict[str, Any]:
        try:
            url = _resolve_skill_registry_url(body.registry_url)
            result = SkillInstaller(
                client=RegistryClient(url),
                store=skill_store,
            ).install(skill_id, allow_untrusted=body.allow_untrusted, force=body.force)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        except SkillVerificationError as exc:
            status_code = 409 if "pinned" in str(exc) else 400
            raise HTTPException(status_code=status_code, detail=str(exc)) from exc
        except (RegistryClientError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return result.model_dump(mode="json")

    @app.post("/api/v2/skills/{skill_id}/enable")
    def enable_skill(skill_id: str) -> dict[str, Any]:
        try:
            record = skill_store.enable(skill_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        return {"skill": record.summary().model_dump(mode="json")}

    @app.post("/api/v2/skills/{skill_id}/disable")
    def disable_skill(skill_id: str) -> dict[str, Any]:
        try:
            record = skill_store.disable(skill_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        return {"skill": record.summary().model_dump(mode="json")}

    @app.get("/api/v2/skills/{skill_id}/versions")
    def get_skill_versions(skill_id: str) -> dict[str, Any]:
        try:
            return {"versions": skill_store.list_versions(skill_id)}
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc

    @app.post("/api/v2/skills/{skill_id}/pin")
    def pin_skill(skill_id: str, body: SkillPinRequest) -> dict[str, Any]:
        try:
            record = skill_store.pin(skill_id, version=body.version)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return {"skill": record.model_dump(mode="json")}

    @app.post("/api/v2/skills/{skill_id}/unpin")
    def unpin_skill(skill_id: str) -> dict[str, Any]:
        try:
            record = skill_store.unpin(skill_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        return {"skill": record.model_dump(mode="json")}

    @app.post("/api/v2/skills/{skill_id}/update-policy")
    def set_skill_update_policy(skill_id: str, body: SkillUpdatePolicyRequest) -> dict[str, Any]:
        try:
            record = skill_store.set_update_policy(skill_id, body.update_policy)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return {"skill": record.model_dump(mode="json")}

    @app.post("/api/v2/skills/{skill_id}/rollback")
    def rollback_skill(skill_id: str, body: SkillRollbackRequest) -> dict[str, Any]:
        try:
            record = skill_store.rollback(skill_id, version=body.version)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill version not found") from exc
        return {"skill": record.model_dump(mode="json")}

    @app.delete("/api/v2/skills/{skill_id}")
    def remove_skill(skill_id: str) -> dict[str, Any]:
        try:
            skill_store.remove(skill_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="skill not found") from exc
        return {"removed": True, "skill_id": skill_id}

    @app.get("/api/v2/providers/settings")
    def get_provider_settings() -> dict[str, Any]:
        return {"settings": settings_store.response()}

    @app.post("/api/v2/providers/settings")
    def save_provider_settings(body: ProviderSettingsRequest) -> dict[str, Any]:
        settings = settings_store.save(body.model_dump(mode="json"))
        return {"settings": settings_store.response(), "status": provider_status(settings)}

    @app.get("/api/v2/providers/status")
    def get_provider_status() -> dict[str, Any]:
        return provider_status(settings_store.load())

    @app.post("/api/v2/providers/test")
    def test_provider_settings(body: ProviderSettingsRequest) -> dict[str, Any]:
        settings = settings_store.load()
        provider = body.model_dump(mode="json").get("provider") or settings.default_provider
        return {"status": provider_status(settings, [str(provider).strip().lower()])}

    @app.get("/api/v2/library")
    def library_index() -> dict[str, Any]:
        return {
            "agents": library.list_agents(),
            "agent_workflows": library.list_agent_workflows(),
        }

    @app.post("/api/v2/knowledge-sources/import-text")
    def import_text_knowledge_source_endpoint(body: KnowledgeTextImportRequest) -> dict[str, Any]:
        try:
            result = import_text_knowledge_source(KnowledgeStore(store_root), body)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        payload = result.model_dump(mode="json")
        payload["index_dirty"] = True
        return payload

    @app.get("/api/v2/knowledge-sources")
    def list_knowledge_sources() -> dict[str, Any]:
        return {
            "sources": [
                source.model_dump(mode="json")
                for source in KnowledgeStore(store_root).list_sources()
            ]
        }

    @app.get("/api/v2/knowledge-sources/{source_id}/chunks")
    def list_knowledge_source_chunks(source_id: str) -> dict[str, Any]:
        return {
            "chunks": [
                chunk.model_dump(mode="json")
                for chunk in KnowledgeStore(store_root).list_chunks(source_id=source_id)
            ]
        }

    @app.post("/api/v2/rag/reindex")
    def rebuild_rag_index() -> dict[str, Any]:
        status = HybridIndexManager(store_root).rebuild()
        return {"status": "completed", **status.model_dump(mode="json")}

    @app.get("/api/v2/rag/status")
    def rag_index_status() -> dict[str, Any]:
        return HybridIndexManager(store_root).status().model_dump(mode="json")

    @app.post("/api/v2/library/agents")
    def save_agent(agent: dict[str, Any]) -> dict[str, Any]:
        return {"agent": library.save_agent(agent)}

    @app.get("/api/v2/library/agents/{agent_id}")
    def get_agent(agent_id: str) -> dict[str, Any]:
        try:
            return {"agent": library.get_agent(agent_id)}
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="agent not found") from exc


    @app.post("/api/v2/library/agent-workflows")
    def save_agent_workflow(agent_workflow: dict[str, Any]) -> dict[str, Any]:
        try:
            return {"agent_workflow": library.save_agent_workflow(agent_workflow)}
        except AgentWorkflowValidationError as exc:
            raise HTTPException(status_code=400, detail=exc.result.model_dump(mode="json")) from exc
        except Exception as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @app.get("/api/v2/library/agent-workflows/{workflow_id}")
    def get_agent_workflow(workflow_id: str) -> dict[str, Any]:
        try:
            return {"agent_workflow": library.get_agent_workflow(workflow_id)}
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="agent workflow not found") from exc

    @app.get("/api/v2/agent-workflows/default")
    def get_default_agent_workflow() -> dict[str, Any]:
        agent_workflow = default_planner_led_agent_workflow()
        return {
            "agent_workflow": agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True),
        }

    @app.post("/api/v2/agent-workflows/validate")
    def validate_agent_workflow_endpoint(agent_workflow: dict[str, Any]) -> dict[str, Any]:
        return validate_agent_workflow_payload(agent_workflow).model_dump(mode="json")

    @app.post("/api/v2/agent-workflows/runtime-profiles")
    def agent_workflow_runtime_profiles(agent_workflow: dict[str, Any]) -> dict[str, Any]:
        try:
            _raise_agent_workflow_validation(agent_workflow)
            spec = AgentWorkflowSpec.model_validate(agent_workflow)
        except AgentWorkflowValidationError as exc:
            raise HTTPException(status_code=400, detail=exc.result.model_dump(mode="json")) from exc
        except Exception as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return {
            "profiles": [
                profile.model_dump(mode="json")
                for profile in compile_runtime_profiles(spec)
            ]
        }

    @app.post("/api/v2/planner-chat/draft")
    def create_planner_chat_draft(body: PlannerChatDraftRequest) -> dict[str, Any]:
        if not body.request.strip():
            raise HTTPException(status_code=400, detail="request is required")
        try:
            if body.agent_workflow is None:
                agent_workflow = _load_agent_workflow_for_planner_chat(library, body.workflow_id)
                workflow_payload = agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True)
            else:
                workflow_payload = body.agent_workflow
                agent_workflow = AgentWorkflowSpec.model_validate(workflow_payload)
            _raise_agent_workflow_validation(workflow_payload)
        except AgentWorkflowValidationError as exc:
            raise HTTPException(status_code=400, detail=exc.result.model_dump(mode="json")) from exc
        except Exception as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        planner = next((agent for agent in agent_workflow.agents if agent.id == body.planner_agent_id), None)
        if planner is None or planner.role != "planner":
            raise HTTPException(status_code=400, detail="planner_agent_id must point to a Planner agent")

        draft_id = str(uuid4())
        proposed_scope = list(body.scopes)
        plan_payload, run_contract_payload = _planner_chat_draft_payloads(
            draft_id=draft_id,
            request=body.request.strip(),
            workflow=agent_workflow,
            planner_agent_id=planner.id,
            proposed_scope=proposed_scope,
            knowledge_pack_ids=body.knowledge_pack_ids,
            skill_pack_ids=body.skill_pack_ids,
            memory_pack_ids=body.memory_pack_ids,
        )
        memory_context = _planner_chat_memory_context(
            store_root=store_root,
            query=body.request.strip(),
            repo=body.repo,
        )
        context_packet = build_harness_context_packet(
            mode="planning_chat",
            user_goal=body.request.strip(),
            workflow_id=agent_workflow.id,
            agent_id=planner.id,
            planner_agent_id=planner.id,
            workflow_summary={
                "workflow_id": agent_workflow.id,
                "workflow_name": agent_workflow.name,
                "planner_agent_id": planner.id,
                "agent_count": len(agent_workflow.agents),
            },
            user_constraints=[
                "Planning Chat Mode must not write files or run commands.",
                "Execution starts only after explicit confirmation.",
            ],
            selected_knowledge_pack_ids=body.knowledge_pack_ids,
            selected_skill_pack_ids=body.skill_pack_ids,
            selected_memory_pack_ids=body.memory_pack_ids,
            **memory_context,
        )
        runtime_context = HarnessRuntimeContext(
            run_id=f"planner-chat-draft-{draft_id}",
            agent_id=planner.id,
            workflow_id=agent_workflow.id,
            harness_id="conversation-harness",
            mode="planning_chat",
            profile_id=agent_workflow.harness_bindings.planning_chat.profile_id,
            repo_root=body.repo,
            context_packet=context_packet,
        )
        runtime_result = harness_runtime_manager.run_planning_chat(
            context=runtime_context,
            profile_id=agent_workflow.harness_bindings.planning_chat.profile_id,
            input_artifacts={
                "legacy_operation": "planning_chat",
                "legacy_kwargs": {"draft_payload": plan_payload},
                "draft_id": draft_id,
                "user_request": body.request.strip(),
                "workflow_summary": {"workflow_id": agent_workflow.id, "workflow_name": agent_workflow.name},
            },
        )
        plan_draft = artifact_projector.project(runtime_result, artifact_type="project_plan_draft", artifact_id=draft_id)
        run_contract_draft = artifact_projector.project(
            HarnessRunResult(
                status=runtime_result.status,
                artifact_type="run_contract_draft",
                artifact=run_contract_payload,
                native_event_refs=runtime_result.native_event_refs,
                evidence_refs=runtime_result.evidence_refs,
            ),
            artifact_type="run_contract_draft",
            artifact_id=f"{draft_id}:run_contract",
        )
        planner_chat_drafts[draft_id] = {
            "draft_id": draft_id,
            "status": "drafted",
            "request": body.request.strip(),
            "repo": body.repo,
            "scopes": proposed_scope,
            "agent_workflow": agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True),
            "project_plan_draft": plan_draft,
            "run_contract_draft": run_contract_draft,
            "runtime_status": runtime_result.status,
            "runtime_native_event_refs": list(runtime_result.native_event_refs),
        }
        return {
            "draft_id": draft_id,
            "artifact_type": "project_plan_draft",
            "summary": plan_draft["summary"],
            "proposed_scope": plan_draft["proposed_scope"],
            "success_criteria": plan_draft["success_criteria"],
            "risks": plan_draft["risks"],
            "requires_confirmation": plan_draft["requires_confirmation"],
        }

    @app.post("/api/v2/planner-chat/confirm")
    def confirm_planner_chat_draft(body: PlannerChatConfirmRequest) -> dict[str, Any]:
        draft = planner_chat_drafts.get(body.draft_id)
        if draft is None:
            raise HTTPException(status_code=404, detail="planner chat draft not found")
        if not body.approved:
            draft["status"] = "cancelled"
            return {"draft_id": body.draft_id, "status": "cancelled"}

        agent_workflow = AgentWorkflowSpec.model_validate(draft["agent_workflow"])
        request_text = str(body.edits.get("request") or draft["request"]).strip()
        repo = body.repo or draft.get("repo") or "."
        repo_root = resolve_existing_dir(str(repo))
        scopes = body.scopes or list(draft.get("scopes") or [])
        try:
            normalized_scopes = normalize_scope_paths(repo_root, scopes)
        except (FileNotFoundError, NotADirectoryError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        initial_data = dict(body.initial_data)
        initial_data.update(
            {
                "request": request_text,
                "approved": True,
                "preapprove_all": True,
                "scopes": normalized_scopes,
                "planner_chat_draft": draft["project_plan_draft"],
                "run_contract_draft": draft["run_contract_draft"],
                "agent_workflow": agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True),
                "skill_index": build_skill_index(skill_store.list_installed()).model_dump(mode="json"),
                "skill_store_root": str(Path(store_root)),
            }
        )
        live = agent_manager.start(
            agent_workflow=agent_workflow,
            repo_root=str(repo_root),
            request=request_text,
            initial_data=initial_data,
        )
        draft["status"] = "confirmed"
        draft["run_id"] = live.id
        return {
            "run_id": live.id,
            "status": live.status,
        }

    @app.post("/api/v2/planner-chat/sessions")
    def create_planner_chat_session(body: PlannerChatSessionCreateRequest) -> dict[str, Any]:
        try:
            if body.agent_workflow is None:
                agent_workflow = _load_agent_workflow_for_planner_chat(library, body.workflow_id)
                workflow_payload = agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True)
            else:
                workflow_payload = body.agent_workflow
                agent_workflow = AgentWorkflowSpec.model_validate(workflow_payload)
            _raise_agent_workflow_validation(workflow_payload)
        except AgentWorkflowValidationError as exc:
            raise HTTPException(status_code=400, detail=exc.result.model_dump(mode="json")) from exc
        except Exception as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

        planner = next((agent for agent in agent_workflow.agents if agent.id == body.planner_agent_id), None)
        if planner is None or planner.role != "planner":
            raise HTTPException(status_code=400, detail="planner_agent_id must point to a Planner agent")

        session = PlannerChatSessionRecord(
            session_id=str(uuid4()),
            workflow_id=agent_workflow.id,
            planner_agent_id=planner.id,
            agent_workflow=agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True),
            repo=body.repo,
            scopes=list(body.scopes),
            knowledge_pack_ids=list(body.knowledge_pack_ids),
            skill_pack_ids=list(body.skill_pack_ids),
            memory_pack_ids=list(body.memory_pack_ids),
            interaction_mode=body.interaction_mode,
        )
        planner_chat_sessions[session.session_id] = session
        return session.model_dump(mode="json")

    @app.get("/api/v2/planner-chat/sessions/{session_id}")
    def get_planner_chat_session(session_id: str) -> dict[str, Any]:
        session = planner_chat_sessions.get(session_id)
        if session is None:
            raise HTTPException(status_code=404, detail="planner chat session not found")
        return session.model_dump(mode="json")

    @app.post("/api/v2/planner-chat/sessions/{session_id}/turn")
    def send_planner_chat_turn(session_id: str, body: PlannerChatTurnRequest) -> dict[str, Any]:
        session = planner_chat_sessions.get(session_id)
        if session is None:
            raise HTTPException(status_code=404, detail="planner chat session not found")
        message = body.message.strip()
        if not message:
            raise HTTPException(status_code=400, detail="message is required")

        interaction_mode = body.interaction_mode or session.interaction_mode
        session.interaction_mode = interaction_mode
        session.messages.append(message_record("user", message))
        agent_workflow = AgentWorkflowSpec.model_validate(session.agent_workflow)
        planner = next((agent for agent in agent_workflow.agents if agent.id == session.planner_agent_id), None)
        if planner is None or planner.role != "planner":
            raise HTTPException(status_code=400, detail="planner_agent_id must point to a Planner agent")

        memory_context = _planner_chat_memory_context(
            store_root=store_root,
            query=message,
            repo=session.repo,
            session_id=session.session_id,
        )
        context_packet = build_harness_context_packet(
            mode="planning_chat",
            user_goal=message,
            workflow_id=agent_workflow.id,
            agent_id=planner.id,
            planner_agent_id=planner.id,
            workflow_summary={
                "workflow_id": agent_workflow.id,
                "workflow_name": agent_workflow.name,
                "planner_agent_id": planner.id,
                "agent_count": len(agent_workflow.agents),
            },
            user_constraints=_planner_chat_mode_constraints(interaction_mode),
            selected_knowledge_pack_ids=session.knowledge_pack_ids,
            selected_skill_pack_ids=session.skill_pack_ids,
            selected_memory_pack_ids=session.memory_pack_ids,
            **memory_context,
        )
        context_packet.setdefault("hot", {})["planner_interaction_mode"] = interaction_mode
        runtime_context = HarnessRuntimeContext(
            run_id=f"planner-chat-session-{session.session_id}",
            agent_id=planner.id,
            workflow_id=agent_workflow.id,
            harness_id="conversation-harness",
            mode="planning_chat",
            profile_id=agent_workflow.harness_bindings.planning_chat.profile_id,
            repo_root=session.repo,
            context_packet=context_packet,
        )
        fallback_turn = _fallback_planner_chat_turn_payload(
            message=message,
            interaction_mode=interaction_mode,
            task_state=session.task_state.model_dump(mode="json"),
        )
        runtime_result = harness_runtime_manager.run_planning_chat(
            context=runtime_context,
            profile_id=agent_workflow.harness_bindings.planning_chat.profile_id,
            input_artifacts={
                "requested_artifact_type": "planner_chat_turn",
                "legacy_operation": "planning_chat",
                "legacy_kwargs": {"turn_payload": fallback_turn},
                "user_request": message,
                "interaction_mode": interaction_mode,
                "messages": [item.model_dump(mode="json") for item in session.messages[-12:]],
                "task_state": session.task_state.model_dump(mode="json"),
                "workflow_summary": {"workflow_id": agent_workflow.id, "workflow_name": agent_workflow.name},
            },
        )
        if runtime_result.status != "completed":
            session.status = "blocked"
            raise HTTPException(status_code=502, detail=runtime_result.error or {"message": "planner chat turn failed"})
        try:
            turn_payload = artifact_projector.project(
                runtime_result,
                artifact_type="planner_chat_turn",
                artifact_id=f"{session.session_id}:turn-{session.generation + 1}",
            )
        except ArtifactProjectionError as exc:
            session.status = "blocked"
            raise HTTPException(status_code=502, detail=str(exc)) from exc

        turn = PlannerChatTurn.model_validate(turn_payload)
        session.last_turn = turn
        session.task_state = PlannerTaskState.model_validate(turn_payload["task_state"])
        session.messages.append(message_record("assistant", turn.assistant_message))
        session.generation += 1
        if session.task_state.readiness == "ready_to_execute":
            session.status = "ready"
        else:
            session.status = "chatting"

        run_id: str | None = None
        if (
            interaction_mode == "work"
            and body.start_if_ready
            and turn.decision == "start_workflow"
            and turn.handoff is not None
        ):
            try:
                live = _start_planner_chat_session_workflow(
                    session=session,
                    agent_workflow=agent_workflow,
                    request_text=turn.handoff.workflow_request,
                    turn_payload=turn.model_dump(mode="json"),
                    agent_manager=agent_manager,
                    skill_store=skill_store,
                    store_root=store_root,
                )
            except (FileNotFoundError, NotADirectoryError, ValueError) as exc:
                session.status = "blocked"
                raise HTTPException(status_code=400, detail=str(exc)) from exc
            run_id = live.id
            session.run_id = live.id
            session.status = "running"

        planner_chat_sessions[session.session_id] = session
        return {
            "session_id": session.session_id,
            "generation": session.generation,
            "status": session.status,
            "run_id": run_id or session.run_id,
            "turn": turn.model_dump(mode="json"),
            "session": session.model_dump(mode="json"),
        }

    @app.get("/api/v2/runs")
    def list_runs() -> dict[str, Any]:
        return {"runs": store.list()}

    @app.post("/api/v2/live-agent-runs")
    def create_live_agent_run(body: AgentRunRequest) -> dict[str, Any]:
        try:
            _raise_agent_workflow_validation(body.agent_workflow)
            agent_workflow = AgentWorkflowSpec.model_validate(body.agent_workflow)
        except AgentWorkflowValidationError as exc:
            raise HTTPException(status_code=400, detail=exc.result.model_dump(mode="json")) from exc
        except Exception as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        repo_root = resolve_existing_dir(body.repo)
        initial_data = _initial_data_from_request(body, repo_root)
        initial_data["agent_workflow"] = agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True)
        initial_data["skill_index"] = build_skill_index(skill_store.list_installed()).model_dump(mode="json")
        initial_data["skill_store_root"] = str(Path(store_root))
        live = agent_manager.start(
            agent_workflow=agent_workflow,
            repo_root=str(repo_root),
            request=body.request,
            initial_data=initial_data,
        )
        return {
            "run_id": live.id,
            "status": live.status,
            "events_url": f"/api/v2/live-agent-runs/{live.id}/events",
            "result_url": f"/api/v2/live-agent-runs/{live.id}",
        }

    @app.post("/api/v2/patches/rollback")
    def rollback_patch_endpoint(body: RollbackRequest) -> dict[str, Any]:
        repo_root = resolve_existing_dir(body.repo)
        try:
            scopes = normalize_scope_paths(repo_root, body.scopes)
            result = rollback_patch(
                {"snapshot_id": body.snapshot_id},
                {"repo_root": str(repo_root), "scopes": scopes, "data": {}},
            )
        except (FileNotFoundError, NotADirectoryError, ValueError) as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        return {"rollback": result}

    @app.get("/api/v2/runs/{run_id}")
    def get_run(run_id: str, include_events: bool = True) -> dict[str, Any]:
        try:
            stored = store.get(run_id, include_events=include_events)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="run not found") from exc
        return stored.model_dump(mode="json")

    @app.delete("/api/v2/runs/{run_id}")
    def delete_run(run_id: str) -> dict[str, Any]:
        try:
            return store.delete(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="run not found") from exc

    @app.get("/api/v2/runs/{run_id}/events")
    def stream_events(
        run_id: str,
        cursor: int | None = None,
        limit: int = 100,
    ) -> Any:
        if cursor is not None:
            try:
                return store.get_events(run_id, cursor=cursor, limit=limit)
            except KeyError as exc:
                raise HTTPException(status_code=404, detail="run not found") from exc
            except ValueError as exc:
                raise HTTPException(status_code=400, detail=str(exc)) from exc

        try:
            events = store.get_events(run_id)["events"]
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="run not found") from exc

        def event_stream():
            for event in events:
                payload = json.dumps(event, ensure_ascii=False)
                yield f"event: {event['type']}\n"
                yield f"data: {payload}\n\n"

        return StreamingResponse(event_stream(), media_type="text/event-stream")

    @app.get("/api/v2/runs/{run_id}/context-packets/{packet_id}")
    def get_context_packet(run_id: str, packet_id: str) -> dict[str, Any]:
        try:
            packet = store.get_context_packet(run_id, packet_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="context packet not found") from exc
        return {
            "packet_id": packet_id,
            "packet": packet,
        }

    @app.get("/api/v2/runs/{run_id}/artifacts/{artifact_id}")
    def get_artifact(run_id: str, artifact_id: str) -> dict[str, Any]:
        try:
            artifact = store.get_artifact(run_id, artifact_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="artifact not found") from exc
        return {
            "artifact_id": artifact_id,
            "artifact": artifact,
        }

    @app.get("/api/v2/runs/{run_id}/tool-results/{tool_result_id}")
    def get_tool_result(run_id: str, tool_result_id: str) -> dict[str, Any]:
        try:
            result = store.get_tool_result(run_id, tool_result_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="tool result not found") from exc
        return {
            "tool_result_id": tool_result_id,
            "result": result,
        }

    @app.get("/api/v2/runs/{run_id}/blobs/{blob_id}")
    def get_blob(run_id: str, blob_id: str) -> dict[str, Any]:
        try:
            store.get(run_id, include_events=False)
            return store.get_blob(blob_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="blob not found") from exc

    @app.get("/api/v2/live-agent-runs/{run_id}")
    def get_live_agent_run(run_id: str) -> dict[str, Any]:
        try:
            live = agent_manager.get(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live agent run not found") from exc
        return {
            "id": live.id,
            "workflow_id": live.agent_workflow.id,
            "runtime_type": "agent_graph",
            "repo_root": live.repo_root,
            "request": live.request,
            "status": live.status,
            "events": [event.model_dump(mode="json") for event in live.events],
            "result": live.result.model_dump(mode="json") if live.result else None,
            "stored_run_id": live.stored_run_id,
            "error": live.error,
            "run_group_id": live.initial_data.get("run_group_id"),
            "parent_run_id": live.initial_data.get("parent_run_id"),
            "continued_from_run_id": live.initial_data.get("continued_from_run_id"),
            "turn_index": live.initial_data.get("turn_index"),
            "heartbeat": agent_manager.heartbeat(run_id),
            "approval_required": False,
        }

    @app.get("/api/v2/live-agent-runs/{run_id}/events")
    def stream_live_agent_events(run_id: str) -> StreamingResponse:
        try:
            agent_manager.get(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live agent run not found") from exc

        def event_stream():
            for event in agent_manager.stream(run_id):
                payload = json.dumps(event.model_dump(mode="json"), ensure_ascii=False)
                yield f"event: {event.type}\n"
                yield f"data: {payload}\n\n"

        return StreamingResponse(event_stream(), media_type="text/event-stream")

    @app.post("/api/v2/live-agent-runs/{run_id}/pause")
    def pause_live_agent_run(run_id: str) -> dict[str, Any]:
        try:
            live = agent_manager.pause(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live agent run not found") from exc
        except ValueError as exc:
            raise HTTPException(status_code=409, detail=str(exc)) from exc
        return {"run_id": live.id, "status": live.status, "heartbeat": agent_manager.heartbeat(run_id)}

    @app.post("/api/v2/live-agent-runs/{run_id}/resume")
    def resume_live_agent_run(run_id: str) -> dict[str, Any]:
        try:
            live = agent_manager.resume(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live agent run not found") from exc
        except ValueError as exc:
            raise HTTPException(status_code=409, detail=str(exc)) from exc
        return {"run_id": live.id, "status": live.status, "heartbeat": agent_manager.heartbeat(run_id)}

    @app.post("/api/v2/live-agent-runs/{run_id}/cancel")
    def cancel_live_agent_run(run_id: str) -> dict[str, Any]:
        try:
            live = agent_manager.cancel(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live agent run not found") from exc
        except ValueError as exc:
            raise HTTPException(status_code=409, detail=str(exc)) from exc
        return {"run_id": live.id, "status": live.status, "heartbeat": agent_manager.heartbeat(run_id)}

    @app.get("/api/v2/live-agent-runs/{run_id}/heartbeat")
    def get_live_agent_run_heartbeat(run_id: str) -> dict[str, Any]:
        try:
            return agent_manager.heartbeat(run_id)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail="live agent run not found") from exc

    frontend_path = Path(frontend_dist) if frontend_dist else Path.cwd() / "frontend" / "dist"
    if frontend_path.exists():
        app.mount("/", StaticFiles(directory=frontend_path, html=True), name="frontend")

    return app


def _planning_chat_fallback_runner(
    *,
    draft_payload: dict[str, Any] | None = None,
    turn_payload: dict[str, Any] | None = None,
    emit: Any | None = None,
) -> dict[str, Any]:
    if emit is not None:
        emit(
            "harness_runtime.fallback.planning_chat",
            "InternalFallbackProvider generated deterministic planning chat output",
            mode="planning_chat",
        )
    if turn_payload is not None:
        return dict(turn_payload)
    return dict(draft_payload or {})


def _planner_chat_memory_context(
    *,
    store_root: str | Path,
    query: str,
    repo: str | None,
    session_id: str | None = None,
) -> dict[str, Any]:
    root = Path(store_root)
    memory_root = root if root.name == "memory" else root / "memory"
    if not memory_root.exists():
        return {}
    try:
        from coder_workbench.memory import (
            AgentScopedMemoryStore,
            KnowledgeStore,
            MemoryRetrievalRequest,
            MemoryRetriever,
            policy_for_role,
        )

        request = MemoryRetrievalRequest(
            role="planning_chat",
            requested_context="assistant_message",
            query=query,
            project_id=str(repo) if repo else None,
            session_id=session_id,
        )
        cards = MemoryRetriever(
            memory_store=AgentScopedMemoryStore(root),
            knowledge_store=KnowledgeStore(root),
        ).retrieve(request)
        memory_cards = [card for card in cards if card.card_type == "memory_record"]
        knowledge_hits = [card for card in cards if card.card_type == "knowledge_chunk"]
        policy = policy_for_role("planning_chat")
        return {
            "memory_cards": memory_cards,
            "knowledge_hits": knowledge_hits,
            "memory_token_budget": {
                "limit": policy.max_tokens,
                "used": sum(card.token_estimate for card in cards),
            },
        }
    except Exception:
        return {}


def _planner_chat_draft_payloads(
    *,
    draft_id: str,
    request: str,
    workflow: AgentWorkflowSpec,
    planner_agent_id: str,
    proposed_scope: list[str],
    knowledge_pack_ids: list[str],
    skill_pack_ids: list[str],
    memory_pack_ids: list[str],
) -> tuple[dict[str, Any], dict[str, Any]]:
    success_criteria = [
        "User confirms the draft before execution starts.",
        f"Complete the requested goal: {request}",
    ]
    risks = [
        "Execution may be blocked if required credentials, network, or dependencies are unavailable.",
        "Task execution must stay inside the confirmed workflow and sandbox policy.",
    ]
    plan_draft = validate_artifact(
        {
            "artifact_type": "project_plan_draft",
            "draft_id": draft_id,
            "summary": f"Draft plan for {workflow.name}: {request}",
            "proposed_scope": proposed_scope,
            "success_criteria": success_criteria,
            "risks": risks,
            "requires_confirmation": True,
        },
        expected_type="project_plan_draft",
        artifact_id=draft_id,
    )
    run_contract_draft = validate_artifact(
        {
            "artifact_type": "run_contract_draft",
            "draft_id": draft_id,
            "user_goal": request,
            "workflow_id": workflow.id,
            "planner_agent_id": planner_agent_id,
            "success_criteria": success_criteria,
            "constraints": [
                "Planning Chat Mode cannot write files or run commands.",
                "Workflow execution starts only after confirm approved=true.",
            ],
            "selected_knowledge_pack_ids": knowledge_pack_ids,
            "selected_skill_pack_ids": skill_pack_ids,
            "selected_memory_pack_ids": memory_pack_ids,
            "requires_confirmation": True,
        },
        expected_type="run_contract_draft",
        artifact_id=f"{draft_id}:run_contract",
    )
    return plan_draft, run_contract_draft


def _planner_chat_mode_constraints(interaction_mode: str) -> list[str]:
    constraints = [
        "Planning Chat Mode must not write files or run commands.",
        "Do not expose private chain-of-thought, raw prompts, logs, diffs, secrets, or model internals.",
    ]
    if interaction_mode == "discuss":
        constraints.append("Discuss mode must never start workflow execution.")
    else:
        constraints.append("Work mode may start the existing workflow only when the task is ready_to_execute.")
    return constraints


def _fallback_planner_chat_turn_payload(
    *,
    message: str,
    interaction_mode: str,
    task_state: dict[str, Any],
) -> dict[str, Any]:
    current = dict(task_state or {})
    goal = str(current.get("goal") or message).strip()
    success_criteria = [str(item) for item in current.get("success_criteria", []) if str(item).strip()]
    open_questions = [str(item) for item in current.get("open_questions", []) if str(item).strip()]
    readiness = str(current.get("readiness") or "not_ready")
    can_start = (
        interaction_mode == "work"
        and readiness == "ready_to_execute"
        and bool(goal)
        and bool(success_criteria)
        and not open_questions
    )
    if can_start:
        return validate_artifact(
            {
                "artifact_type": "planner_chat_turn",
                "assistant_message": "I have enough detail and will start the workflow.",
                "interaction_mode": interaction_mode,
                "decision": "start_workflow",
                "visible_thinking": {"phase": "ready_to_start", "summary": "Ready to start the workflow."},
                "task_state": {**current, "goal": goal, "success_criteria": success_criteria, "open_questions": [], "readiness": "ready_to_execute"},
                "handoff": {
                    "workflow_request": goal,
                    "scope": [str(item) for item in current.get("scope", []) if str(item).strip()],
                    "success_criteria": success_criteria,
                    "risks": [str(item) for item in current.get("risks", []) if str(item).strip()],
                },
            },
            expected_type="planner_chat_turn",
        )
    question = "What success criteria should I use before starting work?"
    next_state = {
        **current,
        "goal": goal,
        "open_questions": open_questions or [question],
        "readiness": "needs_clarification",
    }
    return validate_artifact(
        {
            "artifact_type": "planner_chat_turn",
            "assistant_message": question if interaction_mode == "work" else f"I can help plan this. {question}",
            "interaction_mode": interaction_mode,
            "decision": "blocked_needs_clarification" if interaction_mode == "work" else "continue_chat",
            "visible_thinking": {"phase": "clarifying", "summary": "Clarifying success criteria."},
            "task_state": next_state,
            "handoff": None,
        },
        expected_type="planner_chat_turn",
    )


def _start_planner_chat_session_workflow(
    *,
    session: PlannerChatSessionRecord,
    agent_workflow: AgentWorkflowSpec,
    request_text: str,
    turn_payload: dict[str, Any],
    agent_manager: AgentGraphRunManager,
    skill_store: InstalledSkillStore,
    store_root: str | Path,
):
    repo = session.repo or "."
    repo_root = resolve_existing_dir(str(repo))
    normalized_scopes = normalize_scope_paths(repo_root, session.scopes)
    initial_data = {
        "request": request_text,
        "approved": True,
        "preapprove_all": True,
        "scopes": normalized_scopes,
        "planner_chat_turn": turn_payload,
        "planner_task_state": turn_payload.get("task_state") or {},
        "planner_interaction_mode": session.interaction_mode,
        "planner_chat_session_id": session.session_id,
        "agent_workflow": agent_workflow.model_dump(mode="json", by_alias=True, exclude_none=True),
        "skill_index": build_skill_index(skill_store.list_installed()).model_dump(mode="json"),
        "skill_store_root": str(Path(store_root)),
    }
    return agent_manager.start(
        agent_workflow=agent_workflow,
        repo_root=str(repo_root),
        request=request_text,
        initial_data=initial_data,
    )


def _raise_agent_workflow_validation(agent_workflow: dict[str, Any]) -> None:
    validation = validate_agent_workflow_payload(agent_workflow)
    if validation.status == "error":
        raise AgentWorkflowValidationError(validation)


def _load_agent_workflow_for_planner_chat(library: LibraryStore, workflow_id: str) -> AgentWorkflowSpec:
    if workflow_id == "default-planner-led":
        return default_planner_led_agent_workflow()
    try:
        return AgentWorkflowSpec.model_validate(library.get_agent_workflow(workflow_id))
    except KeyError as exc:
        raise ValueError("agent workflow not found") from exc


def _resolve_skill_registry_url(registry_url: str | None) -> str:
    url = (registry_url or os.getenv("CODER_SKILL_REGISTRY_URL") or "").strip()
    if not url:
        raise ValueError("registry_url is required when CODER_SKILL_REGISTRY_URL is not configured")
    return url


def _skill_update_report(skill_store: InstalledSkillStore, entries: list[Any]) -> list[dict[str, Any]]:
    by_id = {entry.id: entry for entry in entries}
    updates: list[dict[str, Any]] = []
    for record in skill_store.list_installed():
        entry = by_id.get(record.id)
        if entry is None:
            updates.append(
                {
                    "skill_id": record.id,
                    "installed_version": record.manifest.version,
                    "available_version": None,
                    "update_available": False,
                    "auto_update_eligible": False,
                    "pinned_version": record.pinned_version,
                    "update_policy": record.update_policy,
                    "reason": "not in registry",
                }
            )
            continue
        update_available = entry.version != record.manifest.version or entry.sha256 != record.package_sha256
        auto_update_eligible = is_auto_update_allowed(record, entry)
        updates.append(
            {
                "skill_id": record.id,
                "installed_version": record.manifest.version,
                "available_version": entry.version,
                "update_available": update_available,
                "auto_update_eligible": auto_update_eligible,
                "pinned_version": record.pinned_version,
                "update_policy": record.update_policy,
                "risk_level": entry.risk_level,
                "trust_level": entry.trust_level,
                "external_effect": entry.external_effect,
                "reason": (
                    None
                    if auto_update_eligible
                    else skill_auto_update_block_reason(record, entry)
                ),
            }
        )
    return updates


def _initial_data_from_request(body: AgentRunRequest, repo_root: Path) -> dict[str, Any]:
    try:
        scopes = normalize_scope_paths(repo_root, body.scopes)
    except (FileNotFoundError, NotADirectoryError, ValueError) as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    initial_data = dict(body.initial_data)
    initial_data.update({
        "request": body.request,
        "approved": body.approved,
        "preapprove_all": body.approved,
        "scopes": scopes,
    })
    return initial_data
