from __future__ import annotations

import html
import json
from collections import defaultdict
from pathlib import Path
from typing import TypedDict

from .state import FileSummary


class ModuleInfo(TypedDict):
    id: str
    name: str
    path: str
    file_count: int
    size_bytes: int
    importance: str
    risk: str
    reason: str


HIGH_IMPORTANCE_HINTS = {
    "auth",
    "login",
    "user",
    "account",
    "payment",
    "billing",
    "order",
    "security",
    "permission",
    "store",
    "state",
    "router",
    "api",
    "database",
    "db",
    "electron",
    "main",
}

LOW_IMPORTANCE_HINTS = {
    "docs",
    "test",
    "tests",
    "__tests__",
    "examples",
    "demo",
    "assets",
    "styles",
}

HIGH_RISK_HINTS = {
    "config",
    "vite",
    "webpack",
    "electron",
    "main",
    "preload",
    "auth",
    "payment",
    "database",
    "db",
    "security",
    "store",
    "state",
}


def build_module_map(files: list[FileSummary]) -> list[ModuleInfo]:
    groups: dict[str, list[FileSummary]] = defaultdict(list)

    for item in files:
        module_path = _module_path(item["path"])
        groups[module_path].append(item)

    modules: list[ModuleInfo] = []
    for index, (module_path, module_files) in enumerate(sorted(groups.items())):
        size = sum(item["size_bytes"] for item in module_files)
        importance, risk, reason = _score_module(module_path, module_files, size)
        modules.append(
            {
                "id": f"module-{index + 1}",
                "name": Path(module_path).name or module_path,
                "path": module_path,
                "file_count": len(module_files),
                "size_bytes": size,
                "importance": importance,
                "risk": risk,
                "reason": reason,
            }
        )

    return modules


def write_module_map(outputs_dir: Path, modules: list[ModuleInfo]) -> tuple[Path, Path]:
    outputs_dir.mkdir(parents=True, exist_ok=True)
    json_path = outputs_dir / "module-map.json"
    html_path = outputs_dir / "module-map.html"

    json_path.write_text(
        json.dumps({"modules": modules}, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    html_path.write_text(_render_html(modules), encoding="utf-8")
    return json_path, html_path


def _module_path(path: str) -> str:
    parts = Path(path).parts
    if not parts:
        return "."

    if parts[0] in {"src", "app", "packages", "electron", "shared"} and len(parts) >= 2:
        return str(Path(parts[0], parts[1])).replace("\\", "/")

    return parts[0]


def _score_module(
    module_path: str,
    module_files: list[FileSummary],
    size_bytes: int,
) -> tuple[str, str, str]:
    lowered = module_path.lower()
    file_count = len(module_files)

    importance_score = 1
    risk_score = 1
    reasons: list[str] = []

    if any(hint in lowered for hint in HIGH_IMPORTANCE_HINTS):
        importance_score += 2
        reasons.append("name suggests core behavior")
    if any(hint in lowered for hint in LOW_IMPORTANCE_HINTS):
        importance_score -= 1
        reasons.append("name suggests supporting material")
    if file_count >= 20 or size_bytes >= 120_000:
        importance_score += 1
        risk_score += 1
        reasons.append("large module")
    if any(hint in lowered for hint in HIGH_RISK_HINTS):
        risk_score += 2
        reasons.append("name suggests high-impact changes")
    if any(item["path"].endswith((".config.ts", ".config.js", "package.json")) for item in module_files):
        risk_score += 2
        reasons.append("contains configuration files")

    return (
        _level(importance_score),
        _level(risk_score),
        ", ".join(reasons) or "standard source module",
    )


def _level(score: int) -> str:
    if score >= 3:
        return "high"
    if score == 2:
        return "medium"
    return "low"


def _render_html(modules: list[ModuleInfo]) -> str:
    cards = "\n".join(_module_card(module) for module in modules)
    data = html.escape(json.dumps(modules, ensure_ascii=False))
    return f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Coder Module Map</title>
  <style>
    body {{
      margin: 0;
      font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: #0f172a;
      color: #e5e7eb;
    }}
    header {{
      padding: 24px;
      border-bottom: 1px solid #1f2937;
      background: #111827;
    }}
    main {{
      display: grid;
      grid-template-columns: minmax(320px, 1fr) 360px;
      gap: 16px;
      padding: 16px;
    }}
    .grid {{
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
      gap: 12px;
    }}
    .card {{
      border: 1px solid #334155;
      border-radius: 14px;
      padding: 14px;
      background: #111827;
      cursor: pointer;
      transition: 120ms ease;
    }}
    .card:hover, .card.selected {{
      border-color: #38bdf8;
      transform: translateY(-1px);
    }}
    .path {{
      color: #93c5fd;
      font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
      font-size: 13px;
      word-break: break-all;
    }}
    .meta {{
      margin-top: 10px;
      color: #cbd5e1;
      font-size: 13px;
    }}
    .badge {{
      display: inline-flex;
      align-items: center;
      border-radius: 999px;
      padding: 3px 8px;
      margin-right: 6px;
      margin-top: 8px;
      font-size: 12px;
      border: 1px solid #475569;
    }}
    .high {{ background: #7f1d1d; color: #fecaca; }}
    .medium {{ background: #713f12; color: #fde68a; }}
    .low {{ background: #064e3b; color: #bbf7d0; }}
    aside {{
      position: sticky;
      top: 16px;
      height: calc(100vh - 32px);
      border: 1px solid #334155;
      border-radius: 16px;
      padding: 16px;
      background: #111827;
    }}
    button {{
      width: 100%;
      margin-top: 10px;
      border: 0;
      border-radius: 10px;
      padding: 10px 12px;
      background: #2563eb;
      color: white;
      cursor: pointer;
    }}
    button.secondary {{ background: #334155; }}
    textarea {{
      width: 100%;
      min-height: 88px;
      margin-top: 10px;
      border-radius: 10px;
      border: 1px solid #334155;
      background: #020617;
      color: #e5e7eb;
      padding: 10px;
      resize: vertical;
    }}
    .hint {{ color: #94a3b8; font-size: 13px; line-height: 1.5; }}
  </style>
</head>
<body>
  <header>
    <h1>Coder Module Map</h1>
    <p class="hint">Click a module to inspect it. High-risk modules should ask for confirmation before broad edits.</p>
  </header>
  <main>
    <section class="grid">{cards}</section>
    <aside>
      <h2 id="detail-title">Select a module</h2>
      <p id="detail-path" class="path"></p>
      <p id="detail-meta" class="meta"></p>
      <p id="detail-reason" class="hint"></p>
      <textarea id="command" readonly placeholder="A command will appear after selecting a module."></textarea>
      <button onclick="copyCommand()">Copy command</button>
      <button class="secondary" disabled>Undo / Redo buttons arrive after patch execution is enabled</button>
      <p class="hint">Current rule: after the user selects a module, the workflow can run up to 3 loops unless it needs to leave the selected scope or touches high-risk areas.</p>
    </aside>
  </main>
  <script type="application/json" id="module-data">{data}</script>
  <script>
    const modules = JSON.parse(document.getElementById("module-data").textContent);
    let selected = null;

    function selectModule(id) {{
      selected = modules.find((module) => module.id === id);
      document.querySelectorAll(".card").forEach((card) => card.classList.remove("selected"));
      document.querySelector(`[data-id="${{id}}"]`).classList.add("selected");
      document.getElementById("detail-title").textContent = selected.name;
      document.getElementById("detail-path").textContent = selected.path;
      document.getElementById("detail-meta").textContent =
        `${{selected.file_count}} files · ${{selected.size_bytes}} bytes · importance: ${{selected.importance}} · risk: ${{selected.risk}}`;
      document.getElementById("detail-reason").textContent = selected.reason;
      document.getElementById("command").value =
        `langgraph-coder --repo "YOUR_PROJECT_PATH" --scope "${{selected.path}}" --max-iterations 3`;
    }}

    function copyCommand() {{
      const value = document.getElementById("command").value;
      if (!value) return;
      navigator.clipboard.writeText(value);
    }}
  </script>
</body>
</html>"""


def _module_card(module: ModuleInfo) -> str:
    return f"""
      <article class="card" data-id="{html.escape(module['id'])}" onclick="selectModule('{html.escape(module['id'])}')">
        <h3>{html.escape(module['name'])}</h3>
        <div class="path">{html.escape(module['path'])}</div>
        <div>
          <span class="badge {html.escape(module['importance'])}">importance: {html.escape(module['importance'])}</span>
          <span class="badge {html.escape(module['risk'])}">risk: {html.escape(module['risk'])}</span>
        </div>
        <div class="meta">{module['file_count']} files · {module['size_bytes']} bytes</div>
      </article>
    """
