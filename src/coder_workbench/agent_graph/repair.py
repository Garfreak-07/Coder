from __future__ import annotations

import json
from typing import Any


def parse_json_object(value: str) -> dict[str, Any] | None:
    text = value.strip()
    if not text:
        return None
    fenced = _strip_code_fence(text)
    parsed = _loads_object(fenced)
    if parsed is not None:
        return parsed
    start = fenced.find("{")
    end = fenced.rfind("}")
    if start == -1 or end == -1 or end <= start:
        return None
    return _loads_object(fenced[start : end + 1])


def validation_error_to_compact_text(errors: list[dict[str, Any]], *, max_errors: int = 8) -> str:
    parts: list[str] = []
    for error in errors[:max_errors]:
        loc = ".".join(str(item) for item in error.get("loc", [])) or "value"
        msg = str(error.get("msg") or "invalid")
        parts.append(f"{loc}: {msg}")
    if len(errors) > max_errors:
        parts.append(f"... {len(errors) - max_errors} more errors")
    return "\n".join(parts)


def build_repair_prompt(
    *,
    expected_type: str,
    invalid_output: str,
    errors: list[dict[str, Any]],
    schema_notes: str,
) -> str:
    return "\n\n".join(
        [
            "Repair the assistant output so it is valid JSON for the requested artifact type.",
            "Return one JSON object only. Do not include markdown, prose, or code fences.",
            f"Expected artifact type: {expected_type}",
            "Validation errors:",
            validation_error_to_compact_text(errors),
            "Schema notes:",
            schema_notes,
            "Invalid output:",
            invalid_output[:6000],
        ]
    )


def _strip_code_fence(value: str) -> str:
    text = value.strip()
    if not text.startswith("```"):
        return text
    lines = text.splitlines()
    if len(lines) >= 2 and lines[-1].strip() == "```":
        return "\n".join(lines[1:-1]).strip()
    return text.strip("`").strip()


def _loads_object(value: str) -> dict[str, Any] | None:
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None
