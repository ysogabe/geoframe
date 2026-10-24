#!/usr/bin/env python3
"""Drive の索引（`index_drive`）と取得（`fetch_derivatives`）の契約テスト用フィクスチャ
（Pythonの`drive.index_drive`・`drive.fetch_derivatives`が基準）。

- `drive-index-python.json`: 合成のDriveの応答（`files.list`の要素。名前・ID・時刻・座標はすべて合成）と、
  合成の原本（JPEG・PNG・TIFF・HEIC・壊れたファイル。実写真ではない）を、Pythonの本体に流した結果。
  Googleのライブラリは使わず、`svc`（`files().list(...).execute()`）と`googleapiclient.http.MediaIoBaseDownload`を
  偽物に差し替える（Pythonの`index_drive`は`svc`を引数に取り、`fetch_derivatives`はダウンロードの部品を関数の中で
  importするため）。段階を順に流し、各段階のあとの`photos`の全行（`id`を含む全列）・Pythonが出した問い合わせ
  （`q`・`fields`・`pageSize`・`pageToken`）・`(リモート件数, 追加, 削除)`を記録する。RustのテストがGoogleのスタブ
  HTTPサーバーで同じ応答を返して、同じ結果になること（意図した差を除く）を確かめる
  （`crates/geoframe/src/drive/tests.rs`の`matches_the_python_fixture`）。
  `indexed_at`・`fetched_at`は実行時刻なので値を記録せず、**書き換えられたか**（目印が残ったか・値が入ったか）だけを残す。

    python3 tools/gen_drive_index_fixture.py          # 書き出す（PNG・TIFFの本体はPillowが作るので、版が変わると変わる）
    python3 tools/gen_drive_index_fixture.py --check  # 保存済みの入力をPythonに通し直し、結果が一致するか検査する

`--check`は入力を作り直さない（Pillowの版でエンコード結果が変わってもCIが落ちないように、保存済みの
バイト列を入力にして、Pythonの結果が保存済みの期待値と変わっていないことだけを見る）。
HEICを変換するには`pillow-heif`が要る（`app/requirements.txt`）。出力先は crates/geoframe/tests/fixtures/ の下。
"""

import base64
import io
import json
import os
import sqlite3
import sys
import tempfile
import types
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
APP = REPO / "app"
FIXTURES = REPO / "crates" / "geoframe" / "tests" / "fixtures"
FIXTURE = FIXTURES / "drive-index-python.json"
EXIF_FIXTURE = FIXTURES / "exif-python.json"

SENTINEL = "2000-01-01T00:00:00"
FOLDER_NAME = "Frame Test"
FOLDER_ID = "folder_id_0000000000001"
PAGE_SIZE = 2  # 偽のDriveは、要求の`pageSize`にかかわらず1ページ2件で返す（ページ送りを通すため）


def b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def exif_jpeg(name: str) -> bytes:
    doc = json.loads(EXIF_FIXTURE.read_text(encoding="utf-8"))
    for case in doc["cases"]:
        if case["name"] == name:
            return base64.b64decode(case["jpeg_base64"])
    raise KeyError(name)


def pillow_bytes(fmt: str, color) -> bytes:
    from PIL import Image
    buf = io.BytesIO()
    Image.new("RGB", (12, 8), color).save(buf, fmt)
    return buf.getvalue()


def remote(id, name, mime="image/jpeg", md5=None, time=None, lat=None, lon=None) -> dict:
    """`files.list`の要素。Driveと同じく、無い項目は含めない"""
    item = {"id": id, "name": name, "mimeType": mime}
    if md5 is not None:
        item["md5Checksum"] = md5
    meta = {}
    if time is not None:
        meta["time"] = time
    loc = {}
    if lat is not None:
        loc["latitude"] = lat
    if lon is not None:
        loc["longitude"] = lon
    if loc:
        meta["location"] = loc
    if meta:
        item["imageMediaMetadata"] = meta
    return item


