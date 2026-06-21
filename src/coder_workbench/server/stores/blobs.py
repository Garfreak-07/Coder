from __future__ import annotations

import hashlib
from pathlib import Path


class BlobStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.blobs_dir = self.root / "blobs"

    def write_text(self, content: str) -> str:
        digest = hashlib.sha256(content.encode("utf-8")).hexdigest()
        blob_id = f"sha256:{digest}"
        path = self._path(blob_id)
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            path.write_text(content, encoding="utf-8")
        return blob_id

    def read_text(self, blob_id: str) -> str:
        path = self._path(blob_id)
        if not path.exists():
            raise KeyError(blob_id)
        return path.read_text(encoding="utf-8")

    def _path(self, blob_id: str) -> Path:
        digest = _safe_digest(blob_id)
        return self.blobs_dir / "sha256" / digest[:2] / f"sha256-{digest}"


def _safe_digest(blob_id: str) -> str:
    prefix = "sha256:"
    if not blob_id.startswith(prefix):
        raise KeyError(blob_id)
    digest = blob_id[len(prefix):]
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise KeyError(blob_id)
    return digest
