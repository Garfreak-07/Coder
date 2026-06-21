from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from coder_workbench.server.stores._paths import run_dir, safe_object_id


class ArtifactStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)

    def write(self, run_id: str, artifact_id: str, artifact: dict[str, Any]) -> dict[str, Any]:
        safe_id = safe_object_id(artifact_id)
        path = run_dir(self.root, run_id) / "artifacts" / f"{safe_id}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(artifact, ensure_ascii=False, indent=2), encoding="utf-8")
        return {"run_id": run_id, "artifact_id": safe_id, "path": str(path)}

    def read(self, run_id: str, artifact_id: str) -> dict[str, Any]:
        path = run_dir(self.root, run_id) / "artifacts" / f"{safe_object_id(artifact_id)}.json"
        if not path.exists():
            raise KeyError(artifact_id)
        return json.loads(path.read_text(encoding="utf-8"))
