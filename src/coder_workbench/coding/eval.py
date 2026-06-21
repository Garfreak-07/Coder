from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .artifacts import CodingEvaluationReportArtifact, CodingTaskSpec


def load_coding_task(path: str | Path) -> CodingTaskSpec:
    return CodingTaskSpec.model_validate(json.loads(Path(path).read_text(encoding="utf-8")))


def evaluate_fake_coding_task(task: CodingTaskSpec | dict[str, Any]) -> CodingEvaluationReportArtifact:
    spec = CodingTaskSpec.model_validate(task)
    tests_pass = 1.0 if spec.acceptance.tests_pass else 0.0
    forbidden_change = 0.0 if spec.acceptance.no_forbidden_files_changed else 1.0
    task_pass = 1.0 if tests_pass and forbidden_change == 0.0 else 0.0
    return CodingEvaluationReportArtifact(
        task_id=spec.task_id,
        task_pass_rate=task_pass,
        patch_created_rate=1.0 if spec.expected_changed_files else 0.0,
        patch_apply_rate=1.0,
        tests_pass_rate=tests_pass,
        forbidden_change_rate=forbidden_change,
        planner_rounds=1,
        worker_interrupt_rate=0.0,
        human_prompt_rate=0.0,
        estimated_tokens=0,
        repair_count=0,
        details={
            "repo_fixture": spec.repo_fixture,
            "check_commands": spec.check_commands,
            "expected_changed_files": spec.expected_changed_files,
        },
    )


def build_run_coding_eval(data: dict[str, Any], events: list[Any] | None = None) -> dict[str, Any]:
    graph_run_cache = data.get("graph_run_cache") if isinstance(data, dict) else None
    if not isinstance(graph_run_cache, dict):
        return CodingEvaluationReportArtifact().model_dump(mode="json")

    execution_cache = graph_run_cache.get("execution_cache") if isinstance(graph_run_cache.get("execution_cache"), dict) else {}
    test_cache = graph_run_cache.get("test_cache") if isinstance(graph_run_cache.get("test_cache"), dict) else {}
    hidden_effects = graph_run_cache.get("hidden_effects") if isinstance(graph_run_cache.get("hidden_effects"), list) else []
    token_ledger = data.get("token_ledger") if isinstance(data.get("token_ledger"), list) else []
    rounds = data.get("rounds") if isinstance(data.get("rounds"), list) else []
    debug_findings = data.get("debug_findings") if isinstance(data.get("debug_findings"), list) else []

    total_items = max(1, len(execution_cache))
    patch_created = sum(1 for effect in hidden_effects if effect.get("effect_type") == "modify_files" and effect.get("status") == "patch_preview_created")
    check_effects = [effect for effect in hidden_effects if effect.get("effect_type") == "optional_check_command"]
    checks_passed = sum(1 for effect in check_effects if effect.get("status") == "completed" and effect.get("passed", True) is not False)
    tests = [record for records in test_cache.values() if isinstance(records, list) for record in records if isinstance(record, dict)]
    test_pass_count = sum(1 for record in tests if record.get("status") == "pass")
    interrupts = graph_run_cache.get("interrupts") if isinstance(graph_run_cache.get("interrupts"), list) else []
    human_prompts = [event for event in (events or []) if getattr(event, "type", "") == "planner.human_prompt"]
    estimated_tokens = 0
    for entry in token_ledger:
        if isinstance(entry, dict):
            estimated_tokens += int(entry.get("estimated_input_tokens") or 0)
            estimated_tokens += int(entry.get("estimated_output_tokens") or 0)
    repair_count = len([event for event in (events or []) if "repair" in getattr(event, "type", "")])

    report = CodingEvaluationReportArtifact(
        task_pass_rate=1.0 if _run_succeeded(execution_cache, tests) else 0.0,
        patch_created_rate=patch_created / total_items,
        patch_apply_rate=1.0 if patch_created else 0.0,
        tests_pass_rate=(test_pass_count / len(tests)) if tests else (1.0 if not check_effects else checks_passed / max(1, len(check_effects))),
        forbidden_change_rate=0.0,
        planner_rounds=len(rounds) or int(graph_run_cache.get("round") or 0),
        worker_interrupt_rate=len(interrupts) / total_items,
        human_prompt_rate=len(human_prompts) / max(1, len(rounds) or 1),
        estimated_tokens=estimated_tokens,
        repair_count=repair_count,
        details={
            "patch_created": bool(patch_created),
            "sandbox_tests_passed": bool(check_effects and checks_passed == len(check_effects)),
            "sandbox_checks_passed": bool(check_effects and checks_passed == len(check_effects)),
            "debug_findings": len(debug_findings),
            "forbidden_change": False,
            "worker_interrupts": len(interrupts),
            "human_prompts": len(human_prompts),
        },
    )
    return report.model_dump(mode="json")


def _run_succeeded(execution_cache: dict[str, Any], tests: list[dict[str, Any]]) -> bool:
    if any(record.get("status") in {"blocked", "failed"} for record in execution_cache.values() if isinstance(record, dict)):
        return False
    if any(record.get("status") in {"fail", "blocked"} for record in tests):
        return False
    return True
