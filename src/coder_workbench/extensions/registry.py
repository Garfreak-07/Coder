from __future__ import annotations

from coder_workbench.skills import InstalledSkillRecord

from .schema import ExtensionManifest, PluginManifest, SkillManifest


def builtin_plugin_manifests() -> list[PluginManifest]:
    return [
        PluginManifest(
            id="command-runner",
            name="Command Runner",
            description="Runs approved local commands through CommandService.",
            operations=["run_check", "sandbox_check"],
            external_effect=True,
            requires_preview=True,
            tags=["coding", "checks"],
        ),
        PluginManifest(
            id="filesystem-patch",
            name="File Patch Service",
            description="Creates patch previews, applies authorized patches, and rolls back snapshots.",
            operations=["patch_preview", "apply_patch", "rollback_patch"],
            external_effect=True,
            requires_preview=True,
            tags=["coding", "files"],
        ),
        PluginManifest(
            id="openhands-task-executor-runtime",
            name="OpenHands Task Executor Runtime",
            description="Harness runtime provider for coding work items.",
            extension_type="harness_runtime",
            operations=["harness_runtime.run_task_execution"],
            tags=["harness_runtime", "executor", "coding"],
        ),
    ]


def skill_manifest_from_record(record: InstalledSkillRecord) -> SkillManifest:
    summary = record.summary()
    return SkillManifest(
        id=summary.id,
        name=summary.name,
        version=summary.version,
        description=summary.description,
        installed=True,
        enabled=summary.enabled,
        risk_level=summary.risk_level,
        trust_level=summary.trust_level,
        category=summary.category,
        produces=summary.produces,
        requires=summary.requires,
        tags=["skill", summary.category],
    )


def extension_search(
    *,
    query: str,
    skills: list[InstalledSkillRecord],
    plugins: list[PluginManifest] | None = None,
) -> list[ExtensionManifest]:
    normalized = query.strip().lower()
    candidates: list[ExtensionManifest] = [*(plugins or builtin_plugin_manifests())]
    candidates.extend(skill_manifest_from_record(record) for record in skills)
    if not normalized:
        return candidates
    return [
        extension
        for extension in candidates
        if normalized in extension.id.lower()
        or normalized in extension.name.lower()
        or normalized in extension.description.lower()
        or any(normalized in tag.lower() for tag in extension.tags)
    ]
