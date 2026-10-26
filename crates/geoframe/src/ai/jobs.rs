//! 永続ジョブ `ai_jobs` の repository（P4 / T4.1a。設計は p4-design.md §4）。
//!
//! AI の仕事（画像 Embedding・位置の手がかり・位置候補・座標化・写真比較）の進行と再試行だけを覚える表。
//! 結果そのものは別の表（後続のタスク）に置き、結果の保存と `succeeded` は [`complete`] で同じ
//! トランザクションに入れる。HTTP・設定・ワーカーはここに無い。呼び出し側（後続のワーカー）が
//! writer（[`crate::writer::Writer::call`]）の上で、ここの関数を**短いトランザクション**として呼ぶ。
//! AI の通信・画像の準備のあいだは writer もトランザクションも持たない。
//!
//! # いつ作るか
//!
//! `ai_jobs` は [`crate::schema::migrate`]（Python と同じスキーマの再現）には入れず、取り込みが有効なときの
//! 起動で [`migrate`] が足す。`INGEST=0`（読み取り専用）の起動では作らない。表が無い旧 DB は
//! `db::REQUIRED_SCHEMA` を満たしたまま読め、[`status_counts`] は空を返す。
//!
//! # 冪等キー・世代
//!
//! 冪等キーは `(kind, target_key, input_hash, processor_version)` だけ（[`JobKey`]）。URL・タイムアウト・
//! 送信許可・設定のハッシュはキーに入れない（変えても成功済みの結果は再送しない）。入力が変わると
//! `input_hash` が変わり、別のジョブになる。古い世代のジョブは [`supersede`] か、確定時の入力検査
//! （[`complete`]）で `obsolete` になる。同じ入力の版を並行して持つ必要がある場合（例: 空間ごとの
//! 画像 Embedding）は、`target_key` にその識別を含める（`supersede` は同じ `kind`・`target_key` の
//! 別世代だけを無効にする）。
//!
//! # 失敗の履歴（`terminal`）
//!
//! 入力が A→B→A と往復しても、同じキーの試行回数は増やせない。`obsolete` からの復活（[`enqueue`]）は
//! `attempts`・`defer_count`・`interruption_count`・待ち時刻を引き継ぎ、一度でも `failed`（上限・恒久失敗・
//! 中断上限）になったキーは `terminal` の印で覚え、`obsolete` から戻すときも `failed` に戻して claim させない。
//! カウンタを 0 にして再び走らせるのは、明示の手動再試行（[`retry_failed`] の [`Recheck::Requeue`]）だけ。
//!
//! # 所有（run_id・claim_token）
//!
//! 時限の lease は無い（`.ingest.lock` で 1 プロセス・1 ワーカーに限るため）。[`claim_next`] が `running` にし、
//! 起動ごとの `run_id` とランダムな `claim_token` を付ける。結果の確定・失敗の記録は両方が合う `running` の
//! 行にだけ効く（[`Completion::Rejected`] / `None`）。再起動後の [`reclaim_interrupted`]・正常停止の
//! [`release_for_shutdown`]・[`supersede`] は token を失効させるので、遅れて届いた旧い応答は保存されない。
//!
//! # 時刻
//!
//! 全ての関数は `now`（UTC の Unix 秒）を引数に取る。時計を持たないので、試験で偽の時計を渡せる。
//! 他の表のテキスト時刻と違い整数にしてある（比較・加算のため）。
//!
//! # 秘匿
//!
//! 失敗の理由は固定の [`ErrorCode`] だけを保存する（例外の本文・URL・応答は保存しない）。`target_key`・
//! `input_hash`・`claim_token` は `Debug` に出さない。

use std::fmt;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};

/// このモジュールが作る表・索引（主キー・UNIQUE の自動索引は表に付いてくる）。
pub const TABLES: &[&str] = &["ai_jobs"];
pub const INDEXES: &[&str] = &["idx_ai_jobs_runnable"];

