//! 人が選んだ代表（`photo_curation`）と、フィード・旧 AI 選別・状況タブが共有する「有効な推奨」の SQL 片
//! （設計は docs/admin-library/design.md の「`photo_curation` 表」）。
//!
//! - 人の記録は**写真ごと**に持ち、写真の世代（`display`・`md5`・`mtime`・内容の SHA の 4 つ）を鍵にする。
//!   `photo_quality` の `group_id` に紐付けないので、`quality-scan` の再構築（全行の入れ替え・`group_id` の
//!   振り直し）をまたいで意味が保たれる。世代が合わない記録は **stale**（読むだけ。消さない）。
//! - グループに有効な `representative` が 1 枚でもあれば、そのグループの `quality_recommended` はその写真だけが
//!   `true`（解析の `recommended` は使わない）。無ければ解析のまま。
//! - 表は書き込み側が起動時に作る（[`ensure_tables`]。`SCHEMA` 定数には足さない）。無い DB（`INGEST=0` の古い DB）では
//!   [`available`] が偽になり、読む側は従来の SQL をそのまま使う。
//!
//! SQL 片は別名を受ける関数にしてある（フィードは `q`/`c`・`p`、旧 AI 選別は `mq`/`mc`・`mp`）。内側の
//! 副問い合わせの別名（`q2`・`p2`・`c2`・`qr`・`cr`）は固定なので、呼び手の別名と重ならないこと。

use rusqlite::{Connection, TransactionBehavior};

/// この表が作る表の名前（[`ensure_tables`] の対象）。
pub const TABLES: &[&str] = &["photo_curation"];

/// `photo_curation` の DDL。`IF NOT EXISTS` だけで、他の表には触れない。
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS photo_curation(
  photo_id INTEGER PRIMARY KEY,
  display TEXT NOT NULL,
  source_md5 TEXT,
  source_mtime REAL,
  content_sha256 TEXT NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('representative','reviewed')),
  decided_at TEXT NOT NULL
);";

/// 表があるか（`sqlite_master` の 1 行）。読み取り接続でも呼べる。
pub fn available(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='photo_curation')",
        [],
        |row| row.get(0),
    )
}

/// まだ無い表の名前（書き込みなし。`--dry-run` 用）。
pub fn pending(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    Ok(if available(conn)? {
        Vec::new()
    } else {
        TABLES.iter().map(|t| (*t).to_string()).collect()
    })
}

/// 表を作る（無いときだけ。作った名前を返す）。書き込み用の接続で、起動時に 1 回呼ぶ。
pub fn ensure_tables(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    let missing = pending(conn)?;
    if missing.is_empty() {
        return Ok(missing);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    tx.commit()?;
    Ok(missing)
}

// ---- SQL 片 ----

/// `QUALITY_JOIN` の後ろに付ける。`c` は `q` の写真に付いた**有効な**記録だけ（世代の 4 列が一致）。`photo_id` が
/// 主キーなので `q` の行が増えることはない。
pub(crate) fn join(q: &str, c: &str) -> String {
    format!(
        "LEFT JOIN photo_curation {c} ON {c}.photo_id={q}.photo_id AND {c}.display={q}.display
  AND {c}.source_md5 IS {q}.source_md5 AND {c}.source_mtime IS {q}.source_mtime
  AND {c}.content_sha256={q}.content_sha256"
    )
}

/// `q` のグループに、有効な代表（世代の合う `photo_quality` 行で、有効な `representative` の記録を持つ）があるか。
pub(crate) fn group_has_pick(q: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM photo_quality q2
          JOIN photos p2 ON p2.id=q2.photo_id AND p2.display=q2.display
           AND p2.md5 IS q2.source_md5 AND p2.mtime IS q2.source_mtime
          JOIN photo_curation c2 ON c2.photo_id=q2.photo_id AND c2.display=q2.display
           AND c2.source_md5 IS q2.source_md5 AND c2.source_mtime IS q2.source_mtime
           AND c2.content_sha256=q2.content_sha256 AND c2.role='representative'
         WHERE q2.group_kind={q}.group_kind AND q2.group_id={q}.group_id)"
    )
}

/// 有効な推奨（0 か 1）。`q` に行が無い（グループ外）写真は 0。
pub(crate) fn effective_recommended(q: &str, c: &str) -> String {
    format!(
        "CASE WHEN {q}.group_id IS NULL THEN 0
       WHEN {pick} THEN COALESCE({c}.role='representative',0)
       ELSE {q}.recommended END",
        pick = group_has_pick(q)
    )
}

