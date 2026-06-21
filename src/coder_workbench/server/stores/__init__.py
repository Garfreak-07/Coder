from __future__ import annotations

from pathlib import Path

from .artifacts import ArtifactStore
from .blobs import BlobStore
from .cache import CacheStore
from .extensions import ExtensionStore
from .ledgers import LedgerStore
from .run_events import RunEventStore


class PartitionedRunStores:
    """Logical store facade over the existing .coder file layout."""

    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self.events = RunEventStore(self.root)
        self.artifacts = ArtifactStore(self.root)
        self.blobs = BlobStore(self.root)
        self.ledgers = LedgerStore(self.root)
        self.extensions = ExtensionStore(self.root)
        self.cache = CacheStore(self.root)


__all__ = [
    "ArtifactStore",
    "BlobStore",
    "CacheStore",
    "ExtensionStore",
    "LedgerStore",
    "PartitionedRunStores",
    "RunEventStore",
]
