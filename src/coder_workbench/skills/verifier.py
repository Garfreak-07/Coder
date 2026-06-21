from __future__ import annotations

import hashlib
import hmac
import io
import json
import re
import zipfile
from pathlib import Path, PurePosixPath
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from coder_workbench.skills.schema import SkillPackageManifest


class SkillVerificationError(ValueError):
    pass


class SkillPackageVerification(BaseModel):
    model_config = ConfigDict(extra="forbid")

    sha256: str
    manifest: SkillPackageManifest
    warnings: list[str] = Field(default_factory=list)
    signature_present: bool = False
    signature_status: Literal["missing", "verified", "unverified"] = "missing"
    signature_key_id: str | None = None
    signature_error: str | None = None


def sha256_digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_directory(root: str | Path) -> str:
    base = Path(root)
    hasher = hashlib.sha256()
    for path in sorted(item for item in base.rglob("*") if item.is_file()):
        relative = path.relative_to(base).as_posix()
        if relative == "installed.json":
            continue
        hasher.update(relative.encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(path.read_bytes())
        hasher.update(b"\0")
    return hasher.hexdigest()


def verify_sha256(data: bytes, expected: str) -> str:
    expected_digest = _normalize_sha256(expected)
    actual = sha256_digest(data)
    if actual != expected_digest:
        raise SkillVerificationError(f"checksum mismatch: expected {expected_digest}, got {actual}")
    return actual


def sign_package_sha256(package_sha256: str, *, key_id: str, secret: str) -> str:
    digest = _hmac_signature(package_sha256, secret)
    return f"hmac-sha256:{key_id}:{digest}"


def verify_package_signature(
    *,
    package_sha256: str,
    signature: str | None,
    trusted_keys: dict[str, str] | None = None,
    require_verified: bool = False,
) -> dict[str, str | None]:
    if not signature:
        if require_verified:
            raise SkillVerificationError("package signature is required")
        return {"status": "missing", "key_id": None, "error": None}

    trusted_keys = trusted_keys or {}
    parts = signature.strip().split(":")
    if len(parts) != 3 or parts[0] != "hmac-sha256":
        if require_verified:
            raise SkillVerificationError("unsupported package signature format")
        return {"status": "unverified", "key_id": None, "error": "unsupported signature format"}

    _scheme, key_id, expected = parts
    secret = trusted_keys.get(key_id)
    if not secret:
        if require_verified:
            raise SkillVerificationError(f"no trusted key configured for signature key {key_id!r}")
        return {"status": "unverified", "key_id": key_id, "error": "trusted key not configured"}

    actual = _hmac_signature(package_sha256, secret)
    if not hmac.compare_digest(actual, expected.lower()):
        raise SkillVerificationError("package signature mismatch")
    return {"status": "verified", "key_id": key_id, "error": None}


def safe_extract_zip(data: bytes, destination: str | Path) -> Path:
    dest = Path(destination)
    dest.mkdir(parents=True, exist_ok=True)
    dest_resolved = dest.resolve()
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            for member in archive.infolist():
                _validate_zip_member(member.filename, dest_resolved)
            archive.extractall(dest)
    except zipfile.BadZipFile as exc:
        raise SkillVerificationError("skill package must be a valid zip archive") from exc
    return dest


def locate_skill_root(extracted_dir: str | Path) -> Path:
    root = Path(extracted_dir)
    if (root / "skill.json").is_file():
        return root
    candidates = [path.parent for path in root.rglob("skill.json") if path.is_file()]
    if len(candidates) == 1:
        return candidates[0]
    raise SkillVerificationError("skill package must contain exactly one skill.json")


def read_skill_manifest(skill_root: str | Path) -> SkillPackageManifest:
    path = Path(skill_root) / "skill.json"
    if not path.exists():
        raise SkillVerificationError("skill package is missing skill.json")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise SkillVerificationError("skill.json must be valid JSON") from exc
    return SkillPackageManifest.model_validate(payload)


def verify_package_layout(skill_root: str | Path) -> list[str]:
    root = Path(skill_root)
    warnings: list[str] = []
    if not (root / "SKILL.md").is_file():
        raise SkillVerificationError("skill package is missing SKILL.md")
    if (root / "scripts").exists():
        warnings.append("scripts directory is present but runtime script execution is disabled")
    if (root / "hooks").exists():
        warnings.append("hooks directory is present but hook execution is disabled")
    warnings.extend(_scan_unsafe_files(root))
    return warnings


def verify_extracted_package(
    skill_root: str | Path,
    *,
    package_sha256: str,
    signature: str | None = None,
    trusted_signature_keys: dict[str, str] | None = None,
    require_verified_signature: bool = False,
) -> SkillPackageVerification:
    manifest = read_skill_manifest(skill_root)
    warnings = verify_package_layout(skill_root)
    signature_result = verify_package_signature(
        package_sha256=package_sha256,
        signature=signature,
        trusted_keys=trusted_signature_keys,
        require_verified=require_verified_signature,
    )
    if signature_result["status"] == "unverified":
        warnings.append(f"package signature could not be verified: {signature_result['error']}")
    return SkillPackageVerification(
        sha256=package_sha256,
        manifest=manifest,
        warnings=warnings,
        signature_present=signature is not None,
        signature_status=str(signature_result["status"]),
        signature_key_id=signature_result["key_id"],
        signature_error=signature_result["error"],
    )


def _normalize_sha256(value: str) -> str:
    text = value.strip().lower()
    if text.startswith("sha256:"):
        text = text.removeprefix("sha256:")
    if len(text) != 64 or any(char not in "0123456789abcdef" for char in text):
        raise SkillVerificationError("expected sha256 must be a 64-character hex digest")
    return text


def _hmac_signature(package_sha256: str, secret: str) -> str:
    digest = _normalize_sha256(package_sha256)
    return hmac.new(secret.encode("utf-8"), digest.encode("utf-8"), hashlib.sha256).hexdigest()


def _validate_zip_member(name: str, dest_resolved: Path) -> None:
    normalized = name.replace("\\", "/")
    if normalized.startswith("/"):
        raise SkillVerificationError(f"unsafe absolute package path: {name}")
    if re.match(r"^[A-Za-z]:", normalized):
        raise SkillVerificationError(f"unsafe drive-qualified package path: {name}")
    parts = PurePosixPath(normalized).parts
    if ".." in parts:
        raise SkillVerificationError(f"unsafe parent traversal in package path: {name}")
    target = (dest_resolved / Path(*parts)).resolve()
    if not target.is_relative_to(dest_resolved):
        raise SkillVerificationError(f"unsafe package path: {name}")


def _scan_unsafe_files(root: Path) -> list[str]:
    warnings: list[str] = []
    unsafe_suffixes = {
        ".bat",
        ".cmd",
        ".com",
        ".dll",
        ".exe",
        ".msi",
        ".ps1",
        ".scr",
        ".sh",
        ".so",
    }
    for path in sorted(item for item in root.rglob("*") if item.is_file()):
        relative = path.relative_to(root).as_posix()
        if path.suffix.lower() in unsafe_suffixes:
            warnings.append(f"unsafe executable-like file is present but disabled: {relative}")
    return warnings
