//! 更新 API（P3 / T3.1）: E12 `POST /api/admin/setloc`（手動の位置）と E13 `POST /api/admin/setflag`
//! （お気に入り・除外）。現行 `admin.py` の `admin_setloc`・`admin_setflag` と、入力の検査・更新する列・
//! 応答の形・エラーの文面を揃える（棚卸し E12・E13、S4・S5）。
//!
//! # 書き込みの経路
//!
//! 書くのは **writer（単一接続・専用スレッド。[`crate::writer`]）だけ**。同期サイクルの段と同じ接続・同じ
//! キューを通るので、段の 1 回の書き込み（1 トランザクション）の途中に割り込まない。割り込めるのは「段が
//! 計画を読んでから書くまで」の間で、そこで利用者の変更が失われないことは各段の書き込み側の条件
//! （`infer` の `loc_source IS NULL OR 'inferred'`、`enrich` の `lat=? AND lon=? AND place IS NULL`）で守る
//! （server.md §17.3）。writer が無い（`GEOFRAME_INGEST=0`）ときは**書かずに** [`ApiError::Unavailable`]。
//!
//! # 検査の順序
//!
//! 入力の検査（400）を先に済ませ、通ったものだけ writer の有無を見る（Python に無い 503 は、400 より後）。
//! 検査は現行と同じ順（`id` → `lat` → `lon`、`id` → `favorite` → `hidden`）で、最初に見つけた違反を返す。
//!
//! # ログ
//!
//! 成功しても何も出さない（座標・id を出さない。リクエストログは [`crate::server`] のメソッド・ルート・状態
//! コード・所要時間だけ）。失敗の文面は [`ApiError::Internal`] に入れ、`post.rs` がログにだけ出す。

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use crate::post::ApiError;
use crate::writer::Writer;

/// 検査を通った `id`。`shown` は 404 の文面に出す元の表記。
pub(crate) struct Id {
    /// SQLite の rowid に入る値。`i64` を超える整数は `None`（SQL を実行せずに 404 にする。
    /// `i64::MAX` に丸めると、その rowid の行が実在するとき、より大きい `id` の要求が当たってしまう）。
    pub(crate) value: Option<i64>,
    shown: String,
}

impl Id {
    pub(crate) fn not_found(&self) -> ApiError {
        ApiError::NotFound(format!("id={} は存在しません", self.shown))
    }
}

/// 現行の `_req_id`。`bool` は拒否（Python では `True` が `int` として通ってしまうため）、整数だけ、正の数だけ。
///
/// 差: `i64` を超える整数は、Python では SQLite に渡せず 500 になる。Rust は存在しない `id` として 404 にする
/// （`value` は `None`。ほかの検査と writer の確認のあと、SQL を実行せずに 404）。
pub(crate) fn req_id(payload: &Map<String, Value>) -> Result<Id, ApiError> {
    let not_int = || ApiError::BadRequest("id は整数で指定してください".into());
    let Some(Value::Number(n)) = payload.get("id") else {
        return Err(not_int());
    };
    if let Some(v) = n.as_i64() {
        if v <= 0 {
            return Err(ApiError::BadRequest("id が範囲外です".into()));
        }
        return Ok(Id {
            value: Some(v),
            shown: v.to_string(),
        });
    }
    if n.is_u64() {
        return Ok(Id {
            value: None,
            shown: n.to_string(),
        });
    }
    Err(not_int())
}

/// 現行の `_req_coord`。`bool` は拒否、整数・小数を `float` に直す。`lo <= v <= hi` を外れる・NaN は 400。
/// 範囲の文面は Python の `f"{lo}〜{hi}"`（`-90.0〜90.0`）と同じ。
fn req_coord(payload: &Map<String, Value>, key: &str, lo: f64, hi: f64) -> Result<f64, ApiError> {
    let Some(v) = payload.get(key).and_then(Value::as_f64) else {
        return Err(ApiError::BadRequest(format!(
            "{key} は数値で指定してください"
        )));
    };
    if !(lo <= v && v <= hi) {
        return Err(ApiError::BadRequest(format!(
            "{key} が範囲外です（{lo:?}〜{hi:?}）"
        )));
    }
    Ok(v)
}

