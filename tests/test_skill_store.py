from __future__ import annotations

import json
import tempfile
import unittest
import zipfile
from pathlib import Path

from fastapi.testclient import TestClient

from coder_workbench.server.app import create_app
from coder_workbench.skills import (
    InstalledSkillStore,
    RegistryClient,
    SkillInstaller,
    SkillVerificationError,
    build_skill_index,
    select_skills_for_work_item,
    sha256_digest,
    sign_package_sha256,
    verify_sha256,
)
from coder_workbench.skills.schema import SkillPackageManifest


class SkillStoreSchemaTests(unittest.TestCase):
    def test_valid_manifest_accepts_skillpack_fields(self) -> None:
        manifest = SkillPackageManifest.model_validate(_manifest())

        self.assertEqual(manifest.id, "github-research")
        self.assertEqual(manifest.skill_type, "procedure")
        self.assertEqual(manifest.context_policy.max_skill_tokens, 1200)

    def test_invalid_manifest_rejects_unknown_type_and_external_effect_without_approval(self) -> None:
        invalid_type = _manifest(skill_type="automation")
        with self.assertRaises(Exception):
            SkillPackageManifest.model_validate(invalid_type)

        unsafe_effect = _manifest(
            id="publishing-skill",
            external_effect=True,
            requires_preview=False,
            requires_human_approval=False,
        )
        with self.assertRaises(Exception):
            SkillPackageManifest.model_validate(unsafe_effect)

    def test_connector_operation_descriptor_lock_is_generated_and_summarized(self) -> None:
        manifest = SkillPackageManifest.model_validate(
            _manifest(connector_operations=[_connector_operation()])
        )

        operation = manifest.connector_operations[0]
        self.assertIsNotNone(operation.descriptor_sha256)
        self.assertEqual(len(operation.descriptor_sha256 or ""), 64)

        summary = manifest.summary(trust_level="official", package_sha256="a" * 64)
        self.assertEqual(summary.connector_operations[0]["descriptor_sha256"], operation.descriptor_sha256)
        self.assertEqual(summary.connector_operations[0]["package_sha256"], "a" * 64)

    def test_connector_operation_rejects_mismatched_descriptor_and_unknown_connector(self) -> None:
        with self.assertRaises(Exception):
            SkillPackageManifest.model_validate(
                _manifest(connector_operations=[_connector_operation(descriptor_sha256="0" * 64)])
            )

        with self.assertRaises(Exception):
            SkillPackageManifest.model_validate(
                _manifest(connector_operations=[_connector_operation(connector_id="slack_readonly")])
            )

    def test_external_effect_connector_operation_requires_preview_and_manifest_external_effect(self) -> None:
        with self.assertRaises(Exception):
            SkillPackageManifest.model_validate(
                _manifest(
                    external_effect=True,
                    requires_preview=True,
                    requires_human_approval=True,
                    connector_operations=[
                        _connector_operation(
                            external_effect=True,
                            requires_preview=False,
                            requires_human_approval=True,
                        )
                    ],
                )
            )

        with self.assertRaises(Exception):
            SkillPackageManifest.model_validate(
                _manifest(
                    connector_operations=[
                        _connector_operation(
                            external_effect=True,
                            requires_preview=True,
                            requires_human_approval=True,
                        )
                    ]
                )
            )

    def test_checksum_verifier_rejects_mismatch(self) -> None:
        with self.assertRaises(SkillVerificationError):
            verify_sha256(b"package", "0" * 64)


