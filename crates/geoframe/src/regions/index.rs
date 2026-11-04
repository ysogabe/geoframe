//! 境界の索引と点の内外判定（分類器）。設計は docs/international-places/design.md の「判定手順」。
//!
//! [`RegionIndex`] は不変で、`Arc` で共有できる。**呼び出し側が明示的に作り**（[`RegionIndex::load`]）、使い終えたら
//! 手放す。起動時やモジュールの読み込み時に自動では作らず、グローバルにも保持しない（初回の移行の後は、ほぼ毎回候補が
//! 0 件なので、展開の費用より常駐メモリを避ける）。
//!
//! # 判定
//!
//! 1. 緯度・経度は有限で、緯度 ±90・経度 ±180 以内のものだけ（[`InvalidCoordinate`]）。
//! 2. 国のポリゴンで、**内部または境界上**にある国を集める。同一国の複数片は 1 候補。0 国なら国は `outside`。
//! 3. 1 国なら、その国の地域だけを探す。1 地域なら `inside`、0 地域なら国だけ（地域 `outside`）、複数なら地域 `ambiguous`。
//! 4. 複数国なら国が `ambiguous`（地域は決めない）。国境の共有辺も同じ。配列の順や名前の順では決めない。
//!
//! 点の内外は偶奇判定で、境界上（辺・頂点。外側の輪も穴の縁も）は内部として扱う。境界上の判定は座標の厳密な一致
//! （外積がちょうど 0）なので、斜めの辺の上の点は、倍精度で厳密に載るときだけ境界上になる。それ以外は丸め誤差の範囲で
//! 内外のどちらかに決まる。
//!
//! # 索引
//!
//! 多角形ごとの外側の輪の包絡矩形を 1 度の格子に登録し（層ごと）、点の入る格子の候補だけを偶奇判定する。
//! 座標は [`super::data::PolygonSet`] の平らな配列をそのまま使い、追加するのは矩形と格子の表（数 MB）だけ。

use std::fmt;

use super::data::{self, DataError, Layer, PolygonSet};

/// 格子の大きさ（度）。経度 360 × 緯度 180 マス。
const CELL_DEG: f64 = 1.0;
const GRID_W: usize = 360;
const GRID_H: usize = 180;

/// 判定の方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// 1 件に決まった。
    Inside,
    /// 該当なし。
    Outside,
    /// 複数に当たり決められない。
    Ambiguous,
}

impl Method {
    /// DB に保存する文字列。
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Inside => "inside",
            Method::Outside => "outside",
            Method::Ambiguous => "ambiguous",
        }
    }
}

/// 緯度・経度が無効（有限でない、または緯度 ±90・経度 ±180 の外）。ポリゴン判定は行わない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCoordinate;

impl fmt::Display for InvalidCoordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("無効な座標")
    }
}

impl std::error::Error for InvalidCoordinate {}

/// 判定の結果。
///
/// - `country_method == Inside` のときだけ `country_id` が `Some`。
/// - `region_id` が `Some` なら、`region_method == Inside` で、その地域は `country_id` の国に属する（索引が保証する）。
/// - 国が `Outside` / `Ambiguous` のときは地域を探さず、`region_method` は国と同じ値で `region_id` は `None`。
/// - 国が `Inside` で地域が 0 件のときは `region_method == Outside`（国のみ）、複数のときは `Ambiguous`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification<'a> {
    pub country_id: Option<&'a str>,
    pub region_id: Option<&'a str>,
    pub country_method: Method,
    pub region_method: Method,
}

/// 境界の索引（不変）。
#[derive(Debug)]
pub struct RegionIndex {
    set: PolygonSet,
    countries: LayerIndex,
    regions: LayerIndex,
}

/// 1 層の格子。
#[derive(Debug)]
struct LayerIndex {
    /// 多角形ごとの外側の輪の包絡矩形 `[min_x, min_y, max_x, max_y]`。
    bbox: Vec<[f64; 4]>,
    /// 多角形 → 地物。
    feature: Vec<u32>,
    /// マスごとの候補（多角形の通し番号）の範囲。`cell_start[c]..cell_start[c+1]` を `cell_polys` に持つ。
    cell_start: Vec<u32>,
    cell_polys: Vec<u32>,
}

