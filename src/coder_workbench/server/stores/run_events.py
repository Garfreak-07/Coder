from __future__ import annotations

import json
from pathlib import Path

from coder_workbench.runtime import RunEvent
from coder_workbench.server.stores._paths import run_dir


class RunEventStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)

    def append(self, run_id: str, event: RunEvent) -> None:
        path = run_dir(self.root, run_id) / "events.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(event.model_dump(mode="json"), ensure_ascii=False) + "\n")

    def read(self, run_id: str) -> list[RunEvent]:
        path = run_dir(self.root, run_id) / "events.jsonl"
        if not path.exists():
            return []
        events: list[RunEvent] = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                events.append(RunEvent.model_validate(json.loads(line)))
        return events
