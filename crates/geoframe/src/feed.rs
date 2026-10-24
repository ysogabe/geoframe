//! `GET /api/v1/photos` が返す写真1件の型（応答の形）。
//!
//! 契約は [`docs/architecture/photo-feed-contract.md`] と
//! [`docs/architecture/photo-feed.schema.json`] に定める。この型は現行の Python 実装
//! （`app/geoframe/photos.py:api_photos`）が出すキー・JSON 型・`null` の出し方をそのまま
//! 持つので、T1.5 で Python と Rust の応答を全件比較できる。
//!
//! 応答は [`Photo`] の JSON 配列。DB からの取得・除外・並び・旅番号の計算は T1.2 の仕事で、
//! ここでは形だけを固定する。
//!
//! [`docs/architecture/photo-feed-contract.md`]: ../../../docs/architecture/photo-feed-contract.md
//! [`docs/architecture/photo-feed.schema.json`]: ../../../docs/architecture/photo-feed.schema.json

use serde::{Deserialize, Serialize};

/// 品質解析のグループの種類。JSON では `"exact"` / `"similar"` / 空文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityKind {
    /// グループに入らない、または未解析（JSON は空文字）
    #[serde(rename = "")]
    None,
    /// 完全に同じ写真
    #[serde(rename = "exact")]
    Exact,
    /// 3分以内に撮った見た目のほぼ同じ写真
    #[serde(rename = "similar")]
    Similar,
}

/// 写真1件。フィールドの並びは Python の出力順に合わせている（JSON のキー順に意味はない）。
///
/// 「契約」と書いたフィールドは表示クライアントが頼ってよい。それ以外は内部値で、予告なく
/// 変わる。ただし移行中は Python と同じ値を出す（T1.5 で全件比較する）。
///
/// 契約フィールドには `#[serde(default)]` を付けない。欠けた応答は読み込みに失敗させる
/// （`Option` の内部値だけは serde の既定で、欠けたら `None` になる）。未知のキーは読み飛ばす
/// （`deny_unknown_fields` は付けない）。`Option` のフィールドは Python と同じく、値が無い
/// とき `null` を出す（キーを省かない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Photo {
    /// 契約。写真のID。一覧の中で一意で、取り直しても変わらない。
    pub id: i64,
    /// 契約。GEOFRAME へ取り込んだ日時（`YYYY-MM-DDTHH:MM:SS`、ローカル時刻）。不明なら空文字。
    /// 書式は [`is_fetched_at`]。
    pub fetched_at: String,
    /// 契約。利用者がお気に入りにした写真。
    pub fav: bool,
    /// 契約。表示用画像の相対パス（`derivatives/<id>.jpg` など）。
    pub file: String,
    /// 契約。表示名。空にはならない。
    pub title: String,
    /// 契約。緯度（度）。一覧には位置のある写真だけが入る。
    pub lat: f64,
    /// 契約。経度（度）。
    pub lon: f64,
    /// 契約。撮影日時（`YYYY-MM-DD HH:MM`、ローカル時刻）。不明なら空文字。
    /// 書式は [`is_time`]。
    pub time: String,
    /// 内部値。町名など。無ければ空文字。
    pub place: String,
    /// 契約。都道府県名。引けなければ空文字。
    pub pref: String,
    /// 契約。市区町村名。引けなければ空文字。
    pub city: String,
    /// 内部値。標高（m）。無ければ `null`。
    pub elev: Option<f64>,
    /// 内部値。位置を前後の写真から推定した写真か（`loc_source == "inferred"`）。
    pub inferred: bool,
    /// 内部値。品質スコア。グループに入らなければ `null`。
    pub quality_score: Option<f64>,
    /// 内部値。品質解析のグループ番号（整数）。グループに入らなければ `null`。
    pub quality_group: Option<i64>,
    /// 契約。品質解析のグループの種類。
    pub quality_kind: QualityKind,
    /// 内部値。品質解析のグループ内の順位。グループに入らなければ `null`。
    pub quality_rank: Option<i64>,
    /// 契約。品質解析でグループの上位として推奨された写真。グループに入らなければ `false`。
    pub quality_recommended: bool,
    /// 内部値。`"recommended"` / `"alternate"`、グループに入らなければ空文字。
    pub quality_role: String,
    /// 内部値。AI整理のグループID（文字列）。入らなければ空文字。
    pub ai_group: String,
    /// 内部値。AI整理のグループの状態。無ければ空文字。
    pub ai_status: String,
    /// 内部値。AI整理で同じ場面と判定されたか。グループに入らなければ `false`。
    pub ai_same_scene: bool,
    /// 内部値。AI整理のグループ内の順位。入らなければ `null`。
    pub ai_rank: Option<i64>,
    /// 内部値。AI整理で推奨された写真か。入らなければ `false`。
    pub ai_recommended: bool,
    /// 内部値。AI整理の事前スコア。入らなければ `null`。
    pub ai_local_score: Option<f64>,
    /// 契約。撮影日時を48時間超の間隔で区切った旅の番号（0から）。
    /// **この一覧の中だけで意味を持つ**（写真が増えると番号がずれる）。
    pub trip: i64,
}