fn cell_x(x: f64) -> usize {
    (((x + 180.0) / CELL_DEG).floor().max(0.0) as usize).min(GRID_W - 1)
}

fn cell_y(y: f64) -> usize {
    (((y + 90.0) / CELL_DEG).floor().max(0.0) as usize).min(GRID_H - 1)
}

impl LayerIndex {
    fn build(layer: &Layer) -> Result<Self, DataError> {
        let polygons = layer.polygons_len();
        let mut bbox = Vec::with_capacity(polygons);
        let mut feature = Vec::with_capacity(polygons);
        for f in 0..layer.len() {
            for p in layer.polygon_range(f) {
                let outer = layer.ring(layer.ring_range(p).start);
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for &[x, y] in outer {
                    b[0] = b[0].min(x);
                    b[1] = b[1].min(y);
                    b[2] = b[2].max(x);
                    b[3] = b[3].max(y);
                }
                bbox.push(b);
                feature.push(f as u32);
            }
        }
        // 2 パス: マスごとの個数を数え、先頭位置を決めてから詰める
        let mut start = vec![0u32; GRID_W * GRID_H + 1];
        for b in &bbox {
            for cy in cell_y(b[1])..=cell_y(b[3]) {
                for cx in cell_x(b[0])..=cell_x(b[2]) {
                    start[cy * GRID_W + cx + 1] += 1;
                }
            }
        }
        for i in 1..start.len() {
            start[i] = start[i]
                .checked_add(start[i - 1])
                .ok_or_else(|| DataError("索引が大きすぎる".into()))?;
        }
        let mut fill = start.clone();
        let mut cell_polys = vec![0u32; *start.last().expect("格子") as usize];
        for (p, b) in bbox.iter().enumerate() {
            for cy in cell_y(b[1])..=cell_y(b[3]) {
                for cx in cell_x(b[0])..=cell_x(b[2]) {
                    let c = cy * GRID_W + cx;
                    cell_polys[fill[c] as usize] = p as u32;
                    fill[c] += 1;
                }
            }
        }
        Ok(Self {
            bbox,
            feature,
            cell_start: start,
            cell_polys,
        })
    }