/// 通常の失敗の上限（5 回目で `failed`）。
pub const MAX_ATTEMPTS: u32 = 5;
/// 予期しない中断の上限（3 回目で `failed/interrupted`）。
pub const MAX_INTERRUPTIONS: u32 = 3;
/// 同じジョブだけが反復して失敗したとき、失敗として数え始める回数（[`Failure::Repeated`]）。
pub const REPEATED_FAILURE_THRESHOLD: u32 = 3;
/// 後回しの最短・最長（秒）。Retry-After もこの範囲に収める。
pub const MIN_DEFER_SECS: u32 = 60;
pub const MAX_DEFER_SECS: u32 = 3600;

/// `ai_jobs`。`kind`・`state` は文字列の列挙で、CHECK 制約で固定する。`query_embedding` は入れない
/// （検索語の Embedding は永続キューに載せない）。
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_jobs(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN
    ('image_embedding','location_clue','location_suggest','location_geocode','photo_judge')),
  target_key TEXT NOT NULL,
  input_hash TEXT NOT NULL,
  processor_version TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN
    ('pending','running','deferred','succeeded','failed','obsolete')),
  attempts INTEGER NOT NULL DEFAULT 0,
  defer_count INTEGER NOT NULL DEFAULT 0,
  interruption_count INTEGER NOT NULL DEFAULT 0,
  terminal INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  run_id TEXT,
  claim_token TEXT,
  last_error_code TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(kind, target_key, input_hash, processor_version)
);
CREATE INDEX IF NOT EXISTS idx_ai_jobs_runnable ON ai_jobs(state, next_attempt_at, id);";

// ---------------------------------------------------------------- 型

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    ImageEmbedding,
    LocationClue,
    LocationSuggest,
    LocationGeocode,
    PhotoJudge,
}

impl JobKind {
    pub const ALL: [Self; 5] = [
        Self::ImageEmbedding,
        Self::LocationClue,
        Self::LocationSuggest,
        Self::LocationGeocode,
        Self::PhotoJudge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImageEmbedding => "image_embedding",
            Self::LocationClue => "location_clue",
            Self::LocationSuggest => "location_suggest",
            Self::LocationGeocode => "location_geocode",
            Self::PhotoJudge => "photo_judge",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobState {
    Pending,
    Running,
    Deferred,
    Succeeded,
    Failed,
    Obsolete,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Deferred => "deferred",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Obsolete => "obsolete",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Pending,
            Self::Running,
            Self::Deferred,
            Self::Succeeded,
            Self::Failed,
            Self::Obsolete,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }
}

/// 失敗・後回しの理由。**固定の列挙だけ**を保存し、例外の本文・URL・応答は入れない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Provider が使えない（available=false・GPU lease・到達不能）。
    Unavailable,
    RateLimited,
    Timeout,
    ServerError,
    BadResponse,
    InputTooLarge,
    InputInvalid,
    PolicyDisabled,
    /// 正常停止で戻した。
    Shutdown,
    /// 予期しない中断（起動時の回収）。
    Interrupted,
    Panic,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::ServerError => "server_error",
            Self::BadResponse => "bad_response",
            Self::InputTooLarge => "input_too_large",
            Self::InputInvalid => "input_invalid",
            Self::PolicyDisabled => "policy_disabled",
            Self::Shutdown => "shutdown",
            Self::Interrupted => "interrupted",
            Self::Panic => "panic",
            Self::Internal => "internal",
        }
    }
}

/// 冪等キー。`target_key`・`input_hash` は内部の値（写真 ID・SHA など）なので `Debug` に出さない。
#[derive(Clone, PartialEq, Eq)]
pub struct JobKey {
    pub kind: JobKind,
    pub target_key: String,
    pub input_hash: String,
    pub processor_version: String,
}

impl JobKey {
    pub fn new(
        kind: JobKind,
        target_key: impl Into<String>,
        input_hash: impl Into<String>,
        processor_version: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            target_key: target_key.into(),
            input_hash: input_hash.into(),
            processor_version: processor_version.into(),
        }
    }
}

