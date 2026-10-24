//! 特徴量から、完全重複・類似のグループと推奨を決める。現行 `quality.py` の `build_quality_rows`・`rank_group`。
//!
//! - **完全重複**: `content_sha256` が同じ写真は 1 つのグループ。
//! - **類似**: 撮影時刻の差が 3 分以内（ちょうど 3 分を含む）で、pHash のハミング距離が 12 以下。
//! - グループは、どちらかでつながった写真の連結成分（union-find）。2 枚以上のものだけが行になる。
//! - グループ内はスコア（鮮明さ 62%・露出 25%・コントラスト 13%から白飛び・黒つぶれの減点）の降順、同点は `id` の
//!   昇順。推奨は、SHA ごとに最上位の 1 枚を代表にして、代表の中の上位 2 枚（完全重複だけのグループは 1 枚）。
//! - `kind` は、グループの SHA が 1 種類だけなら `exact`、それ以外は `similar`。
//!
//! 入力は純粋なデータで、DB にも画像にも触れない（同じ入力なら同じ出力。Python との突き合わせの fixture はここを通る）。

use std::collections::HashMap;

use rusqlite::types::Value;

use super::features::Metrics;

/// 類似とみなす撮影時刻の差（秒）。
const TIME_WINDOW_S: i64 = 3 * 60;
/// 類似とみなす pHash のハミング距離の上限。
const PHASH_DISTANCE: u32 = 12;

/// グルーピングに入れる 1 枚。`display`・`source_md5`・`source_mtime` は `photo_quality` へそのまま写す
/// （SQLite の値のまま。Python も受け取った値をそのまま書く）。
#[derive(Debug, Clone)]
pub struct QualityPhoto {
    pub id: i64,
    pub display: Value,
    pub source_md5: Value,
    pub source_mtime: Value,
    /// 並べ替えとグループの順に使う文字列（NULL は空）。
    pub taken_at: String,
    /// `taken_at` を解釈した、naive な日時の通算秒。読めなければ `None`（類似の判定に入らない）。
    pub captured: Option<i64>,
    pub sha256: String,
    pub phash: u64,
    pub metrics: Metrics,
}

/// `photo_quality` の 1 行（`analyzed_at`・`algorithm` は書くときに付ける）。
#[derive(Debug, Clone, PartialEq)]
pub struct QualityRow {
    pub photo_id: i64,
    pub display: Value,
    pub source_md5: Value,
    pub source_mtime: Value,
    pub content_sha256: String,
    pub group_id: i64,
    pub kind: &'static str,
    pub rank: i64,
    pub recommended: bool,
    pub score: f64,
    /// `json.dumps(list, ensure_ascii=False)` と同じ文字列。
    pub reasons_json: String,
    pub issues_json: String,
}

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u32>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        Self {
            parent: (0..size).collect(),
            rank: vec![0; size],
        }
    }

    fn find(&mut self, mut value: usize) -> usize {
        while self.parent[value] != value {
            self.parent[value] = self.parent[self.parent[value]];
            value = self.parent[value];
        }
        value
    }

    fn union(&mut self, left: usize, right: usize) {
        let (mut left, mut right) = (self.find(left), self.find(right));
        if left == right {
            return;
        }
        if self.rank[left] < self.rank[right] {
            std::mem::swap(&mut left, &mut right);
        }
        self.parent[right] = left;
        if self.rank[left] == self.rank[right] {
            self.rank[left] += 1;
        }
    }
}

/// Python の `round(x, 1)`（10 進で最も近い値。中間は偶数側）。`{:.1}` の文字列化が同じ丸めをする。
pub(super) fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Python の `math.isclose(a, b)`（相対 1e-9）。
fn is_close(a: f64, b: f64) -> bool {
    (a - b).abs() <= (1e-9 * a.abs().max(b.abs())).max(0.0)
}

fn normalize(value: f64, values: &[f64]) -> f64 {
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if is_close(lo, hi) {
        0.5
    } else {
        (value - lo) / (hi - lo)
    }
}

/// `statistics.median`（偶数個は中央 2 つの平均）。
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

struct Ranked {
    score: f64,
    reasons: Vec<&'static str>,
    issues: Vec<&'static str>,
}

