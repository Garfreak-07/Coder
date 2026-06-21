from __future__ import annotations

import os
import tempfile
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.skills.registry_client import RegistryClient
from coder_workbench.skills.schema import ConnectorOperation, InstalledSkillRecord, RemoteSkillEntry, SkillTrustLevel
from coder_workbench.skills.store import InstalledSkillStore
from coder_workbench.skills.verifier import (
    SkillPackageVerification,
    SkillVerificationError,
    locate_skill_root,
    safe_extract_zip,
    sha256_digest,
    sha256_directory,
    verify_extracted_package,
    verify_sha256,
)


class SkillInstallResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    record: InstalledSkillRecord
    verification: SkillPackageVerification
    warnings: list[str] = Field(default_factory=list)


class SkillAutoUpdateResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    updated: list[SkillInstallResult] = Field(default_factory=list)
    skipped: list[dict[str, str]] = Field(default_factory=list)


class SkillInstaller:
    def __init__(
        self,
        *,
        client: RegistryClient,
        store: InstalledSkillStore,
        trusted_signature_keys: dict[str, str] | None = None,
        require_verified_signatures: bool | None = None,
    ) -> None:
        self.client = client
        self.store = store
        self.trusted_signature_keys = (
            trusted_signature_keys
            if trusted_signature_keys is not None
            else _trusted_signature_keys_from_env()
        )
        self.require_verified_signatures = (
            _require_verified_signatures_from_env()
            if require_verified_signatures is None
            else require_verified_signatures
        )

    def install(self, skill_id: str, *, allow_untrusted: bool = False, force: bool = False) -> SkillInstallResult:
        index = self.client.fetch_index()
        entry = index.get(skill_id)
        return self.install_entry(entry, allow_untrusted=allow_untrusted, force=force)

    def install_entry(
        self,
        entry: RemoteSkillEntry,
        *,
        allow_untrusted: bool = False,
        force: bool = False,
    ) -> SkillInstallResult:
        existing = _get_existing(self.store, entry.id)
        if existing is not None and not force:
            _assert_update_allowed(existing, entry)
        warnings = _install_policy_warnings(entry, allow_untrusted=allow_untrusted)
        package_bytes = self.client.fetch_package(entry)
        package_sha256 = verify_sha256(package_bytes, entry.sha256)
        with tempfile.TemporaryDirectory() as tmp:
            extracted = safe_extract_zip(package_bytes, Path(tmp) / "package")
            skill_root = locate_skill_root(extracted)
            verification = verify_extracted_package(
                skill_root,
                package_sha256=package_sha256,
                signature=entry.signature,
                trusted_signature_keys=self.trusted_signature_keys,
                require_verified_signature=self.require_verified_signatures,
            )
            _assert_manifest_matches_registry(entry, verification)
            if existing is not None:
                _assert_connector_operation_update_allowed(existing, verification, force=force)
            record = self.store.install_from_directory(
                skill_root,
                manifest=verification.manifest,
                package_sha256=package_sha256,
                trust_level=entry.trust_level,
                source="remote",
                source_url=entry.package_url,
                enabled=True,
                clear_pin=force,
            )
        return SkillInstallResult(
            record=record,
            verification=verification,
            warnings=[*warnings, *_signature_policy_warnings(entry, verification), *verification.warnings],
        )

    def import_local(
        self,
        package_path: str | Path,
        *,
        trust_level: SkillTrustLevel = "local",
        enabled: bool = True,
        allow_untrusted: bool = False,
    ) -> SkillInstallResult:
        path = Path(package_path)
        if not path.exists():
            raise SkillVerificationError(f"local skill path does not exist: {path}")
        if trust_level == "untrusted" and not allow_untrusted:
            raise SkillVerificationError("untrusted local skills require advanced mode")
        with tempfile.TemporaryDirectory() as tmp:
            if path.is_dir():
                skill_root = locate_skill_root(path)
                package_sha256 = sha256_directory(skill_root)
            else:
                package_bytes = path.read_bytes()
                package_sha256 = sha256_digest(package_bytes)
                extracted = safe_extract_zip(package_bytes, Path(tmp) / "package")
                skill_root = locate_skill_root(extracted)
            verification = verify_extracted_package(
                skill_root,
                package_sha256=package_sha256,
                signature=None,
            )
            if trust_level == "untrusted" and verification.manifest.risk_level == "high":
                raise SkillVerificationError("high-risk untrusted skills cannot be installed")
            record = self.store.install_from_directory(
                skill_root,
                manifest=verification.manifest,
                package_sha256=package_sha256,
                trust_level=trust_level,
                source="local",
                source_url=str(path),
                enabled=enabled,
            )
        return SkillInstallResult(record=record, verification=verification, warnings=verification.warnings)

    def auto_update(self, *, allow_untrusted: bool = False) -> SkillAutoUpdateResult:
        index = self.client.fetch_index()
        entries = {entry.id: entry for entry in index.skills}
        updated: list[SkillInstallResult] = []
        skipped: list[dict[str, str]] = []
        for record in self.store.list_installed():
            entry = entries.get(record.id)
            if entry is None:
                skipped.append({"skill_id": record.id, "reason": "not in registry"})
                continue
            if not is_auto_update_allowed(record, entry):
                skipped.append({"skill_id": record.id, "reason": "not eligible"})
                continue
            updated.append(self.install_entry(entry, allow_untrusted=allow_untrusted))
        return SkillAutoUpdateResult(updated=updated, skipped=skipped)