/// フィードの品質の表示条件（exact は有効な推奨だけ出す）。
pub(crate) fn quality_visible(q: &str, c: &str) -> String {
    format!(
        "({q}.group_kind IS NULL OR {q}.group_kind!='exact' OR {eff}=1)",
        eff = effective_recommended(q, c)
    )
}

/// `photos` の別名 `p` の写真に、有効な `representative` の記録があるか（グループは問わない）。旧 AI 選別が
/// `favorite` と同じく抑止を免れるかの判定に使う（`c` の別名を持たない場所用に自己完結している）。
pub(crate) fn has_valid_representative(p: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM photo_quality qr
          JOIN photo_curation cr ON cr.photo_id=qr.photo_id AND cr.display=qr.display
           AND cr.source_md5 IS qr.source_md5 AND cr.source_mtime IS qr.source_mtime
           AND cr.content_sha256=qr.content_sha256 AND cr.role='representative'
         WHERE qr.photo_id={p}.id AND qr.display={p}.display
           AND qr.source_md5 IS {p}.md5 AND qr.source_mtime IS {p}.mtime)"
    )
}

/// 状態が未確認・再確認のグループの数（`unconfirmed` + `recheck`。確認済みでないもの全部）。グループは世代の合う
/// `photo_quality` の行が 2 枚以上のもので、`confirmed` は「全メンバーに有効な記録がある」だけ。
/// 表が無ければ、すべてのグループが未確認。
pub fn pending_groups(conn: &Connection) -> rusqlite::Result<i64> {
    let (curation_join, missing) = if available(conn)? {
        (join("q", "c"), "SUM(c.photo_id IS NULL)>0")
    } else {
        (String::new(), "1")
    };
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM (
               SELECT 1 FROM photo_quality q JOIN photos p
                 ON p.id=q.photo_id AND p.display=q.display
                AND p.md5 IS q.source_md5 AND p.mtime IS q.source_mtime
               {curation_join}
              GROUP BY q.group_kind, q.group_id
             HAVING COUNT(*)>=2 AND {missing})"
        ),
        [],
        |row| row.get(0),
    )
}

// ---- 状態とトークン ----

/// メンバーの人の記録の見え方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    /// 有効な `representative`。
    Representative,
    /// 有効な `reviewed`。
    Reviewed,
    /// 記録はあるが世代が合わない。
    Stale,
    /// 記録が無い。
    None,
}

impl Record {
    /// `token` に入れる名前。`groups` の `curation` の値（`None` は `null`）とも同じ。
    pub fn as_str(self) -> &'static str {
        match self {
            Record::Representative => "representative",
            Record::Reviewed => "reviewed",
            Record::Stale => "stale",
            Record::None => "none",
        }
    }

    /// 有効な記録か（代表か確認済み）。
    pub fn is_valid(self) -> bool {
        matches!(self, Record::Representative | Record::Reviewed)
    }
}

/// グループの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// どのメンバーにも記録が無い。
    Unconfirmed,
    /// 全メンバーに有効な記録がある。
    Confirmed,
    /// それ以外（写真が加わった・写真が変わって記録が stale になった）。
    Recheck,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Unconfirmed => "unconfirmed",
            State::Confirmed => "confirmed",
            State::Recheck => "recheck",
        }
    }
}

/// グループの状態。`records` は世代の合うメンバー全員の [`Record`]。
pub fn group_state(records: &[Record]) -> State {
    if records.iter().all(|r| *r == Record::None) {
        State::Unconfirmed
    } else if records.iter().all(|r| r.is_valid()) {
        State::Confirmed
    } else {
        State::Recheck
    }
}

/// `token` に使うグループのメンバー 1 枚（世代の 4 列・解析の推奨・人の記録）。
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub photo_id: i64,
    pub display: String,
    pub source_md5: Option<String>,
    pub source_mtime: Option<f64>,
    pub content_sha256: String,
    /// 解析の `recommended`（人の記録は反映しない）。
    pub recommended: bool,
    pub record: Record,
}

/// 更新時刻の文字列。`photos.mtime` は REAL なので、読み書き（`groups`・`confirm`・`reset`）で同じ形にそろえる
/// （`{:?}` は `1.0` を `1.0` と書く）。NULL は `-`。
pub fn mtime_text(mtime: Option<f64>) -> String {
    mtime.map_or_else(|| "-".to_string(), |m| format!("{m:?}"))
}

