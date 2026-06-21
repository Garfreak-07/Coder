from __future__ import annotations

from pathlib import Path

from .artifacts import RiskFile, RiskMapArtifact


DEFAULT_RISK_PATHS = {
    ".env": "local secrets",
    ".env.example": "secret-adjacent configuration template",
    ".git": "repository internals",
    ".coder": "local Coder state",
    ".coder_history": "local run history",
    ".codex": "local Codex state",
    "node_modules": "generated dependencies",
    ".venv": "local virtual environment",
    "venv": "local virtual environment",
}


def build_risk_map(repo_root: str | Path, *, include_missing: bool = True) -> RiskMapArtifact:
    root = Path(repo_root).resolve()
    items: list[RiskFile] = []
    for path, reason in sorted(DEFAULT_RISK_PATHS.items()):
        if include_missing or (root / path).exists():
            items.append(RiskFile(path=path, risk_level="high", reason=reason))
    return RiskMapArtifact(
        risk_files=[item.path for item in items],
        items=items,
        confidence="high",
    )


def is_risk_path(path: str, risk_map: RiskMapArtifact | dict | None = None) -> bool:
    normalized = Path(path).as_posix().strip("/")
    if not normalized:
        return False
    if isinstance(risk_map, RiskMapArtifact):
        risk_files = risk_map.risk_files
    elif isinstance(risk_map, dict):
        risk_files = [str(item) for item in risk_map.get("risk_files", [])]
    else:
        risk_files = list(DEFAULT_RISK_PATHS)
    parts = normalized.split("/")
    for risk_path in risk_files:
        risk = Path(risk_path).as_posix().strip("/")
        if not risk:
            continue
        if normalized == risk or normalized.startswith(f"{risk}/") or parts[0] == risk:
            return True
    return False
