"""写真フィードの契約テスト用フィクスチャ（合成データだけ）を作る。

本物のスキーマ（store.SCHEMA と移行）で一時DBを作り、合成の写真を入れて、現行の
`photos.api_photos()` が出す JSON を得る。Rust の契約テスト（crates/geoframe/tests）は
この JSON を読み、Python と同じ形で出せることを確かめる。

再生成: `python3 tools/gen_photo_feed_fixture.py`（差分の検査は `--check`）。
座標・地名・ファイル名は実在のものを使わない。
"""
import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from geoframe import photos, store

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_PATH = ROOT / "crates/geoframe/tests/fixtures/photo-feed-python.json"
SCHEMA_PATH = ROOT / "docs/architecture/photo-feed.schema.json"

PHOTO_COLUMNS = ("id", "source", "display", "name", "md5", "taken_at", "lat", "lon",
                 "loc_source", "place", "elev", "title", "fetched_at", "pref", "city",
                 "favorite", "hidden")


def _photo(id, **over):
    row = {"id": id, "source": "drive", "display": f"derivatives/p{id:02d}.jpg",
           "name": f"IMG_{id:04d}.jpg", "md5": f"md5-{id}", "taken_at": "2025-06-01 10:00",
           "lat": 35.0, "lon": 139.0, "loc_source": "exif", "place": None, "elev": None,
           "title": None, "fetched_at": None, "pref": None, "city": None,
           "favorite": 0, "hidden": 0}
    row.update(over)
    return row


# 一覧に出る写真（出る順は taken_at 昇順、空文字は最後）と、出ない写真
PHOTOS = [
    # 品質・AIなし。住所なし（海外など）。タイトルはファイル名
    _photo(1, taken_at="2025-06-01 10:00", lat=35.1, lon=139.1),
    # similar の推奨。お気に入り。住所あり。取り込み日時あり・標高あり。位置は推定
    _photo(2, taken_at="2025-06-01 12:00", pref="千葉県", city="千葉市", place="中央",
           favorite=1, fetched_at="2025-06-02T09:30:00", elev=12.5, loc_source="inferred",
           lat=35.2, lon=139.2),
    # similar の推奨外（一覧には残る）。町名なしのタイトルは市区町村だけ
    _photo(3, taken_at="2025-06-01 12:01", pref="千葉県", city="千葉市",
           fetched_at="2025-06-02T09:30:01", lat=35.2, lon=139.2),
    # exact の推奨。手動のタイトル。前の写真から48時間以上後なので次の旅
    _photo(4, taken_at="2025-06-05 10:00", title="夏の海", elev=3.0, lat=35.3, lon=139.3),
    # exact の推奨外（一覧から除かれる）
    _photo(5, taken_at="2025-06-05 10:00", lat=35.3, lon=139.3),
    # AI整理で同じ場面と判定された推奨
    _photo(6, taken_at="2025-06-05 11:00", lat=35.4, lon=139.4),
    # AI整理で同じ場面と判定された推奨外（一覧から除かれる）
    _photo(7, taken_at="2025-06-05 11:01", lat=35.4, lon=139.4),
    # 同じ AI グループの推奨外だがお気に入り（一覧に残る）
    _photo(8, taken_at="2025-06-05 11:02", favorite=1, lat=35.4, lon=139.4),
    # AI整理が未判定のグループ（推奨外でも一覧に残る）
    _photo(9, taken_at="2025-06-05 11:03", lat=35.5, lon=139.5),
    # 撮影日時が空文字（最後に並ぶ）。lat/lon は整数値の浮動小数（35.0 / 139.0）
    _photo(10, taken_at=""),
    # 一覧に出ない: 非表示、表示用画像なし、位置なし
    _photo(11, hidden=1),
    _photo(12, display=None),
    _photo(13, lat=None, lon=None),
]

# photo_quality: (photo_id, group_id, group_kind, rank_in_group, recommended, score)
QUALITY = [
    (2, 1, "similar", 1, 1, 0.9),
    (3, 1, "similar", 2, 0, 0.4),
    (4, 2, "exact", 1, 1, 0.8),
    (5, 2, "exact", 2, 0, 0.7),
]

# photo_ai_groups: (group_id, status, same_scene)
AI_GROUPS = [("ai-1", "judged", 1), ("ai-2", "pending", 1)]

# photo_ai_group_members: (group_id, photo_id, rank_in_group, recommended, local_score)
AI_MEMBERS = [
    ("ai-1", 6, 1, 1, 0.8),
    ("ai-1", 7, 2, 0, 0.5),
    ("ai-1", 8, 3, 0, None),
    ("ai-2", 9, 1, 0, 0.6),
]


def build_items():
    """合成DBに対する現行 `api_photos()` の出力（JSONに直せる dict のリスト）。"""
    with tempfile.TemporaryDirectory() as tmp:
        with patch.object(store, "DB_PATH", Path(tmp) / "catalog.db"), \
                patch.object(store, "_db", None):
            try:
                _seed()
                return photos.api_photos()
            finally:
                if store._db is not None:
                    store._db.close()


def _seed():
    marks = ",".join("?" * len(PHOTO_COLUMNS))
    store.dbm(f"INSERT INTO photos({','.join(PHOTO_COLUMNS)}) VALUES({marks})",
              [tuple(p[c] for c in PHOTO_COLUMNS) for p in PHOTOS])
    by_id = {p["id"]: p for p in PHOTOS}
    store.dbm("""INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime,
                   content_sha256, group_id, group_kind, rank_in_group, recommended, score,
                   algorithm, analyzed_at)
                 VALUES(?, ?, ?, NULL, 'sha', ?, ?, ?, ?, ?, 'test', '2025-06-10T00:00:00')""",
              [(pid, by_id[pid]["display"], by_id[pid]["md5"], gid, kind, rank, rec, score)
               for pid, gid, kind, rank, rec, score in QUALITY])
    store.dbm("""INSERT INTO photo_ai_groups(group_id, kind, algorithm, analyzed_at, status,
                   same_scene) VALUES(?, 'scene', 'test', '2025-06-10T00:00:00', ?, ?)""",
              AI_GROUPS)
    store.dbm("""INSERT INTO photo_ai_group_members(group_id, photo_id, rank_in_group,
                   recommended, local_score) VALUES(?, ?, ?, ?, ?)""", AI_MEMBERS)


def render(items):
    """フィクスチャのファイルに書く文字列（キー順固定・日本語はそのまま）。"""
    return json.dumps(items, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