impl fmt::Debug for JobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobKey")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// [`claim_next`] が取った 1 件。確定・失敗の記録にそのまま渡す。
#[derive(Clone, PartialEq, Eq)]
pub struct Claimed {
    pub id: i64,
    pub key: JobKey,
    /// これまでの失敗の数（[`Failure::Attempt`]）。
    pub attempts: u32,
    /// これまでの反復失敗の数（[`Failure::Repeated`]）。
    pub defer_count: u32,
    pub(super) run_id: String,
    pub(super) token: String,
}

impl fmt::Debug for Claimed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claimed")
            .field("id", &self.id)
            .field("kind", &self.key.kind)
            .field("attempts", &self.attempts)
            .finish_non_exhaustive()
    }
}

/// 乱数が取れなかった（`run_id`・token を作れない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RandomError;

impl fmt::Display for RandomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("乱数を取得できません")
    }
}

impl std::error::Error for RandomError {}

/// 起動 ID・claim token に使う 128 ビットのランダムな 16 進文字列。乱数が取れないときは作らない
/// （0 埋めの token を黙って使い回さない）。
pub fn random_id() -> Result<String, RandomError> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| RandomError)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// 今の UTC の Unix 秒（本番の時計。試験は偽の値を渡す）。
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

// ---------------------------------------------------------------- 表の作成

