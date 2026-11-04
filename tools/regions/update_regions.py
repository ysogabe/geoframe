#!/usr/bin/env python3
"""Natural Earth の境界データを取得・変換し、`crates/geoframe/data/regions/` の成果物を作る。

通常のビルドや実行では使わない。境界データを更新するときだけ、明示的に実行する（手順は README.md）。

    python3 -I tools/regions/update_regions.py fetch      # 固定した版を取得し、SHA256 を検証する
    python3 -I tools/regions/update_regions.py convert    # 変換して成果物と manifest を書く
    python3 -I tools/regions/update_regions.py check      # 変換し直して、コミット済みの成果物と一致するか確かめる
    python3 -I tools/regions/update_regions.py all        # fetch → convert

Python 依存は pyshp だけ（requirements.txt。shapefile の読み込みに既存のライブラリを使う）。
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import sys
import urllib.parse
import urllib.request
import zlib
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import regions_lib as lib  # noqa: E402

REPO = HERE.parent.parent
DEFAULT_OUT = REPO / "crates" / "geoframe" / "data" / "regions"
DEFAULT_WORK = HERE / ".cache"
CONVERTER_VERSION = "1.0.0"
ALLOWED_HOSTS = {"raw.githubusercontent.com"}

# サイズ上限（manifest に書く。Rust 側の読み込みもこれを超える展開を拒否する）。
# PRD AC10 の目標「リリースバイナリの増加 15MB 以下」に合わせ、圧縮後の合計の上限を 15 MB とする
LIMIT_POLYGONS_GZ = 14_500_000
LIMIT_POLYGONS_RAW = 40_000_000
LIMIT_NAMES_GZ = 500_000
LIMIT_NAMES_RAW = 3_000_000
LIMIT_TOTAL_GZ = 15_000_000


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def source_file_name(layer: dict, ext: str) -> str:
    return f"{layer['stem']}.{ext}"


# ---------------------------------------------------------------- fetch

def fetch(sources: dict, work: Path, print_hashes: bool) -> Path:
    """固定したコミットのファイルを work/<版>/ へ取得し、サイズと SHA256 を検証する。"""
    dest = work / sources["data_version"]
    dest.mkdir(parents=True, exist_ok=True)
    problems = []
    for key, layer in sources["layers"].items():
        for ext, meta in layer["files"].items():
            name = source_file_name(layer, ext)
            path = dest / name
            if not path.exists():
                url = sources["base_url"] + name
                parsed = urllib.parse.urlparse(url)
                if parsed.scheme != "https" or parsed.hostname not in ALLOWED_HOSTS:
                    raise SystemExit(f"許可していない取得元: {parsed.scheme}://{parsed.hostname}")
                print(f"取得: {name}", file=sys.stderr)
                with urllib.request.urlopen(url, timeout=120) as res:  # noqa: S310（https と許可ホストだけ）
                    data = res.read()
                tmp = path.with_suffix(path.suffix + ".part")
                tmp.write_bytes(data)
                tmp.replace(path)
            data = path.read_bytes()
            digest = sha256_hex(data)
            if print_hashes:
                print(f"{key}.{ext}: bytes={len(data)} sha256={digest}")
            if len(data) != meta["bytes"] or digest != meta["sha256"]:
                problems.append(name)
    if problems and not print_hashes:
        # 検証に失敗した取得物は使わない（残さない）
        for name in problems:
            (dest / name).unlink(missing_ok=True)
        raise SystemExit("SHA256 が sources.json と一致しない: " + ", ".join(problems))
    return dest


def verify_dir(sources: dict, directory: Path) -> None:
    for layer in sources["layers"].values():
        for ext, meta in layer["files"].items():
            path = directory / source_file_name(layer, ext)
            if not path.exists():
                raise SystemExit(f"元データが無い: {path}（先に fetch を実行する）")
            data = path.read_bytes()
            if len(data) != meta["bytes"] or sha256_hex(data) != meta["sha256"]:
                raise SystemExit(f"SHA256 が sources.json と一致しない: {path.name}")


# ---------------------------------------------------------------- 読み込み

def read_layer(directory: Path, layer: dict):
    try:
        import shapefile  # pyshp
    except ImportError as e:
        raise SystemExit("pyshp が必要です: pip install -r tools/regions/requirements.txt") from e
    reader = shapefile.Reader(str(directory / layer["stem"]), encoding="utf-8")
    if reader.shapeTypeName != "POLYGON":
        raise lib.RegionDataError(f"{layer['stem']}: POLYGON ではない（{reader.shapeTypeName}）")
    records = [r.as_dict() for r in reader.records()]
    shapes = list(reader.iterShapes())
    if len(records) != len(shapes):
        raise lib.RegionDataError(f"{layer['stem']}: 属性と形状の件数が違う")
    return records, shapes


def polygons_of(shape) -> list[lib.Polygon]:
    """pyshp が輪を外側と穴に分けた多角形の一覧（座標はそのまま）。"""
    geo = shape.__geo_interface__
    if geo["type"] == "Polygon":
        raw = [geo["coordinates"]]
    elif geo["type"] == "MultiPolygon":
        raw = geo["coordinates"]
    else:
        raise lib.RegionDataError(f"想定外の形状 {geo['type']}")
    return [[[(float(x), float(y)) for x, y in ring] for ring in polygon] for polygon in raw]


# ---------------------------------------------------------------- 変換

def build(directory: Path, sources: dict, tables_dir: Path) -> dict:
    id_tables = load_json(tables_dir / "id_resolution.json")
    jp_table = load_json(tables_dir / "japan_prefectures.json")["prefectures"]
    name_overrides = load_json(tables_dir / "country_names_ja.json")["overrides"]

    recs0, shapes0 = read_layer(directory, sources["layers"]["admin0"])
    recs1, shapes1 = read_layer(directory, sources["layers"]["admin1"])

    # ---- Admin0（国）
    countries: list[dict] = []
    seen: set[str] = set()
    for rec, shape in zip(recs0, shapes0):
        a3 = lib.clean_name(rec["ADM0_A3"])
        cid = f"ne:{a3}"
        if not a3 or not lib.ID_RE.match(cid):
            raise lib.RegionDataError(f"Admin0: 使えない ID {a3!r}（対応表で解決する）")
        if cid in seen:
            raise lib.RegionDataError(f"Admin0: ID の重複 {cid}")
        seen.add(cid)
        polygons = polygons_of(shape)
        for pi, polygon in enumerate(polygons):
            lib.validate_polygon(polygon, f"Admin0 {cid} 片{pi}")
        ja_raw = lib.clean_name(rec["NAME_JA"])
        ja = name_overrides.get(a3, ja_raw)
        en = lib.clean_name(rec["NAME_EN"]) or lib.clean_name(rec["NAME"])
        native = lib.clean_name(rec["NAME"])
        name = lib.display_name(ja, en, native, a3)
        search = lib.dedupe(
            [
                name,
                ja,
                ja_raw,
                en,
                native,
                lib.clean_name(rec["NAME_LONG"]),
                lib.clean_name(rec["FORMAL_EN"]),
                *lib.split_alt(rec["NAME_ALT"]),
            ]
        )
        entry = {"id": cid, "name": name, "search": search}
        iso2 = lib.clean_name(rec.get("ISO_A2_EH"))
        if len(iso2) == 2:
            entry["iso2"] = iso2
        countries.append({"id": cid, "polygons": polygons, "names": entry, "a3": a3})
    for a3 in name_overrides:
        if f"ne:{a3}" not in seen:
            raise lib.RegionDataError(f"country_names_ja.json: 使われない項目 {a3}")
    countries.sort(key=lambda c: c["id"])
    country_index = {c["id"]: i for i, c in enumerate(countries)}

    # ---- Admin1（第1行政区画）
    id_map: dict = id_tables["admin1_id"]
    parent_map: dict = id_tables["admin1_parent"]["map"]
    exclude = set(id_tables["admin1_exclude"]["codes"])
    pref_by_code = {row["adm1_code"]: row for row in jp_table}
    used_ids: set[str] = set()
    used_parents: set[str] = set()
    used_exclude: set[str] = set()
    used_pref: set[str] = set()
    regions: list[dict] = []
    seen_regions: set[str] = set()
    codes_seen: set[str] = set()
    for rec, shape in zip(recs1, shapes1):
        code = lib.clean_name(rec["adm1_code"])
        if not code:
            raise lib.RegionDataError("Admin1: adm1_code が空")
        if code in codes_seen:
            raise lib.RegionDataError(f"Admin1: adm1_code の重複 {code}")
        codes_seen.add(code)
        if code in exclude:
            used_exclude.add(code)
            continue
        rid_code = id_map.get(code, code)
        if code in id_map:
            used_ids.add(code)
        rid = f"ne:{rid_code}"
        if not lib.ID_RE.match(rid):
            raise lib.RegionDataError(f"Admin1: 使えない ID {rid!r}（id_resolution.json の admin1_id で解決する）")
        if rid in seen_regions:
            raise lib.RegionDataError(f"Admin1: ID の衝突 {rid}")
        seen_regions.add(rid)
        a3 = lib.clean_name(rec["adm0_a3"])
        parent_a3 = parent_map.get(a3, a3)
        if a3 in parent_map:
            used_parents.add(a3)
        parent = f"ne:{parent_a3}"
        if parent not in country_index:
            raise lib.RegionDataError(f"Admin1 {rid}: 国 {a3} が Admin0 に無い（admin1_parent で付け替える）")
        polygons = polygons_of(shape)
        for pi, polygon in enumerate(polygons):
            lib.validate_polygon(polygon, f"Admin1 {rid} 片{pi}")
        ja = lib.clean_name(rec["name_ja"])
        en = lib.clean_name(rec["name_en"]) or lib.clean_name(rec["name"])
        local = lib.clean_name(rec["name_local"])
        ne_name = lib.clean_name(rec["name"])
        name = lib.display_name(ja, en, local, ne_name, lib.clean_name(rec["gn_name"]), lib.clean_name(rec["woe_name"]))
        if not name:
            raise lib.RegionDataError(f"Admin1 {rid}: 表示名が空（admin1_exclude に載せるか、名称を補う）")
        search = lib.dedupe([name, ja, en, ne_name, local, *lib.split_alt(rec["name_alt"])])
        entry = {"id": rid, "country": parent, "name": name, "search": search}
        iso = lib.clean_name(rec["iso_3166_2"])
        # NE は ISO に無い地域に 'AQ-X01~' のような仮の値を入れる。補助属性なので、ISO の形のものだけ残す
        if lib.ISO_3166_2_RE.match(iso):
            entry["iso"] = iso
        if code in pref_by_code:
            row = pref_by_code[code]
            if parent != "ne:JPN" or iso != row["iso_3166_2"]:
                raise lib.RegionDataError(f"japan_prefectures.json: {code} が日本の {row['iso_3166_2']} と合わない")
            entry["pref"] = row["pref"]
            used_pref.add(code)
        regions.append(
            {
                "id": rid,
                "country": country_index[parent],
                "polygons": polygons,
                "names": entry,
                "code": code,
                "declared": a3,
                "reassigned": a3 in parent_map,
            }
        )

    for label, wanted, got in (
        ("admin1_id", set(id_map), used_ids),
        ("admin1_parent", set(parent_map), used_parents),
        ("admin1_exclude", exclude, used_exclude),
    ):
        if wanted - got:
            raise lib.RegionDataError(f"id_resolution.json の {label} に使われない項目: {sorted(wanted - got)}")
    check_japan(jp_table, pref_by_code, used_pref, regions)
    regions.sort(key=lambda r: r["id"])

    return {"countries": countries, "regions": regions, "excluded": sorted(used_exclude)}


def check_japan(jp_table: list, pref_by_code: dict, used: set, regions: list) -> None:
    if len(jp_table) != 47 or len(pref_by_code) != 47:
        raise lib.RegionDataError("japan_prefectures.json: 47 件ではない")
    if len({row["pref"] for row in jp_table}) != 47 or len({row["iso_3166_2"] for row in jp_table}) != 47:
        raise lib.RegionDataError("japan_prefectures.json: 都道府県名または ISO コードの重複")
    if used != set(pref_by_code):
        raise lib.RegionDataError(f"日本の Admin1 に対応しない都道府県: {sorted(set(pref_by_code) - used)}")
    japan = [r for r in regions if r["names"]["country"] == "ne:JPN"]
    unmapped = [r["id"] for r in japan if "pref" not in r["names"]]
    if unmapped:
        raise lib.RegionDataError(f"都道府県に対応しない日本の Admin1: {unmapped}")
    if len(japan) != 47:
        raise lib.RegionDataError(f"日本の Admin1 が 47 件ではない: {len(japan)}")


# ---------------------------------------------------------------- 国参照の突き合わせ

def cross_reference(built: dict) -> tuple[list[dict], list[str]]:
    """Admin1 の各多角形の代表点が、日本視点版 Admin0 のどの国に入るかを調べ、宣言された国と違うものを返す。

    `id_resolution.json` の `admin1_parent` で明示的に国を付け替えた地域は、既知の食い違いの一覧とは別に検査する。
    付け替え先の国にすべての片の代表点が入らなければ、変換を止める（付け替えの誤りであり、既知の食い違いではない）。

    Admin1 は既定版（日本視点ではない）なので、北方領土・竹島のように国が食い違うことがある。
    食い違いは分類器では『その国に Admin1 が当たらない点＝地域不明』になる（設計「境界データと分類器」）。
    """
    countries = built["countries"]
    index: list[tuple[str, lib.PolygonIndex]] = []
    for c in countries:
        for polygon in c["polygons"]:
            index.append((c["id"], lib.PolygonIndex(polygon)))
    mismatches = []
    reassign_failures = []
    nowhere = "(どの国にも入らない)"
    for region in built["regions"]:
        parent = region["names"]["country"]
        found: Counter = Counter()
        for polygon in region["polygons"]:
            px, py = lib.PolygonIndex(polygon).interior_point()
            hit = [cid for cid, pidx in index if pidx.contains(px, py)]
            found[hit[0] if hit else nowhere] += 1
        wrong = {k: v for k, v in found.items() if k != parent}
        if region["reassigned"]:
            # 明示した付け替え先は、代表点の一致を必須にする。既知の食い違いの一覧には混ぜない
            if wrong:
                reassign_failures.append(
                    f"{region['id']}（{region['declared']}→{parent}）: "
                    + ", ".join(f"{k}:{v}" for k, v in sorted(wrong.items()))
                )
            continue
        if wrong:
            other = {k: v for k, v in wrong.items() if k != nowhere}
            mismatches.append(
                {
                    "region": region["id"],
                    "name": region["names"]["name"],
                    "declared_country": region["declared"],
                    "assigned_country": parent,
                    "parts": sum(found.values()),
                    "parts_other_country": other,
                    "parts_in_no_country": wrong.get(nowhere, 0),
                }
            )
    other_country = [m for m in mismatches if m["parts_other_country"]]
    only_coast = [m for m in mismatches if not m["parts_other_country"]]
    if reassign_failures:
        raise lib.RegionDataError(
            "admin1_parent で付け替えた地域の代表点が付け替え先の国に入らない（付け替え先を直す）: "
            + "; ".join(reassign_failures)
        )
    lines = [
        "# Admin1（既定版）の各片の代表点を、Admin0（日本視点版）の国と突き合わせた食い違いの一覧",
        "# 列: Admin1 の ID / 名称 / Admin1 の宣言(adm0_a3) / 採用した国 / 片の数 / 別の国に入る片(国:片数) / どの国にも入らない片",
        "# 『別の国』は国境の見方の違い（北方領土など）、『どの国にも入らない』は海岸線の細部の違い（沖合の小島など）",
        "# 分類器では、国が決まった後に、その国の Admin1 に当たらない点は『地域不明』になる",
        "",
        f"## 別の国に入る片がある Admin1: {len(other_country)} 件",
    ]
    for m in other_country:
        elsewhere = ", ".join(f"{k}:{v}" for k, v in sorted(m["parts_other_country"].items()))
        lines.append(
            f"{m['region']}\t{m['name']}\t{m['declared_country']}\t{m['assigned_country']}\t{m['parts']}\t{elsewhere}\t{m['parts_in_no_country']}"
        )
    lines += ["", f"## 海岸線の違いだけ（どの国にも入らない片だけ）の Admin1: {len(only_coast)} 件"]
    for m in only_coast:
        lines.append(
            f"{m['region']}\t{m['name']}\t{m['declared_country']}\t{m['assigned_country']}\t{m['parts']}\t-\t{m['parts_in_no_country']}"
        )
    return mismatches, lines


# ---------------------------------------------------------------- 書き出し

def gz(data: bytes) -> bytes:
    # mtime=0・ファイル名なしで、同じ入力から同じ出力にする
    return gzip.compress(data, compresslevel=9, mtime=0)


def render(built: dict, sources: dict) -> dict:
    countries = built["countries"]
    regions = built["regions"]
    polygons_raw = lib.encode_polygons(
        [{"id": c["id"], "polygons": c["polygons"]} for c in countries],
        [{"id": r["id"], "country": r["country"], "polygons": r["polygons"]} for r in regions],
    )
    names = {
        "format": lib.FORMAT_VERSION,
        "countries": [c["names"] for c in countries],
        "regions": [r["names"] for r in regions],
    }
    names_raw = json.dumps(names, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
    return {
        "polygons_raw": polygons_raw,
        "polygons_gz": gz(polygons_raw),
        "names_raw": names_raw,
        "names_gz": gz(names_raw),
    }


def stats(built: dict) -> dict:
    out = {}
    for key in ("countries", "regions"):
        features = built[key]
        out[key] = {
            "features": len(features),
            "polygons": sum(len(f["polygons"]) for f in features),
            "rings": sum(len(p) for f in features for p in f["polygons"]),
            "holes": sum(len(p) - 1 for f in features for p in f["polygons"]),
            "vertices": sum(len(r) for f in features for p in f["polygons"] for r in p),
        }
    return out


def make_manifest(built: dict, sources: dict, blobs: dict) -> dict:
    s = stats(built)
    try:
        import shapefile

        pyshp_version = shapefile.__version__
    except ImportError:  # pragma: no cover
        pyshp_version = "unknown"
    layers = {}
    for key, layer in sources["layers"].items():
        layers[key] = {
            "stem": layer["stem"],
            "files": {
                ext: {
                    "url": sources["base_url"] + source_file_name(layer, ext),
                    "bytes": meta["bytes"],
                    "sha256": meta["sha256"],
                }
                for ext, meta in layer["files"].items()
            },
        }
    dataset = hashlib.sha256()
    dataset.update(sha256_hex(blobs["polygons_raw"]).encode())
    dataset.update(sha256_hex(blobs["names_raw"]).encode())
    return {
        "format": lib.FORMAT_VERSION,
        "source": {
            "name": sources["name"],
            "data_version": sources["data_version"],
            "license": sources["license"],
            "terms_url": sources["terms_url"],
            "git_repo": sources["git"]["repo"],
            "git_tag": sources["git"]["tag"],
            "git_commit": sources["git"]["commit"],
            "layers": layers,
        },
        "converter": {
            "script": "tools/regions/update_regions.py",
            "version": CONVERTER_VERSION,
            "pyshp": pyshp_version,
            "coordinates": "IEEE 754 倍精度のまま。丸め・簡略化なし",
        },
        "counts": {
            "countries": s["countries"],
            "regions": s["regions"],
            "excluded_admin1": built["excluded"],
        },
        "artifacts": {
            "polygons": {
                "file": "polygons.bin.gz",
                "bytes": len(blobs["polygons_gz"]),
                "sha256": sha256_hex(blobs["polygons_gz"]),
                "raw_bytes": len(blobs["polygons_raw"]),
                "raw_sha256": sha256_hex(blobs["polygons_raw"]),
                "max_bytes": LIMIT_POLYGONS_GZ,
                "max_raw_bytes": LIMIT_POLYGONS_RAW,
            },
            "names": {
                "file": "names.json.gz",
                "bytes": len(blobs["names_gz"]),
                "sha256": sha256_hex(blobs["names_gz"]),
                "raw_bytes": len(blobs["names_raw"]),
                "raw_sha256": sha256_hex(blobs["names_raw"]),
                "max_bytes": LIMIT_NAMES_GZ,
                "max_raw_bytes": LIMIT_NAMES_RAW,
            },
        },
        "max_total_bytes": LIMIT_TOTAL_GZ,
        "dataset_sha256": dataset.hexdigest(),
    }


def expand(gz: bytes, max_raw: int) -> bytes | None:
    """gzip を展開する。壊れている・`max_raw` を超えるときは None（展開爆弾を避けるため、上限 +1 バイトまでしか読まない）。"""
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(gz)) as f:
            raw = f.read(max_raw + 1)
    except (OSError, EOFError, zlib.error):
        return None
    return raw if len(raw) <= max_raw else None


def verify_committed(out: Path, committed: dict) -> dict[str, bytes]:
    """コミット済みの成果物そのものが、committed manifest の記録（圧縮後のサイズ・SHA256、展開後のサイズ・SHA256・上限）と
    一致するか。一致しなければ止まる。戻りは展開後の内容（名前→バイト列）。

    変換し直した結果との比較（zlib の版による gzip の違いを許す）より前に行う。成果物が壊れているのに、
    zlib の版違いとして『一致』と報告しないため。
    """
    raws: dict[str, bytes] = {}
    for name in ("polygons", "names"):
        art = committed["artifacts"][name]
        path = out / art["file"]
        if not path.is_file():
            raise SystemExit(f"{art['file']}: コミット済みの成果物が無い")
        gz = path.read_bytes()
        if len(gz) != art["bytes"] or sha256_hex(gz) != art["sha256"]:
            raise SystemExit(f"{art['file']}: 成果物のサイズまたは SHA256 が manifest と違う（壊れている）")
        raw = expand(gz, art["max_raw_bytes"])
        if raw is None:
            raise SystemExit(f"{art['file']}: gzip を展開できない、または展開後が上限を超える（壊れている）")
        if len(raw) != art["raw_bytes"] or sha256_hex(raw) != art["raw_sha256"]:
            raise SystemExit(f"{art['file']}: 展開後のサイズまたは SHA256 が manifest と違う（壊れている）")
        raws[name] = raw
    return raws


def keep_existing_gz(blobs: dict, out: Path) -> list[str]:
    """既存の成果物の gzip が、新しい展開後の内容と同じ内容に展開できるなら、既存のバイト列を使う。

    形状（展開後の内容）が同じなのに、zlib の版の違いだけで gzip のバイト列が変わり、履歴に blob が増えるのを避ける。
    戻りは既存を残した成果物の名前。
    """
    kept = []
    for name, file, limit in (("polygons", "polygons.bin.gz", LIMIT_POLYGONS_RAW), ("names", "names.json.gz", LIMIT_NAMES_RAW)):
        path = out / file
        if not path.is_file():
            continue
        old = path.read_bytes()
        if old != blobs[f"{name}_gz"] and expand(old, limit) == blobs[f"{name}_raw"]:
            blobs[f"{name}_gz"] = old
            kept.append(file)
    return kept


def convert(sources: dict, directory: Path, out: Path, write: bool) -> dict:
    verify_dir(sources, directory)
    built = build(directory, sources, HERE / "tables")
    blobs = render(built, sources)
    kept = keep_existing_gz(blobs, out) if write else []
    manifest = make_manifest(built, sources, blobs)
    total = manifest["artifacts"]["polygons"]["bytes"] + manifest["artifacts"]["names"]["bytes"]
    if total > manifest["max_total_bytes"]:
        raise SystemExit(
            f"成果物の合計 {total} B が上限 {manifest['max_total_bytes']} B を超えた。"
            "簡略化せずに止める（PRD AC10。ユーザーに諮る）"
        )
    mismatches, report_lines = cross_reference(built)
    manifest["counts"]["admin1_other_country"] = sum(1 for m in mismatches if m["parts_other_country"])
    manifest["counts"]["admin1_coastline_only"] = sum(1 for m in mismatches if not m["parts_other_country"])
    report = "\n".join(report_lines) + "\n"
    result = {
        "built": built,
        "blobs": blobs,
        "manifest": manifest,
        "report": report,
        "mismatches": mismatches,
        "kept_gz": kept,
    }
    if write:
        out.mkdir(parents=True, exist_ok=True)
        (out / "polygons.bin.gz").write_bytes(blobs["polygons_gz"])
        (out / "names.json.gz").write_bytes(blobs["names_gz"])
        (out / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        (HERE / "crossref-report.tsv").write_text(report, encoding="utf-8")
    return result


def print_summary(result: dict) -> None:
    m = result["manifest"]
    s = stats(result["built"])
    print(f"国 {s['countries']['features']} 件 / 地域 {s['regions']['features']} 件（除外 {len(result['built']['excluded'])} 件）")
    for key in ("countries", "regions"):
        v = s[key]
        print(f"  {key}: 多角形 {v['polygons']} / 輪 {v['rings']}（穴 {v['holes']}）/ 頂点 {v['vertices']}")
    for name, a in m["artifacts"].items():
        print(f"  {a['file']}: {a['bytes']} B（展開後 {a['raw_bytes']} B）")
    total = sum(a["bytes"] for a in m["artifacts"].values())
    print(f"  合計 {total} B / 上限 {m['max_total_bytes']} B")
    c = m["counts"]
    print(
        f"国参照の食い違う Admin1: 別の国に入る片あり {c['admin1_other_country']} 件 / 海岸線の違いだけ {c['admin1_coastline_only']} 件（crossref-report.tsv）"
    )
    for line in result["report"].splitlines():
        if line and not line.startswith("#"):
            print("   ", line)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", choices=["fetch", "convert", "check", "all"])
    ap.add_argument("--work", type=Path, default=DEFAULT_WORK, help="取得物の置き場（既定: tools/regions/.cache）")
    ap.add_argument("--source-dir", type=Path, help="取得済みの元データのディレクトリ（fetch を使わない）")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT, help="成果物の出力先")
    ap.add_argument("--print-hashes", action="store_true", help="fetch で、取得物のサイズと SHA256 を表示するだけにする（版を上げるとき）")
    args = ap.parse_args()

    sources = load_json(HERE / "sources.json")
    directory = args.source_dir
    if args.command in ("fetch", "all"):
        directory = fetch(sources, args.work, args.print_hashes)
        if args.command == "fetch":
            print(f"取得と検証が済んだ: {directory}")
            return
    elif directory is None:
        directory = args.work / sources["data_version"]

    try:
        if args.command == "check":
            committed = load_json(args.out / "manifest.json")
            verify_committed(args.out, committed)  # 既存の成果物自体の検証が先（壊れていれば、ここで止まる）
            result = convert(sources, directory, args.out, write=False)
            new = result["manifest"]
            for name in ("polygons", "names"):
                a, b = committed["artifacts"][name], new["artifacts"][name]
                if a["raw_sha256"] != b["raw_sha256"] or a["raw_bytes"] != b["raw_bytes"]:
                    raise SystemExit(f"{name}: 変換し直した内容がコミット済みの成果物と違う")
            if committed["dataset_sha256"] != new["dataset_sha256"] or committed["source"] != new["source"]:
                raise SystemExit("manifest の内容が違う")
            same_gz = all(
                (args.out / new["artifacts"][n]["file"]).read_bytes() == result["blobs"][f"{n}_gz"]
                for n in ("polygons", "names")
            )
            print_summary(result)
            print("一致: 展開後の内容と manifest が同じ" + (" / gzip もバイト単位で同じ" if same_gz else "（gzip のバイト列は違う。展開後の内容は一致するので zlib の版の違いと判断）"))
            return
        result = convert(sources, directory, args.out, write=True)
        print_summary(result)
        for file in result["kept_gz"]:
            print(f"{file}: 展開後の内容が同じなので、既存の gzip をそのまま残した")
        print(f"書き出した: {args.out}")
    except lib.RegionDataError as e:
        raise SystemExit(f"変換を拒否: {e}") from e


if __name__ == "__main__":
    main()
