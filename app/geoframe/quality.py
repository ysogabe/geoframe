"""新規・更新写真だけを解析し、表示候補の品質順位を更新する。

画像からの特徴量抽出は同期サイクルごとに少量ずつ行う。全表示写真の特徴量が
そろった時だけ photo_quality を原子的に差し替えるため、途中結果で表示候補が
揺れたり、一時的に完全重複写真が復活したりしない。
"""
from __future__ import annotations

import hashlib
import json
import math
import statistics
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Iterable

from PIL import Image, ImageFilter, ImageOps, ImageStat

from .config import DATA_DIR, QUALITY_PER_CYCLE
from .store import dbm, dbq, dbx, replace_photo_quality, setting_get, setting_set


PHASH_SIZE = 32
PHASH_LOW = 8
TIME_WINDOW_MIN = 3.0
PHASH_DISTANCE = 12
ALGORITHM = "poc-phash-v1:t3:h12"
_PHASH_COS = [
    [math.cos(math.pi * (2 * x + 1) * u / (2 * PHASH_SIZE)) for x in range(PHASH_SIZE)]
    for u in range(PHASH_LOW)
]


@dataclass
class Metrics:
    sharpness: float
    luminance: float
    exposure_score: float
    black_clip: float
    white_clip: float
    contrast: float


@dataclass
class QualityPhoto:
    id: int
    display: str
    source_md5: str | None
    source_mtime: float | None
    taken_at: str
    sha256: str
    phash: int
    metrics: Metrics
    captured: datetime | None = None
    score: float = 0.0
    reasons: list[str] = field(default_factory=list)
    issues: list[str] = field(default_factory=list)


class UnionFind:
    def __init__(self, size: int):
        self.parent = list(range(size))
        self.rank = [0] * size

    def find(self, value: int) -> int:
        while self.parent[value] != value:
            self.parent[value] = self.parent[self.parent[value]]
            value = self.parent[value]
        return value

    def union(self, left: int, right: int) -> None:
        left, right = self.find(left), self.find(right)
        if left == right:
            return
        if self.rank[left] < self.rank[right]:
            left, right = right, left
        self.parent[right] = left
        if self.rank[left] == self.rank[right]:
            self.rank[left] += 1


def parse_taken_at(value: str) -> datetime | None:
    if not value:
        return None
    for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"):
        try:
            return datetime.strptime(value, fmt)
        except ValueError:
            pass
    return None


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def perceptual_hash(image: Image.Image) -> int:
    """32x32 DCTの左上8x8から作る64-bit pHash。"""
    gray = ImageOps.fit(image.convert("L"), (PHASH_SIZE, PHASH_SIZE),
                        method=Image.Resampling.LANCZOS)
    pixels = list(gray.get_flattened_data())
    rows = [pixels[y * PHASH_SIZE:(y + 1) * PHASH_SIZE] for y in range(PHASH_SIZE)]
    horizontal = [[0.0] * PHASH_LOW for _ in range(PHASH_SIZE)]
    for y, row in enumerate(rows):
        for u in range(PHASH_LOW):
            horizontal[y][u] = sum(row[x] * _PHASH_COS[u][x] for x in range(PHASH_SIZE))
    coeffs = []
    for v in range(PHASH_LOW):
        for u in range(PHASH_LOW):
            coeffs.append(sum(horizontal[y][u] * _PHASH_COS[v][y]
                              for y in range(PHASH_SIZE)))
    median = statistics.median(coeffs[1:])
    value = 0
    for coefficient in coeffs:
        value = (value << 1) | int(coefficient > median)
    return value


def _percentile(histogram: list[int], fraction: float, total: int) -> int:
    target, current = total * fraction, 0
    for index, count in enumerate(histogram):
        current += count
        if current >= target:
            return index
    return 255