fn object_exists(conn: &Connection, kind: &str, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2",
        [kind, name],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

/// `ai_jobs.terminal`（失敗の履歴）を後から足した列の名前。`pending` / `migrate` にはこの名前で出る。
const TERMINAL_COLUMN: &str = "ai_jobs.terminal";

fn has_terminal_column(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM pragma_table_info('ai_jobs') WHERE name='terminal'",
        [],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

/// まだ無い表・索引・列の名前（読み取りだけ。`--dry-run` 用）。
pub fn pending(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut missing = Vec::new();
    for table in TABLES {
        if !object_exists(conn, "table", table)? {
            missing.push((*table).to_string());
        }
    }
    if object_exists(conn, "table", "ai_jobs")? && !has_terminal_column(conn)? {
        missing.push(TERMINAL_COLUMN.to_string());
    }
    for index in INDEXES {
        if !object_exists(conn, "index", index)? {
            missing.push((*index).to_string());
        }
    }
    Ok(missing)
}

/// `ai_jobs` とその索引を作る（`IF NOT EXISTS`。既にあれば何も書かない）。作った名前を返す。
/// 書き込み用の接続で呼ぶ。他の表・行には触れない。
///
/// `terminal` 列の無い旧い `ai_jobs` には列を足し、既に失敗の履歴があるものに印を付ける: `failed` と、
/// 失敗・中断の上限に達したままの `obsolete`、恒久的な失敗の理由（`input_too_large`・`input_invalid`）が残る
/// `obsolete`。恒久的な失敗でも理由がこの 2 つでない旧い `obsolete` は、履歴が残っておらず復元できない。
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    let missing = pending(conn)?;
    if missing.is_empty() {
        return Ok(missing);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    if !has_terminal_column(&tx)? {
        tx.execute_batch("ALTER TABLE ai_jobs ADD COLUMN terminal INTEGER NOT NULL DEFAULT 0")?;
        tx.execute(
            "UPDATE ai_jobs SET terminal=1
             WHERE state='failed'
                OR (state='obsolete' AND (attempts>=?1 OR interruption_count>=?2
                                          OR last_error_code IN ('input_too_large','input_invalid')))",
            params![MAX_ATTEMPTS, MAX_INTERRUPTIONS],
        )?;
    }
    tx.commit()?;
    Ok(missing)
}

// ---------------------------------------------------------------- enqueue

/// [`enqueue`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    /// 新しい `pending` を作った。
    Created,
    /// 同じキーの未完了の `obsolete` が、入力が戻ったので `pending` に戻った。カウンタ（失敗・反復・中断）と
    /// 待ち時刻はそのまま引き継ぐ（入力の往復で数え直さない）。
    Revived,
    /// 同じキーが既にある（この状態のまま）。`succeeded` は再課金しない。`failed` は手動の再試行
    /// （[`retry_failed`]）まで戻らない。一度 `failed` になって `obsolete` にされたキー（[`supersede`]・
    /// 再検査の `Obsolete`）も、入力が戻ると `failed` に戻り、`Existing(Failed)` になる。
    Existing(JobState),
}

/// 冪等に積む。同じキーのジョブは増えない。**カウンタを 0 に戻すのは [`retry_failed`] の
/// [`Recheck::Requeue`] だけ**で、`enqueue` は決して戻さない（入力の A→B→A の往復で失敗の履歴を消して、
/// 同じ入力の有償の試行を際限なく足せないようにする）。
/// - 無い: `pending` を作る（`Created`）。
/// - `succeeded`・`failed`・未完了: 何も変えない（`Existing`）。
/// - `obsolete`: 一度も `failed` になっていなければ `pending` に戻し（`Revived`）、`failed` になったことが
///   あれば `failed` に戻して（`Existing(Failed)`）claim できないままにする。
pub fn enqueue(conn: &mut Connection, key: &JobKey, now: i64) -> rusqlite::Result<Enqueued> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let inserted = tx.execute(
        "INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,next_attempt_at,
                             created_at,updated_at)
         VALUES (?1,?2,?3,?4,?5,?5,?5)
         ON CONFLICT(kind,target_key,input_hash,processor_version) DO NOTHING",
        params![
            key.kind.as_str(),
            key.target_key,
            key.input_hash,
            key.processor_version,
            now
        ],
    )?;
    if inserted == 1 {
        tx.commit()?;
        return Ok(Enqueued::Created);
    }
    let (id, state, terminal): (i64, String, bool) = tx.query_row(
        "SELECT id,state,terminal FROM ai_jobs
         WHERE kind=?1 AND target_key=?2 AND input_hash=?3 AND processor_version=?4",
        params![
            key.kind.as_str(),
            key.target_key,
            key.input_hash,
            key.processor_version
        ],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let state = JobState::parse(&state).unwrap_or(JobState::Failed);
    if state != JobState::Obsolete {
        return Ok(Enqueued::Existing(state));
    }
    // カウンタ・待ち時刻・理由には触れない
    let (back, outcome) = if terminal {
        ("failed", Enqueued::Existing(JobState::Failed))
    } else {
        ("pending", Enqueued::Revived)
    };
    tx.execute(
        "UPDATE ai_jobs SET state=?2, claim_token=NULL, updated_at=?3 WHERE id=?1",
        params![id, back, now],
    )?;
    tx.commit()?;
    Ok(outcome)
}

/// 同じ `kind`・`target_key` の、`keep` と違う世代（`input_hash` か `processor_version` が違う）の
/// 未完了・失敗のジョブを `obsolete` にする。`running` も対象で、token が失効するので遅れて届く応答は
/// 保存されない。`succeeded` は結果なので変えない。無効にした件数を返す。
pub fn supersede(conn: &mut Connection, keep: &JobKey, now: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE ai_jobs SET state='obsolete', claim_token=NULL, updated_at=?5
         WHERE kind=?1 AND target_key=?2
           AND state IN ('pending','running','deferred','failed')
           AND NOT (input_hash=?3 AND processor_version=?4)",
        params![
            keep.kind.as_str(),
            keep.target_key,
            keep.input_hash,
            keep.processor_version,
            now
        ],
    )
}

// ---------------------------------------------------------------- claim