    /// 点を含む（内部または境界上）地物のうち、異なるものを最大 2 つまで集める。`accept` は地物の絞り込み。
    /// 戻り値は（見つけた最初の地物, 2 つ目の地物）。順序に意味は無い（個数だけを使う）。
    fn hits(
        &self,
        layer: &Layer,
        x: f64,
        y: f64,
        accept: impl Fn(usize) -> bool,
    ) -> (Option<usize>, Option<usize>) {
        let cell = cell_y(y) * GRID_W + cell_x(x);
        let range = self.cell_start[cell] as usize..self.cell_start[cell + 1] as usize;
        let (mut first, mut second) = (None, None);
        for &p in &self.cell_polys[range] {
            let p = p as usize;
            let f = self.feature[p] as usize;
            if first == Some(f) || !accept(f) {
                continue;
            }
            let b = &self.bbox[p];
            if x < b[0] || x > b[2] || y < b[1] || y > b[3] {
                continue;
            }
            if polygon_contains(layer, p, x, y) {
                if first.is_none() {
                    first = Some(f);
                } else {
                    second = Some(f);
                    break;
                }
            }
        }
        (first, second)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RingState {
    Outside,
    Inside,
    Boundary,
}

/// 輪に対する点の位置（偶奇判定。辺・頂点の上は `Boundary`）。
fn ring_state(ring: &[[f64; 2]], px: f64, py: f64) -> RingState {
    let mut inside = false;
    for w in ring.windows(2) {
        let [ax, ay] = w[0];
        let [bx, by] = w[1];
        // 辺の包絡矩形の中でだけ、辺の上かを厳密に見る
        if px >= ax.min(bx) && px <= ax.max(bx) && py >= ay.min(by) && py <= ay.max(by) {
            let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
            if cross == 0.0 {
                return RingState::Boundary;
            }
        }
        // +x 方向の半直線が辺を横切るか（上端を含み下端を含まない半開区間で、頂点の二重数えを避ける）
        if (ay > py) != (by > py) {
            let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
            // 交点の x は px より右か。cross = (by-ay) * (交点x - px)
            if (cross > 0.0) == (by > ay) {
                inside = !inside;
            }
        }
    }
    if inside {
        RingState::Inside
    } else {
        RingState::Outside
    }
}

/// 多角形（外側の輪と穴）が点を含むか。境界上は、外側の縁も穴の縁も含む。
fn polygon_contains(layer: &Layer, polygon: usize, px: f64, py: f64) -> bool {
    let mut rings = layer.ring_range(polygon);
    let outer = rings.next().expect("多角形は輪を持つ");
    match ring_state(layer.ring(outer), px, py) {
        RingState::Outside => false,
        RingState::Boundary => true,
        RingState::Inside => {
            for hole in rings {
                match ring_state(layer.ring(hole), px, py) {
                    RingState::Boundary => return true,
                    RingState::Inside => return false,
                    RingState::Outside => {}
                }
            }
            true
        }
    }
}

impl RegionIndex {
    /// 同梱データを展開・検証して索引を作る。数十 MB の常駐メモリを使うので、使い終えたら手放す。
    pub fn load() -> Result<Self, DataError> {
        Self::from_polygons(data::load_polygons()?)
    }

    /// 復号済みのポリゴンから索引を作る（試験の合成データにも使う）。地域の所属する国の添字が範囲内であることを確かめる。
    pub fn from_polygons(set: PolygonSet) -> Result<Self, DataError> {
        let n = set.countries.len();
        if (0..set.regions.len()).any(|r| set.regions.parent(r).is_none_or(|c| c >= n)) {
            return Err(DataError("地域の国参照が不正".into()));
        }
        let countries = LayerIndex::build(&set.countries)?;
        let regions = LayerIndex::build(&set.regions)?;
        Ok(Self {
            set,
            countries,
            regions,
        })
    }

    /// 試験用: 読み込んだポリゴン。
    #[cfg(test)]
    pub(super) fn polygons(&self) -> &PolygonSet {
        &self.set
    }

    /// 試験用: 格子の表のバイト数（矩形・多角形→地物・マスの範囲・マスごとの候補）。
    #[cfg(test)]
    pub(super) fn grid_bytes(&self) -> usize {
        [&self.countries, &self.regions]
            .iter()
            .map(|l| {
                l.bbox.capacity() * 32
                    + l.feature.capacity() * 4
                    + l.cell_start.capacity() * 4
                    + l.cell_polys.capacity() * 4
            })
            .sum()
    }

    /// 国の数・地域の数。
    pub fn len(&self) -> (usize, usize) {
        (self.set.countries.len(), self.set.regions.len())
    }

    /// 緯度 `lat`・経度 `lon`（度）を判定する。
    pub fn classify(&self, lat: f64, lon: f64) -> Result<Classification<'_>, InvalidCoordinate> {
        if !(lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0) {
            return Err(InvalidCoordinate);
        }
        let (set, x, y) = (&self.set, lon, lat);
        let (first, second) = self.countries.hits(&set.countries, x, y, |_| true);
        let country = match (first, second) {
            (None, _) => return Ok(unplaced(Method::Outside)),
            (Some(_), Some(_)) => return Ok(unplaced(Method::Ambiguous)),
            (Some(c), None) => c,
        };
        let (first, second) = self.regions.hits(&set.regions, x, y, |r| {
            set.regions.parent(r) == Some(country)
        });
        let (region_id, region_method) = match (first, second) {
            (None, _) => (None, Method::Outside),
            (Some(_), Some(_)) => (None, Method::Ambiguous),
            (Some(r), None) => (Some(set.regions.id(r)), Method::Inside),
        };
        Ok(Classification {
            country_id: Some(set.countries.id(country)),
            region_id,
            country_method: Method::Inside,
            region_method,
        })
    }
}

fn unplaced(method: Method) -> Classification<'static> {
    Classification {
        country_id: None,
        region_id: None,
        country_method: method,
        region_method: method,
    }
}