def image_metrics(image: Image.Image) -> Metrics:
    work = image.copy()
    work.thumbnail((512, 512), Image.Resampling.LANCZOS)
    gray = work.convert("L")
    histogram = gray.histogram()
    total = max(1, sum(histogram))
    luminance = sum(index * count for index, count in enumerate(histogram)) / total
    black_clip = sum(histogram[:8]) / total
    white_clip = sum(histogram[248:]) / total
    contrast = float(_percentile(histogram, 0.95, total)
                     - _percentile(histogram, 0.05, total))
    edge = gray.filter(ImageFilter.FIND_EDGES)
    if edge.width > 8 and edge.height > 8:
        edge = edge.crop((4, 4, edge.width - 4, edge.height - 4))
    sharpness = math.log1p(ImageStat.Stat(edge).var[0])
    center_score = 100.0 * math.exp(-((luminance - 128.0) / 100.0) ** 2)
    clip_penalty = min(55.0, (black_clip + white_clip) * 260.0)
    low_contrast_penalty = max(0.0, 45.0 - contrast) * 0.45
    exposure_score = max(0.0, min(100.0,
                                  center_score - clip_penalty - low_contrast_penalty))
    return Metrics(sharpness, luminance, exposure_score, black_clip, white_clip, contrast)


def analyze_path(path: Path) -> tuple[str, int, Metrics]:
    digest = sha256_file(path)
    with Image.open(path) as image:
        image = ImageOps.exif_transpose(image)
        return digest, perceptual_hash(image), image_metrics(image)


def _normalize(value: float, values: Iterable[float]) -> float:
    values = list(values)
    lo, hi = min(values), max(values)
    if math.isclose(lo, hi):
        return 0.5
    return (value - lo) / (hi - lo)


def rank_group(photos: list[QualityPhoto]) -> None:
    sharpness_values = [photo.metrics.sharpness for photo in photos]
    exposure_values = [photo.metrics.exposure_score for photo in photos]
    contrast_values = [photo.metrics.contrast for photo in photos]
    median_sharpness = statistics.median(sharpness_values)
    for photo in photos:
        metric = photo.metrics
        sharp_rel = _normalize(metric.sharpness, sharpness_values)
        exposure_rel = 0.55 * (metric.exposure_score / 100.0) + 0.45 * _normalize(
            metric.exposure_score, exposure_values)
        contrast_rel = _normalize(metric.contrast, contrast_values)
        clip_penalty = min(0.20, (metric.black_clip + metric.white_clip) * 0.8)
        photo.score = round(100.0 * max(0.0, min(1.0,
            0.62 * sharp_rel + 0.25 * exposure_rel + 0.13 * contrast_rel - clip_penalty)), 1)
        photo.issues = []
        if metric.sharpness < median_sharpness * 0.82:
            photo.issues.append("ぼけ傾向")
        if metric.exposure_score < 45:
            photo.issues.append("露出要確認")
        if metric.white_clip > 0.08:
            photo.issues.append("白飛び多め")
        if metric.black_clip > 0.15:
            photo.issues.append("黒つぶれ多め")
        if metric.contrast < 45:
            photo.issues.append("低コントラスト")
        photo.reasons = []
        if metric.sharpness >= max(sharpness_values) * 0.98:
            photo.reasons.append("グループ内で最も鮮明")
        elif metric.sharpness >= median_sharpness:
            photo.reasons.append("鮮明さ良好")
        if metric.exposure_score >= 75:
            photo.reasons.append("露出良好")
        if metric.black_clip + metric.white_clip < 0.015:
            photo.reasons.append("白飛び・黒つぶれ少")
        if not photo.reasons:
            photo.reasons.append("総合スコア上位")


