//! DB だけを扱う。有償回答は immutable な世代別履歴。旧 `photo_ai_*` は読まず・書かない。
use super::*;
use crate::ai::groups::runtime::Prepared;
use crate::ai::jobs::{self, Claimed, InputCheck};
use rusqlite::{Connection, OptionalExtension, params};

pub(in crate::ai) const TABLES: &[&str] = &["ai_scene_judgments"];
pub(in crate::ai) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_scene_judgments(
 input_key TEXT PRIMARY KEY CHECK(length(input_key)=64),
 manifest_json TEXT NOT NULL,
 verdict_json TEXT NOT NULL,
 attempts INTEGER NOT NULL CHECK(attempts BETWEEN 1 AND 5),
 analyzed_at TEXT NOT NULL,
 integrity_sha256 TEXT NOT NULL CHECK(length(integrity_sha256)=64)
) WITHOUT ROWID;";

const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// 検証済みの保存済み有償回答。Debug は内容を出さない。
#[derive(Clone)]
pub struct Saved {
    manifest: Manifest,
    verdict: Verdict,
    attempts: u32,
    analyzed_at: String,
}
impl std::fmt::Debug for Saved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Saved(..)")
    }
}
impl Saved {
    pub fn verdict(&self) -> &Verdict {
        &self.verdict
    }
    pub fn model(&self) -> &str {
        &self.manifest.model
    }
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
    pub fn analyzed_at(&self) -> &str {
        &self.analyzed_at
    }
    pub fn sent_ids(&self) -> Vec<i64> {
        self.manifest.sent.iter().map(|s| s.photo_id).collect()
    }
    /// 送信した写真の位置（送信順の 0 始まり）。送っていない構成員は `None`。
    pub fn position(&self, photo_id: i64) -> Option<usize> {
        self.manifest
            .sent
            .iter()
            .position(|s| s.photo_id == photo_id)
    }
    /// 現在の候補（shortlist の写真と世代）に対してこの回答が有効か。
    pub fn applies_to(&self, candidate: &Candidate) -> bool {
        self.manifest.prompt_version == PROMPT_VERSION
            && self.manifest.processor_version == processor_version(&self.manifest.model)
            && {
                let mut mine = self.manifest.pairs();
                let mut theirs = candidate.sent.clone();
                mine.sort();
                theirs.sort();
                mine == theirs
            }
    }
}

#[derive(Debug)]
pub enum Lookup {
    Reusable(Box<Saved>),
    Absent,
    /// 履歴が破損している、または表が無い。送信せず、再課金もしない。
    Unverified,
}

fn integrity(manifest: &str, verdict: &str, attempts: u32, at: &str) -> String {
    sha(encoded(&(manifest, verdict, attempts, at)).as_bytes())
}
fn valid_manifest(m: &Manifest) -> bool {
    let ids: std::collections::HashSet<_> = m.sent.iter().map(|s| s.photo_id).collect();
    !m.prompt_version.is_empty()
        && m.prompt_version.len() <= 64
        && valid_hash(&m.processor_version)
        && !m.model.is_empty()
        && m.model.len() <= 256
        && (2..=MAX_SEND).contains(&m.sent.len())
        && ids.len() == m.sent.len()
        && m.sent.iter().all(|s| {
            valid_hash(&s.source_sha256) && valid_hash(&s.jpeg_sha256) && valid_hash(&s.generation)
        })
}

/// 履歴の表があるか。1 リクエスト・1 周期で 1 回だけ確かめ、以降は [`lookup_in`] で読む。
pub(super) fn table_present(conn: &Connection) -> rusqlite::Result<bool> {
    jobs::object_exists(conn, "table", TABLES[0])
}

/// 回答 1 件を読み、キー・整合 hash・manifest・正規化済み verdict を全て照合する。
/// 表が無ければ `Unverified`（送らず・再課金もしない）。
pub fn lookup(conn: &Connection, input_key: &str) -> rusqlite::Result<Lookup> {
    if !table_present(conn)? {
        return Ok(Lookup::Unverified);
    }
    lookup_in(conn, input_key)
}