pub(crate) fn writer_or_unavailable(writer: Option<&Writer>) -> Result<&Writer, ApiError> {
    writer.ok_or(ApiError::Unavailable)
}

/// E12 `setloc`。現行 `admin_setloc` が更新する列を正確に合わせる: `lat lon` と `loc_source='manual'`、
/// `inferred_from infer_dt_s` と地名の列（`place elev muni_cd pref city`）を NULL に戻す
/// （`enrich` が `lat IS NOT NULL AND place IS NULL` で拾い直す）。`display` などには触れない。
/// `manual` は `infer` の再評価の対象（`loc_source IS NULL OR 'inferred'`）から外れる。
pub fn set_loc(payload: &Map<String, Value>, writer: Option<&Writer>) -> Result<Value, ApiError> {
    let id = req_id(payload)?;
    let lat = req_coord(payload, "lat", -90.0, 90.0)?;
    let lon = req_coord(payload, "lon", -180.0, 180.0)?;
    let writer = writer_or_unavailable(writer)?;
    let Some(pid) = id.value else {
        return Err(id.not_found());
    };
    let changed = writer.call(move |conn| {
        conn.execute(
            "UPDATE photos SET lat=?1, lon=?2, loc_source='manual',
             inferred_from=NULL, infer_dt_s=NULL,
             place=NULL, elev=NULL, muni_cd=NULL, pref=NULL, city=NULL
             WHERE id=?3",
            params![lat, lon, pid],
        )
    })?;
    if changed == 0 {
        return Err(id.not_found());
    }
    Ok(json!({"ok": true, "id": pid, "lat": lat, "lon": lon, "loc_source": "manual"}))
}

/// E13 `setflag`。`favorite`・`hidden` のどちらか（両方でもよい）を `true`/`false`（JSON の bool だけ。
/// `null`・数値・文字列は 400）で受け、指定した列だけ書く。どちらも無ければ 400。
/// 応答は更新後の両方の値。Python は UPDATE と SELECT を別々に行うが、Rust は 1 回の
/// `UPDATE … RETURNING` で済ませる（間に別の書き込みが入らない）。
pub fn set_flag(payload: &Map<String, Value>, writer: Option<&Writer>) -> Result<Value, ApiError> {
    let id = req_id(payload)?;
    let mut sets: Vec<(&'static str, i64)> = Vec::new();
    for key in ["favorite", "hidden"] {
        if let Some(v) = payload.get(key) {
            let Value::Bool(b) = v else {
                return Err(ApiError::BadRequest(format!(
                    "{key} は true/false で指定してください"
                )));
            };
            sets.push((key, i64::from(*b)));
        }
    }
    if sets.is_empty() {
        return Err(ApiError::BadRequest(
            "favorite か hidden のいずれかを指定してください".into(),
        ));
    }
    let writer = writer_or_unavailable(writer)?;
    let Some(pid) = id.value else {
        return Err(id.not_found());
    };
    // 列名は上の固定の 2 つだけ（利用者の入力は値としてしか入らない）
    let assignments: Vec<String> = sets
        .iter()
        .enumerate()
        .map(|(i, (column, _))| format!("{column}=?{}", i + 1))
        .collect();
    let sql = format!(
        "UPDATE photos SET {} WHERE id=?{} RETURNING favorite, hidden",
        assignments.join(", "),
        sets.len() + 1
    );
    let row = writer.call(move |conn| {
        let mut values: Vec<i64> = sets.iter().map(|(_, v)| *v).collect();
        values.push(pid);
        conn.query_row(&sql, rusqlite::params_from_iter(values), |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .optional()
    })?;
    let Some((favorite, hidden)) = row else {
        return Err(id.not_found());
    };
    Ok(json!({
        "ok": true,
        "id": pid,
        "favorite": favorite.unwrap_or(0) != 0,
        "hidden": hidden.unwrap_or(0) != 0,
    }))
}

#[cfg(test)]
mod tests;
