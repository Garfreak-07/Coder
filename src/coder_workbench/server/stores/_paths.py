from __future__ import annotations

from pathlib import Path


def safe_object_id(value: str) -> str:
    safe = "".join(char for char in str(value) if char.isalnum() or char in {"-", "_"})
    if not safe or safe != value:
        raise KeyError(value)
    return safe


def run_dir(root: str | Path, run_id: str) -> Path:
    return Path(root) / "runs" / safe_object_id(run_id)