fn rank_group(photos: &[&QualityPhoto]) -> Vec<Ranked> {
    let sharpness: Vec<f64> = photos.iter().map(|p| p.metrics.sharpness).collect();
    let exposure: Vec<f64> = photos.iter().map(|p| p.metrics.exposure_score).collect();
    let contrast: Vec<f64> = photos.iter().map(|p| p.metrics.contrast).collect();
    let median_sharpness = median(&sharpness);
    let max_sharpness = sharpness.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    photos
        .iter()
        .map(|photo| {
            let m = &photo.metrics;
            let sharp_rel = normalize(m.sharpness, &sharpness);
            let exposure_rel =
                0.55 * (m.exposure_score / 100.0) + 0.45 * normalize(m.exposure_score, &exposure);
            let contrast_rel = normalize(m.contrast, &contrast);
            let clip_penalty = ((m.black_clip + m.white_clip) * 0.8).min(0.20);
            let score = round1(
                100.0
                    * (0.62 * sharp_rel + 0.25 * exposure_rel + 0.13 * contrast_rel - clip_penalty)
                        .clamp(0.0, 1.0),
            );
            let mut issues = Vec::new();
            if m.sharpness < median_sharpness * 0.82 {
                issues.push("ぼけ傾向");
            }
            if m.exposure_score < 45.0 {
                issues.push("露出要確認");
            }
            if m.white_clip > 0.08 {
                issues.push("白飛び多め");
            }
            if m.black_clip > 0.15 {
                issues.push("黒つぶれ多め");
            }
            if m.contrast < 45.0 {
                issues.push("低コントラスト");
            }
            let mut reasons = Vec::new();
            if m.sharpness >= max_sharpness * 0.98 {
                reasons.push("グループ内で最も鮮明");
            } else if m.sharpness >= median_sharpness {
                reasons.push("鮮明さ良好");
            }
            if m.exposure_score >= 75.0 {
                reasons.push("露出良好");
            }
            if m.black_clip + m.white_clip < 0.015 {
                reasons.push("白飛び・黒つぶれ少");
            }
            if reasons.is_empty() {
                reasons.push("総合スコア上位");
            }
            Ranked {
                score,
                reasons,
                issues,
            }
        })
        .collect()
}

/// `json.dumps(items, ensure_ascii=False)`。
fn json_list(items: &[&str]) -> String {
    let body: Vec<String> = items.iter().map(|s| format!("\"{s}\"")).collect();
    format!("[{}]", body.join(", "))
}

struct Group {
    exact: bool,
    /// 順位順（スコアの降順、同点は `id`）に、`photos` の添字と、その順位での行の材料。
    ranked: Vec<(usize, Ranked)>,
    recommended: Vec<i64>,
    confidence: f64,
}

/// 同じ SHA-256（＝同じバイト列）の写真は、`id` が最小の行の pHash・指標にそろえる。同じ入力なら特徴量は
/// 同じはずで、違うのは算法が違う行（Python が作った行と Rust が作った行）が混ざったときだけ。そろえないと、
/// 同一ファイルの 2 枚のスコアが算法の差で食い違い、どちらを推奨にするかが算法の差で決まってしまう。
/// Python だけが作った行では、同じ SHA の特徴量は同じなので、何も変わらない。
fn unify_same_content(photos: &[QualityPhoto]) -> Vec<QualityPhoto> {
    let mut first: HashMap<&str, &QualityPhoto> = HashMap::new();
    for photo in photos {
        first.entry(photo.sha256.as_str()).or_insert(photo);
    }
    photos
        .iter()
        .map(|photo| {
            let head = first[photo.sha256.as_str()];
            QualityPhoto {
                phash: head.phash,
                metrics: head.metrics,
                ..photo.clone()
            }
        })
        .collect()
}

