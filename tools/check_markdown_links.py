#!/usr/bin/env python3
"""リポジトリ内Markdownの相対リンクが存在するか検査する。"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[1]
LINK = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
SKIP_DIRS = {".git", ".venv", "node_modules", "third_party", "local-notes"}


def markdown_files():
    for path in ROOT.rglob("*.md"):
        if not any(part in SKIP_DIRS for part in path.relative_to(ROOT).parts):
            yield path


def main() -> int:
    broken: list[tuple[Path, str]] = []
    for source in markdown_files():
        text = source.read_text(encoding="utf-8")
        for raw in LINK.findall(text):
            target = raw.strip().strip("<>")
            if target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            path_text = unquote(target.split("#", 1)[0])
            if path_text and not (source.parent / path_text).exists():
                broken.append((source.relative_to(ROOT), raw))
    for source, target in broken:
        print(f"リンク切れ: {source}: {target}", file=sys.stderr)
    if broken:
        return 1
    print("Markdown相対リンク: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