def build_phases() -> dict:
    typical = exif_jpeg("typical")
    heic = (FIXTURES / "synthetic_o1.heic").read_bytes()
    truncated = typical[:60]
    media = {
        "idA": typical,
        "idB": exif_jpeg("no_exif"),
        "idC": heic,
        "idD": pillow_bytes("PNG", (200, 30, 30)),
        "idE": pillow_bytes("TIFF", (30, 200, 30)),
        "idF": b"not an image",
        "idG": truncated,
        "idH": exif_jpeg("south_west"),
        "idI": pillow_bytes("TIFF", (30, 30, 200)),
    }
    # 1回目の一覧。重複するID（idB）は、最初の位置に最後の値が入る
    first = [
        remote("idA", "a.jpg", md5="a1", time="2024:05:06 07:08:09", lat=35.5, lon=139.25),
        remote("idB", "b.jpg", md5="b1"),
        remote("dir1", "sub", mime="application/vnd.google-apps.folder"),
        remote("idC", "c.heic", mime="image/heic", md5="c1", time="2023-01-02T03:04:05"),
        remote("mov1", "m.mp4", mime="video/mp4", md5="mv"),
        remote("idD", "d.png", mime="image/png", md5="d1", lat=1.5),  # 緯度だけ
        remote("idE", "e.tif", mime="image/tiff"),  # md5 なし
        remote("idF", "f.jpg", md5="f1", time="not a time"),
        remote("idG", "g.jpg", md5="g1", time="2024:13:45 25:61:00", lat=0.0, lon=0.0),
        remote("idB", "b-second.jpg", md5="b1b"),
    ]
    second = [dict(x) for x in first]  # 変化なし
    # 変化: idA は md5 が変わって位置が消える／idB は名前だけ変わる（md5 は同じ → 触らない）／
    # idE は消える／idD は md5 が変わる／idH・idI が増える（idH は先頭）。idF（画像でない）は残る
    changed = [
        remote("idH", "h.jpg", md5="h1", time="2025:01:02 03:04:05", lat=10.0, lon=20.0),
        remote("idA", "a2.jpg", md5="a2"),
        remote("idB", "renamed.jpg", md5="b1b"),
        remote("idC", "c.heic", mime="image/heic", md5="c1", time="2023-01-02T03:04:05"),
        remote("idD", "d2.png", mime="image/png", md5="d2", lat=1.5, lon=2.5),
        remote("idF", "f.jpg", md5="f1", time="not a time"),
        remote("idG", "g.jpg", md5="g1", time="2024:13:45 25:61:00", lat=0.0, lon=0.0),
        remote("idI", "i.tif", mime="image/tiff", md5="i1", time="2022:01:01 00:00:00"),
    ]
    setup_changes = [
        # 手動の位置・お気に入り・回転・地名・推定の痕跡・表示・取得時刻（idA は md5 変更で戻るものと残るもの）
        {"op": "sql", "sql": "UPDATE photos SET lat=1.0, lon=2.0, loc_source='manual', place='p', elev=3.5, "
                             "favorite=1, hidden=1, rotation=90, title='t', inferred_from=7, infer_dt_s=60, "
                             "display='derivatives/idA.r90.jpg', fetched_at='2024-01-01T00:00:00', "
                             "muni_cd='13101', pref='東京都', city='千代田区' WHERE drive_id='idA'"},
        {"op": "mkfile", "path": "derivatives/idA.r90.jpg"},
        # idB は取得済みにする（取得の段で対象にならない）
        {"op": "sql", "sql": "UPDATE photos SET display='derivatives/idB.jpg', fetched_at='2024-01-01T00:00:00' "
                             "WHERE drive_id='idB'"},
        {"op": "mkfile", "path": "derivatives/idB.jpg"},
        # 消える行の derivative は、行と一緒に消える
        {"op": "sql", "sql": "UPDATE photos SET display='derivatives/idE.jpg' WHERE drive_id='idE'"},
        {"op": "mkfile", "path": "derivatives/idE.jpg"},
        # ローカルの行は触られない
        {"op": "sql", "sql": "INSERT INTO photos(source, path, name, mtime) VALUES('local','photos/x.jpg','x.jpg',1.0)"},
    ]
    return {
        "folder": {"name": FOLDER_NAME, "id": FOLDER_ID},
        "page_size": PAGE_SIZE,
        "media": {k: b64(v) for k, v in media.items()},
        "phases": [
            {"kind": "index", "name": "initial", "remote": first, "ops": []},
            {"kind": "index", "name": "unchanged", "remote": second, "ops": []},
            {"kind": "index", "name": "changes", "remote": changed, "ops": setup_changes},
            {"kind": "index", "name": "unchanged_again", "remote": changed, "ops": []},
            # 2ページ目の取得に失敗する: Python は例外で、DB には触れない（追加も削除もしない）
            {"kind": "index", "name": "listing_fails_midway",
             "remote": [remote("idZ", "z.jpg", md5="z1")] + changed[:1] + changed[2:], "ops": [], "fail_at_page": 2},
            # 取得: 位置の有無で絞らず、taken_at（同じなら id）の昇順。画像でない idF は毎回先頭の枠に入る
            # （Python は失敗を覚えない。1サイクル1枚なら、idF のせいで後ろの写真はいつまでも取得されない）
            {"kind": "fetch", "name": "cycle1", "limit": 3},
            {"kind": "fetch", "name": "cycle2_limit1", "limit": 1},
            {"kind": "fetch", "name": "cycle3_limit1", "limit": 1},
            {"kind": "fetch", "name": "cycle4", "limit": 25},
            # 一覧が空: Python は全行を消す（Rust は消さない。意図した差）
            {"kind": "index", "name": "emptied", "remote": [], "ops": []},
        ],
    }


