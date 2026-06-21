from __future__ import annotations

from pathlib import Path


class CacheStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.cache_dir = self.root / "cache"

    def namespace(self, name: str) -> Path:
        safe = "".join(char for char in name if char.isalnum() or char in {"-", "_"})
        if not safe:
            raise KeyError(name)
        path = self.cache_dir / safe
        path.mkdir(parents=True, exist_ok=True)
        return path
