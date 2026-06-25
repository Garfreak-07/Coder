from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field


class ExtensionManifest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str
    name: str
    version: str = "builtin"
    description: str = ""
    extension_type: Literal["plugin", "skill", "harness_runtime"]
    installed: bool = True
    enabled: bool = True
    risk_level: str = "low"
    trust_level: str = "local"
    tags: list[str] = Field(default_factory=list)


class PluginManifest(ExtensionManifest):
    extension_type: Literal["plugin", "harness_runtime"] = "plugin"
    operations: list[str] = Field(default_factory=list)
    external_effect: bool = False
    requires_preview: bool = False


class SkillManifest(ExtensionManifest):
    extension_type: Literal["skill"] = "skill"
    category: str = ""
    produces: list[str] = Field(default_factory=list)
    requires: list[str] = Field(default_factory=list)