def _install_policy_warnings(entry: RemoteSkillEntry, *, allow_untrusted: bool) -> list[str]:
    if entry.trust_level in {"local", "untrusted"} and not allow_untrusted:
        raise SkillVerificationError("local or untrusted skills require developer import or advanced mode")
    if entry.trust_level == "untrusted" and entry.risk_level == "high":
        raise SkillVerificationError("high-risk untrusted skills cannot be installed")
    warnings: list[str] = []
    if entry.trust_level == "community":
        warnings.append("community skill installed with warning")
    if entry.external_effect:
        warnings.append("skill declares external effects and requires runtime preview before use")
    if entry.signature and entry.trust_level not in {"official", "verified"}:
        warnings.append("signature is present but publisher trust is not official or verified")
    return warnings


def _signature_policy_warnings(entry: RemoteSkillEntry, verification: SkillPackageVerification) -> list[str]:
    if entry.trust_level not in {"official", "verified"}:
        return []
    if verification.signature_status == "verified":
        return []
    if verification.signature_status == "missing":
        return ["official or verified skill does not declare a package signature"]
    return ["official or verified skill signature is present but was not verified"]


def _assert_manifest_matches_registry(entry: RemoteSkillEntry, verification: SkillPackageVerification) -> None:
    manifest = verification.manifest
    if manifest.id != entry.id:
        raise SkillVerificationError(f"manifest id {manifest.id!r} does not match registry id {entry.id!r}")
    if manifest.version != entry.version:
        raise SkillVerificationError(
            f"manifest version {manifest.version!r} does not match registry version {entry.version!r}"
        )
    if manifest.external_effect != entry.external_effect:
        raise SkillVerificationError("manifest external_effect does not match registry metadata")
    if manifest.risk_level != entry.risk_level:
        raise SkillVerificationError("manifest risk_level does not match registry metadata")
    if entry.connector_operations:
        _assert_connector_operations_match(
            entry.connector_operations,
            manifest.connector_operations,
            "manifest connector_operations do not match registry metadata",
        )


def is_auto_update_allowed(record: InstalledSkillRecord, entry: RemoteSkillEntry) -> bool:
    return skill_auto_update_block_reason(record, entry) is None