/// 最新の特徴量から、グループに属する行だけを作る。`photos` は `id` の昇順（現行の `ORDER BY p.id`）。
pub fn build_quality_rows(photos: &[QualityPhoto]) -> Vec<QualityRow> {
    let photos = &unify_same_content(photos)[..];
    let mut union = UnionFind::new(photos.len());
    // 完全重複: 同じ SHA の 2 枚目以降を最初の 1 枚につなぐ
    let mut first_by_sha: HashMap<&str, usize> = HashMap::new();
    for (index, photo) in photos.iter().enumerate() {
        match first_by_sha.get(photo.sha256.as_str()) {
            Some(&first) => union.union(first, index),
            None => {
                first_by_sha.insert(photo.sha256.as_str(), index);
            }
        }
    }
    // 類似: 撮影時刻の順（同時刻は `id` 順）に、3 分以内の窓の中で pHash を比べる
    let mut timed: Vec<usize> = (0..photos.len())
        .filter(|&i| photos[i].captured.is_some())
        .collect();
    timed.sort_by_key(|&i| photos[i].captured);
    let mut left = 0;
    for right in 0..timed.len() {
        let current = &photos[timed[right]];
        let at = current.captured.unwrap_or_default();
        while left < right && at - photos[timed[left]].captured.unwrap_or_default() > TIME_WINDOW_S
        {
            left += 1;
        }
        for &previous in &timed[left..right] {
            if (current.phash ^ photos[previous].phash).count_ones() <= PHASH_DISTANCE {
                union.union(timed[right], previous);
            }
        }
    }
    // 連結成分。順序は、成分の最小の添字（＝最小の `id`）の昇順
    let mut bucket_of_root: HashMap<usize, usize> = HashMap::new();
    let mut buckets: Vec<Vec<usize>> = Vec::new();
    for index in 0..photos.len() {
        let root = union.find(index);
        let bucket = *bucket_of_root.entry(root).or_insert_with(|| {
            buckets.push(Vec::new());
            buckets.len() - 1
        });
        buckets[bucket].push(index);
    }
    let mut groups: Vec<Group> = Vec::new();
    for bucket in buckets.iter().filter(|b| b.len() >= 2) {
        let members: Vec<&QualityPhoto> = bucket.iter().map(|&i| &photos[i]).collect();
        let scored = rank_group(&members);
        let mut order: Vec<usize> = (0..members.len()).collect();
        order.sort_by(|&a, &b| {
            scored[b]
                .score
                .total_cmp(&scored[a].score)
                .then(members[a].id.cmp(&members[b].id))
        });
        let exact = members.iter().all(|p| p.sha256 == members[0].sha256);
        // SHA ごとに最上位の 1 枚が代表。代表の中の上位 2 枚が推奨
        let mut seen: Vec<&str> = Vec::new();
        let mut pool: Vec<usize> = Vec::new();
        for &i in &order {
            if !seen.contains(&members[i].sha256.as_str()) {
                seen.push(members[i].sha256.as_str());
                pool.push(i);
            }
        }
        let recommended: Vec<i64> = pool.iter().take(2).map(|&i| members[i].id).collect();
        let confidence = if pool.len() > 1 {
            round1(scored[pool[0]].score - scored[pool[(pool.len() - 1).min(2)]].score)
        } else {
            100.0
        };
        let mut scored: Vec<Option<Ranked>> = scored.into_iter().map(Some).collect();
        let ranked = order
            .iter()
            .filter_map(|&i| scored[i].take().map(|r| (bucket[i], r)))
            .collect();
        groups.push(Group {
            exact,
            ranked,
            recommended,
            confidence,
        });
    }
    // exact を先、次に大きい順、次に推奨の信頼度が高い順、次に先頭の撮影時刻の文字列（安定ソート）
    groups.sort_by(|a, b| {
        (!a.exact)
            .cmp(&!b.exact)
            .then(b.ranked.len().cmp(&a.ranked.len()))
            .then(b.confidence.total_cmp(&a.confidence))
            .then(
                photos[a.ranked[0].0]
                    .taken_at
                    .cmp(&photos[b.ranked[0].0].taken_at),
            )
    });
    let mut rows = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        for (rank, (photo_index, ranked)) in group.ranked.iter().enumerate() {
            let photo = &photos[*photo_index];
            rows.push(QualityRow {
                photo_id: photo.id,
                display: photo.display.clone(),
                source_md5: photo.source_md5.clone(),
                source_mtime: photo.source_mtime.clone(),
                content_sha256: photo.sha256.clone(),
                group_id: index as i64 + 1,
                kind: if group.exact { "exact" } else { "similar" },
                rank: rank as i64 + 1,
                recommended: group.recommended.contains(&photo.id),
                score: ranked.score,
                reasons_json: json_list(&ranked.reasons),
                issues_json: json_list(&ranked.issues),
            });
        }
    }
    rows
}
