from __future__ import annotations

from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.skills.schema import InstalledSkillRecord, SkillRiskLevel, SkillTrustLevel


class SkillIndexEntry(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    name: str
    description: str
    when_to_use: list[str] = Field(default_factory=list)
    category: str
    risk_level: SkillRiskLevel
    produces: list[str] = Field(default_factory=list)
    requires: list[str] = Field(default_factory=list)
    connectors: list[str] = Field(default_factory=list)
    trust_level: SkillTrustLevel
    connector_operations: list[dict[str, Any]] = Field(default_factory=list)
    enabled: bool = True
    max_skill_tokens: int = 1200


class SkillIndex(BaseModel):
    model_config = ConfigDict(extra="forbid")

    skills: list[SkillIndexEntry] = Field(default_factory=list)

    def enabled(self) -> list[SkillIndexEntry]:
        return [skill for skill in self.skills if skill.enabled]


def build_skill_index(records: list[InstalledSkillRecord]) -> SkillIndex:
    return SkillIndex(
        skills=[
            SkillIndexEntry(
                id=record.manifest.id,
                name=record.manifest.name,
                description=record.manifest.description,
                when_to_use=record.manifest.trigger_hints,
                category=record.manifest.category,
                risk_level=record.manifest.risk_level,
                produces=record.manifest.produces,
                requires=record.manifest.requires,
                connectors=record.manifest.connectors,
                connector_operations=[
                    operation.summary(package_sha256=record.package_sha256)
                    for operation in record.manifest.connector_operations
                ],
                trust_level=record.trust_level,
                enabled=record.enabled,
                max_skill_tokens=record.manifest.context_policy.max_skill_tokens,
            )
            for record in records
        ]
    )
