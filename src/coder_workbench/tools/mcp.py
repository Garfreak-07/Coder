from __future__ import annotations

import hashlib
import json
from queue import Empty, Queue
import subprocess
from threading import Thread
from typing import Any


def call_mcp_tool(args: dict[str, Any], runtime_context: dict[str, Any]) -> dict[str, Any]:
    """Call one MCP tool over a short-lived stdio JSON-RPC session.

    This intentionally avoids a long-lived server registry for now. It gives the
    workflow runtime a deterministic product contract while keeping actual MCP
    process management replaceable later.
    """

    command = str(args.get("server_command") or "").strip()
    if not command:
        raise ValueError("mcp_tool requires server_command")
    tool_name = str(args.get("tool_name") or args.get("__mcp_tool") or "").strip()
    if not tool_name:
        raise ValueError("mcp_tool requires tool_name")
    cwd = str(args.get("cwd") or ".")
    approval_key = _mcp_approval_key(command, tool_name, cwd)
    if not _mcp_is_approved(approval_key, runtime_context):
        return {
            "status": "blocked",
            "blocked": True,
            "passed": False,
            "requires_approval": True,
            "approval_type": "mcp_tool",
            "approval_key": approval_key,
            "command": command,
            "cwd": cwd,
            "tool_name": tool_name,
            "message": f"Approve MCP tool before running: {tool_name}",
        }

    timeout = int(args.get("timeout_seconds", 30))
    tool_arguments = args.get("arguments") if isinstance(args.get("arguments"), dict) else {}
    process = subprocess.Popen(
        command,
        shell=True,
        text=True,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdin is not None
    assert process.stdout is not None
    try:
        _write_rpc(process, 1, "initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "coder-workbench", "version": "0.1.0"}})
        reader = _LineReader(process)
        initialize = reader.read(timeout)
        _write_notification(process, "notifications/initialized", {})
        _write_rpc(process, 2, "tools/call", {"name": tool_name, "arguments": tool_arguments})
        result = reader.read(timeout)
    finally:
        try:
            process.terminate()
        except Exception:
            pass

    stderr = process.stderr.read() if process.stderr else ""
    if "error" in result:
        return {
            "status": "failed",
            "passed": False,
            "approval_key": approval_key,
            "command": command,
            "tool_name": tool_name,
            "initialize": initialize.get("result"),
            "error": result["error"],
            "stderr": stderr[-4000:],
        }
    return {
        "status": "completed",
        "passed": True,
        "approval_key": approval_key,
        "command": command,
        "tool_name": tool_name,
        "result": result.get("result"),
        "stderr": stderr[-4000:],
    }


def _write_rpc(process: subprocess.Popen[str], request_id: int, method: str, params: dict[str, Any]) -> None:
    assert process.stdin is not None
    process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}) + "\n")
    process.stdin.flush()


def _write_notification(process: subprocess.Popen[str], method: str, params: dict[str, Any]) -> None:
    assert process.stdin is not None
    process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n")
    process.stdin.flush()


class _LineReader:
    def __init__(self, process: subprocess.Popen[str]) -> None:
        self.process = process
        self.queue: Queue[str] = Queue()
        thread = Thread(target=self._read_loop, daemon=True)
        thread.start()

    def read(self, timeout: int) -> dict[str, Any]:
        try:
            line = self.queue.get(timeout=timeout)
        except Empty:
            raise TimeoutError("Timed out waiting for MCP response") from None
        if not line:
            stderr = self.process.stderr.read() if self.process.stderr else ""
            raise RuntimeError(f"MCP server exited before response: {stderr[-1000:]}")
        payload = json.loads(line)
        if not isinstance(payload, dict):
            raise ValueError("MCP response must be a JSON object")
        return payload

    def _read_loop(self) -> None:
        assert self.process.stdout is not None
        line = self.process.stdout.readline()
        while line:
            self.queue.put(line)
            line = self.process.stdout.readline()
        self.queue.put("")


def _mcp_approval_key(command: str, tool_name: str, cwd: str) -> str:
    digest = hashlib.sha256(f"{cwd}\0{command}\0{tool_name}".encode("utf-8")).hexdigest()
    return f"mcp:{digest}"


def _mcp_is_approved(approval_key: str, runtime_context: dict[str, Any]) -> bool:
    data = runtime_context.get("data", {})
    approvals = data.get("mcp_approvals", {})
    return bool(
        data.get("preapprove_all")
        or (isinstance(approvals, dict) and approvals.get(approval_key) is True)
    )
