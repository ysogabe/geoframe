//! 位置の推定と地名（P2 / T2.5）。現行 `geo.py` の `infer_locations`・`enrich_rows`・`muni_table` を、
//! 同期サイクルの段（[`crate::sync::Stage`]）にしたもの。詳細は `docs/rust-migration/server.md` §15。
//!
//! - [`Infer`]（段 `infer`）: 位置の無い写真の位置を、撮影時刻が近い位置つきの写真（錨）から推定する。
//!   DB だけを使う（通信しない）。
//! - [`Enrich`]（段 `enrich`）: 位置があって地名が未設定の写真に、地理院の逆ジオコーダ・標高 API・
//!   `muni.js` で地名・標高・自治体を付ける。1 サイクル 40 件まで。
//!
//! どちらも、**計画を作る関数と、書く関数が分かれていて**、実行と `--dry-run` が同じ関数を通る
//! （`--dry-run` は書き込み接続を持たず、実 DB のメモリ上の写しに計画を当てて、後続の段に
//! 「その段が済んだあと」の状態を見せる。[`crate::sync`]）。
//!
//! # 時刻の解釈
//!
//! `taken_at` は `YYYY-MM-DD HH:MM`（タイムゾーン無し）。現行の `util.t_epoch` は `strptime(...).timestamp()` で
//! 環境の `TZ` として解釈する。差を取るだけなので、Rust は **`TZ` に依存しない naive な日時**（暦の日数×86400 +
//! 時分）で差を取る（[inventory.md §6.1](../../../docs/rust-migration/inventory.md)）。夏時間の無い `TZ`
//! （本番は `Asia/Tokyo`）では現行と一致する。

mod enrich;
mod infer;

pub use enrich::{Enrich, GsiEndpoints};
pub use infer::Infer;

#[cfg(test)]
mod stub;
#[cfg(test)]
mod tests;

/// `taken_at` を、naive な日時の通算秒（任意の起点）にする。解釈できなければ `None`
/// （現行の `t_epoch`: `strptime(s, "%Y-%m-%d %H:%M")` が失敗したとき）。
///
/// `strptime` に合わせた点: 月・日・時・分は 1〜2 桁（`1` でも `01` でも可）、日付と時刻の間は空白 1 文字以上、
/// 暦として不正（2 月 30 日など）・範囲外（24 時、60 分）・末尾の余分は失敗。年は 4 桁で 1〜9999。
/// **ASCII の数字だけを読む**。`strptime` の正規表現の `\d` は Unicode の数字（全角の `０`〜`９` など）にも当たるので、
/// 全角の数字が混じった時刻を現行は読む場合がある（年は 4 桁とも、ほかの欄は 2 桁目や 1 桁のとき）が、
/// Rust は読まない（T1.2 の写真フィードと同じ既知の差。server.md §0）。T2.2 の `norm_time` は数字を ASCII に直して
/// 保存するので、Rust が書く行には起きない。
pub(crate) fn naive_seconds(s: &str) -> Option<f64> {
    let mut chars = s.chars().peekable();
    let number = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>, min: usize, max: usize| {
        let mut value = 0u32;
        let mut digits = 0usize;
        while digits < max {
            let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) else {
                break;
            };
            value = value * 10 + d;
            digits += 1;
            chars.next();
        }
        (digits >= min).then_some(value)
    };
    // 範囲外の 2 桁（例: 月 `13`）は、`strptime` の正規表現が 1 桁に戻して次の区切りを探し、失敗する。
    // 先に 2 桁取って範囲を見る方法と結果が同じなので、取ってから範囲を検査する
    let year = number(&mut chars, 4, 4)?;
    (chars.next()? == '-').then_some(())?;
    let month = number(&mut chars, 1, 2)?;
    (chars.next()? == '-').then_some(())?;
    let day = number(&mut chars, 1, 2)?;
    // 日付と時刻の間は `\s+`
    let mut spaces = 0;
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
        spaces += 1;
    }
    (spaces > 0).then_some(())?;
    let hour = number(&mut chars, 1, 2)?;
    (chars.next()? == ':').then_some(())?;
    let minute = number(&mut chars, 1, 2)?;
    if chars.next().is_some() {
        return None;
    }
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || hour > 23 || minute > 59 {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let days = days_from_civil(i64::from(year), i64::from(month), i64::from(day));
    Some((days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60) as f64)
}

fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 1970-01-01 からの日数（先発グレゴリオ暦。Howard Hinnant の `days_from_civil`）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
