from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from pprint import pprint

from dotenv import load_dotenv

from coder_workbench.core import load_workflow
from coder_workbench.module_map import build_module_map, write_module_map
from coder_workbench.project_index import annotate_recommendations, recommend_modules
from coder_workbench.runtime import run_workflow
from coder_workbench.tools.filesystem import resolve_existing_dir, summarize_project


def main() -> None:
    load_dotenv()
    parser = argparse.ArgumentParser(description="Coder JSON workflow runner")
    parser.add_argument("--repo", required=True, help="Target local project path.")
    parser.add_argument("--request", default="Inspect the selected scope safely.", help="Coding request.")
    parser.add_argument("--scope", action="append", default=[], help="Repo-relative scope. Can be repeated.")
    parser.add_argument("--provider", help="Override CODER_PROVIDER for this run.")
    parser.add_argument("--model", help="Override CODER_MODEL for this run.")
    parser.add_argument("--base-url", help="Override CODER_BASE_URL for this run.")
    parser.add_argument("--map-only", action="store_true", help="Generate a clickable module map and exit.")
    parser.add_argument("--query", default="", help="User goal used to highlight recommended modules in the module map.")
    parser.add_argument("--output-dir", default="outputs", help="Output directory for generated artifacts.")
    parser.add_argument("--workflow", help="Run a JSON workflow spec.")
    parser.add_argument("--approve", action="store_true", help="Approve human gates for this run.")
    args = parser.parse_args()

    if args.provider:
        os.environ["CODER_PROVIDER"] = args.provider
    if args.model:
        os.environ["CODER_MODEL"] = args.model
    if args.base_url:
        os.environ["CODER_BASE_URL"] = args.base_url

    repo_root = resolve_existing_dir(args.repo)

    if args.map_only:
        files = summarize_project(repo_root, args.scope, max_files=800)
        modules = build_module_map(files)
        recommendations = recommend_modules(args.query, modules, files) if args.query else []
        modules = annotate_recommendations(modules, recommendations) if recommendations else modules
        json_path, html_path = write_module_map(Path(args.output_dir), modules, args.query, recommendations, str(repo_root))
        print(f"Module map JSON: {json_path}")
        print(f"Module map HTML: {html_path}")
        return

    if not args.workflow:
        parser.error("--workflow is required unless --map-only is used")

    workflow = load_workflow(args.workflow)
    result = run_workflow(
        workflow=workflow,
        request=args.request,
        repo_root=str(repo_root),
        initial_data={
            "request": args.request,
            "approved": args.approve,
            "preapprove_all": args.approve,
            "scopes": args.scope,
        },
    )
    print("\n=== STATUS ===")
    pprint(
        {
            "status": result.status,
            "agent_calls": result.agent_calls,
            "tool_calls": result.tool_calls,
            "estimated_tokens_used": result.estimated_tokens_used,
        }
    )
    print("\n=== SUMMARIES ===")
    print(json.dumps(result.summaries, ensure_ascii=False, indent=2))
    print("\n=== EVENTS ===")
    for event in result.events:
        print(f"{event.created_at.isoformat()} {event.type} {event.node_id or '-'} {event.message}")


if __name__ == "__main__":
    main()
