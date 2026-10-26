//! 旧 AI 選別（`photo_ai_groups` / `photo_ai_group_members`）の有効判定と、フィード・管理画面が共有する SQL
//! （P4 / T4.3。設計は p4-design.md §6.3。今回の見え方の**意図した変更**）。
//!
//! 旧 `AI_VISIBLE` は、judged・同一場面のグループで推薦でない写真を、メンバー表の行があるだけで隠していた。
//! 写真が更新・回転・削除されたり、推薦写真が消えたり非表示になっても、残りの写真が隠れ続ける。ここでは次の
//! 条件を**全て**満たすときだけ抑止する（Python の判定・旧表の行と列の意味は変えない。読むだけ）:
//!
//! 1. **有効なメンバー**: メンバーの写真の `photo_ai_features` 行の `display`・`source_md5`・`source_mtime` が今の
//!    `photos` と合い、`content_sha256` が今の品質特徴量（`photo_quality_features`。`display`・`md5`・`mtime` が
//!    合い `error=''`）の `content_sha256` と一致する。回転は `display` の名前が変わるので、回転後は一致しない。
//! 2. グループが `judged`・`same_scene=1`、かつそのメンバーが推薦でない。
//! 3. **同じグループに、有効で表示可能な推薦写真が 1 枚以上ある**。表示可能とは、有効なメンバーで、`hidden=0`・
//!    `display` が空でない・`lat`/`lon` がある（フィードに出る位置）・既存の品質の表示条件（`exact` グループで
//!    推薦でないものは出ない）を満たすこと。代表が品質側で非表示でも、場面全体を消さない。
//!
//! `favorite` は常に抑止を免れる（従来どおり）。新しい検索空間（`ai_active_space`）の切替や新しい同一場面の候補は
//! この判定に入らない（旧表の旧選別だけが根拠）。使う表は全て `REQUIRED_SCHEMA` にあるので、AI の追加表が無い
//! 旧 DB（`INGEST=0`）でも動く。
//!
//! 管理画面の `groups`・`group_pending`・`suppressed` も同じ「有効なメンバー」の SQL から数える
//! （[`admin_counts`]）。`ai_*` の列（`ai_group` など）の出し方は従来のまま。

use rusqlite::Connection;

/// 有効な旧メンバー（列: `group_id`・`photo_id`・`recommended`・`shown`）。`shown` は、その写真が今のフィード
/// に出せる（`hidden=0`・`display` あり・位置あり・品質の表示条件を満たす）かどうか。
const VALID_MEMBERS: &str = "SELECT m.group_id AS group_id, m.photo_id AS photo_id,
       m.recommended AS recommended,
       (mp.lat IS NOT NULL AND mp.lon IS NOT NULL AND mp.display IS NOT NULL AND mp.display!=''
        AND COALESCE(mp.hidden,0)=0
        AND (mq.group_kind IS NULL OR mq.group_kind!='exact' OR mq.recommended=1)) AS shown
  FROM photo_ai_group_members m
  JOIN photos mp ON mp.id=m.photo_id
  JOIN photo_ai_features af ON af.photo_id=mp.id AND af.display=mp.display
   AND af.source_md5 IS mp.md5 AND af.source_mtime IS mp.mtime
  JOIN photo_quality_features qf ON qf.photo_id=mp.id AND qf.display=mp.display
   AND qf.source_md5 IS mp.md5 AND qf.source_mtime IS mp.mtime
   AND qf.error='' AND qf.content_sha256=af.content_sha256
  LEFT JOIN photo_quality mq ON mq.photo_id=mp.id AND mq.display=mp.display
   AND mq.source_md5 IS mp.md5 AND mq.source_mtime IS mp.mtime";

/// そのグループに、有効で表示可能な推薦写真があるか（`group` は SQL の式）。
fn has_shown_representative(group: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM ({VALID_MEMBERS}) vr
                  WHERE vr.group_id={group} AND vr.recommended=1 AND vr.shown=1)"
    )
}

/// フィードの `WHERE` に足す条件（`photos p`・`photo_ai_group_members aim`・`photo_ai_groups aig` の別名を使う）。
/// 旧 `AI_VISIBLE` に、メンバーの世代と、表示可能な推薦写真の存在の検査を加えたもの。
pub fn ai_visible() -> String {
    let rep = has_shown_representative("aim.group_id");
    format!(
        "(aig.group_id IS NULL OR aig.status!='judged' OR aig.same_scene!=1
  OR aim.recommended=1 OR COALESCE(p.favorite,0)=1
  OR NOT EXISTS (SELECT 1 FROM ({VALID_MEMBERS}) vm
                  WHERE vm.group_id=aim.group_id AND vm.photo_id=aim.photo_id)
  OR NOT {rep})"
    )
}

/// 管理画面の旧選別の集計。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminCounts {
    /// 有効なメンバーを 1 件以上持つグループの数。
    pub groups: i64,
    /// そのうち `judged` でないもの。
    pub group_pending: i64,
    /// 抑止される有効なメンバーの数（フィードの [`ai_visible`] の抑止側と同じ条件。`favorite` の免除は含めない
    /// ＝従来の集計と同じ）。
    pub suppressed: i64,
}

/// 管理画面の `groups`・`group_pending`・`suppressed`（読み取りだけ）。
pub fn admin_counts(conn: &Connection) -> rusqlite::Result<AdminCounts> {
    let (groups, pending) = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(status!='judged'),0) FROM (
               SELECT g.group_id, g.status FROM photo_ai_groups g
                WHERE EXISTS (SELECT 1 FROM ({VALID_MEMBERS}) v WHERE v.group_id=g.group_id))"
        ),
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let rep = has_shown_representative("m.group_id");
    let suppressed = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM ({VALID_MEMBERS}) m
               JOIN photo_ai_groups g ON g.group_id=m.group_id
              WHERE g.status='judged' AND g.same_scene=1 AND COALESCE(m.recommended,0)!=1
                AND {rep}"
        ),
        [],
        |row| row.get(0),
    )?;
    Ok(AdminCounts {
        groups,
        group_pending: pending,
        suppressed,
    })
}