def build_quality_rows(photos: list[QualityPhoto], analyzed_at: str) -> list[tuple]:
    """最新特徴量から、重複・類似グループに属する行だけを生成する。"""
    positions = {id(photo): index for index, photo in enumerate(photos)}
    union = UnionFind(len(photos))
    by_sha: dict[str, list[QualityPhoto]] = {}
    for photo in photos:
        by_sha.setdefault(photo.sha256, []).append(photo)
    for duplicates in by_sha.values():
        for photo in duplicates[1:]:
            union.union(positions[id(duplicates[0])], positions[id(photo)])
    timed = sorted((photo for photo in photos if photo.captured),
                   key=lambda photo: photo.captured)
    left = 0
    for right, current in enumerate(timed):
        while left < right and (current.captured - timed[left].captured).total_seconds() > TIME_WINDOW_MIN * 60:
            left += 1
        for previous in timed[left:right]:
            if (current.phash ^ previous.phash).bit_count() <= PHASH_DISTANCE:
                union.union(positions[id(current)], positions[id(previous)])
    buckets: dict[int, list[QualityPhoto]] = {}
    for index, photo in enumerate(photos):
        buckets.setdefault(union.find(index), []).append(photo)
    groups = []
    for bucket in buckets.values():
        if len(bucket) < 2:
            continue
        rank_group(bucket)
        kind = "exact" if len({photo.sha256 for photo in bucket}) == 1 else "similar"
        representatives: dict[str, QualityPhoto] = {}
        for photo in sorted(bucket, key=lambda item: (-item.score, item.id)):
            representatives.setdefault(photo.sha256, photo)
        pool = sorted(representatives.values(), key=lambda item: (-item.score, item.id))
        recommendations = {photo.id for photo in pool[:2]}
        ranked_scores = [photo.score for photo in pool]
        confidence = (ranked_scores[0] - ranked_scores[min(2, len(ranked_scores) - 1)]
                      if len(ranked_scores) > 1 else 100.0)
        ranked = sorted(bucket, key=lambda item: (-item.score, item.id))
        groups.append((kind, ranked, recommendations, round(confidence, 1)))
    groups.sort(key=lambda group: (group[0] != "exact", -len(group[1]), -group[3],
                                   group[1][0].taken_at))
    rows = []
    for group_id, (kind, ranked, recommendations, _confidence) in enumerate(groups, 1):
        for rank, photo in enumerate(ranked, 1):
            rows.append((
                photo.id, photo.display, photo.source_md5, photo.source_mtime,
                photo.sha256, group_id, kind, rank, int(photo.id in recommendations),
                photo.score, json.dumps(photo.reasons, ensure_ascii=False),
                json.dumps(photo.issues, ensure_ascii=False), ALGORITHM, analyzed_at,
            ))
    return rows


def _pending_rows(limit: int) -> list:
    return dbq("""SELECT p.id,p.display,p.md5,p.mtime
                  FROM photos p
                  LEFT JOIN photo_quality_features f
                    ON f.photo_id=p.id AND f.display=p.display
                   AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                  WHERE p.display IS NOT NULL AND p.display!='' AND f.photo_id IS NULL
                  ORDER BY p.id LIMIT ?""", (limit,))


def pending_count() -> int:
    return dbq("""SELECT COUNT(*) c FROM photos p
                  LEFT JOIN photo_quality_features f
                    ON f.photo_id=p.id AND f.display=p.display
                   AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                  WHERE p.display IS NOT NULL AND p.display!='' AND f.photo_id IS NULL""")[0]["c"]


def _analyze_pending(limit: int) -> int:
    rows = _pending_rows(limit)
    if not rows:
        return 0
    analyzed_at = datetime.now().astimezone().isoformat(timespec="seconds")
    values = []
    for row in rows:
        digest = phash_hex = None
        metrics = None
        error = ""
        try:
            root = DATA_DIR.resolve()
            path = (root / row["display"]).resolve()
            path.relative_to(root)
            digest, phash, metrics = analyze_path(path)
            phash_hex = f"{phash:016x}"
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"[:500]
        values.append((
            row["id"], row["display"], row["md5"], row["mtime"], digest, phash_hex,
            metrics.sharpness if metrics else None,
            metrics.luminance if metrics else None,
            metrics.exposure_score if metrics else None,
            metrics.black_clip if metrics else None,
            metrics.white_clip if metrics else None,
            metrics.contrast if metrics else None,
            error, analyzed_at,
        ))
    dbm("""INSERT INTO photo_quality_features(
             photo_id,display,source_md5,source_mtime,content_sha256,phash,
             sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at
           ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(photo_id) DO UPDATE SET
             display=excluded.display,source_md5=excluded.source_md5,
             source_mtime=excluded.source_mtime,content_sha256=excluded.content_sha256,
             phash=excluded.phash,sharpness=excluded.sharpness,luminance=excluded.luminance,
             exposure_score=excluded.exposure_score,black_clip=excluded.black_clip,
             white_clip=excluded.white_clip,contrast=excluded.contrast,error=excluded.error,
             analyzed_at=excluded.analyzed_at""", values)
    setting_set("photo_quality_dirty", "1")
    errors = sum(bool(value[-2]) for value in values)
    print(f"[quality] 特徴量 {len(values)}件を解析（失敗{errors}件）")
    return len(values)


