from __future__ import annotations

import argparse
from pathlib import Path

import uvicorn

from .app import create_app


def main() -> None:
    parser = argparse.ArgumentParser(description="Run the Coder v2 runtime API")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8876)
    parser.add_argument("--store-root", default=".coder_v2")
    parser.add_argument(
        "--frontend-dist",
        default=None,
        help="Optional built frontend directory to serve. Defaults to frontend/dist when it exists.",
    )
    args = parser.parse_args()

    app = create_app(Path(args.store_root), frontend_dist=args.frontend_dist)
    uvicorn.run(app, host=args.host, port=args.port)


if __name__ == "__main__":
    main()
