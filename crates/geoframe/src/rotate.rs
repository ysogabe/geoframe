//! 更新 API E14 `POST /api/admin/rotate`（P3 / T3.2）: Drive 由来の写真の derivative を時計回りに `delta` 度
//! 回して別名で保存し、`display`・`rotation` を書き換え、古いファイルを消す。現行 `admin.py` の
//! `admin_rotate` と、入力の検査・状態の変更・応答の形・エラーの文面を揃える（棚卸し E14、S6）。
//!
//! # 流れ
//!
//! 1. `id`（T3.1 の [`req_id`]）→ `delta`（整数の 90・180・270・-90 だけ）の検査（400）。
//! 2. writer が無ければ（取り込みが無効）、**ファイルに触れる前に** 503。
//! 3. 行を読む（writer 経由）。無ければ 404。`source != 'drive'` か `drive_id` が空なら 400（「回せるのは
//!    Driveの写真だけです」）。`display` が無い・通常のファイルでなければ 400（「表示用の画像がまだありません」）。
//! 4. 新しい `rotation = (旧 rotation（NULL は 0）+ delta) mod 360`。今の derivative を `delta` 度回し
//!    （[`imaging::rotate_jpeg`]。EXIF の向きを当てず、縮小せず、JPEG 品質 85・EXIF なし）、
//!    `derivative_name(drive_id, rotation)` の名前で `.part` 経由（排他的な新規作成＋`O_NOFOLLOW`＋`fsync`＋
//!    `rename`）で保存する。
//! 5. DB を**条件付きで**更新する（読んだ時点の `display`・`rotation`・`md5`・`fetched_at` のままなら）。
//! 6. 旧ファイルと新ファイルが違えば旧ファイルを消す（`derivatives/` 直下の通常ファイルだけ）。
//!
//! # 失敗したとき
//!
//! - 画像の書き出しに失敗: `.part` も新しいファイルも残らず、旧ファイルと DB は元のまま（500）。
//! - DB の更新に失敗: 新しいファイルを消す（旧ファイルと DB は元のまま。500）。
//! - 旧ファイルを消せない: 応答は成功にする（現行の `unlink(missing_ok=True)` と同じ）。ログは固定の文言だけ。
//! - 新しい名前と旧ファイルの名前が同じ（`display` と `rotation` が食い違った行）: 現行と同じくそのファイルに
//!   上書きする。DB の更新に失敗したら、読んでおいた元の内容を書き戻す。
//!
//! # 競合（server.md §17.4）
//!
//! - 回転どうし、回転と取得（`fetch_derivatives`）: 処理の全体を [`imaging::lock_derivatives`] で直列化する
//!   （取得はファイルを作ってから `display` を書くまで同じロックを持つ）。
//! - 同期サイクルの段が `display` を NULL にした（推定の錨が消えた取り消し、Drive の `md5` の変更）、または
//!   行が消えた・`md5` が変わった: 手順 5 の条件が外れて何も書かず、新しいファイルを消して 409。
//!   `display` が消えたファイルを指すことも、孤児のファイルが増えることもない。
//! - 品質の解析が旧ファイルを読んでいる最中: 解析は読んだ `display` を特徴量の行に書くので、`display` が変わった
//!   あとは「未解析」に戻って次のサイクルで解析し直す（`quality.rs`）。

use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::types::Value as Sql;
use serde_json::{Map, Value, json};

use crate::datadir::DataDir;
use crate::imaging::{self, ImageError};
use crate::post::ApiError;
use crate::update::{req_id, writer_or_unavailable};
use crate::writer::Writer;

/// 現行の `delta not in (90, 180, 270, -90)`。`bool`・文字列・`null` は 400。
///
/// 差: 現行は `90.0` や `9e1`（小数）も通す（`90.0 == 90`）。通すと `drive_id.r90.0.jpg` という名前の
/// ファイルを作り、応答の `rotation` も `90.0` になる。Rust は 400（server.md §0）。
fn req_delta(payload: &Map<String, Value>) -> Result<i32, ApiError> {
    match payload.get("delta").and_then(Value::as_i64) {
        Some(d @ (90 | 180 | 270 | -90)) => Ok(d as i32),
        _ => Err(ApiError::BadRequest(
            "delta は 90 / 180 / 270 / -90 で指定してください".into(),
        )),
    }
}

/// 回転を読む時点の行。比較に使う列は DB の型のまま持つ（`IS ?` で NULL どうしも一致させる）。
struct Row {
    source: Sql,
    drive_id: Sql,
    display: Sql,
    rotation: Sql,
    md5: Sql,
    fetched_at: Sql,
}