/// 実行できる 1 件を取り、`running`・`run_id`・`token` を付ける（1 文なので 1 回の短いトランザクション）。
/// 実行できるのは `pending`／`deferred` で `next_attempt_at <= now` のもの。`kinds` に無い種類は飛ばす
/// （利用できない用途を後回しにして、別の用途を進めるため）。空なら何も取らない。
///
/// `run_id` は起動ごとに 1 つ、`token` は claim ごとに新しく（[`random_id`]）。
pub fn claim_next(
    conn: &Connection,
    now: i64,
    run_id: &str,
    token: &str,
    kinds: &[JobKind],
) -> rusqlite::Result<Option<Claimed>> {
    if kinds.is_empty() {
        return Ok(None);
    }
    // 種類は固定の定数文字列だけ（利用者の入力は入らない）
    let in_list = kinds
        .iter()
        .map(|k| format!("'{}'", k.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE ai_jobs SET state='running', run_id=?1, claim_token=?2, updated_at=?3
         WHERE id=(SELECT id FROM ai_jobs
                   WHERE state IN ('pending','deferred') AND next_attempt_at<=?3
                     AND kind IN ({in_list})
                   ORDER BY next_attempt_at, id LIMIT 1)
         RETURNING id, kind, target_key, input_hash, processor_version, attempts, defer_count"
    );
    let row = conn
        .query_row(&sql, params![run_id, token, now], |row| {
            let kind: String = row.get(1)?;
            Ok(Claimed {
                id: row.get(0)?,
                key: JobKey {
                    kind: JobKind::parse(&kind).ok_or(rusqlite::Error::InvalidQuery)?,
                    target_key: row.get(2)?,
                    input_hash: row.get(3)?,
                    processor_version: row.get(4)?,
                },
                attempts: row.get(5)?,
                defer_count: row.get(6)?,
                run_id: run_id.to_string(),
                token: token.to_string(),
            })
        })
        .optional()?;
    Ok(row)
}

/// この claim が今も有効（`running` で `run_id`・token が合う）か。
fn owns(conn: &Connection, claimed: &Claimed) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM ai_jobs
         WHERE id=?1 AND state='running' AND run_id=?2 AND claim_token=?3",
        params![claimed.id, claimed.run_id, claimed.token],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

// ---------------------------------------------------------------- 結果の確定

/// [`complete`] の `persist` が返す、現在の入力の検査結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputCheck {
    /// 入力は今も同じ。`persist` が書いた結果を確定する。
    Current,
    /// 写真・候補集合・処理版などが変わっている。`persist` が書いたものは捨て、ジョブを `obsolete` にする。
    Changed,
}

/// [`complete`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// 結果と `succeeded` を確定した。
    Succeeded,
    /// 入力が変わっていた。結果は保存せず `obsolete`。
    Obsolete,
    /// この claim はもう有効でない（token の失効・旧起動・既に確定済み・無効化済み）。`persist` は呼ばない。
    Rejected,
}

/// 結果の保存とジョブの `succeeded` を**同じトランザクション**で確定する。
///
/// `persist` は writer の接続上、そのトランザクションの中で呼ばれる。**現在の入力の検査**（photos の
/// 世代など、job 行の `input_hash` ではなく今の写真）と**結果の保存**をその中で行い、[`InputCheck`] を返す。
/// - `Current`: 結果もジョブの `succeeded` も確定する。
/// - `Changed`: `persist` が書いた分も含めて結果を捨て、ジョブを `obsolete` にする（結果の行 0）。
/// - `Err`（またはパニック）: 全部ロールバックする。ジョブは `running` のままなので、呼び出し側が
///   [`record_failure`] で失敗を記録する。「結果は無いのに `succeeded` だけ残る」ことは無い。
///
/// `persist` の中で外部 I/O・長い処理をしない（writer を塞ぐ）。
pub fn complete<F>(
    conn: &mut Connection,
    claimed: &Claimed,
    now: i64,
    persist: F,
) -> rusqlite::Result<Completion>
where
    F: FnOnce(&Connection) -> rusqlite::Result<InputCheck>,
{
    let mut tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !owns(&tx, claimed)? {
        return Ok(Completion::Rejected);
    }
    let mut savepoint = tx.savepoint()?;
    let check = persist(&savepoint)?;
    let (state, outcome) = match check {
        InputCheck::Current => {
            savepoint.commit()?;
            ("succeeded", Completion::Succeeded)
        }
        InputCheck::Changed => {
            savepoint.rollback()?;
            drop(savepoint);
            ("obsolete", Completion::Obsolete)
        }
    };
    tx.execute(
        "UPDATE ai_jobs SET state=?2, claim_token=NULL, last_error_code='', updated_at=?3
         WHERE id=?1",
        params![claimed.id, state, now],
    )?;
    tx.commit()?;
    Ok(outcome)
}

