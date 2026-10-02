from __future__ import annotations

import argparse
import json
from pathlib import Path

from .cas import pack_directory, verify_store, unpack_store


def analyze(path: Path) -> dict:
    files = [p for p in path.rglob("*") if p.is_file()]
    total = sum(p.stat().st_size for p in files)
    return {"path": str(path), "files": len(files), "logical_bytes": total, "modified": False}


def main() -> None:
    parser = argparse.ArgumentParser(prog="playsparse-lab")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("analyze")
    p.add_argument("source", type=Path)

    p = sub.add_parser("pack")
    p.add_argument("source", type=Path)
    p.add_argument("store", type=Path)
    p.add_argument("--level", type=int, default=3)

    p = sub.add_parser("verify")
    p.add_argument("store", type=Path)

    p = sub.add_parser("unpack")
    p.add_argument("store", type=Path)
    p.add_argument("destination", type=Path)

    args = parser.parse_args()
    if args.command == "analyze":
        result = analyze(args.source)
    elif args.command == "pack":
        result = pack_directory(args.source, args.store, level=args.level)
    elif args.command == "verify":
        result = verify_store(args.store)
    elif args.command == "unpack":
        unpack_store(args.store, args.destination)
        result = {"ok": True, "destination": str(args.destination)}
    else:
        raise AssertionError(args.command)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