def skill_auto_update_block_reason(record: InstalledSkillRecord, entry: RemoteSkillEntry) -> str | None:
    if entry.version == record.manifest.version and entry.sha256 == record.package_sha256:
        return "current"
    if record.pinned_version:
        return "pinned"
    if record.update_policy != "auto_official_low_risk":
        return "manual update policy"
    if _connector_operation_locks_changed(record, entry):
        return "connector operation lock changed"
    if entry.trust_level != "official" or record.trust_level != "official":
        return "not official"
    if entry.risk_level != "low" or record.manifest.risk_level != "low":
        return "not low risk"
    if entry.external_effect or record.manifest.external_effect:
        return "external effect"
    return None


def _get_existing(store: InstalledSkillStore, skill_id: str) -> InstalledSkillRecord | None:
    try:
        return store.get_skill(skill_id)
    except KeyError:
        return None


def _assert_connector_operation_update_allowed(
    existing: InstalledSkillRecord,
    verification: SkillPackageVerification,
    *,
    force: bool,
) -> None:
    if force:
        return
    current = _connector_operation_locks(
        existing.manifest.connector_operations,
        package_sha256=existing.package_sha256,
    )
    updated = _connector_operation_locks(
        verification.manifest.connector_operations,
        package_sha256=verification.sha256,
    )
    if current != updated and (current or updated):
        raise SkillVerificationError(
            "connector operation locks changed; force update requires Planner-owned re-approval"
        )


def _assert_connector_operations_match(
    registry_operations: list[ConnectorOperation],
    manifest_operations: list[ConnectorOperation],
    message: str,
) -> None:
    registry_locks = _connector_operation_descriptor_locks(registry_operations)
    manifest_locks = _connector_operation_descriptor_locks(manifest_operations)
    if registry_locks != manifest_locks:
        raise SkillVerificationError(message)


def _connector_operation_locks_changed(record: InstalledSkillRecord, entry: RemoteSkillEntry) -> bool:
    current = _connector_operation_locks(
        record.manifest.connector_operations,
        package_sha256=record.package_sha256,
    )
    if not entry.connector_operations:
        return bool(current) and entry.sha256 != record.package_sha256
    available = _connector_operation_locks(entry.connector_operations, package_sha256=entry.sha256)
    return current != available


def _connector_operation_locks(
    operations: list[ConnectorOperation],
    *,
    package_sha256: str,
) -> list[tuple[str, str, str, str]]:
    package_lock = _normalize_lock_digest(package_sha256)
    return [
        (*lock, package_lock)
        for lock in _connector_operation_descriptor_locks(operations)
    ]


def _connector_operation_descriptor_locks(
    operations: list[ConnectorOperation],
) -> list[tuple[str, str, str]]:
    return sorted(
        (
            operation.connector_id,
            operation.operation_id,
            _normalize_lock_digest(operation.descriptor_sha256 or ""),
        )
        for operation in operations
    )


def _normalize_lock_digest(value: str) -> str:
    return value.strip().lower().removeprefix("sha256:")


def _assert_update_allowed(existing: InstalledSkillRecord, entry: RemoteSkillEntry) -> None:
    if existing.pinned_version and entry.version != existing.pinned_version:
        raise SkillVerificationError(
            f"skill {existing.id!r} is pinned to version {existing.pinned_version!r}; unpin before updating"
        )
    if _connector_operation_locks_changed(existing, entry):
        raise SkillVerificationError(
            "connector operation locks changed; force update requires Planner-owned re-approval"
        )


def _trusted_signature_keys_from_env() -> dict[str, str]:
    raw = os.getenv("CODER_SKILL_SIGNATURE_KEYS", "").strip()
    if not raw:
        return {}
    keys: dict[str, str] = {}
    for item in raw.split(";"):
        if not item.strip() or "=" not in item:
            continue
        key_id, secret = item.split("=", 1)
        key_id = key_id.strip()
        secret = secret.strip()
        if key_id and secret:
            keys[key_id] = secret
    return keys


def _require_verified_signatures_from_env() -> bool:
    return os.getenv("CODER_SKILL_REQUIRE_SIGNATURES", "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }
