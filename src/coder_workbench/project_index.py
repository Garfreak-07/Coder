from __future__ import annotations

import re
from collections import Counter, defaultdict
from typing import Any

FileSummary = dict[str, Any]
ModuleInfo = dict[str, Any]


STOPWORDS = {
    "the",
    "and",
    "for",
    "with",
    "from",
    "this",
    "that",
    "into",
    "your",
    "my",
    "our",
    "change",
    "improve",
    "implement",
    "feature",
    "我的",
    "这个",
    "那个",
    "修改",
    "优化",
    "实现",
    "功能",
}


def build_project_modules(files: list[FileSummary]) -> list[ModuleInfo]:
    """Group project files into small repo-relative modules."""

    grouped: dict[str, list[FileSummary]] = defaultdict(list)
    for file in files:
        path = str(file.get("path") or "")
        if not path:
            continue
        parts = path.split("/")
        module_path = parts[0] if len(parts) == 1 else "/".join(parts[:2])
        grouped[module_path].append(file)

    modules: list[ModuleInfo] = []
    for index, (path, module_files) in enumerate(sorted(grouped.items())):
        size = sum(int(file.get("size_bytes") or 0) for file in module_files)
        modules.append(
            {
                "id": f"module_{index + 1}",
                "name": path,
                "path": path,
                "file_count": len(module_files),
                "size_bytes": size,
                "importance": _module_importance(path, module_files),
                "risk": _module_risk(path),
                "reason": _module_reason(path, module_files),
            }
        )
    return modules


def recommend_modules(
    query: str,
    modules: list[ModuleInfo],
    files: list[FileSummary],
    limit: int = 8,
) -> list[dict]:
    """Recommend modules with zero-token lexical scoring."""

    terms = _query_terms(query)
    if not terms:
        return []

    module_files = _group_files_by_module(files, modules)
    scored: list[dict] = []
    for module in modules:
        score, hits = _score_module(module, module_files[module["path"]], terms)
        if score <= 0:
            continue
        scored.append(
            {
                "module_id": module["id"],
                "path": module["path"],
                "score": score,
                "hits": hits,
            }
        )

    return sorted(scored, key=lambda item: item["score"], reverse=True)[:limit]


def annotate_recommendations(
    modules: list[ModuleInfo],
    recommendations: list[dict],
) -> list[ModuleInfo]:
    by_id = {item["module_id"]: item for item in recommendations}
    annotated: list[ModuleInfo] = []
    for module in modules:
        updated = dict(module)
        recommendation = by_id.get(module["id"])
        if recommendation:
            updated["recommended"] = True
            updated["match_score"] = recommendation["score"]
            updated["match_hits"] = recommendation["hits"]
        else:
            updated["recommended"] = False
            updated["match_score"] = 0
            updated["match_hits"] = []
        annotated.append(updated)  # type: ignore[arg-type]

    return sorted(
        annotated,
        key=lambda item: (not item.get("recommended", False), -int(item.get("match_score", 0)), item["path"]),
    )


def _query_terms(query: str) -> list[str]:
    terms = re.findall(r"[\w\u4e00-\u9fff]+", query.lower())
    return [term for term in terms if len(term) > 1 and term not in STOPWORDS]


def _group_files_by_module(
    files: list[FileSummary],
    modules: list[ModuleInfo],
) -> dict[str, list[FileSummary]]:
    grouped: dict[str, list[FileSummary]] = defaultdict(list)
    module_paths = [module["path"] for module in modules]
    for file in files:
        for module_path in module_paths:
            if file["path"] == module_path or file["path"].startswith(f"{module_path}/"):
                grouped[module_path].append(file)
                break
    return grouped


def _score_module(
    module: ModuleInfo,
    files: list[FileSummary],
    terms: list[str],
) -> tuple[int, list[str]]:
    score = 0
    hits: Counter[str] = Counter()
    haystacks = [
        (module["path"].lower(), 6),
        (module["name"].lower(), 5),
        (module["reason"].lower(), 2),
    ]
    haystacks.extend((file["path"].lower(), 3) for file in files[:200])

    for term in terms:
        for text, weight in haystacks:
            if term in text:
                score += weight
                hits[term] += 1

    return score, list(hits.keys())[:8]


def _module_importance(path: str, files: list[FileSummary]) -> str:
    important_names = {"pyproject.toml", "package.json", "README.md", "README"}
    if path in {"src", "frontend", "tests"}:
        return "high"
    if any(str(file.get("path") or "").split("/")[-1] in important_names for file in files):
        return "high"
    if len(files) >= 10:
        return "medium"
    return "low"


def _module_risk(path: str) -> str:
    if path.startswith(".github") or path in {"pyproject.toml", "package.json"}:
        return "medium"
    if path.startswith("tests"):
        return "low"
    if path.startswith("src") or path.startswith("frontend"):
        return "medium"
    return "low"


def _module_reason(path: str, files: list[FileSummary]) -> str:
    kinds = sorted({str(file.get("kind") or "text") for file in files})
    return f"{path} contains {len(files)} project files ({', '.join(kinds[:4])})."
