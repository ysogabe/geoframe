#!/usr/bin/env python3
"""Rust 版と比較用の Python 版を同じ要求で突き合わせ、一致の集計だけを出す（T1.5）。

検証環境の本番コピーを、Rust（--rust）と現行 Python（--python。AI・Drive 無効の比較用）の
両方に読ませて、次を比べる。

  1. GET /api/v1/photos      JSON を構文解析して完全一致か（差は件数とキー名だけ）
  2. GET /api/admin/status   server_time・disk を除いて一致か
  3. GET /api/admin/photos   filter × page × per の組み合わせと不正なクエリ（状態コードと JSON）
  4. GET /derivatives/…      一覧の写真ぜんぶ（--derivatives で数を絞れる）のバイト列を sha256 で
  5. 管理画面ページ・埋め込みファイル・拒否されるべきパス・メソッド・Host の状態コード

出力は**集計だけ**。写真の値・ファイル名・位置・タイトル・接続先は出さない（差があっても、
出すのは件数とキーの名前、クエリの filter/page/per、固定のパス見本の名前まで）。
標準ライブラリだけで動く。

使い方:
  python3 tools/compare_with_python.py --rust http://127.0.0.1:8080 --python http://127.0.0.1:8090

終了コード: 0 = 想定外の差なし、1 = 差あり、2 = 接続できないなど。
Python 版は本番コピーのデータを書き込む（起動時の移行など）ので、Rust と同じ DB は読ませないこと。
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import sys
import threading
import urllib.parse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]

# 既知の意図した差（docs/rust-migration/server.md §9.7・§10.6）。比較の対象にせず、別に数える。
#   /healthz は Python に無い（403）、Rust は新設（200）。
KNOWN_STATUS_DIFFS = {"/healthz": (403, 200)}

ADMIN_FILTERS = ["all", "noloc", "fav", "hidden", "inferred", "manual", "suggest"]
ADMIN_PER = [1, 10, 50, 200]


class Target:
    """1つのサーバーへの接続。スレッドごとに持ち、keep-alive が切られたら繋ぎ直す。"""

    def __init__(self, label: str, base: str):
        u = urllib.parse.urlsplit(base)
        if u.scheme != "http" or not u.hostname:
            raise SystemExit(f"{label}: http://host:port の形で指定する")
        self.label = label
        self.host = u.hostname
        self.port = u.port or 80
        self._local = threading.local()

    def _conn(self) -> http.client.HTTPConnection:
        c = getattr(self._local, "conn", None)
        if c is None:
            c = http.client.HTTPConnection(self.host, self.port, timeout=60)
            self._local.conn = c
        return c

    def _drop(self) -> None:
        c = getattr(self._local, "conn", None)
        if c is not None:
            c.close()
        self._local.conn = None

    def request(self, method: str, path: str, headers: dict | None = None,
                body: bytes | None = None, keep: bool = True):
        """(状態コード, ヘッダ(小文字キー), 本文の sha256, 本文の長さ, 本文) を返す。
        keep=False なら本文は捨てる（大きなファイルを比べるとき。ハッシュと長さだけ）。"""
        for attempt in (0, 1):
            try:
                c = self._conn()
                c.putrequest(method, path, skip_host=bool(headers and "Host" in headers),
                             skip_accept_encoding=True)
                for k, v in (headers or {}).items():
                    c.putheader(k, v)
                if body is not None:
                    c.putheader("Content-Length", str(len(body)))
                c.endheaders(body)
                r = c.getresponse()
                h = hashlib.sha256()
                n = 0
                chunks = []
                while True:
                    b = r.read(1 << 16)
                    if not b:
                        break
                    h.update(b)
                    n += len(b)
                    if keep:
                        chunks.append(b)
                hdrs = {k.lower(): v for k, v in r.getheaders()}
                if r.will_close:
                    self._drop()
                return r.status, hdrs, h.hexdigest(), n, b"".join(chunks)
            except (http.client.HTTPException, ConnectionError, TimeoutError, OSError):
                self._drop()
                if attempt == 1:
                    raise
        raise AssertionError("unreachable")


def quote_path(p: str) -> str:
    return urllib.parse.quote(p, safe="/%._-~")


def key_paths(a, b, prefix: str = "") -> set[str]:
    """2つの JSON で値が違う場所を、キーの名前の並び（配列の添字は []）で返す。値は返さない。"""
    out: set[str] = set()
    if isinstance(a, dict) and isinstance(b, dict):
        for k in a.keys() | b.keys():
            p = f"{prefix}.{k}" if prefix else k
            if k not in a or k not in b:
                out.add(p + "(片方に無い)")
            else:
                out |= key_paths(a[k], b[k], p)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.add(prefix + "[](長さ)")
        for x, y in zip(a, b):
            out |= key_paths(x, y, prefix + "[]")
    elif a != b or type(a) is not type(b):
        out.add(prefix or "(root)")
    return out


class Report:
    def __init__(self) -> None:
        self.failures = 0

    def section(self, title: str) -> None:
        print(f"\n== {title}")

    def line(self, text: str) -> None:
        print("  " + text)

    def result(self, ok: bool, text: str) -> None:
        if not ok:
            self.failures += 1
        print(("  OK   " if ok else "  DIFF ") + text)


def get_json(t: Target, path: str):
    status, hdrs, _, _, body = t.request("GET", path)
    try:
        return status, hdrs, json.loads(body.decode("utf-8")) if body else None
    except (ValueError, UnicodeDecodeError):
        return status, hdrs, ("<json でない>", len(body))


def compare_feed(rust: Target, py: Target, rep: Report):
    rep.section("1. GET /api/v1/photos")
    sr, hr, jr = get_json(rust, "/api/v1/photos")
    sp, hp, jp = get_json(py, "/api/v1/photos")
    rep.result(sr == sp == 200, f"状態コード Rust={sr} Python={sp}")
    if not (isinstance(jr, list) and isinstance(jp, list)):
        rep.result(False, "どちらかが JSON 配列でない")
        return jr
    rep.line(f"件数 Rust={len(jr)} Python={len(jp)}")
    if jr == jp:
        rep.result(True, f"JSON 完全一致（{len(jr)}/{len(jp)} 件、並びも一致）")
    else:
        ids_r = [p.get("id") for p in jr]
        ids_p = [p.get("id") for p in jp]
        only_r = set(ids_r) - set(ids_p)
        only_p = set(ids_p) - set(ids_r)
        common = set(ids_r) & set(ids_p)
        by_r = {p["id"]: p for p in jr if "id" in p}
        by_p = {p["id"]: p for p in jp if "id" in p}
        value_diff = [i for i in common if by_r[i] != by_p[i]]
        keys: Counter[str] = Counter()
        for i in value_diff:
            for k in key_paths(by_r[i], by_p[i]):
                keys[k] += 1
        same_order = [i for i in ids_r if i in common] == [i for i in ids_p if i in common]
        rep.result(False, f"JSON が一致しない: Rust のみ {len(only_r)} 件、Python のみ {len(only_p)} 件、"
                          f"共通 {len(common)} 件のうち値の差 {len(value_diff)} 件、共通分の並び"
                          f"{'は同じ' if same_order else 'が違う'}")
        for k, n in keys.most_common():
            rep.line(f"  差のあるキー: {k} … {n} 件")
    for name in ("content-type", "cache-control"):
        rep.result(hr.get(name) == hp.get(name), f"ヘッダ {name} が同じ")
    return jr


def compare_status(rust: Target, py: Target, rep: Report):
    rep.section("2. GET /api/admin/status（server_time・disk を除く）")
    sr, _, jr = get_json(rust, "/api/admin/status")
    sp, _, jp = get_json(py, "/api/admin/status")
    rep.result(sr == sp == 200, f"状態コード Rust={sr} Python={sp}")
    if not (isinstance(jr, dict) and isinstance(jp, dict)):
        rep.result(False, "どちらかが JSON オブジェクトでない")
        return
    for d in (jr, jp):
        d.pop("server_time", None)
        d.pop("disk", None)
    diff = sorted(key_paths(jr, jp))
    rep.result(not diff, f"除外後の JSON 完全一致（トップレベルのキー {len(jr)}/{len(jp)} 個）"
               if not diff else f"差のあるキー {len(diff)} 個")
    for k in diff:
        rep.line(f"  差のあるキー: {k}")


def compare_admin_photos(rust: Target, py: Target, rep: Report, feed_total: int):
    rep.section("3. GET /api/admin/photos（filter × page × per ＋ 不正なクエリ）")
    # 総ページ数の見当を、filter=all・per=最小で Python から取る
    _, _, j = get_json(py, "/api/admin/photos?filter=all&per=200")
    last = int(j.get("pages", 1)) if isinstance(j, dict) and isinstance(j.get("pages"), int) else 1
    queries = []
    for f in ADMIN_FILTERS:
        for per in ADMIN_PER:
            for page in (1, 2, 3, last, last + 1, 0, -1, 99999):
                queries.append(f"filter={f}&page={page}&per={per}")
    # 既定値・不正な値・parse_qs と int() の癖
    queries += ["", "filter=bogus", "filter=ALL", "filter=", "page=abc", "per=abc", "page=", "per=",
                "per=0", "per=-5", "per=201", "per=1_0", "page=%2B2", "page=+2", "page=%EF%BC%92",
                "page=1&page=2", "filter=fav&filter=all", "page=1;per=1", "per=10#frag", "%zz=1&per=7"]
    queries = list(dict.fromkeys(queries))

    def one(q: str):
        path = "/api/admin/photos" + (("?" + q) if q else "")
        r = get_json(rust, path)
        p = get_json(py, path)
        return q, r, p

    with ThreadPoolExecutor(max_workers=4) as ex:
        results = list(ex.map(one, queries))
    same_status = same_body = 0
    diffs = []
    keys: Counter[str] = Counter()
    for q, (sr, _, jr), (sp, _, jp) in results:
        same_status += sr == sp
        if sr == sp and jr == jp:
            same_body += 1
        else:
            diffs.append((q, sr, sp))
            if sr == sp:
                for k in key_paths(jr, jp):
                    keys[k] += 1
    n = len(results)
    rep.result(same_status == n, f"状態コードが一致 {same_status}/{n}")
    rep.result(same_body == n, f"状態コードと JSON がともに一致 {same_body}/{n}")
    for q, sr, sp in diffs[:20]:
        rep.line(f"  差のあるクエリ: [{q}] Rust={sr} Python={sp}")
    if len(diffs) > 20:
        rep.line(f"  （ほか {len(diffs) - 20} 件）")
    for k, c in keys.most_common():
        rep.line(f"  差のあるキー: {k} … {c} 件")
    ok_codes = Counter(sr for _, (sr, _, _), _ in results)
    rep.line("Rust の状態コード内訳: " + ", ".join(f"{k}:{v}" for k, v in sorted(ok_codes.items())))


def compare_derivatives(rust: Target, py: Target, rep: Report, feed, limit: int | None, workers: int):
    rep.section("4. 表示用画像（/derivatives/ ほか、一覧の file）のバイト一致")
    files = [p["file"] for p in feed if isinstance(p, dict) and isinstance(p.get("file"), str)]
    if limit is not None:
        files = files[:limit]
    rep.line(f"対象 {len(files)} 件（一覧の file）")

    def one(f: str):
        path = quote_path("/" + f)
        a = rust.request("GET", path, keep=False)
        b = py.request("GET", path, keep=False)
        return a, b

    total = same = status_diff = hash_diff = ct_diff = lm_same = 0
    bytes_total = 0
    errors = 0
    with ThreadPoolExecutor(max_workers=workers) as ex:
        for res in ex.map(lambda f: _safe(one, f), files):
            total += 1
            if res is None:
                errors += 1
                continue
            (sa, ha, da, na, _), (sb, hb, db, nb, _) = res
            if sa != sb:
                status_diff += 1
            elif da != db or na != nb:
                hash_diff += 1
            else:
                same += 1
                bytes_total += na
            if ha.get("content-type") != hb.get("content-type"):
                ct_diff += 1
            if ha.get("last-modified") == hb.get("last-modified"):
                lm_same += 1
    rep.result(same == total, f"バイト列（sha256・長さ）が一致 {same}/{total}（計 {bytes_total / 1048576:.0f} MiB）")
    if status_diff or hash_diff or errors:
        rep.line(f"  状態コードの差 {status_diff} 件、ハッシュの差 {hash_diff} 件、取得エラー {errors} 件")
    rep.result(ct_diff == 0, f"Content-Type が同じ {total - ct_diff}/{total}")
    rep.line(f"（参考）Last-Modified が同じ {lm_same}/{total}。別コピーのデータなので揃わなくてよい")


def _safe(fn, *a):
    try:
        return fn(*a)
    except (http.client.HTTPException, OSError):
        return None


def cmp_pair(rust: Target, py: Target, method: str, path: str, headers=None, body=None):
    a = rust.request(method, path, headers=headers, body=body)
    b = py.request(method, path, headers=headers, body=body)
    return a, b


def compare_pages_and_rejects(rust: Target, py: Target, rep: Report, repo: Path, sample_derivative: str | None):
    rep.section("5. 管理画面・埋め込みファイル・拒否されるべきパス（状態コード、本文は sha256）")

    # --- 管理画面のページ ---
    pages = ["/", "/admin", "/admin/", "//admin", "/admin?x=1", "/blog-admin", "/ai-search"]
    ok_status = ok_body = 0
    for p in pages:
        (sa, _, da, _, _), (sb, _, db, _, _) = cmp_pair(rust, py, "GET", p)
        ok_status += sa == sb
        ok_body += sa == sb and da == db
    rep.result(ok_status == len(pages), f"ページの状態コードが一致 {ok_status}/{len(pages)}")
    rep.result(ok_body == len(pages), f"ページの状態コードと本文が一致 {ok_body}/{len(pages)}")

    # --- 埋め込みファイル（app/static・app/vendor の全ファイル） ---
    embedded = sorted(f.relative_to(repo / "app") for d in ("static", "vendor")
                      for f in (repo / "app" / d).rglob("*") if f.is_file() and not f.name.startswith("."))
    if not embedded:
        rep.result(False, "埋め込み元（リポジトリの app/static・app/vendor）が見つからない（--repo を確認）")
    s_py = s_body = s_local = 0
    for rel in embedded:
        path = quote_path("/" + rel.as_posix())
        (sa, _, da, na, _), (sb, _, db, _, _) = cmp_pair(rust, py, "GET", path)
        local = hashlib.sha256((repo / "app" / rel).read_bytes()).hexdigest()
        s_py += sa == sb == 200
        s_body += sa == sb and da == db
        s_local += sa == 200 and da == local
    n = len(embedded)
    rep.result(s_local == n, f"Rust の埋め込みファイルがリポジトリの実ファイルとバイト一致 {s_local}/{n}")
    rep.result(s_body == n, f"Python と状態コード・本文が一致 {s_body}/{n}"
               "（Python 側の app/ が Rust の埋め込み元と同じ版のとき一致する）")

    # --- 拒否されるべきパス（Rust と Python の状態コードが同じこと）---
    rejects = [
        "/nope", "/api/nope", "/api/v1/photos/", "/.env", "/catalog.db", "/derivatives", "/derivatives/",
        "/derivatives/.hidden", "/derivatives/.dl_x", "/blog_exports/.tmp/x", "/%2e/derivatives/.x",
        "/derivatives/../catalog.db", "/derivatives/%2e%2e/catalog.db", "/%2e%2e/%2e%2e/etc/passwd",
        "/derivatives/..%2fcatalog.db", "/derivatives/%252e%252e/catalog.db", "/static/../catalog.db",
        "/photos/../catalog.db", "//etc/passwd", "/%2fderivatives/x.jpg", "/derivatives/x%00.jpg",
        "/favicon.ico", "/derivatives/does-not-exist.jpg", "/derivatives/does-not-exist.jpg/",
        "/static", "/static/", "/static/js", "/vendor/", "/blog", "/admin/x", "/api/admin/statusX",
        "/tiles", "/api/v1/input",
    ]
    ok_status = ok_body = 0
    diffs = []
    body_diffs = []
    for p in rejects:
        (sa, _, da, _, _), (sb, _, db, _, _) = cmp_pair(rust, py, "GET", p)
        if sa == sb:
            ok_status += 1
            # 前方一致で status の JSON が返るパスは、時刻とディスク使用量が本文に入るので本文を比べない
            same_body = da == db or p.startswith("/api/admin/status")
            ok_body += same_body
            if not same_body:
                body_diffs.append(f"{p}: 状態コード {sa}")
        else:
            diffs.append(f"{p}: Rust={sa} Python={sb}")
    rep.result(ok_status == len(rejects), f"拒否・404 のパスの状態コードが一致 {ok_status}/{len(rejects)}")
    rep.line(f"（参考）本文も同じ {ok_body}/{len(rejects)}（エラーページの理由句はヘッダ側の差）")
    for d in diffs:
        rep.line("  差: " + d)
    for d in body_diffs:
        rep.line("  本文だけ違う: " + d)

    # --- メソッド ---
    cases = [("HEAD", "/api/v1/photos", None), ("POST", "/api/v1/photos", b""), ("POST", "/", b""),
             ("OPTIONS", "/", None), ("PUT", "/", b""), ("DELETE", "/", None), ("HEAD", "/", None),
             ("HEAD", "/admin", None), ("HEAD", "/nope", None)]
    labels = [p for _, p, _ in cases]
    if sample_derivative:
        cases.append(("HEAD", sample_derivative, None))
        labels.append("<一覧の最初の画像>")  # ファイル名は出さない
    ok = 0
    for (m, p, body), label in zip(cases, labels):
        (sa, ha, _, _, _), (sb, hb, _, _, _) = cmp_pair(rust, py, m, p, body=body)
        same = sa == sb
        if m == "HEAD" and sa == 200:
            same = same and ha.get("content-length") == hb.get("content-length")
        ok += same
        if not same:
            rep.line(f"  差: {m} {label}: Rust={sa} Python={sb}")
    rep.result(ok == len(cases), f"メソッド（HEAD・POST・OPTIONS・PUT・DELETE）の状態コードが一致 {ok}/{len(cases)}")

    # --- Host 検査 ---
    hosts = ["localhost", "evil.example", "evil.example:80", "localhost.evil.example", "010.0.0.1", "1.2.3",
             "[::1]", "127.0.0.1", "localhost:0", "localhost:1"]
    ok = 0
    for h in hosts:
        # 待ち受けポートが違う（Rust と Python は別ポート）ので、ポート無しの形だけを比べる
        (sa, *_), (sb, *_) = cmp_pair(rust, py, "GET", "/api/v1/photos", headers={"Host": h})
        ok += sa == sb
        if sa != sb:
            rep.line(f"  差: Host の見本 #{hosts.index(h)}: Rust={sa} Python={sb}")
    rep.result(ok == len(hosts), f"Host 検査（IP・名前・ポートの見本）の状態コードが一致 {ok}/{len(hosts)}")

    # --- 既知の差（比較から外す。想定どおりかだけ確かめる）---
    ok = 0
    for p, (want_py, want_rust) in KNOWN_STATUS_DIFFS.items():
        (sa, *_), (sb, *_) = cmp_pair(rust, py, "GET", p)
        ok += (sb, sa) == (want_py, want_rust)
        if (sb, sa) != (want_py, want_rust):
            rep.line(f"  既知の差のはずが違う: {p}: Rust={sa} Python={sb}（想定 Rust={want_rust} Python={want_py}）")
    rep.result(ok == len(KNOWN_STATUS_DIFFS), f"既知の差（/healthz は Python 403・Rust 200）が想定どおり {ok}/{len(KNOWN_STATUS_DIFFS)}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--rust", required=True, help="Rust 版のベース URL（http://host:port）")
    ap.add_argument("--python", required=True, help="比較用 Python 版のベース URL")
    ap.add_argument("--derivatives", default="all", help="表示用画像を比べる数（all か整数。既定 all）")
    ap.add_argument("--workers", type=int, default=6, help="画像の比較の並列数（既定 6）")
    ap.add_argument("--repo", type=Path, default=REPO, help="埋め込み元の app/ を持つリポジトリ（既定: このリポジトリ）")
    args = ap.parse_args()
    limit = None if args.derivatives == "all" else int(args.derivatives)

    rust, py = Target("Rust", args.rust), Target("Python", args.python)
    rep = Report()
    try:
        feed = compare_feed(rust, py, rep)
        compare_status(rust, py, rep)
        compare_admin_photos(rust, py, rep, len(feed) if isinstance(feed, list) else 0)
        files = [p["file"] for p in feed if isinstance(p, dict) and isinstance(p.get("file"), str)] \
            if isinstance(feed, list) else []
        compare_derivatives(rust, py, rep, feed if isinstance(feed, list) else [], limit, max(1, args.workers))
        compare_pages_and_rejects(rust, py, rep, args.repo, quote_path("/" + files[0]) if files else None)
    except (http.client.HTTPException, OSError) as e:
        print(f"\n接続できない・応答が途切れた: {type(e).__name__}", file=sys.stderr)
        return 2
    print(f"\n== 結果: 想定外の差 {rep.failures} 項目")
    return 0 if rep.failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
