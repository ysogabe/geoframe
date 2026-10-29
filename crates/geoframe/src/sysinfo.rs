//! OS に直接聞く小さな関数: ディスクの使用量（`statvfs`）・現地時刻（`localtime_r`）・実効ユーザー ID（`geteuid`）。管理画面の
//! `/api/admin/status` の `disk` と `server_time` が、Python の `shutil.disk_usage`・`time.strftime` と
//! 同じ値になるように使う。**このクレートで `unsafe` を使うのはここだけ**（`lib.rs` が `deny` している）。
//!
//! 現地時刻は C ライブラリに任せる。musl は `TZ`（unit が `Asia/Tokyo` を与える）と
//! `/usr/share/zoneinfo` を実行時に読むので、Python（glibc）と同じ時刻になる。tzdata が無い機体では
//! どちらも UTC になる。
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// `shutil.disk_usage` と同じ3つ（バイト）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

/// `path` があるファイルシステムの使用量。`shutil.disk_usage` と同じく、
/// `total = f_blocks*f_frsize`、`used = (f_blocks-f_bfree)*f_frsize`、`free = f_bavail*f_frsize`。
// 各欄の型は OS で違う（x86_64 では u64 だが、32 ビットでは u32）ので、`u64::from` で受ける
#[allow(clippy::useless_conversion)]
pub fn disk_usage(path: &Path) -> io::Result<DiskUsage> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "パスに NUL を含みます"))?;
    let mut st = MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c_path` は NUL 終端の C 文字列で、`st` は `statvfs` 1つぶんの書き込み先として有効。
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `statvfs` が成功したので、構造体は書き込み済み（確保時にゼロ埋めもしてある）。
    let st = unsafe { st.assume_init() };
    let frsize = u64::from(st.f_frsize);
    let blocks = u64::from(st.f_blocks);
    Ok(DiskUsage {
        total: frsize.saturating_mul(blocks),
        used: frsize.saturating_mul(blocks.saturating_sub(u64::from(st.f_bfree))),
        free: frsize.saturating_mul(u64::from(st.f_bavail)),
    })
}

/// このプロセスの実効ユーザー ID（鍵ファイルの所有者の検査用）。
pub fn effective_uid() -> u32 {
    // SAFETY: `geteuid` は引数を取らず、失敗しない。
    unsafe { libc::geteuid() }
}

/// 今の現地時刻を `YYYY-MM-DD HH:MM:SS` で（`time.strftime("%Y-%m-%d %H:%M:%S")` と同じ形）。
pub fn local_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_local(secs)
}

/// 今の現地時刻を `YYYY-MM-DDTHH:MM:SS`（オフセット無し）で。`datetime.now().isoformat(timespec="seconds")`
/// と同じ形で、`indexed_at`・`fetched_at` に入れる（inventory.md §6.1）。
pub fn local_isoformat() -> String {
    local_timestamp().replacen(' ', "T", 1)
}

fn format_local(secs: u64) -> String {
    local_parts(secs).map_or_else(String::new, |(text, _)| text)
}

/// 今の現地時刻を UTC からのずれ付きで（`YYYY-MM-DDTHH:MM:SS+09:00`）。`datetime.now().astimezone()
/// .isoformat(timespec="seconds")` と同じ形で、`analyzed_at`（品質・AI・位置候補）に入れる（inventory.md §6.1）。
pub fn local_isoformat_offset() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_local_offset(secs)
}

fn format_local_offset(secs: u64) -> String {
    let Some((text, off)) = local_parts(secs) else {
        return String::new();
    };
    let sign = if off < 0 { '-' } else { '+' };
    let off = off.abs();
    format!(
        "{}{sign}{:02}:{:02}",
        text.replacen(' ', "T", 1),
        off / 3600,
        off % 3600 / 60
    )
}

/// 現地時刻（`YYYY-MM-DD HH:MM:SS`）と、UTC からのずれ（秒。東が正）。
// `tm_gmtoff` の型は OS で違う（x86_64 では i64 だが、32 ビットでは i32）ので、`i64::from` で受ける
#[allow(clippy::useless_conversion)]
fn local_parts(secs: u64) -> Option<(String, i64)> {
    let secs = i64::try_from(secs).ok()?;
    // `time_t` は64ビット（muslの`libc::time_t`は移行中で非推奨の警告が出るので、型名を書かない）
    let t = secs as _;
    let mut tm = MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `t` と `tm` はこの関数の中の有効な値で、`localtime_r` はスレッドセーフ（結果を `tm` に書く）。
    let done = unsafe { libc::localtime_r(&t, tm.as_mut_ptr()) };
    if done.is_null() {
        return None;
    }
    // SAFETY: `localtime_r` が成功（非 NULL）したので書き込み済み。
    let tm = unsafe { tm.assume_init() };
    let text = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        i64::from(tm.tm_year) + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    );
    Some((text, i64::from(tm.tm_gmtoff)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_usage_of_the_temp_dir_is_consistent() {
        let usage = disk_usage(&std::env::temp_dir()).unwrap();
        assert!(usage.total > 0);
        assert!(usage.used <= usage.total, "{usage:?}");
        assert!(usage.free <= usage.total, "{usage:?}");
    }

    #[test]
    fn disk_usage_of_a_missing_path_is_an_error() {
        let err = disk_usage(Path::new("/nonexistent-geoframe-test")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(disk_usage(Path::new("a\0b")).is_err());
    }

    #[test]
    fn local_timestamp_has_the_strftime_shape() {
        let text = local_timestamp();
        let bytes = text.as_bytes();
        assert_eq!(bytes.len(), 19, "{text}");
        for (i, b) in bytes.iter().enumerate() {
            match i {
                4 | 7 => assert_eq!(*b, b'-', "{text}"),
                10 => assert_eq!(*b, b' ', "{text}"),
                13 | 16 => assert_eq!(*b, b':', "{text}"),
                _ => assert!(b.is_ascii_digit(), "{text}"),
            }
        }
        assert!(text.starts_with("20"), "{text}");
    }

    #[test]
    fn local_isoformat_is_the_naive_iso_shape() {
        let text = local_isoformat();
        let bytes = text.as_bytes();
        assert_eq!(bytes.len(), 19, "{text}");
        for (i, b) in bytes.iter().enumerate() {
            match i {
                4 | 7 => assert_eq!(*b, b'-', "{text}"),
                10 => assert_eq!(*b, b'T', "{text}"),
                13 | 16 => assert_eq!(*b, b':', "{text}"),
                _ => assert!(b.is_ascii_digit(), "{text}"),
            }
        }
    }

    #[test]
    fn local_isoformat_offset_is_the_aware_iso_shape() {
        let text = local_isoformat_offset();
        let bytes = text.as_bytes();
        assert_eq!(bytes.len(), 25, "{text}");
        for (i, b) in bytes.iter().enumerate() {
            match i {
                4 | 7 => assert_eq!(*b, b'-', "{text}"),
                10 => assert_eq!(*b, b'T', "{text}"),
                13 | 16 | 22 => assert_eq!(*b, b':', "{text}"),
                19 => assert!(matches!(*b, b'+' | b'-'), "{text}"),
                _ => assert!(b.is_ascii_digit(), "{text}"),
            }
        }
    }

    #[test]
    fn epoch_zero_is_formatted_in_local_time() {
        // 1970-01-01 00:00:00 UTC は、どの TZ でも 1969-12-31 か 1970-01-01（UTC±14時間）
        let text = format_local(0);
        assert!(
            text.starts_with("1970-01-01") || text.starts_with("1969-12-31"),
            "{text}"
        );
    }
}
