#!/usr/bin/env python3
"""紹介ページ（site/）の静的な検査。標準ライブラリだけで動く。

- 内部リンク（href・src・og:image）の参照先とfragment（#id）が存在する
- 外部のホストは許可リストだけ（フォント・CDN・analyticsを持ち込まない）
- スクリプト・iframe・フォームを使わない。CSSも外部を読まない
- site/ に置けるのは静的な資産だけ（JSON・DB・写真の形式を置かない）
"""

from __future__ import annotations

import re
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SITE = ROOT / "site"

ALLOWED_HOSTS = {"github.com", "ysogabe.github.io", "rustup.rs"}
ALLOWED_SUFFIXES = {".html", ".css", ".svg", ".png", ".webp", ".txt", ".xml", ".ico"}
FORBIDDEN_TAGS = {"script", "iframe", "form", "object", "embed"}
REQUIRED_META = [("name", "description"), ("property", "og:title"), ("property", "og:description"), ("property", "og:url")]
CSS_URL = re.compile(r"url\(\s*['\"]?([^'\")]+)", re.IGNORECASE)
CSS_IMPORT = re.compile(r"@import\s+(?:url\()?\s*['\"]?([^'\")\s;]+)", re.IGNORECASE)


class PageParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.ids: set[str] = set()
        self.refs: list[tuple[str, str]] = []  # (属性名, 値)
        self.tags: list[str] = []
        self.metas: set[tuple[str, str]] = set()
        self.lang = ""
        self.has_title = False
        self.img_without_alt = 0

    def handle_starttag(self, tag, attrs):
        a = {k: (v or "") for k, v in attrs}
        self.tags.append(tag)
        if tag == "html":
            self.lang = a.get("lang", "")
        if tag == "title":
            self.has_title = True
        if "id" in a:
            self.ids.add(a["id"])
        if tag == "img" and "alt" not in a:
            self.img_without_alt += 1
        if tag == "meta":
            for key in ("name", "property"):
                if key in a:
                    self.metas.add((key, a[key]))
            if a.get("property") == "og:image" and a.get("content"):
                self.refs.append(("og:image", a["content"]))
            # og:url は自身の公開URLなので、外部ホストの検査（href・src）には含めない
        for key in ("href", "src", "poster", "action"):
            if a.get(key):
                kind = f"{tag}:{key}"
                if tag == "a" and key == "href":
                    kind = "href"
                elif tag == "link" and a.get("rel", "").lower() == "canonical":
                    kind = "canonical"
                self.refs.append((kind, a[key]))


def parse(path: Path) -> PageParser:
    parser = PageParser()
    parser.feed(path.read_text(encoding="utf-8"))
    return parser


def check_external(where: str, url: str, errors: list[str]) -> None:
    host = (urlsplit(url).hostname or "").lower()
    if host not in ALLOWED_HOSTS:
        errors.append(f"{where}: 許可していない外部ホスト: {url}")


def check_site(site: Path) -> list[str]:
    errors: list[str] = []
    site = site.resolve()
    if not site.is_dir():
        return [f"{site} がありません"]
    pages = {p.resolve(): parse(p) for p in sorted(site.rglob("*.html"))}
    if not (site / "index.html").is_file():
        errors.append("index.html がありません")

    for path in sorted(p for p in site.rglob("*") if p.is_file()):
        if path.suffix.lower() not in ALLOWED_SUFFIXES:
            errors.append(f"{path.relative_to(site)}: site/ に置けない種類のファイル（許可: {', '.join(sorted(ALLOWED_SUFFIXES))}）")

    for path, page in pages.items():
        rel = path.relative_to(site)
        if not page.lang:
            errors.append(f"{rel}: <html lang> がありません")
        if not page.has_title:
            errors.append(f"{rel}: <title> がありません")
        for key, name in REQUIRED_META:
            if (key, name) not in page.metas:
                errors.append(f"{rel}: <meta {key}=\"{name}\"> がありません")
        if ("name", "viewport") not in page.metas:
            errors.append(f"{rel}: viewport がありません")
        if page.img_without_alt:
            errors.append(f"{rel}: alt の無い <img> が {page.img_without_alt} 件あります")
        for tag in sorted(set(page.tags) & FORBIDDEN_TAGS):
            errors.append(f"{rel}: <{tag}> は使えません")
        if not any(v.startswith("#") for k, v in page.refs if k == "href"):
            errors.append(f"{rel}: ページ内リンク（スキップリンクなど）がありません")
        for kind, value in page.refs:
            where = f"{rel} ({kind})"
            parts = urlsplit(value)
            if parts.scheme in ("http", "https"):
                check_external(where, value, errors)
                if kind not in {"href", "canonical", "og:image"}:
                    errors.append(f"{where}: 外部資産は読み込めません: {value}")
                continue
            if parts.scheme or value.startswith("//"):
                errors.append(f"{where}: 使えないリンク: {value}")
                continue
            target = path if not parts.path else (path.parent / unquote(parts.path)).resolve()
            if not target.is_relative_to(site):
                errors.append(f"{where}: site/ の外を指しています: {value}")
                continue
            if target.is_dir():
                target = target / "index.html"
            if not target.is_file():
                errors.append(f"{where}: 参照先がありません: {value}")
                continue
            if parts.fragment:
                other = pages.get(target)
                if other is None:
                    errors.append(f"{where}: #{parts.fragment} を確かめられない（HTMLではない）: {value}")
                elif parts.fragment not in other.ids:
                    errors.append(f"{where}: fragmentの id がありません: {value}")

    for css in sorted(site.rglob("*.css")):
        text = css.read_text(encoding="utf-8")
        rel = css.relative_to(site)
        for ref in CSS_URL.findall(text) + CSS_IMPORT.findall(text):
            if re.match(r"^(https?:)?//", ref):
                errors.append(f"{rel}: CSSが外部を読んでいます: {ref}")
            elif not ref.startswith("data:"):
                target = (css.parent / unquote(ref.split("#", 1)[0])).resolve()
                if not (target.is_relative_to(site) and target.is_file()):
                    errors.append(f"{rel}: CSSの参照先がありません: {ref}")
    return errors


def main(argv: list[str]) -> int:
    site = Path(argv[1]) if len(argv) > 1 else DEFAULT_SITE
    errors = check_site(site)
    for message in errors:
        print(f"サイト検査: {message}", file=sys.stderr)
    if errors:
        return 1
    print("サイト（site/）: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