def _remove_orphans() -> int:
    count = dbq("""SELECT COUNT(*) c FROM photo_quality_features f
                   WHERE NOT EXISTS (
                     SELECT 1 FROM photos p WHERE p.id=f.photo_id
                       AND p.display IS NOT NULL AND p.display!='')""")[0]["c"]
    if count:
        dbx("""DELETE FROM photo_quality_features
               WHERE NOT EXISTS (
                 SELECT 1 FROM photos p WHERE p.id=photo_quality_features.photo_id
                   AND p.display IS NOT NULL AND p.display!='')""")
        setting_set("photo_quality_dirty", "1")
    return count


def _load_features() -> list[QualityPhoto]:
    rows = dbq("""SELECT p.id,p.display,p.md5,p.mtime,p.taken_at,
                         f.content_sha256,f.phash,f.sharpness,f.luminance,
                         f.exposure_score,f.black_clip,f.white_clip,f.contrast
                  FROM photos p JOIN photo_quality_features f
                    ON f.photo_id=p.id AND f.display=p.display
                   AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                  WHERE p.display IS NOT NULL AND p.display!='' AND f.error=''
                    AND f.content_sha256 IS NOT NULL AND f.phash IS NOT NULL
                  ORDER BY p.id""")
    photos = []
    for row in rows:
        photo = QualityPhoto(
            row["id"], row["display"], row["md5"], row["mtime"], row["taken_at"] or "",
            row["content_sha256"], int(row["phash"], 16),
            Metrics(row["sharpness"], row["luminance"], row["exposure_score"],
                    row["black_clip"], row["white_clip"], row["contrast"]),
        )
        photo.captured = parse_taken_at(photo.taken_at)
        photos.append(photo)
    return photos


def rebuild_quality_if_ready() -> tuple[bool, int]:
    remaining = pending_count()
    if remaining or setting_get("photo_quality_dirty", "0") != "1":
        return False, remaining
    analyzed_at = datetime.now().astimezone().isoformat(timespec="seconds")
    quality_rows = build_quality_rows(_load_features(), analyzed_at)
    replace_photo_quality(quality_rows)
    setting_set("photo_quality_dirty", "0")
    print(f"[quality] 品質結果を更新: グループ対象{len(quality_rows)}件")
    return True, 0


def quality_cycle(limit: int = QUALITY_PER_CYCLE) -> dict:
    """同期ワーカーから呼ぶ1回分。画像解析数は limit を超えない。"""
    removed = _remove_orphans()
    analyzed = _analyze_pending(max(1, limit))
    rebuilt, remaining = rebuild_quality_if_ready()
    return {"analyzed": analyzed, "removed": removed,
            "remaining": remaining, "rebuilt": rebuilt}


def quality_backfill(batch_size: int = QUALITY_PER_CYCLE) -> dict:
    """初回導入や保守用。待ちがなくなるまで同期的に特徴量を作る。"""
    total = 0
    _remove_orphans()
    while True:
        count = _analyze_pending(max(1, batch_size))
        total += count
        if not count:
            break
    rebuilt, remaining = rebuild_quality_if_ready()
    return {"analyzed": total, "remaining": remaining, "rebuilt": rebuilt}