/// 送信前など、結果を作る前に入力が古いと分かったジョブを `obsolete` にする（token が合うときだけ）。
/// 効いたら `true`。
pub fn mark_obsolete(conn: &mut Connection, claimed: &Claimed, now: i64) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE ai_jobs SET state='obsolete', claim_token=NULL, updated_at=?4
         WHERE id=?1 AND state='running' AND run_id=?2 AND claim_token=?3",
        params![claimed.id, claimed.run_id, claimed.token, now],
    )?;
    Ok(changed == 1)
}

// ---------------------------------------------------------------- 失敗・後回し

/// 失敗の扱い（[`record_failure`]）。どれを選ぶかは呼び出し側（Provider の状態を知っている）が決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Provider の停止・GPU lease・到達不能・429 など、写真の不良ではない後回し。失敗に数えず `deferred`。
    /// 次は 60〜3600 秒後（`retry_after_secs` があればそれを範囲に収める）。
    Defer { retry_after_secs: Option<u32> },
    /// 失敗 1 回（応答構造の不正、クラウドへの送信後の時間切れ・5xx など課金され得る試行）。`attempts` を
    /// 加算し、60/120/240/480 秒のバックオフで `pending`。[`MAX_ATTEMPTS`] 回で `failed`。
    Attempt,
    /// Provider は使えるのに同じジョブだけが反復して失敗する（5xx・時間切れ）。`defer_count` を加算して
    /// `deferred`（待ち行列の後ろへ）。[`REPEATED_FAILURE_THRESHOLD`] 回目からは各失敗を `attempts` にも
    /// 数え、[`MAX_ATTEMPTS`] 回で `failed`。
    Repeated,
    /// そのジョブだけの恒久的な失敗（入力サイズ超過・不正）。即 `failed`。後続のジョブは進む。
    Permanent,
}

/// 通常の失敗の n 回目（1 始まり）のあとの待ち秒: 60/120/240/480（以降 480）。
pub fn backoff_secs(n: u32) -> i64 {
    60i64 << n.saturating_sub(1).min(3)
}

