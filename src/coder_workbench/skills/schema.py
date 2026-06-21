from __future__ import annotations

import hashlib
import json
import re
from datetime import datetime, timezone
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator


SkillType = Literal["knowledge", "procedure", "connector", "artifact", "evaluation"]
SkillRiskLevel = Literal["low", "medium", "high"]
SkillTrustLevel = Literal["official", "verified", "community", "local", "untrusted"]
SkillAuthority = Literal["planner", "worker", "tester", "synthesizer"]
SkillUpdatePolicy = Literal["manual", "auto_official_low_risk"]

_SKILL_ID_RE = re.compile(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}$")


def _utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


class SkillContextPolicy(BaseModel):
    model_config = ConfigDict(extra="forbid")

    load_mode: Literal["on_demand", "always_summary", "manual"] = "on_demand"
    max_skill_tokens: int = Field(default=1200, ge=0)


class SkillCompatibility(BaseModel):
    model_config = ConfigDict(extra="forbid")

    coder_min_version: str = "0.7.0"
    agent_graph_runtime: bool = True


class ConnectorOperation(BaseModel):
    """Locked connector operation metadata imported from a SkillPack."""

    model_config = ConfigDict(extra="forbid")

    connector_id: str
    operation_id: str
    description: str = ""
    input_schema: dict[str, Any] = Field(default_factory=dict)
    risk_level: SkillRiskLevel = "low"
    external_effect: bool = False
    requires_preview: bool = False
    requires_human_approval: bool = False
    descriptor_sha256: str | None = None

    @field_validator("connector_id", "operation_id")
    @classmethod
    def require_identifier(cls, value: str, info: Any) -> str:
        return _non_empty(value, info.field_name)

    @model_validator(mode="after")
    def lock_descriptor(self) -> "ConnectorOperation":
        if self.external_effect and not self.requires_preview:
            raise ValueError("external-effect connector operations must require preview")
        if self.external_effect and not self.requires_human_approval:
            raise ValueError("external-effect connector operations must require human approval")
        expected = _connector_operation_sha256(self)
        if self.descriptor_sha256 is None:
            self.descriptor_sha256 = expected
        elif self.descriptor_sha256.lower().removeprefix("sha256:") != expected:
            raise ValueError("connector operation descriptor_sha256 does not match locked metadata")
        return self

    def summary(self, *, package_sha256: str | None = None) -> dict[str, Any]:
        summary = {
            "connector_id": self.connector_id,
            "operation_id": self.operation_id,
            "risk_level": self.risk_level,
            "external_effect": self.external_effect,
            "requires_preview": self.requires_preview,
            "requires_human_approval": self.requires_human_approval,
            "descriptor_sha256": self.descriptor_sha256,
        }
        if package_sha256 is not None:
            summary["package_sha256"] = package_sha256
        return summary


class SkillPackageManifest(BaseModel):
    """Validated SkillPack manifest stored as skill.json."""

    model_config = ConfigDict(extra="forbid")

    id: str
    name: str
    version: str
    description: str
    category: str
    skill_type: SkillType
    risk_level: SkillRiskLevel
    publisher: str
    allowed_authorities: list[SkillAuthority]
    requires: list[str] = Field(default_factory=list)
    produces: list[str] = Field(default_factory=list)
    connectors: list[str] = Field(default_factory=list)
    connector_operations: list[ConnectorOperation] = Field(default_factory=list)
    external_effect: bool = False
    requires_preview: bool = False
    requires_human_approval: bool = False
    context_policy: SkillContextPolicy = Field(default_factory=SkillContextPolicy)
    compatibility: SkillCompatibility = Field(default_factory=SkillCompatibility)
    trigger_hints: list[str] = Field(default_factory=list)

    @field_validator("id")
    @classmethod
    def validate_id(cls, value: str) -> str:
        value = _non_empty(value, "id")
        if not _SKILL_ID_RE.match(value):
            raise ValueError("skill id must use letters, numbers, dots, underscores, or hyphens")
        return value

    @field_validator("name", "version", "description", "category", "publisher")
    @classmethod
    def require_text(cls, value: str, info: Any) -> str:
        return _non_empty(value, info.field_name)

    @field_validator("allowed_authorities")
    @classmethod
    def require_authority(cls, value: list[SkillAuthority]) -> list[SkillAuthority]:
        if not value:
            raise ValueError("allowed_authorities must not be empty")
        return _dedupe(value)

    @field_validator("requires", "produces", "connectors", "trigger_hints")
    @classmethod
    def clean_string_list(cls, value: list[str]) -> list[str]:
        return _dedupe([item.strip() for item in value if str(item).strip()])

    @model_validator(mode="after")
    def enforce_external_effect_policy(self) -> "SkillPackageManifest":
        if self.external_effect and not self.requires_preview:
            raise ValueError("external_effect skills must require preview")
        if self.external_effect and not self.requires_human_approval:
            raise ValueError("external_effect skills must require human approval")
        connector_ids = set(self.connectors)
        for operation in self.connector_operations:
            if operation.connector_id not in connector_ids:
                raise ValueError("connector_operations must reference declared connectors")
            if operation.external_effect and not self.external_effect:
                raise ValueError("external-effect connector operations require manifest external_effect=true")
        return self

    def summary(
        self,
        *,
        trust_level: SkillTrustLevel,
        enabled: bool = True,
        package_sha256: str | None = None,
    ) -> "SkillSummary":
        return SkillSummary(
            id=self.id,
            name=self.name,
            version=self.version,
            description=self.description,
            category=self.category,
            risk_level=self.risk_level,
            publisher=self.publisher,
            requires=self.requires,
            produces=self.produces,
            connectors=self.connectors,
            connector_operations=[
                operation.summary(package_sha256=package_sha256)
                for operation in self.connector_operations
            ],
            trust_level=trust_level,
            enabled=enabled,
            external_effect=self.external_effect,
            when_to_use=self.trigger_hints,
        )