fn read_row(writer: &Writer, pid: i64) -> Result<Option<Row>, ApiError> {
    Ok(writer.call(move |conn| {
        conn.query_row(
            "SELECT source, drive_id, display, rotation, md5, fetched_at FROM photos WHERE id=?1",
            [pid],
            |r| {
                Ok(Row {
                    source: r.get(0)?,
                    drive_id: r.get(1)?,
                    display: r.get(2)?,
                    rotation: r.get(3)?,
                    md5: r.get(4)?,
                    fetched_at: r.get(5)?,
                })
            },
        )
        .optional()
    })?)
}

fn non_empty_text(v: &Sql) -> Option<&str> {
    match v {
        Sql::Text(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

const NO_DISPLAY: &str = "表示用の画像がまだありません";

/// 回す元の derivative を読む。無い・通常のファイルでない（リンク・ディレクトリ・FIFO）・
/// `derivatives/<安全な名前>` でない `display` は `None`（400）。シンボリックリンクは辿らない
/// （現行の `is_file()` はリンクを辿り、`DATA_DIR` の外のファイルも読んで消す）。
fn read_source(data: &DataDir, display: &str) -> Result<Option<Vec<u8>>, ApiError> {
    let Ok(path) = data.derivative_file(display) else {
        return Ok(None);
    };
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ApiError::Internal(format!(
                "表示用の画像を調べられません: {e}"
            )));
        }
    }
    // 調べたあとに差し替えられても、リンクは辿らず、FIFO などを開いて止まらない
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.raw_os_error() == Some(libc::ELOOP) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(ApiError::Internal(format!("表示用の画像を開けません: {e}"))),
    };
    let meta = file
        .metadata()
        .map_err(|e| ApiError::Internal(format!("表示用の画像を調べられません: {e}")))?;
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > imaging::MAX_INPUT_BYTES {
        return Err(ApiError::Internal(ImageError::FileTooLarge.to_string()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(imaging::MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ApiError::Internal(format!("表示用の画像を読めません: {e}")))?;
    if bytes.len() as u64 > imaging::MAX_INPUT_BYTES {
        return Err(ApiError::Internal(ImageError::FileTooLarge.to_string()));
    }
    Ok(Some(bytes))
}

/// `derivatives/` 直下の通常ファイルだけ消す。無ければ成功。リンク・ディレクトリは消さない（成功扱い）。
fn remove_regular(data: &DataDir, display: &str) -> std::io::Result<()> {
    let path = data
        .derivative_file(display)
        .map_err(|_| std::io::Error::other("消してよい名前ではありません"))?;
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => fs::remove_file(&path),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

const BUSY: &str = "他の処理と重なったので回転しませんでした。もう一度お試しください";

const UPDATE: &str = "UPDATE photos SET display=?1, rotation=?2
     WHERE id=?3 AND display IS ?4 AND rotation IS ?5 AND md5 IS ?6 AND fetched_at IS ?7";

/// E14 `rotate`。
pub fn rotate(
    payload: &Map<String, Value>,
    writer: Option<&Writer>,
    data_dir: &Path,
) -> Result<Value, ApiError> {
    rotate_with(payload, writer, data_dir, &|| {})
}

/// [`rotate`]。`between` は、新しいファイルを書いたあと・DB を更新する前に呼ぶ（割り込みの試験用。
/// 本番は何もしない）。
pub(crate) fn rotate_with(
    payload: &Map<String, Value>,
    writer: Option<&Writer>,
    data_dir: &Path,
    between: &dyn Fn(),
) -> Result<Value, ApiError> {
    let id = req_id(payload)?;
    let delta = req_delta(payload)?;
    let writer = writer_or_unavailable(writer)?;
    let Some(pid) = id.value else {
        return Err(id.not_found());
    };

    // 回転どうし・取得との直列化。読む前から取る（読んだ行を、書くまで他の誰もファイルごと変えない）
    let _files = imaging::lock_derivatives();

    let Some(row) = read_row(writer, pid)? else {
        return Err(id.not_found());
    };
    let drive_id = match non_empty_text(&row.drive_id) {
        Some(drive_id) if row.source == Sql::Text("drive".into()) => drive_id.to_owned(),
        _ => {
            return Err(ApiError::BadRequest("回せるのはDriveの写真だけです".into()));
        }
    };
    let data = DataDir::open(data_dir)
        .map_err(|e| ApiError::Internal(format!("データ置き場を開けません: {e}")))?;
    let old_display = match non_empty_text(&row.display) {
        Some(display) => display.to_owned(),
        None => return Err(ApiError::BadRequest(NO_DISPLAY.into())),
    };
    let Some(old_bytes) = read_source(&data, &old_display)? else {
        return Err(ApiError::BadRequest(NO_DISPLAY.into()));
    };

    let old_rotation = match &row.rotation {
        Sql::Null => 0,
        Sql::Integer(n) => i128::from(*n),
        _ => return Err(ApiError::Internal("rotation が整数ではありません".into())),
    };
    let new_rotation = (old_rotation + i128::from(delta)).rem_euclid(360) as i32;
    let name = imaging::derivative_name(&drive_id, new_rotation)
        .map_err(|e| ApiError::Internal(format!("保存先の名前を作れません: {e}")))?;
    let (dest, new_display) = data
        .derivative_dest(&name)
        .map_err(|e| ApiError::Internal(format!("保存先を作れません: {e}")))?;

    // 回して保存する（`.part` 経由。失敗しても自分の `.part` は残さず、`dest` は元のまま）
    let rendered = imaging::rotate_jpeg(&old_bytes, delta)
        .map_err(|e| ApiError::Internal(format!("回転に失敗しました: {e}")))?;
    imaging::write_atomic(&dest, &rendered.jpeg)
        .map_err(|e| ApiError::Internal(format!("回転した画像を保存できません: {e}")))?;
    drop(rendered);

    between();

    let same_file = new_display == old_display;
    let commit = {
        let new_display = new_display.clone();
        let pid_row = (pid, row.display.clone(), row.rotation.clone());
        let (md5, fetched_at) = (row.md5.clone(), row.fetched_at.clone());
        writer.call(move |conn| {
            let (pid, old_display, old_rotation) = pid_row;
            let changed = conn.execute(
                UPDATE,
                params![
                    new_display,
                    new_rotation,
                    pid,
                    old_display,
                    old_rotation,
                    md5,
                    fetched_at
                ],
            )?;
            // 条件が外れたとき、DB が今どのファイルを指しているか（新しいファイルを消してよいかの判断）
            let current: Option<Sql> = conn
                .query_row("SELECT display FROM photos WHERE id=?1", [pid], |r| {
                    r.get(0)
                })
                .optional()?;
            // 回転後の世代（thumb.rs の lookup と同じ入力）。更新できたときだけ要る
            let thumb_url = if changed == 0 {
                String::new()
            } else {
                conn.query_row(
                    "SELECT display, md5, mtime FROM photos WHERE id=?1",
                    [pid],
                    |r| {
                        Ok(crate::thumb::thumb_url(
                            pid,
                            r.get::<_, Option<String>>(0)?.as_deref(),
                            r.get::<_, Option<String>>(1)?.as_deref(),
                            r.get::<_, Option<f64>>(2)?,
                        ))
                    },
                )?
            };
            Ok((changed, current, thumb_url))
        })
    };

    let (changed, current, thumb_url) = match commit {
        Ok(done) => done,
        Err(e) => {
            discard_new(&data, &dest, &new_display, same_file, &old_bytes, None);
            return Err(e.into());
        }
    };
    if changed == 0 {
        let current_display = match &current {
            Some(Sql::Text(s)) => Some(s.as_str()),
            _ => None,
        };
        discard_new(
            &data,
            &dest,
            &new_display,
            same_file,
            &old_bytes,
            current_display,
        );
        // 行が消えていれば存在しない、そうでなければ他の処理が先に変えた
        return Err(match current {
            None => id.not_found(),
            Some(_) => ApiError::Busy(BUSY.into()),
        });
    }

    if !same_file && remove_regular(&data, &old_display).is_err() {
        // 固定の文言だけ（ファイル名・drive_id を出さない）。DB は新しいファイルを指している
        tracing::warn!("[api] /api/admin/rotate 古い表示用ファイルを削除できませんでした");
    }
    Ok(json!({
        "ok": true,
        "id": pid,
        "rotation": new_rotation,
        "thumb": format!("/{new_display}"),
        "thumb_url": thumb_url,
    }))
}

/// DB を更新できなかった（失敗・条件が外れた）ときの後始末: 新しいファイルを消す。ただし DB が今そのファイルを
/// 指しているなら消さない。旧ファイルと同じ名前だったときは、消さずに元の内容を書き戻す。
fn discard_new(
    data: &DataDir,
    dest: &Path,
    new_display: &str,
    same_file: bool,
    old_bytes: &[u8],
    current_display: Option<&str>,
) {
    if same_file {
        if imaging::write_atomic(dest, old_bytes).is_err() {
            tracing::warn!("[api] /api/admin/rotate 回転前の画像を書き戻せませんでした");
        }
    } else if current_display != Some(new_display) && remove_regular(data, new_display).is_err() {
        tracing::warn!("[api] /api/admin/rotate 回転した画像を削除できませんでした");
    }
}

/// 他のモジュールの試験が、割り込み（`between`）つきで呼ぶための入口。
#[cfg(test)]
pub(crate) fn rotate_with_for_tests(
    payload: &Map<String, Value>,
    writer: &Writer,
    data_dir: &Path,
    between: &dyn Fn(),
) -> Result<Value, ApiError> {
    rotate_with(payload, Some(writer), data_dir, between)
}

#[cfg(test)]
pub(crate) mod tests;