/// 失敗・後回しを記録する。token が合う `running` の行にだけ効き、効かなければ `None`（旧い応答）。
/// 効いたときは記録後の状態を返す。理由の [`ErrorCode`] は固定の文字列だけが保存される。
pub fn record_failure(
    conn: &mut Connection,
    claimed: &Claimed,
    now: i64,
    code: ErrorCode,
    failure: Failure,
) -> rusqlite::Result<Option<JobState>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !owns(&tx, claimed)? {
        return Ok(None);
    }
    let (mut attempts, mut defer_count): (u32, u32) = tx.query_row(
        "SELECT attempts, defer_count FROM ai_jobs WHERE id=?1",
        [claimed.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (state, wait) = match failure {
        Failure::Defer { retry_after_secs } => (
            JobState::Deferred,
            i64::from(
                retry_after_secs
                    .unwrap_or(MIN_DEFER_SECS)
                    .clamp(MIN_DEFER_SECS, MAX_DEFER_SECS),
            ),
        ),
        Failure::Attempt => {
            attempts += 1;
            if attempts >= MAX_ATTEMPTS {
                (JobState::Failed, 0)
            } else {
                (JobState::Pending, backoff_secs(attempts))
            }
        }
        Failure::Repeated => {
            defer_count += 1;
            if defer_count < REPEATED_FAILURE_THRESHOLD {
                (JobState::Deferred, backoff_secs(defer_count))
            } else {
                attempts += 1;
                if attempts >= MAX_ATTEMPTS {
                    (JobState::Failed, 0)
                } else {
                    (JobState::Deferred, backoff_secs(attempts))
                }
            }
        }
        Failure::Permanent => (JobState::Failed, 0),
    };
    tx.execute(
        "UPDATE ai_jobs SET state=?2, attempts=?3, defer_count=?4,
                next_attempt_at=CASE WHEN ?2='failed' THEN next_attempt_at ELSE ?5 END,
                terminal=CASE WHEN ?2='failed' THEN 1 ELSE terminal END, claim_token=NULL,
                last_error_code=?6, updated_at=?7
         WHERE id=?1",
        params![
            claimed.id,
            state.as_str(),
            attempts,
            defer_count,
            now + wait,
            code.as_str(),
            now
        ],
    )?;
    tx.commit()?;
    Ok(Some(state))
}

// ---------------------------------------------------------------- 起動・停止

/// [`reclaim_interrupted`] の結果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reclaimed {
    /// 中断を 1 回数えて `pending` に戻した。
    pub requeued: usize,
    /// 中断が [`MAX_INTERRUPTIONS`] 回に達して `failed/interrupted` になった。
    pub failed: usize,
}

/// 起動時（ロックを取ったあと）、**別の起動**の `running` を回収する。失敗の回数（`attempts`）は増やさず、
/// `interruption_count` を加算して `pending` に戻す。[`MAX_INTERRUPTIONS`] 回目で `failed`（そのジョブだけ隔離し、
/// 同じジョブが全 AI を塞ぐ無限ループを防ぐ）。token は失効する。`current_run_id` の行には触れない。
pub fn reclaim_interrupted(
    conn: &mut Connection,
    current_run_id: &str,
    now: i64,
) -> rusqlite::Result<Reclaimed> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let failed = tx.execute(
        "UPDATE ai_jobs SET state='failed', interruption_count=interruption_count+1, terminal=1,
                claim_token=NULL, last_error_code='interrupted', updated_at=?2
         WHERE state='running' AND run_id IS NOT ?1 AND interruption_count+1>=?3",
        params![current_run_id, now, MAX_INTERRUPTIONS],
    )?;
    let requeued = tx.execute(
        "UPDATE ai_jobs SET state='pending', interruption_count=interruption_count+1,
                next_attempt_at=?2, claim_token=NULL, last_error_code='interrupted', updated_at=?2
         WHERE state='running' AND run_id IS NOT ?1",
        params![current_run_id, now],
    )?;
    tx.commit()?;
    Ok(Reclaimed { requeued, failed })
}

/// 正常停止の待機期限で、まだ終わっていない自分の起動の `running` を `deferred/shutdown` に戻す。
/// 失敗にも中断にも数えず、token を失効させる（その後に届く応答は保存されない）。戻した件数を返す。
pub fn release_for_shutdown(
    conn: &mut Connection,
    run_id: &str,
    now: i64,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE ai_jobs SET state='deferred', next_attempt_at=?2, claim_token=NULL,
                last_error_code='shutdown', updated_at=?2
         WHERE state='running' AND run_id=?1",
        params![run_id, now],
    )
}

// ---------------------------------------------------------------- 手動の再試行

/// [`retry_failed`] の対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryScope {
    All,
    Kind(JobKind),
    Id(i64),
}