/// [`lookup`] のうち表の存在を確かめない部分。呼出し側が [`table_present`] で確かめ済みのときだけ使う。
pub(super) fn lookup_in(conn: &Connection, input_key: &str) -> rusqlite::Result<Lookup> {
    let row = conn
        .query_row(
            "SELECT manifest_json,verdict_json,attempts,analyzed_at,integrity_sha256
             FROM ai_scene_judgments WHERE input_key=?1",
            [input_key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((m, v, attempts, at, hash)) = row else {
        return Ok(Lookup::Absent);
    };
    let parsed = (|| {
        if m.len() > MAX_MANIFEST_BYTES
            || !(1..=jobs::MAX_ATTEMPTS).contains(&attempts)
            || at.len() != 20
            || integrity(&m, &v, attempts, &at) != hash
        {
            return None;
        }
        let manifest: Manifest = serde_json::from_str(&m).ok()?;
        if !valid_manifest(&manifest)
            || manifest.input_key() != input_key
            || encoded(&manifest) != m
        {
            return None;
        }
        let verdict = Verdict::from_stored(&v, manifest.sent.len())?;
        Some(Saved {
            manifest,
            verdict,
            attempts,
            analyzed_at: at,
        })
    })();
    Ok(parsed.map_or(Lookup::Unverified, |s| Lookup::Reusable(Box::new(s))))
}

/// 履歴にある回答の総数（STATUS 用。表が無ければ 0）。
pub(in crate::ai) fn count(conn: &Connection) -> rusqlite::Result<i64> {
    if !table_present(conn)? {
        return Ok(0);
    }
    conn.query_row("SELECT COUNT(*) FROM ai_scene_judgments", [], |r| r.get(0))
}

/// 現世代の置換候補（shortlist の写真と世代が一致する LocalRanked の Exact / Disjoint グループ）が
/// 今も在るか。全写真・全構成員を読むので、writer の tx 内では使わない（送る直前の確認用）。
/// `prepared` は `Prepared::read` したもの。
pub(super) fn candidate_current(
    conn: &Connection,
    prepared: &Prepared,
    wanted: &Candidate,
) -> rusqlite::Result<Option<Candidate>> {
    let loaded = prepared
        .load_in(conn)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(loaded
        .groups()
        .iter()
        .filter_map(Candidate::from_group)
        .find(|c| c == wanted))
}

/// 送った入力が今も有効か: 送った各写真が今もあり、世代が送信時と同じで（表示ファイル・品質解析・
/// 向き・更新時刻・撮影時刻が変わっていない）、hidden が許可内。場面（グループ）の状態は見ない。
/// 読むのは送った写真（最大 [`MAX_SEND`] 枚）の行だけで、写真の数に比例しない。
pub(super) fn input_current(
    conn: &Connection,
    candidate: &Candidate,
    include_hidden: bool,
) -> rusqlite::Result<bool> {
    let ids = candidate.sent_ids();
    let current =
        crate::ai::groups::generations_in(conn, &ids).map_err(|_| rusqlite::Error::InvalidQuery)?;
    Ok(candidate
        .sent
        .iter()
        .all(|(id, generation)| current.get(id) == Some(generation))
        && hidden_ok(conn, &ids, include_hidden)?)
}

/// hidden を許可していない用途では、送る写真に hidden が一枚でもあれば不可。
pub(super) fn hidden_ok(
    conn: &Connection,
    ids: &[i64],
    include_hidden: bool,
) -> rusqlite::Result<bool> {
    if include_hidden {
        return Ok(true);
    }
    for id in ids {
        let hidden: Option<bool> = conn
            .query_row(
                "SELECT COALESCE(hidden,0)!=0 FROM photos WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if hidden != Some(false) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `Handler::run` が返す Persist 用（`jobs::complete` の tx 内のみ）。有効な token・送った入力の現在性
/// （送った写真の ID と世代・hidden 許可）・未保存を同じ tx で再照合して不変履歴へ書く。読むのは claim・
/// 送った写真の行・履歴 1 件・ジョブ 1 件だけで、単一 writer を止める時間は写真の数に比例しない。
/// 場面（グループ）の状態は見ない: 応答待ちに Waiting や Partial になっても、送った入力が同じなら
/// 有償回答は履歴に残し（再課金を避ける）、表示に使えるかは読むときに `applies_to` と Disjoint で決める。
/// 入力が変わった結果・旧 token では何も書かない。ファイル・HTTP はここで行わない。
pub fn persist(
    conn: &Connection,
    claimed: &Claimed,
    plan: &Plan,
    verdict: &Verdict,
    include_hidden: bool,
    now: i64,
) -> rusqlite::Result<InputCheck> {
    if conn.is_autocommit() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if !jobs::owns(conn, claimed)?
        || claimed.key != plan.key()
        || verdict.photos().len() != plan.manifest.sent.len()
        || !input_current(conn, &plan.candidate, include_hidden)?
    {
        return Ok(InputCheck::Changed);
    }
    let key = plan.candidate.input_key(&plan.manifest.model);
    match lookup(conn, &key)? {
        // 既に同じ入力の回答がある。別の回答で置き換えない（同じ内容なら成功扱い）。
        Lookup::Reusable(prior) => {
            return if prior.verdict == *verdict {
                Ok(InputCheck::Current)
            } else {
                Err(rusqlite::Error::InvalidQuery)
            };
        }
        Lookup::Unverified => return Ok(InputCheck::Changed),
        Lookup::Absent => {}
    }
    let attempts: u32 = conn.query_row(
        "SELECT attempts FROM ai_jobs WHERE id=?1",
        [claimed.id],
        |r| r.get(0),
    )?;
    if attempts >= jobs::MAX_ATTEMPTS {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let attempts = attempts + 1;
    let at = crate::ai::waits::utc_iso(now);
    let (m, v) = (encoded(&plan.manifest), verdict.normalized_json());
    if m.len() > MAX_MANIFEST_BYTES {
        return Err(rusqlite::Error::InvalidQuery);
    }
    conn.execute(
        "INSERT INTO ai_scene_judgments(input_key,manifest_json,verdict_json,attempts,analyzed_at,integrity_sha256)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![key, m, v, attempts, at, integrity(&m, &v, attempts, &at)],
    )?;
    Ok(InputCheck::Current)
}
