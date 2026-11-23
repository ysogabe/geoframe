//! OS に直接聞く小さな関数（実効ユーザー ID・flock・現地時刻）。`unsafe` を使うのは、ここと [`crate::cancel`]・[`crate::rclone`]（3 段目で消す）・
//! [`crate::rclone_conf`] だけ。
//!
//! 現地時刻は C ライブラリ（`localtime_r`）に任せる。`TZ`（unit が与える）と zoneinfo を読む。ジョブの記録の時刻は、
//! `2026-01-12T19:30:00+09:00` のような時差つきの RFC 3339（`crates/geoframe` の `sysinfo.rs` と同じ求め方。
//! `geoframe` crate には依存しない）。

use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::time::{SystemTime, UNIX_EPOCH};

/// 実効ユーザー ID。
pub fn euid() -> u32 {
    // SAFETY: `geteuid` は引数を取らず、失敗しない。
    unsafe { libc::geteuid() }
}

/// `flock(LOCK_EX | LOCK_NB)`。取れたら `true`、他が持っていれば `false`。ロックは開いたファイル（open file description）に
/// 付く。`file` を閉じる（手放す）と外れる。
pub fn try_flock_exclusive(file: &File) -> io::Result<bool> {
    loop {
        // SAFETY: `file` は開いているファイルで、fd はこの呼び出しの間有効。
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(false),
            Some(libc::EINTR) => {}
            _ => return Err(err),
        }
    }
}

/// 今の UNIX 時刻（秒）。
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// 今の UNIX 時刻（ナノ秒。ジョブの記録の並べ替えの鍵。2262 年まで `i64` に収まる）。
pub fn unix_now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// `secs`（UNIX 時刻）の現地時刻を `YYYY-MM-DDTHH:MM:SS+09:00` の形にする。現地時刻を求められなければ UTC（`+00:00`）。
// `tm_gmtoff` の型は OS で違う（64 ビットでは i64、32 ビットでは i32）ので、`i64::from` で受ける
#[allow(clippy::useless_conversion)]
pub fn rfc3339_local(secs: i64) -> String {
    let t = secs as _;
    let mut tm = MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `t` と `tm` はこの関数の中の有効な値で、`localtime_r` はスレッドセーフ（結果を `tm` に書く）。
    let done = unsafe { libc::localtime_r(&t, tm.as_mut_ptr()) };
    if done.is_null() {
        return format_parts(utc_parts(secs), 0);
    }
    // SAFETY: `localtime_r` が成功（非 NULL）したので書き込み済み。
    let tm = unsafe { tm.assume_init() };
    let parts = (
        i64::from(tm.tm_year) + 1900,
        i64::from(tm.tm_mon) + 1,
        i64::from(tm.tm_mday),
        i64::from(tm.tm_hour),
        i64::from(tm.tm_min),
        i64::from(tm.tm_sec),
    );
    format_parts(parts, i64::from(tm.tm_gmtoff))
}

/// 今の現地時刻（RFC 3339、時差つき）。
pub fn now_rfc3339() -> String {
    rfc3339_local(unix_now())
}

type Parts = (i64, i64, i64, i64, i64, i64);

fn format_parts((y, mo, d, h, mi, s): Parts, gmtoff: i64) -> String {
    let sign = if gmtoff < 0 { '-' } else { '+' };
    let off = gmtoff.abs();
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}{sign}{:02}:{:02}",
        off / 3600,
        (off % 3600) / 60
    )
}

/// UTC の年月日時分秒（`localtime_r` が使えないときの代わり。1970 年以降の日付の換算）。
fn utc_parts(secs: i64) -> Parts {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // 民間暦の換算（Howard Hinnant の civil_from_days）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_conversion_matches_known_instants() {
        assert_eq!(format_parts(utc_parts(0), 0), "1970-01-01T00:00:00+00:00");
        // 2000-02-29 12:34:56 UTC
        assert_eq!(
            format_parts(utc_parts(951_827_696), 0),
            "2000-02-29T12:34:56+00:00"
        );
        assert_eq!(format_parts(utc_parts(-1), 0), "1969-12-31T23:59:59+00:00");
    }

    #[test]
    fn offsets_are_written_with_a_sign() {
        assert!(format_parts((2030, 1, 2, 3, 4, 5), 9 * 3600).ends_with("+09:00"));
        assert!(format_parts((2030, 1, 2, 3, 4, 5), -(3 * 3600 + 30 * 60)).ends_with("-03:30"));
    }

    #[test]
    fn local_time_has_the_rfc3339_shape() {
        let s = now_rfc3339();
        let b = s.as_bytes();
        assert_eq!(s.len(), 25, "{s}");
        assert_eq!(
            (b[4], b[7], b[10], b[13], b[16]),
            (b'-', b'-', b'T', b':', b':')
        );
        assert!(b[19] == b'+' || b[19] == b'-', "{s}");
        assert_eq!(b[22], b':', "{s}");
    }

    #[test]
    fn a_second_flock_on_the_same_file_fails_until_the_first_is_dropped() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/unit-tmp")
            .join(format!("sys-flock-{}", std::process::id()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let a = File::create(&path).unwrap();
        assert!(try_flock_exclusive(&a).unwrap());
        let b = File::open(&path).unwrap();
        assert!(!try_flock_exclusive(&b).unwrap());
        drop(a);
        assert!(try_flock_exclusive(&b).unwrap());
        let _ = std::fs::remove_file(&path);
    }
}
