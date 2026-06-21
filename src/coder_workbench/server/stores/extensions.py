from __future__ import annotations

from pathlib import Path


class ExtensionStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.extensions_dir = self.root / "extensions"

    @property
    def plugins_dir(self) -> Path:
        path = self.extensions_dir / "plugins"
        path.mkdir(parents=True, exist_ok=True)
        return path

    @property
    def skills_dir(self) -> Path:
        path = self.extensions_dir / "skills"
        path.mkdir(parents=True, exist_ok=True)
        return path
