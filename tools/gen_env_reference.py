#!/usr/bin/env python3
"""app/geoframe/config.py の環境変数から docs/reference/configuration.md を生成する。

    python3 tools/gen_env_reference.py           # 生成（既定動作）
    python3 tools/gen_env_reference.py --check   # 生成結果が古ければ exit 1（CI用）

`config.py` は `NAME = os.environ.get("GEOFRAME_...", "既定")` の形で環境変数を読み、
その直前に用途のコメントを置く規約で書かれている。この規約を唯一の真実として文書を
作り直すことで、「コードが読む変数」と「文書が説明する変数」のずれをCIで検出できる。
抽出に失敗した変数は説明を省いて名前だけを載せる（生成が落ちるより欠落を見せる）。
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / "app" / "geoframe" / "config.py"
OUT = ROOT / "docs" / "reference" / "configuration.md"

ENV_GET = re.compile(r"""os\.environ\.get\(\s*["']([A-Z0-9_]+)["']""")
NESTED_GET = re.compile(r"""os\.environ\.get\(\s*["']([A-Z0-9_]+)["'][^)]*\)""")
COMMENT = re.compile(r"^\s*#\s?(.*)$")
BANNER = re.compile(r"[=-]{4,}")
ASSIGN = re.compile(r"^([A-Z][A-Z0-9_]*)\s*=")


def _call_args(text, start):
    """`os.environ.get(` 直後の位置から、第2引数と閉じ括弧の位置を返す。

    既定値には `str(FETCH_PER_CYCLE)` のような式も書かれるため、括弧の対応を
    数えて切り出す。第2引数が無ければ (None, 閉じ括弧の位置)。
    """
    i = start
    while i < len(text) and text[i].isspace():
        i += 1
    if i >= len(text) or text[i] != ",":
        return None, i
    i += 1
    depth = 0
    begin = i
    while i < len(text):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                break
            depth -= 1
        i += 1
    return text[begin:i].strip(), i


def _env_calls(text):
    """1つの文に現れる環境変数参照を (変数名, 既定値の表示) で返す。"""
    out = []
    for m in ENV_GET.finditer(text):
        name = m.group(1)
        arg, close = _call_args(text, m.end())
        if arg is not None:
            out.append((name, _default(arg)))
            continue
        # `os.environ.get("X") or フォールバック` の形を拾う
        rest = text[close + 1:]
        alt = re.match(r"\s*or\s+", rest)
        if not alt:
            out.append((name, "（既定なし）"))
            continue
        j = alt.end()
        depth = 0
        while j < len(rest):
            c = rest[j]
            if c in "([{":
                depth += 1
            elif c in ")]}":
                if depth == 0:
                    break
                depth -= 1
            j += 1
        # この形の変数自体に既定値は無く、未設定なら別の値へ委ねる。
        # 「既定値 X」と書くと X が既定値だと誤読されるため、委譲先として示す
        out.append((name, f"なし（未設定時は {_default(rest[alt.end():j])}）"))
    return out


def collect_external(known):
    """config.py 以外が直接読む環境変数を {変数名: [モジュール名]} で返す。

    `config.py` を経由しない変数は上の一覧に構造的に現れず、文書から漏れる。
    モジュール名だけを載せる（行番号は編集のたびに動くため、同期検査が
    無意味に失敗するのを避ける）。"""
    found = {}
    for path in sorted((ROOT / "app" / "geoframe").glob("*.py")):
        if path.name == "config.py":
            continue
        text = path.read_text(encoding="utf-8")
        for name in set(ENV_GET.findall(text)):
            if name in known:
                continue
            found.setdefault(name, set()).add(path.name)
    return {k: sorted(v) for k, v in sorted(found.items())}


def _default(raw):
    """既定値の式を文書に出す形へ整える。"""
    raw = raw.strip()
    if not raw:
        return "（空文字）"
    lit = re.fullmatch(r"""["'](.*)["']""", raw)
    if lit:
        return f"`{lit.group(1)}`" if lit.group(1) else "（空文字）"
    # `os.environ.get("OTHER", "")` のような入れ子は、参照先の変数名だけを見せる
    return f"`{NESTED_GET.sub(lambda m: m.group(1), raw)}`"


def _comment_block(lines, index):
    """index行の直前にある連続コメント行を、上から順に返す。

    バナー（`# ====` や `# ----` だけの行）に当たったら打ち切る。バナーの上は
    節の見出しであり、その変数の説明ではない。
    """
    block = []
    i = index - 1
    while i >= 0:
        m = COMMENT.match(lines[i])
        if not m:
            break
        text = m.group(1).strip()
        if BANNER.fullmatch(text):
            break
        if text:
            block.append(text)
        i -= 1
    block.reverse()
    return block


def _line_depth(line):
    """その行の括弧の増減（コメント以降は数えない）。既定値が次行に続く文を検出する。"""
    code = line.split("#", 1)[0]
    return code.count("(") - code.count(")")


def _statement_start(lines, index):
    """代入行が複数行にまたがる場合に、その代入の開始行を探す。"""
    for i in range(index, -1, -1):
        if ASSIGN.match(lines[i]):
            return i
    return index


def collect():
    lines = CONFIG.read_text(encoding="utf-8").splitlines()
    entries = []
    seen = set()
    for i, line in enumerate(lines):
        if not ENV_GET.search(line):
            continue
        # 既定値が次行以降に続く文（URLなど）を1つの文として組み立てる
        text = line
        depth = _line_depth(line)
        j = i
        while depth > 0 and j + 1 < len(lines):
            j += 1
            text += " " + lines[j]
            depth += _line_depth(lines[j])
        calls = _env_calls(text)
        if not calls:
            continue
        start = _statement_start(lines, i)
        head = ASSIGN.match(lines[start])
        constant = head.group(1) if head else ""
        notes = _comment_block(lines, start)
        if not notes:
            # 行末コメント（`... # 例: xxx`）だけが説明のこともある
            trail = re.search(r"#\s*(.+)$", text)
            if trail:
                notes = [trail.group(1).strip().rstrip("。")]
        for name, default in calls:
            if name in seen:
                continue
            seen.add(name)
            entries.append({
                "name": name,
                "constant": constant,
                "default": default,
                "notes": notes,
            })
    return entries


def _collapse(notes):
    """コメント本文を1段落へ連結する。"""
    out = []
    for n in notes:
        if not out:
            out.append(n)
        elif out[-1].endswith(("。", "）", "`")):
            out.append(n)
        else:
            out.append(" " + n)
    return "".join(out).strip()


def render(entries):
    body = [
        "# 設定リファレンス（環境変数）",
        "",
        "> [!NOTE]",
        "> この文書は `app/geoframe/config.py` から自動生成しています。手で編集せず、",
        "> 変数や説明を変える場合は `config.py` の代入と直前のコメントを直したうえで",
        "> `python3 tools/gen_env_reference.py` を実行してください。CIが差分の有無を検査します。",
        "",
        "## 読み込み順序",
        "",
        "- `app/.env` を読み込みます。書式は `KEY=VALUE` で1行1項目、`#` 始まりの行は無視します。",
        "- **環境変数として直接指定した値が優先**されます（`.env` は未設定の項目だけを埋めます）。",
        "- systemd の `Environment=` / `EnvironmentFile=` でも指定できます。",
        "",
        "## 一覧",
        "",
        "既定値は未設定時の値です。式で書かれている場合は `config.py` の記述をそのまま示します",
        "（`ROOT` は `app/`、`DATA_DIR` はデータルートを指します）。定義順（＝機能のまとまり順）に",
        "並べています。外部連携を有効にする手順は [外部連携](../integrations/) を参照してください。",
        "",
    ]
    for e in entries:
        line = f"- `{e['name']}`（`config.py`: `{e['constant']}`） — 既定値 {e['default']}"
        note = _collapse(e["notes"])
        if note:
            line += "。" + note
        body.append(line)
    external = collect_external({e["name"] for e in entries})
    if external:
        body += [
            "",
            "## `config.py` 以外が読む環境変数",
            "",
            "以下は各モジュールが直接読みます（`config.py` を経由しないため上の一覧には現れません）。",
            "設定方法と既定の挙動は、それぞれの変数の説明先を参照してください。",
            "",
        ]
        for name, modules in external.items():
            body.append(f"- `{name}` — {', '.join(f'`{m}`' for m in modules)}")
    body.append("")
    return "\n".join(body)


def main(argv):
    check = "--check" in argv
    content = render(collect())
    current = OUT.read_text(encoding="utf-8") if OUT.exists() else None
    if check:
        if current != content:
            print(f"{OUT.relative_to(ROOT)} が config.py と一致しません。"
                  " `python3 tools/gen_env_reference.py` を実行してください。", file=sys.stderr)
            return 1
        print("設定リファレンス: OK")
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(content, encoding="utf-8")
    print(f"{OUT.relative_to(ROOT)} を生成しました（{content.count(chr(10))}行）。")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
