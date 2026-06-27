from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from coder_workbench.context.repo_discovery import RepoFileDiscoveryService


class RepoFileDiscoveryServiceTests(unittest.TestCase):
    def test_discovers_files_in_temp_repo(self) -> None:
        with _repo() as root:
            (root / "src" / "app.py").write_text("print('ok')\n", encoding="utf-8")
            (root / "README.md").write_text("# Project\n", encoding="utf-8")

            files = _FallbackDiscovery(repo_root=root).list_files()

            self.assertIn("src/app.py", [item.path for item in files])
            self.assertIn("README.md", [item.path for item in files])

    def test_respects_scope_paths(self) -> None:
        with _repo() as root:
            (root / "src" / "app.py").write_text("app\n", encoding="utf-8")
            (root / "docs" / "note.md").write_text("note\n", encoding="utf-8")

            files = _FallbackDiscovery(repo_root=root, scope_paths=["src"]).list_files()

            self.assertEqual([item.path for item in files], ["src/app.py"])

    def test_ignores_git_and_coder_dirs(self) -> None:
        with _repo() as root:
            (root / ".git" / "config").write_text("ignored\n", encoding="utf-8")
            (root / ".coder" / "state.json").write_text("{}\n", encoding="utf-8")
            (root / "src" / "app.py").write_text("app\n", encoding="utf-8")

            files = _FallbackDiscovery(repo_root=root).list_files()

            paths = [item.path for item in files]
            self.assertEqual(paths, ["src/app.py"])

    def test_filters_by_extension(self) -> None:
        with _repo() as root:
            (root / "src" / "app.py").write_text("app\n", encoding="utf-8")
            (root / "docs" / "note.md").write_text("note\n", encoding="utf-8")

            files = _FallbackDiscovery(repo_root=root).list_files(extensions=["py"])

            self.assertEqual([item.path for item in files], ["src/app.py"])

    def test_query_and_max_results_are_enforced(self) -> None:
        with _repo() as root:
            for index in range(5):
                (root / "src" / f"target_{index}.py").write_text("x\n", encoding="utf-8")
            (root / "src" / "other.py").write_text("x\n", encoding="utf-8")

            files = _FallbackDiscovery(repo_root=root).list_files(query="target", max_results=2)

            self.assertEqual(len(files), 2)
            self.assertTrue(all("target" in item.path for item in files))

    def test_python_fallback_works_when_commands_unavailable(self) -> None:
        with _repo() as root:
            (root / "src" / "app.py").write_text("app\n", encoding="utf-8")

            files = _FallbackDiscovery(repo_root=root).list_files()

            self.assertEqual([item.path for item in files], ["src/app.py"])


class _FallbackDiscovery(RepoFileDiscoveryService):
    def _git_files(self) -> list[str]:
        return []

    def _rg_files(self) -> list[str]:
        return []

    def _fd_files(self) -> list[str]:
        return []


class _repo:
    def __enter__(self) -> Path:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        (root / "src").mkdir()
        (root / "docs").mkdir()
        (root / ".git").mkdir()
        (root / ".coder").mkdir()
        return root

    def __exit__(self, *_args: object) -> None:
        self.tmp.cleanup()


if __name__ == "__main__":
    unittest.main()