/// 競合の検出に使うグループの印（16 桁の小文字 hex）。メンバーを `photo_id` 昇順に並べ、1 枚を
/// `<photo_id>\t<display>\t<source_md5|->\t<source_mtime|->\t<content_sha256>\t<recommended 0/1>\t<記録>` にして
/// 改行でつないだ文字列の SHA-256 の先頭 64 bit。メンバーの集合・各写真の世代・解析の推奨・人の記録の
/// どれが変わっても変わる。秘密ではない。
pub fn group_token(members: &[Member]) -> String {
    let mut sorted: Vec<&Member> = members.iter().collect();
    sorted.sort_by_key(|m| m.photo_id);
    let text = sorted
        .iter()
        .map(|m| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                m.photo_id,
                m.display,
                m.source_md5.as_deref().unwrap_or("-"),
                mtime_text(m.source_mtime),
                m.content_sha256,
                u8::from(m.recommended),
                m.record.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: i64) -> Member {
        Member {
            photo_id: id,
            display: format!("derivatives/p{id}.jpg"),
            source_md5: Some(format!("m{id}")),
            source_mtime: Some(id as f64),
            content_sha256: format!("sha-{id}"),
            recommended: id == 1,
            record: Record::None,
        }
    }

    #[test]
    fn the_token_does_not_depend_on_the_order() {
        let a = [member(1), member(2), member(3)];
        let b = [member(3), member(1), member(2)];
        assert_eq!(group_token(&a), group_token(&b));
        let token = group_token(&a);
        assert_eq!(token.len(), 16);
        assert!(
            token
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
    }

    #[test]
    fn the_token_changes_with_any_single_field() {
        let base = vec![member(1), member(2)];
        let token = group_token(&base);
        type Change = fn(&mut Member);
        let mutations: Vec<(&str, Change)> = vec![
            ("photo_id", |m| m.photo_id = 9),
            ("display", |m| m.display.push('x')),
            ("md5", |m| m.source_md5 = Some("other".into())),
            ("md5 null", |m| m.source_md5 = None),
            ("md5 empty", |m| m.source_md5 = Some(String::new())),
            ("mtime", |m| m.source_mtime = Some(99.5)),
            ("mtime null", |m| m.source_mtime = None),
            ("sha", |m| m.content_sha256.push('x')),
            ("recommended", |m| m.recommended = !m.recommended),
            ("record representative", |m| {
                m.record = Record::Representative
            }),
            ("record reviewed", |m| m.record = Record::Reviewed),
            ("record stale", |m| m.record = Record::Stale),
        ];
        let mut seen = std::collections::HashSet::from([token.clone()]);
        for (what, change) in &mutations {
            let mut members = base.clone();
            change(&mut members[1]);
            let changed = group_token(&members);
            assert_ne!(changed, token, "{what}");
            seen.insert(changed);
        }
        // 変えた項目ごとに別の印になる（元の印を含めて全部違う。取り違えない）
        assert_eq!(seen.len(), mutations.len() + 1);
        // 1 枚減っても・増えても変わる
        assert_ne!(group_token(&base[..1]), token);
        assert_ne!(
            group_token(&[base[0].clone(), base[1].clone(), member(3)]),
            token
        );
    }

    #[test]
    fn the_mtime_text_is_stable_and_marks_null() {
        assert_eq!(mtime_text(Some(1.0)), "1.0");
        assert_eq!(mtime_text(Some(1700000000.25)), "1700000000.25");
        assert_eq!(mtime_text(None), "-");
    }

    #[test]
    fn the_state_has_three_outcomes() {
        use Record::*;
        assert_eq!(group_state(&[None, None]), State::Unconfirmed);
        assert_eq!(group_state(&[Representative, Reviewed]), State::Confirmed);
        assert_eq!(group_state(&[Reviewed, Reviewed]), State::Confirmed);
        // 写真が加わった（記録の無いメンバーがいる）
        assert_eq!(group_state(&[Reviewed, Reviewed, None]), State::Recheck);
        // 写真が変わった（stale のメンバーがいる）
        assert_eq!(group_state(&[Representative, Stale]), State::Recheck);
        // 全員 stale（再構築で内容が変わった）は未確認ではなく再確認
        assert_eq!(group_state(&[Stale, Stale]), State::Recheck);
        assert_eq!(State::Recheck.as_str(), "recheck");
    }
}