# ---- Python本体を流す ------------------------------------------------------------

class FakeExec:
    def __init__(self, fn):
        self.fn = fn

    def execute(self):
        return self.fn()


class FakeSvc:
    """`index_drive`が使う`svc.files().list(...).execute()`だけを持つ偽のDrive"""

    def __init__(self, items, fail_at_page=None):
        self.items = items
        self.fail_at_page = fail_at_page
        self.requests = []

    def files(self):
        return self

    def get_media(self, fileId):
        return ("media", fileId)

    def list(self, **kw):
        self.requests.append({k: kw.get(k) for k in ("q", "fields", "pageSize", "pageToken")})
        return FakeExec(lambda: self._list(kw))

    def _list(self, kw):
        q = kw["q"]
        if q.startswith("name='"):
            return {"files": [{"id": FOLDER_ID}] if f"name='{FOLDER_NAME}'" in q else []}
        assert q == f"'{FOLDER_ID}' in parents and trashed=false", q
        start = int((kw.get("pageToken") or "page-0").removeprefix("page-"))
        if self.fail_at_page is not None and start == (self.fail_at_page - 1) * PAGE_SIZE:
            raise RuntimeError("boom")
        end = min(start + PAGE_SIZE, len(self.items))
        page = {"files": self.items[start:end]}
        if end < len(self.items):
            page["nextPageToken"] = f"page-{end}"
        return page


class FakeDownload:
    """`MediaIoBaseDownload`の偽物。呼ばれた`fileId`の順を記録し、合成の原本を書く"""
    media = {}
    calls = []

    def __init__(self, fh, request):
        self.fh = fh
        self.file_id = request[1]
        FakeDownload.calls.append(self.file_id)

    def next_chunk(self):
        self.fh.write(FakeDownload.media[self.file_id])
        return None, True


def columns_rows(conn: sqlite3.Connection) -> list:
    cur = conn.execute("SELECT * FROM photos ORDER BY id")
    names = [d[0] for d in cur.description]
    return [dict(zip(names, row)) for row in cur.fetchall()]


def normalize(rows: list) -> list:
    """`indexed_at`・`fetched_at`は値を捨て、書き換えられたか・値が入ったかだけを残す"""
    out = []
    for row in rows:
        row = dict(row)
        at = row.pop("indexed_at")
        row["touched"] = at != SENTINEL
        fetched = row.pop("fetched_at")
        row["fetched"] = fetched is not None and fetched != "2024-01-01T00:00:00"
        row["fetched_kept"] = fetched == "2024-01-01T00:00:00"
        out.append(row)
    return out


def jpeg_sizes(derivatives: Path) -> dict:
    from PIL import Image
    sizes = {}
    if derivatives.is_dir():
        for p in sorted(derivatives.iterdir()):
            if p.suffix == ".jpg" and p.stat().st_size > 20:  # 目印だけのファイルは除く
                try:
                    with Image.open(p) as im:
                        sizes[p.name] = list(im.size)
                except Exception:
                    pass
    return sizes