/// 手動の再試行で、失敗したジョブを今の状態に照らして検査した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recheck {
    /// 入力・ポリシーが今も有効で、結果も無い。カウンタと失敗の履歴（`terminal`）を 0 にして `pending` に戻す。
    /// カウンタを消す唯一の経路（[`enqueue`] は消さない）。
    Requeue,
    /// 入力が変わった・ポリシーで許されない。`obsolete`（失敗の履歴は残り、入力が戻って [`enqueue`] されても
    /// `failed` のまま。再び走らせるには改めて手動の再試行が要る）。
    Obsolete,
    /// 現在の結果表に適合する結果が既にある。`succeeded`（再計算・再課金しない）。
    Satisfied,
}

/// [`retry_failed`] の集計。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Retried {
    pub requeued: usize,
    pub obsolete: usize,
    pub satisfied: usize,
}

/// `failed` のジョブだけを対象に、`recheck`（writer のトランザクション内で、現在の入力・ポリシー・既存結果を
/// 見る）の判断で戻す。`succeeded` など他の状態は決して変えない。
pub fn retry_failed<F>(
    conn: &mut Connection,
    now: i64,
    scope: RetryScope,
    mut recheck: F,
) -> rusqlite::Result<Retried>
where
    F: FnMut(&Connection, &JobKey) -> rusqlite::Result<Recheck>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (filter, arg) = match scope {
        RetryScope::All => ("1=1", None),
        RetryScope::Kind(kind) => ("kind=?1", Some(Value::from(kind.as_str().to_string()))),
        RetryScope::Id(id) => ("id=?1", Some(Value::from(id))),
    };
    let rows: Vec<(i64, JobKey)> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT id, kind, target_key, input_hash, processor_version FROM ai_jobs
             WHERE state='failed' AND {filter} ORDER BY id"
        ))?;
        stmt.query_map(params_from_iter(arg.iter()), |row| {
            let kind: String = row.get(1)?;
            Ok((
                row.get(0)?,
                JobKey {
                    kind: JobKind::parse(&kind).ok_or(rusqlite::Error::InvalidQuery)?,
                    target_key: row.get(2)?,
                    input_hash: row.get(3)?,
                    processor_version: row.get(4)?,
                },
            ))
        })?
        .collect::<Result<_, _>>()?
    };
    let mut done = Retried::default();
    for (id, key) in rows {
        match recheck(&tx, &key)? {
            Recheck::Requeue => {
                tx.execute(
                    "UPDATE ai_jobs SET state='pending', attempts=0, defer_count=0,
                            interruption_count=0, terminal=0, next_attempt_at=?2, claim_token=NULL,
                            last_error_code='', updated_at=?2
                     WHERE id=?1 AND state='failed'",
                    params![id, now],
                )?;
                done.requeued += 1;
            }
            Recheck::Obsolete => {
                tx.execute(
                    "UPDATE ai_jobs SET state='obsolete', claim_token=NULL, updated_at=?2
                     WHERE id=?1 AND state='failed'",
                    params![id, now],
                )?;
                done.obsolete += 1;
            }
            Recheck::Satisfied => {
                tx.execute(
                    "UPDATE ai_jobs SET state='succeeded', claim_token=NULL, last_error_code='',
                            updated_at=?2
                     WHERE id=?1 AND state='failed'",
                    params![id, now],
                )?;
                done.satisfied += 1;
            }
        }
    }
    tx.commit()?;
    Ok(done)
}

// ---------------------------------------------------------------- 状態の集計

/// 種類・状態ごとの件数（STATUS 用）。表が無い DB（`INGEST=0` の旧 DB）では空。読み取り専用の接続でよい。
pub fn status_counts(conn: &Connection) -> rusqlite::Result<Vec<(JobKind, JobState, i64)>> {
    if !object_exists(conn, "table", "ai_jobs")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT kind, state, COUNT(*) FROM ai_jobs GROUP BY kind, state ORDER BY 1, 2")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, state, count) = row?;
        if let (Some(kind), Some(state)) = (JobKind::parse(&kind), JobState::parse(&state)) {
            out.push((kind, state, count));
        }
    }
    Ok(out)
}
