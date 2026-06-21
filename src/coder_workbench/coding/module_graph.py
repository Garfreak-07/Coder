from __future__ import annotations

from pathlib import Path
from typing import Any


def build_module_graph(repo_root: str | Path, symbol_index: dict[str, Any] | None = None) -> dict[str, Any]:
    """Return a small deterministic module graph placeholder for coding context.

    The v0.8 kernel keeps this intentionally simple: SymbolIndex is the primary
    navigation artifact, and dependency edges can be expanded later without
    changing callers.
    """

    files = []
    if symbol_index:
        files = [str(item.get("path") or "") for item in symbol_index.get("files", [])]
    return {
        "artifact_type": "module_graph",
        "repo_root": str(Path(repo_root).resolve()),
        "nodes": [{"path": path} for path in files if path],
        "edges": [],
        "confidence": "low",
    }