def apply_op(root: Path, op: dict, db_exec) -> None:
    if op["op"] == "mkfile":
        p = root / op["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(b"derivative")
    elif op["op"] == "sql":
        db_exec(op["sql"])
    else:
        raise ValueError(op["op"])


def run_python(doc: dict) -> list:
    with tempfile.TemporaryDirectory(prefix="geoframe-drive-index-") as data_dir:
        return _run_python(doc, data_dir)


def _run_python(doc: dict, data_dir: str) -> list:
    os.environ["GEOFRAME_DATA_DIR"] = data_dir
    os.environ["GEOFRAME_DRIVE_FOLDER"] = doc["folder"]["name"]
    sys.path.insert(0, str(APP))
    # `fetch_derivatives`は関数の中で`from googleapiclient.http import MediaIoBaseDownload`をする
    pkg = types.ModuleType("googleapiclient")
    http = types.ModuleType("googleapiclient.http")
    http.MediaIoBaseDownload = FakeDownload
    pkg.http = http
    sys.modules["googleapiclient"] = pkg
    sys.modules["googleapiclient.http"] = http
    from geoframe import drive
    from geoframe.config import EXTS
    from geoframe.store import db, dbx
    if ".heic" not in EXTS:
        sys.exit("pillow-heif が無い Python では HEIC を変換できないので、このフィクスチャは作れません")
    FakeDownload.media = {k: base64.b64decode(v) for k, v in doc["media"].items()}
    root = Path(data_dir)
    results = []
    marker_files = ["derivatives/idA.r90.jpg", "derivatives/idB.jpg", "derivatives/idE.jpg"]
    for phase in doc["phases"]:
        dbx("UPDATE photos SET indexed_at=?", (SENTINEL,))
        for op in phase.get("ops", []):
            apply_op(root, op, lambda sql: dbx(sql))
        dbx("UPDATE photos SET indexed_at=? WHERE indexed_at IS NULL", (SENTINEL,))
        out = {}
        real, sys.stdout = sys.stdout, open(os.devnull, "w")
        try:
            if phase["kind"] == "index":
                svc = FakeSvc(phase["remote"], phase.get("fail_at_page"))
                try:
                    out["counts"] = list(drive.index_drive(svc))
                    out["raised"] = False
                except RuntimeError:
                    out["counts"] = None
                    out["raised"] = True
                out["requests"] = svc.requests
            else:
                FakeDownload.calls = []
                svc = FakeSvc([])
                drive.fetch_derivatives(svc, phase["limit"])
                out["attempts"] = list(FakeDownload.calls)
                out["tmp_leftovers"] = sorted(p.name for p in (root / "tmp").glob(".dl_*"))
                out["sizes"] = jpeg_sizes(root / "derivatives")
        finally:
            sys.stdout.close()
            sys.stdout = real
        out["rows"] = normalize(columns_rows(db()))
        out["exists"] = {p: (root / p).exists() for p in marker_files}
        results.append(out)
    return results


def render(doc: dict) -> str:
    return json.dumps(doc, indent=1, ensure_ascii=False) + "\n"


def main() -> int:
    check = "--check" in sys.argv[1:]
    if check:
        if not FIXTURE.exists():
            print(f"{FIXTURE} がありません。python3 tools/gen_drive_index_fixture.py で作ってください", file=sys.stderr)
            return 1
        stored = json.loads(FIXTURE.read_text(encoding="utf-8"))
        doc = {k: stored[k] for k in ("folder", "page_size", "media")}
        doc["phases"] = [{k: v for k, v in p.items() if k != "python"} for p in stored["phases"]]
    else:
        doc = build_phases()
    results = run_python(doc)
    out = {k: doc[k] for k in ("folder", "page_size", "media")}
    out["phases"] = [dict(p, python=r) for p, r in zip(doc["phases"], results)]
    text = render(out)
    if check:
        if FIXTURE.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE.name} が Python の結果と違います。python3 tools/gen_drive_index_fixture.py で作り直して"
                  "差分を確認してください", file=sys.stderr)
            return 1
        print(f"{FIXTURE.name}: 一致（{len(doc['phases'])}段階）")
        return 0
    FIXTURE.write_text(text, encoding="utf-8")
    print(f"{FIXTURE} を書きました（{len(doc['phases'])}段階、最後の行数 {len(results[-1]['rows'])}）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