/// `YYYY-MM-DD HH:MM`（撮影日時）か、空文字（不明）。範囲は日付が月1〜12・日1〜31、
/// 時刻が00:00〜23:59まで見る（月ごとの日数や閏年までは見ない）。
pub fn is_time(s: &str) -> bool {
    s.is_empty() || matches_datetime(s, b' ', false)
}

/// `YYYY-MM-DDTHH:MM:SS`（取り込み日時）か、空文字（不明）。範囲の見方は [`is_time`] と同じ。
pub fn is_fetched_at(s: &str) -> bool {
    s.is_empty() || matches_datetime(s, b'T', true)
}

fn matches_datetime(s: &str, sep: u8, seconds: bool) -> bool {
    let b = s.as_bytes();
    let len = if seconds { 19 } else { 16 };
    if b.len() != len {
        return false;
    }
    let digits = |from: usize, to: usize| {
        b[from..to]
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| s[from..to].parse::<u32>().ok())
            .flatten()
    };
    let in_range = |v: Option<u32>, lo: u32, hi: u32| v.is_some_and(|v| (lo..=hi).contains(&v));
    b[4] == b'-'
        && b[7] == b'-'
        && b[10] == sep
        && b[13] == b':'
        && digits(0, 4).is_some()
        && in_range(digits(5, 7), 1, 12)
        && in_range(digits(8, 10), 1, 31)
        && in_range(digits(11, 13), 0, 23)
        && in_range(digits(14, 16), 0, 59)
        && (!seconds || (b[16] == b':' && in_range(digits(17, 19), 0, 59)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format() {
        for ok in ["", "2025-06-07 11:00", "2025-12-31 23:59"] {
            assert!(is_time(ok), "{ok:?}");
        }
        for ng in [
            "2025-06-07",
            "2025-06-07T11:00",
            "2025-06-07 11:00:00",
            "2025-13-07 11:00",
            "2025-06-07 24:00",
            "2025-06-07 11:6",
            "２０２５-06-07 11:00",
            " 2025-06-07 11:00",
        ] {
            assert!(!is_time(ng), "{ng:?}");
        }
    }

    #[test]
    fn fetched_at_format() {
        for ok in ["", "2025-06-08T09:30:00"] {
            assert!(is_fetched_at(ok), "{ok:?}");
        }
        for ng in [
            "2025-06-08 09:30:00",
            "2025-06-08T09:30",
            "2025-06-08T09:30:60",
            "2025-06-08T09:30:00Z",
            "2025-06-08T09:30:00+09:00",
        ] {
            assert!(!is_fetched_at(ng), "{ng:?}");
        }
    }

    #[test]
    fn quality_kind_json_names() {
        let json = |k| serde_json::to_string(&k).unwrap();
        assert_eq!(json(QualityKind::None), r#""""#);
        assert_eq!(json(QualityKind::Exact), r#""exact""#);
        assert_eq!(json(QualityKind::Similar), r#""similar""#);
        assert!(serde_json::from_str::<QualityKind>(r#""other""#).is_err());
    }
}