class SkillInstallerTests(unittest.TestCase):
    def test_installer_verifies_package_and_persists_skill(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_path, digest = _write_skill_package(root)
            registry_path = _write_registry(root, package_path, digest)

            store = InstalledSkillStore(root / ".coder")
            result = SkillInstaller(client=RegistryClient(str(registry_path)), store=store).install("github-research")

            self.assertEqual(result.record.manifest.id, "github-research")
            self.assertEqual(result.record.trust_level, "official")
            self.assertEqual(result.verification.sha256, digest)
            self.assertTrue((root / ".coder" / "skills" / "github-research" / "SKILL.md").exists())

            loaded = store.get_skill("github-research")
            self.assertTrue(loaded.enabled)
            self.assertEqual(loaded.package_sha256, digest)

    def test_installer_verifies_configured_package_signature(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_path, digest = _write_skill_package(root)
            signature = sign_package_sha256(digest, key_id="coder-official", secret="test-secret")
            registry_path = _write_registry(root, package_path, digest, signature=signature)

            store = InstalledSkillStore(root / ".coder")
            result = SkillInstaller(
                client=RegistryClient(str(registry_path)),
                store=store,
                trusted_signature_keys={"coder-official": "test-secret"},
                require_verified_signatures=True,
            ).install("github-research")

            self.assertEqual(result.verification.signature_status, "verified")
            self.assertEqual(result.verification.signature_key_id, "coder-official")
            self.assertFalse(any("signature" in warning for warning in result.warnings))

    def test_installer_rejects_signature_mismatch_when_required(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_path, digest = _write_skill_package(root)
            signature = sign_package_sha256(digest, key_id="coder-official", secret="wrong-secret")
            registry_path = _write_registry(root, package_path, digest, signature=signature)

            store = InstalledSkillStore(root / ".coder")
            with self.assertRaises(SkillVerificationError):
                SkillInstaller(
                    client=RegistryClient(str(registry_path)),
                    store=store,
                    trusted_signature_keys={"coder-official": "test-secret"},
                    require_verified_signatures=True,
                ).install("github-research")

    def test_installer_rejects_checksum_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_path, _digest = _write_skill_package(root)
            registry_path = _write_registry(root, package_path, "1" * 64)

            store = InstalledSkillStore(root / ".coder")
            with self.assertRaises(SkillVerificationError):
                SkillInstaller(client=RegistryClient(str(registry_path)), store=store).install("github-research")

    def test_skill_index_includes_connector_operation_locks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            operation = _connector_operation()
            package_path, digest = _write_skill_package(
                root,
                manifest=_manifest(connector_operations=[operation]),
            )
            registry_path = _write_registry(
                root,
                package_path,
                digest,
                connector_operations=[operation],
            )
            store = InstalledSkillStore(root / ".coder")
            SkillInstaller(client=RegistryClient(str(registry_path)), store=store).install("github-research")

            skill_index = build_skill_index(store.list_installed())
            locks = skill_index.skills[0].connector_operations

            self.assertEqual(locks[0]["connector_id"], "github_readonly")
            self.assertEqual(locks[0]["operation_id"], "search_repositories")
            self.assertEqual(locks[0]["package_sha256"], digest)
            self.assertEqual(len(locks[0]["descriptor_sha256"]), 64)

    def test_installer_rejects_registry_connector_operation_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_path, digest = _write_skill_package(
                root,
                manifest=_manifest(connector_operations=[_connector_operation()]),
            )
            registry_path = _write_registry(
                root,
                package_path,
                digest,
                connector_operations=[
                    _connector_operation(
                        input_schema={
                            "type": "object",
                            "properties": {"topic": {"type": "string"}},
                            "required": ["topic"],
                        }
                    )
                ],
            )

            store = InstalledSkillStore(root / ".coder")
            with self.assertRaises(SkillVerificationError):
                SkillInstaller(client=RegistryClient(str(registry_path)), store=store).install("github-research")

    def test_auto_update_skips_connector_operation_lock_change(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            operation_v1 = _connector_operation()
            package_v1, digest_v1 = _write_skill_package(
                root,
                manifest=_manifest(connector_operations=[operation_v1]),
                package_name="github-research-0.1.0.zip",
            )
            registry_v1 = _write_registry(
                root,
                package_v1,
                digest_v1,
                connector_operations=[operation_v1],
            )
            store = InstalledSkillStore(root / ".coder")
            SkillInstaller(client=RegistryClient(str(registry_v1)), store=store).install("github-research")
            store.set_update_policy("github-research", "auto_official_low_risk")

            operation_v2 = _connector_operation(
                input_schema={
                    "type": "object",
                    "properties": {"topic": {"type": "string"}},
                    "required": ["topic"],
                }
            )
            package_v2, digest_v2 = _write_skill_package(
                root,
                manifest=_manifest(version="0.2.0", connector_operations=[operation_v2]),
                package_name="github-research-0.2.0.zip",
            )
            registry_v2 = _write_registry(
                root,
                package_v2,
                digest_v2,
                version="0.2.0",
                connector_operations=[operation_v2],
            )

            result = SkillInstaller(client=RegistryClient(str(registry_v2)), store=store).auto_update()

            self.assertEqual(result.updated, [])
            self.assertEqual(result.skipped, [{"skill_id": "github-research", "reason": "not eligible"}])
            self.assertEqual(store.get_skill("github-research").manifest.version, "0.1.0")

    def test_skill_router_selects_matching_installed_skill(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_path, digest = _write_skill_package(root)
            registry_path = _write_registry(root, package_path, digest)
            store = InstalledSkillStore(root / ".coder")
            SkillInstaller(client=RegistryClient(str(registry_path)), store=store).install("github-research")
            skill_index = build_skill_index(store.list_installed())

            decision = select_skills_for_work_item(
                skill_index=skill_index,
                user_request="Research GitHub repositories for similar projects.",
                work_item={"work_item_id": "research", "task_summary": "Search GitHub sources"},
                role="researcher",
            )

            self.assertEqual(decision.allowed_skill_ids, ["github-research"])
            self.assertEqual(decision.loaded_skill_refs, ["skill:github-research:SKILL.md"])


class SkillApiTests(unittest.TestCase):
    def test_skill_api_discovers_installs_and_disables_skill(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_path, digest = _write_skill_package(root)
            registry_path = _write_registry(root, package_path, digest)
            client = TestClient(create_app(store_root=root / ".coder", frontend_dist=root))

            discover = client.get("/api/v2/skills/discover", params={"registry_url": str(registry_path)})
            self.assertEqual(discover.status_code, 200)
            self.assertEqual(discover.json()["skills"][0]["id"], "github-research")
            self.assertFalse(discover.json()["skills"][0]["installed"])

            install = client.post(
                "/api/v2/skills/install",
                json={"skill_id": "github-research", "registry_url": str(registry_path)},
            )
            self.assertEqual(install.status_code, 200)
            self.assertEqual(install.json()["record"]["manifest"]["id"], "github-research")

            installed = client.get("/api/v2/skills/installed")
            self.assertEqual(installed.status_code, 200)
            self.assertEqual(installed.json()["skills"][0]["id"], "github-research")
            self.assertEqual(installed.json()["index"]["skills"][0]["id"], "github-research")

            disabled = client.post("/api/v2/skills/github-research/disable")
            self.assertEqual(disabled.status_code, 200)
            self.assertFalse(disabled.json()["skill"]["enabled"])

    def test_skill_api_manages_developer_import_updates_pin_rollback_and_remove(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package_v1, digest_v1 = _write_skill_package(root, package_name="github-research-0.1.0.zip")
            registry_v1 = _write_registry(root, package_v1, digest_v1)
            client = TestClient(create_app(store_root=root / ".coder", frontend_dist=root))

            install = client.post(
                "/api/v2/skills/install",
                json={"skill_id": "github-research", "registry_url": str(registry_v1)},
            )
            self.assertEqual(install.status_code, 200)

            local_root = root / "local-skill"
            local_root.mkdir()
            (local_root / "skill.json").write_text(
                json.dumps(
                    _manifest(
                        id="local-research",
                        name="Local Research",
                        publisher="local-dev",
                        connectors=[],
                    ),
                    ensure_ascii=False,
                    indent=2,
                ),
                encoding="utf-8",
            )
            (local_root / "SKILL.md").write_text("# Local Research\n\nLocal developer fallback.\n", encoding="utf-8")
            imported = client.post(
                "/api/v2/skills/developer-import",
                json={"path": str(local_root)},
            )
            self.assertEqual(imported.status_code, 200)
            self.assertEqual(imported.json()["record"]["source"], "local")
            self.assertEqual(imported.json()["record"]["trust_level"], "local")

            pinned = client.post("/api/v2/skills/github-research/pin", json={})
            self.assertEqual(pinned.status_code, 200)
            self.assertEqual(pinned.json()["skill"]["pinned_version"], "0.1.0")

            package_v2, digest_v2 = _write_skill_package(
                root,
                manifest=_manifest(version="0.2.0"),
                package_name="github-research-0.2.0.zip",
            )
            registry_v2 = _write_registry(root, package_v2, digest_v2, version="0.2.0")
            blocked_update = client.post(
                "/api/v2/skills/github-research/update",
                json={"registry_url": str(registry_v2)},
            )
            self.assertEqual(blocked_update.status_code, 409)

            unpinned = client.post("/api/v2/skills/github-research/unpin")
            self.assertEqual(unpinned.status_code, 200)
            self.assertIsNone(unpinned.json()["skill"]["pinned_version"])

            updated = client.post(
                "/api/v2/skills/github-research/update",
                json={"registry_url": str(registry_v2)},
            )
            self.assertEqual(updated.status_code, 200)
            self.assertEqual(updated.json()["record"]["manifest"]["version"], "0.2.0")

            versions = client.get("/api/v2/skills/github-research/versions")
            self.assertEqual(versions.status_code, 200)
            self.assertIn("0.1.0", {item["version"] for item in versions.json()["versions"]})

            rolled_back = client.post("/api/v2/skills/github-research/rollback", json={"version": "0.1.0"})
            self.assertEqual(rolled_back.status_code, 200)
            self.assertEqual(rolled_back.json()["skill"]["manifest"]["version"], "0.1.0")

            policy = client.post(
                "/api/v2/skills/github-research/update-policy",
                json={"update_policy": "auto_official_low_risk"},
            )
            self.assertEqual(policy.status_code, 200)

            updates = client.get("/api/v2/skills/updates", params={"registry_url": str(registry_v2)})
            self.assertEqual(updates.status_code, 200)
            self.assertTrue(updates.json()["updates"][0]["update_available"])
            self.assertTrue(updates.json()["updates"][0]["auto_update_eligible"])

            auto_updated = client.post(
                "/api/v2/skills/auto-update",
                json={"registry_url": str(registry_v2)},
            )
            self.assertEqual(auto_updated.status_code, 200)
            self.assertEqual(auto_updated.json()["updated"][0]["record"]["manifest"]["version"], "0.2.0")

            removed = client.delete("/api/v2/skills/local-research")
            self.assertEqual(removed.status_code, 200)
            self.assertTrue(removed.json()["removed"])


def _connector_operation(**overrides: object) -> dict[str, object]:
    operation: dict[str, object] = {
        "connector_id": "github_readonly",
        "operation_id": "search_repositories",
        "description": "Search repositories.",
        "input_schema": {
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
        },
        "risk_level": "low",
        "external_effect": False,
        "requires_preview": False,
        "requires_human_approval": False,
    }
    operation.update(overrides)
    return operation


def _manifest(**overrides: object) -> dict[str, object]:
    manifest: dict[str, object] = {
        "id": "github-research",
        "name": "GitHub Research",
        "version": "0.1.0",
        "description": "Search and compare open-source GitHub repositories.",
        "category": "research",
        "skill_type": "procedure",
        "risk_level": "low",
        "publisher": "coder-official",
        "allowed_authorities": ["planner", "worker", "tester", "synthesizer"],
        "requires": ["search_query"],
        "produces": ["source_collection", "synthesis_artifact", "execution_result"],
        "connectors": ["github_readonly"],
        "external_effect": False,
        "requires_preview": False,
        "requires_human_approval": False,
        "context_policy": {
            "load_mode": "on_demand",
            "max_skill_tokens": 1200,
        },
        "compatibility": {
            "coder_min_version": "0.7.0",
            "agent_graph_runtime": True,
        },
        "trigger_hints": ["github", "repository", "open source research"],
    }
    manifest.update(overrides)
    return manifest


def _write_skill_package(
    root: Path,
    *,
    manifest: dict[str, object] | None = None,
    package_name: str = "github-research.zip",
) -> tuple[Path, str]:
    package_path = root / package_name
    with zipfile.ZipFile(package_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("skill.json", json.dumps(manifest or _manifest(), ensure_ascii=False, indent=2))
        archive.writestr("SKILL.md", "# GitHub Research\n\nUse for GitHub source research.\n")
    data = package_path.read_bytes()
    return package_path, sha256_digest(data)


def _write_registry(
    root: Path,
    package_path: Path,
    digest: str,
    *,
    version: str = "0.1.0",
    signature: str | None = "sig-placeholder",
    connector_operations: list[dict[str, object]] | None = None,
) -> Path:
    registry_path = root / "registry.json"
    registry = {
        "registry_version": "0.1",
        "generated_at": "2026-06-20T00:00:00Z",
        "skills": [
            {
                "id": "github-research",
                "name": "GitHub Research",
                "version": version,
                "description": "Search and compare open-source GitHub repositories.",
                "category": "research",
                "publisher": "coder-official",
                "package_url": package_path.name,
                "manifest_url": None,
                "sha256": digest,
                "signature": signature,
                "risk_level": "low",
                "external_effect": False,
                "requires_connectors": ["github_readonly"],
                "connector_operations": connector_operations or [],
            }
        ],
    }
    registry_path.write_text(json.dumps(registry, ensure_ascii=False, indent=2), encoding="utf-8")
    return registry_path


if __name__ == "__main__":
    unittest.main()
