from __future__ import annotations

from pathlib import Path

from .config import load_runtime_config
from .llm import create_chat_model
from .module_map import build_module_map
from .state import CodingState
from .tools.commands import run_check
from .tools.filesystem import normalize_allowed_paths, resolve_existing_dir, summarize_project


def intake_node(state: CodingState) -> CodingState:
    repo_root = resolve_existing_dir(state["repo_root"])
    reference_roots = [
        str(resolve_existing_dir(path))
        for path in state.get("reference_roots", [])
    ]

    target_scope = state.get("target_scope", [])
    allowed_paths = state.get("allowed_paths") or target_scope

    return {
        **state,
        "repo_root": str(repo_root),
        "reference_roots": reference_roots,
        "target_scope": target_scope,
        "allowed_paths": normalize_allowed_paths(repo_root, allowed_paths) if allowed_paths else [],
        "approval_required": True,
        "approved": False,
        "iteration": state.get("iteration", 0),
        "max_iterations": state.get("max_iterations", 2),
        "status": "created",
    }


def scan_repo_node(state: CodingState) -> CodingState:
    repo_root = Path(state["repo_root"])
    target_scope = state.get("target_scope", [])
    reference_files: dict[str, list[dict]] = {}

    for reference_root in state.get("reference_roots", []):
        root = Path(reference_root)
        reference_files[str(root)] = summarize_project(root, max_files=200)

    return {
        **state,
        "repo_files": summarize_project(repo_root, target_scope, max_files=400),
        "reference_files": reference_files,
    }


def module_map_node(state: CodingState) -> CodingState:
    return {
        **state,
        "modules": build_module_map(state.get("repo_files", [])),
    }


def plan_node(state: CodingState) -> CodingState:
    config = load_runtime_config()
    if not config.has_llm_credentials:
        return {
            **state,
            "plan": _fallback_plan(state),
            "proposed_changes": [
                "No LLM credentials found. Configure CODER_PROVIDER and the matching API key to generate a concrete implementation plan.",
                "Keep execution in dry-run until a human approves specific files and changes.",
            ],
            "status": "planned",
        }

    llm = create_chat_model(config)
    response = llm.invoke(_planning_prompt(state))
    return {
        **state,
        "plan": response.content,
        "proposed_changes": _split_plan_lines(response.content),
        "status": "planned",
    }


def approval_node(state: CodingState) -> CodingState:
    if state.get("approved"):
        return {**state, "status": "approved"}

    return {
        **state,
        "review_notes": (
            "Approval required before execution. Re-run with --approve after reviewing the plan. "
            "Use --scope/--allow to constrain writable paths."
        ),
        "next_step": "blocked",
        "status": "blocked",
    }


def execute_node(state: CodingState) -> CodingState:
    # Safety-first v1: execution is deliberately dry-run. This node records what would happen.
    return {
        **state,
        "changed_files": [],
        "status": "executed",
    }


def check_node(state: CodingState) -> CodingState:
    passed, output = run_check(state.get("check_command", ""), Path(state["repo_root"]))
    return {
        **state,
        "check_passed": passed,
        "check_output": output,
        "status": "checked",
    }


def review_node(state: CodingState) -> CodingState:
    changed_files = set(state.get("changed_files", []))
    allowed = set(state.get("allowed_paths", []))
    escaped = sorted(changed_files - allowed) if allowed else []

    if escaped:
        return {
            **state,
            "risk_level": "high",
            "review_notes": f"Blocked: changed files outside allowed paths: {escaped}",
            "next_step": "blocked",
            "status": "blocked",
        }

    if not state.get("check_passed", True):
        iteration = state.get("iteration", 0) + 1
        max_iterations = state.get("max_iterations", 2)
        return {
            **state,
            "risk_level": "medium",
            "iteration": iteration,
            "review_notes": "Checks failed; retry is allowed if iteration budget remains.",
            "next_step": "retry" if iteration < max_iterations else "blocked",
            "status": "checked",
        }

    return {
        **state,
        "risk_level": "low",
        "review_notes": "Dry-run completed. No files were changed.",
        "next_step": "done",
        "status": "done",
    }


def route_after_approval(state: CodingState) -> str:
    return "execute" if state.get("approved") else "blocked"


def route_after_review(state: CodingState) -> str:
    return state.get("next_step", "blocked")


def _fallback_plan(state: CodingState) -> str:
    files = state.get("repo_files", [])[:30]
    file_list = "\n".join(f"- {item['path']} ({item['kind']})" for item in files)
    return (
        "LLM planning is disabled because no model credentials are configured.\n\n"
        f"User request:\n{state['user_request']}\n\n"
        "Visible candidate files:\n"
        f"{file_list or '- No text files found in scope.'}\n\n"
        "Recommended next step: configure CODER_PROVIDER and the matching API key, provide a narrow --scope, then review the generated plan."
    )


def _planning_prompt(state: CodingState) -> str:
    repo_files = "\n".join(
        f"- {item['path']} ({item['kind']}, {item['size_bytes']} bytes)"
        for item in state.get("repo_files", [])[:120]
    )
    references = []
    for root, files in state.get("reference_files", {}).items():
        lines = "\n".join(f"  - {item['path']} ({item['kind']})" for item in files[:80])
        references.append(f"Reference root: {root}\n{lines}")

    return f"""
You are a cautious coding workflow planner.

Goal:
{state['user_request']}

Target repo:
{state['repo_root']}

Allowed paths:
{state.get('allowed_paths', [])}

Target repo files:
{repo_files}

Detected modules:
{_module_lines(state)}

Reference projects:
{chr(10).join(references) if references else 'None'}

Produce a conservative implementation plan. Do not produce code yet.
Include:
1. files likely relevant
2. proposed small steps
3. risk notes
4. checks to run
5. questions that require human approval
"""


def _split_plan_lines(plan: str) -> list[str]:
    return [line.strip() for line in plan.splitlines() if line.strip()]


def _module_lines(state: CodingState) -> str:
    modules = state.get("modules", [])
    if not modules:
        return "None"
    return "\n".join(
        f"- {module['path']} (importance={module['importance']}, risk={module['risk']}, files={module['file_count']})"
        for module in modules[:80]
    )