class RemoteSkillEntry(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    name: str
    version: str
    description: str
    category: str
    publisher: str
    package_url: str
    manifest_url: str | None = None
    sha256: str
    signature: str | None = None
    risk_level: SkillRiskLevel
    external_effect: bool = False
    requires_connectors: list[str] = Field(default_factory=list)
    connector_operations: list[ConnectorOperation] = Field(default_factory=list)
    trust_level: SkillTrustLevel = "community"

    @model_validator(mode="before")
    @classmethod
    def infer_trust_level(cls, data: Any) -> Any:
        if not isinstance(data, dict) or data.get("trust_level"):
            return data
        migrated = dict(data)
        publisher = str(migrated.get("publisher") or "").strip().lower()
        if publisher in {"coder-official", "coder"} or publisher.startswith("coder-official/"):
            migrated["trust_level"] = "official"
        return migrated

    @field_validator("id")
    @classmethod
    def validate_id(cls, value: str) -> str:
        value = _non_empty(value, "id")
        if not _SKILL_ID_RE.match(value):
            raise ValueError("skill id must use letters, numbers, dots, underscores, or hyphens")
        return value

    @field_validator("name", "version", "description", "category", "publisher", "package_url", "sha256")
    @classmethod
    def require_text(cls, value: str, info: Any) -> str:
        return _non_empty(value, info.field_name)

    @field_validator("requires_connectors")
    @classmethod
    def clean_connectors(cls, value: list[str]) -> list[str]:
        return _dedupe([item.strip() for item in value if str(item).strip()])

    @model_validator(mode="after")
    def enforce_connector_operation_policy(self) -> "RemoteSkillEntry":
        connector_ids = set(self.requires_connectors)
        for operation in self.connector_operations:
            if operation.connector_id not in connector_ids:
                raise ValueError("connector_operations must reference declared requires_connectors")
            if operation.external_effect and not self.external_effect:
                raise ValueError("external-effect connector operations require registry external_effect=true")
        return self

    def summary(self, *, installed: bool = False) -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "description": self.description,
            "category": self.category,
            "risk_level": self.risk_level,
            "publisher": self.publisher,
            "trust_level": self.trust_level,
            "requires_connectors": self.requires_connectors,
            "external_effect": self.external_effect,
            "connector_operations": [
                operation.summary(package_sha256=self.sha256)
                for operation in self.connector_operations
            ],
            "installed": installed,
        }


class RemoteSkillIndex(BaseModel):
    model_config = ConfigDict(extra="forbid")

    registry_version: str
    generated_at: str
    skills: list[RemoteSkillEntry]

    @field_validator("registry_version", "generated_at")
    @classmethod
    def require_text(cls, value: str, info: Any) -> str:
        return _non_empty(value, info.field_name)

    def get(self, skill_id: str) -> RemoteSkillEntry:
        for skill in self.skills:
            if skill.id == skill_id:
                return skill
        raise KeyError(skill_id)


class InstalledSkillRecord(BaseModel):
    model_config = ConfigDict(extra="forbid")

    manifest: SkillPackageManifest
    installed_at: str = Field(default_factory=_utc_now)
    source: Literal["remote", "local"] = "remote"
    source_url: str | None = None
    package_sha256: str
    trust_level: SkillTrustLevel
    enabled: bool = True
    pinned_version: str | None = None
    update_policy: SkillUpdatePolicy = "manual"

    @property
    def id(self) -> str:
        return self.manifest.id

    def summary(self) -> "SkillSummary":
        return self.manifest.summary(
            trust_level=self.trust_level,
            enabled=self.enabled,
            package_sha256=self.package_sha256,
        )


class SkillSummary(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    name: str
    version: str
    description: str
    category: str
    risk_level: SkillRiskLevel
    publisher: str
    requires: list[str] = Field(default_factory=list)
    produces: list[str] = Field(default_factory=list)
    connectors: list[str] = Field(default_factory=list)
    connector_operations: list[dict[str, Any]] = Field(default_factory=list)
    trust_level: SkillTrustLevel
    enabled: bool = True
    external_effect: bool = False
    when_to_use: list[str] = Field(default_factory=list)


def _non_empty(value: str, field_name: str) -> str:
    text = str(value or "").strip()
    if not text:
        raise ValueError(f"{field_name} must not be empty")
    return text


def _dedupe(items: list[Any]) -> list[Any]:
    seen = set()
    output = []
    for item in items:
        if item in seen:
            continue
        seen.add(item)
        output.append(item)
    return output


def _connector_operation_sha256(operation: ConnectorOperation) -> str:
    payload = {
        "connector_id": operation.connector_id,
        "operation_id": operation.operation_id,
        "description": operation.description,
        "input_schema": operation.input_schema,
        "risk_level": operation.risk_level,
        "external_effect": operation.external_effect,
        "requires_preview": operation.requires_preview,
        "requires_human_approval": operation.requires_human_approval,
    }
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()
