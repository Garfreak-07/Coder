from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from coder_workbench.tools.patching import apply_patch, propose_patch, rollback_patch


class PatchToolTests(unittest.TestCase):
    def test_apply_and_rollback_text_patch(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            target = repo / "src" / "example.txt"
            target.parent.mkdir()
            target.write_text("before\n", encoding="utf-8")
            context = {"repo_root": str(repo), "scopes": ["src"], "data": {"patch_approved": True}}

            patch = propose_patch(
                {"changes": [{"path": "src/example.txt", "action": "update", "content": "after\n"}]},
                context,
            )
            result = apply_patch({"patch": patch}, context)

            self.assertEqual(result["status"], "applied")
            self.assertEqual(target.read_text(encoding="utf-8"), "after\n")

            rollback = rollback_patch({"snapshot_id": result["snapshot_id"]}, context)

            self.assertEqual(rollback["status"], "rolled_back")
            self.assertEqual(target.read_text(encoding="utf-8"), "before\n")

    def test_rejects_stale_base_without_writing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            target = repo / "src" / "example.txt"
            target.parent.mkdir()
            target.write_text("before\n", encoding="utf-8")
            context = {"repo_root": str(repo), "scopes": ["src"], "data": {"patch_approved": True}}

            patch = propose_patch(
                {"changes": [{"path": "src/example.txt", "action": "update", "content": "after\n"}]},
                context,
            )
            target.write_text("concurrent edit\n", encoding="utf-8")

            result = apply_patch({"patch": patch}, context)

            self.assertEqual(result["status"], "rejected")
            self.assertEqual(result["errors"][0]["code"], "stale_base")
            self.assertEqual(target.read_text(encoding="utf-8"), "concurrent edit\n")
            self.assertFalse((repo / ".coder_history").exists())

    def test_rejects_binary_target(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            target = repo / "src" / "image.bin"
            target.parent.mkdir()
            target.write_bytes(b"\x89PNG\x00binary")
            context = {"repo_root": str(repo), "scopes": ["src"], "data": {"patch_approved": True}}

            result = apply_patch(
                {"files": [{"path": "src/image.bin", "action": "update", "content": "text"}]},
                context,
            )

            self.assertEqual(result["status"], "rejected")
            self.assertEqual(result["errors"][0]["code"], "binary_file")
            self.assertEqual(target.read_bytes(), b"\x89PNG\x00binary")

    def test_rejects_path_outside_scope(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            (repo / "src").mkdir()
            docs = repo / "docs"
            docs.mkdir()
            target = docs / "example.txt"
            target.write_text("before\n", encoding="utf-8")
            context = {"repo_root": str(repo), "scopes": ["src"], "data": {"patch_approved": True}}

            result = apply_patch(
                {"files": [{"path": "docs/example.txt", "action": "update", "content": "after\n"}]},
                context,
            )

            self.assertEqual(result["status"], "rejected")
            self.assertEqual(result["errors"][0]["code"], "invalid_target")
            self.assertEqual(target.read_text(encoding="utf-8"), "before\n")

    def test_creates_new_text_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            (repo / "src").mkdir()
            target = repo / "src" / "new.txt"
            context = {"repo_root": str(repo), "scopes": ["src"], "data": {"patch_approved": True}}

            result = apply_patch(
                {
                    "files": [
                        {
                            "path": "src/new.txt",
                            "action": "create",
                            "expected_before": None,
                            "content": "created\n",
                        }
                    ]
                },
                context,
            )

            self.assertEqual(result["status"], "applied")
            self.assertEqual(target.read_text(encoding="utf-8"), "created\n")


if __name__ == "__main__":
    unittest.main()
