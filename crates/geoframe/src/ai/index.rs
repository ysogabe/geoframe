//! 検索索引の保存と読み取り（P4 / T4.3。設計は p4-design.md §6）。空間 ID・ベクトルの表と repository、索引の
//! 顔ぶれ（有効な対象）の導出、状態の集計、ジョブのキー。切替は [`super::activation`]、検索は [`super::search`]。
//!
//! # 表（追加だけ。旧 `photo_embeddings` / `photo_ai_features` には書かない）
//!
//! - `ai_embedding_spaces`: [`SpaceIdentity`] の全項目。`space_id` が主キーで、識別の組（`space_id` を除く全項目）にも
//!   UNIQUE 制約がある。同じ `space_id` を別の識別で再登録すると**拒否**し（黙って上書きしない）、同じ識別の組を
//!   別の `space_id` で登録することも拒否する。
//! - `ai_embedding_vectors`: `(space_id, input_sha256)` が主キー。`vector` は float32 リトルエンディアンの BLOB で、
//!   長さが `dimension * 4` であることを CHECK 制約でも守る。`input_sha256` は品質特徴量が解析した元 derivative の
//!   SHA（[`photo_quality_features.content_sha256`]）、`sent_sha256` は実際に Provider へ送った再エンコード JPEG の
//!   SHA（監査用。キーではない）。
//!
//! どちらも `REQUIRED_SCHEMA`・Python のスキーマの再現には入れず、取り込みが有効な起動で [`migrate`] が足す。
//! 表が無い旧 DB（`INGEST=0`）は、新しい空間なしとして読める（[`status`] は `NoTables`）。
//!
//! # 索引の顔ぶれ
//!
//! 固定の全 index の表は作らない。**今の対象**は `photos` と、世代（`display`・`md5`・`mtime`）が合う
//! `photo_quality_features`（`error=''`・`content_sha256` あり）の結合で、`display` が空でない写真。同じ
//! `content_sha256` は 1 件に数える（代表は最小の写真 ID。旧 `ai_pipeline` の正本と同じ）。**`hidden`・`favorite` は
//! 対象を変えない**（検索側が `include_hidden` などで絞る。旧 E6 と同じ）。回転は `display` の名前を変えるので、
//! 回転後は世代が合わず「未解析」に戻り、次の品質解析の `content_sha256` で別の入力として扱われる（品質テーブルに
//! 無い列は仮定しない）。更新（`md5`・`mtime`）・削除・`display` の変化も同じく、世代が合わなくなって対象から外れる。
//! 空間のベクトルは `input_sha256` で結合した行だけが有効（古い SHA のベクトルは参照されない）。
//!
//! # ジョブのキー
//!
//! 画像 Embedding のジョブは空間と入力の SHA 単位（[`image_embedding_key`]）。後続（T4.4）は同じ関数でキーを作る。
//! URL・送信ポリシー・設定全体のハッシュはキーに入れない。失敗の判定（[`Progress::failed_excluded`]）は
//! `kind`・`target_key`（空間と SHA）・`input_hash`・`processor_version`（識別の全項目から決まる）の 4 つを厳密に
//! 照合するので、他の空間や古い版の失敗では対象を除かない。

use std::collections::HashSet;
use std::fmt;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::jobs::{JobKey, JobKind, object_exists};
use super::settings::Normalization;
use super::space::{self, IdentityError, SpaceIdentity, VectorError};

pub const TABLES: &[&str] = &["ai_embedding_spaces", "ai_embedding_vectors"];
pub const INDEXES: &[&str] = &["idx_ai_embedding_vectors_sha"];

/// 有効な検索空間を指す `settings` のキー（値は `space_id`）。
pub const ACTIVE_KEY: &str = "ai_active_space";

/// 切替の成功率の既定（設計の `activation_min_coverage`）。呼び出し側（設定・ワーカー）が別の値を渡してよい。
pub const DEFAULT_ACTIVATION_MIN_COVERAGE: f64 = 0.95;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_embedding_spaces(
  space_id TEXT PRIMARY KEY,
  provider_space_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  revision TEXT NOT NULL,
  image_preprocess TEXT NOT NULL,
  text_preprocess TEXT NOT NULL,
  dimension INTEGER NOT NULL CHECK(dimension BETWEEN 1 AND 4096),
  normalization TEXT NOT NULL CHECK(normalization IN ('l2','none')),
  created_at INTEGER NOT NULL,
  UNIQUE(provider_space_id, model_id, revision, image_preprocess, text_preprocess,
         dimension, normalization)
);
CREATE TABLE IF NOT EXISTS ai_embedding_vectors(
  space_id TEXT NOT NULL REFERENCES ai_embedding_spaces(space_id),
  input_sha256 TEXT NOT NULL,
  sent_sha256 TEXT NOT NULL,
  dimension INTEGER NOT NULL CHECK(dimension BETWEEN 1 AND 4096),
  vector BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(space_id, input_sha256),
  CHECK(length(vector)=dimension*4)
);
CREATE INDEX IF NOT EXISTS idx_ai_embedding_vectors_sha ON ai_embedding_vectors(input_sha256);";

// ---------------------------------------------------------------- 表の作成

/// まだ無い表・索引の名前（読み取りだけ。`--dry-run` 用）。
pub fn pending(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut missing = Vec::new();
    for table in TABLES {
        if !object_exists(conn, "table", table)? {
            missing.push((*table).to_string());
        }
    }
    for index in INDEXES {
        if !object_exists(conn, "index", index)? {
            missing.push((*index).to_string());
        }
    }
    Ok(missing)
}

/// 表・索引を作る（`IF NOT EXISTS`。既に最新なら何も書かない）。作った名前を返す。書き込み用の接続で呼ぶ。
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    let missing = pending(conn)?;
    if missing.is_empty() {
        return Ok(missing);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    tx.commit()?;
    Ok(missing)
}

// ---------------------------------------------------------------- 空間の登録

/// 空間の登録の失敗。
#[derive(Debug)]
pub enum SpaceError {
    /// その `space_id` は別の識別で登録済み（上書きしない）。
    SpaceIdConflict,
    /// 同じ識別の組が別の `space_id` で登録済み。
    IdentityConflict,
    /// 保存された行が識別として不正（壊れている）。
    Corrupt,
    Db(rusqlite::Error),
}

impl fmt::Display for SpaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpaceIdConflict => f.write_str("その空間 ID は別の識別で登録済みです"),
            Self::IdentityConflict => f.write_str("同じ識別の空間が別の ID で登録済みです"),
            Self::Corrupt => f.write_str("保存された空間の識別が不正です"),
            Self::Db(e) => write!(f, "DB のエラー: {e}"),
        }
    }
}

impl std::error::Error for SpaceError {}

impl From<rusqlite::Error> for SpaceError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registered {
    Created,
    /// 同じ識別が既にある（何も変えない）。
    Existing,
}

/// 空間を登録する。同じ `space_id` が**同じ識別**なら何もしない。別の識別なら [`SpaceError::SpaceIdConflict`]
/// （上書きしない）。識別の組が別の `space_id` に登録済みなら [`SpaceError::IdentityConflict`]。1 つの短い
/// トランザクション（writer 上で呼ぶ）。
pub fn register_space(
    conn: &mut Connection,
    identity: &SpaceIdentity,
    now: i64,
) -> Result<Registered, SpaceError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(existing) = load_space(&tx, identity.space_id())? {
        return if existing == *identity {
            Ok(Registered::Existing)
        } else {
            Err(SpaceError::SpaceIdConflict)
        };
    }
    let same_tuple: Option<String> = tx
        .query_row(
            "SELECT space_id FROM ai_embedding_spaces
             WHERE provider_space_id=?1 AND model_id=?2 AND revision=?3 AND image_preprocess=?4
               AND text_preprocess=?5 AND dimension=?6 AND normalization=?7",
            params![
                identity.provider_space_id(),
                identity.model_id(),
                identity.revision(),
                identity.image_preprocess(),
                identity.text_preprocess(),
                identity.dimension(),
                identity.normalization().as_str(),
            ],
            |row| row.get(0),
        )
        .optional()?;
    if same_tuple.is_some() {
        return Err(SpaceError::IdentityConflict);
    }
    tx.execute(
        "INSERT INTO ai_embedding_spaces(space_id,provider_space_id,model_id,revision,
           image_preprocess,text_preprocess,dimension,normalization,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            identity.space_id(),
            identity.provider_space_id(),
            identity.model_id(),
            identity.revision(),
            identity.image_preprocess(),
            identity.text_preprocess(),
            identity.dimension(),
            identity.normalization().as_str(),
            now,
        ],
    )?;
    tx.commit()?;
    Ok(Registered::Created)
}

/// 登録済みの空間の識別（無ければ `None`。行が不正なら [`SpaceError::Corrupt`]）。
pub fn load_space(conn: &Connection, space_id: &str) -> Result<Option<SpaceIdentity>, SpaceError> {
    let row = conn
        .query_row(
            "SELECT provider_space_id,model_id,revision,image_preprocess,text_preprocess,
                    dimension,normalization
             FROM ai_embedding_spaces WHERE space_id=?1",
            [space_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((provider, model, revision, image, text, dimension, normalization)) = row else {
        return Ok(None);
    };
    let normalization = match normalization.as_str() {
        "l2" => Normalization::L2,
        "none" => Normalization::Raw,
        _ => return Err(SpaceError::Corrupt),
    };
    let dimension = u32::try_from(dimension).map_err(|_| SpaceError::Corrupt)?;
    SpaceIdentity::new(
        space_id,
        &provider,
        &model,
        &revision,
        &image,
        &text,
        dimension,
        normalization,
    )
    .map(Some)
    .map_err(|_: IdentityError| SpaceError::Corrupt)
}

// ---------------------------------------------------------------- ベクトルの保存

/// ベクトルの保存の失敗。
#[derive(Debug)]
pub enum StoreError {
    /// 入力の SHA が不正（空・長すぎる・空白や制御文字を含む）。
    BadSha,
    Vector(VectorError),
    /// その空間が未登録、または登録済みの識別と違う（別の空間のベクトルとして混ぜない）。
    UnknownSpace,
    SpaceMismatch,
    Corrupt,
    Db(rusqlite::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSha => f.write_str("入力の SHA が不正です"),
            Self::Vector(e) => write!(f, "{e}"),
            Self::UnknownSpace => f.write_str("その空間は登録されていません"),
            Self::SpaceMismatch => f.write_str("空間の識別が登録済みのものと違います"),
            Self::Corrupt => f.write_str("保存された空間の識別が不正です"),
            Self::Db(e) => write!(f, "DB のエラー: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

impl From<VectorError> for StoreError {
    fn from(e: VectorError) -> Self {
        Self::Vector(e)
    }
}

fn is_sha(text: &str) -> bool {
    !text.is_empty() && text.len() <= 128 && text.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// 検査・正規化の済んだ、保存できるベクトル（書き込みの前に作る。重い計算は writer の外）。
pub struct PreparedVector {
    identity: SpaceIdentity,
    input_sha256: String,
    sent_sha256: String,
    blob: Vec<u8>,
}

impl fmt::Debug for PreparedVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedVector").finish_non_exhaustive()
    }
}

impl PreparedVector {
    /// `values` を `identity` で検査（次元・有限数・ゼロでないノルム）して、正規化規則どおりの形にする。
    pub fn new(
        identity: &SpaceIdentity,
        input_sha256: &str,
        sent_sha256: &str,
        values: &[f32],
    ) -> Result<Self, StoreError> {
        if !is_sha(input_sha256) || !is_sha(sent_sha256) {
            return Err(StoreError::BadSha);
        }
        let stored = space::prepare(identity, values)?;
        Ok(Self {
            identity: identity.clone(),
            input_sha256: input_sha256.to_string(),
            sent_sha256: sent_sha256.to_string(),
            blob: space::encode(&stored),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Inserted,
    /// 同じ `(space_id, input_sha256)` の有効なベクトルが既にある（最初に保存したものを残す）。
    Existing,
    /// 既存の行が壊れていたので置き換えた。
    Repaired,
}

/// 検査済みのベクトルを保存する（1 つの短いトランザクション。writer 上で呼ぶ）。同じ空間・同じ入力 SHA が既に
/// 有効なら何も変えない。登録済みの識別と違えば拒否する。旧 `photo_embeddings` / `photo_ai_features` には
/// 書かない。
pub fn store_prepared(
    conn: &mut Connection,
    vector: &PreparedVector,
    now: i64,
) -> Result<Stored, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match load_space(&tx, vector.identity.space_id()) {
        Ok(Some(registered)) if registered == vector.identity => {}
        Ok(Some(_)) => return Err(StoreError::SpaceMismatch),
        Ok(None) => return Err(StoreError::UnknownSpace),
        Err(SpaceError::Db(e)) => return Err(StoreError::Db(e)),
        Err(_) => return Err(StoreError::Corrupt),
    }
    let existing: Option<Vec<u8>> = tx
        .query_row(
            "SELECT vector FROM ai_embedding_vectors WHERE space_id=?1 AND input_sha256=?2",
            params![vector.identity.space_id(), vector.input_sha256],
            |row| row.get(0),
        )
        .optional()?;
    let outcome = match existing {
        Some(blob) if space::decode(&vector.identity, &blob).is_ok() => {
            return Ok(Stored::Existing);
        }
        Some(_) => {
            tx.execute(
                "UPDATE ai_embedding_vectors SET sent_sha256=?3, dimension=?4, vector=?5, created_at=?6
                 WHERE space_id=?1 AND input_sha256=?2",
                params![
                    vector.identity.space_id(),
                    vector.input_sha256,
                    vector.sent_sha256,
                    vector.identity.dimension(),
                    vector.blob,
                    now
                ],
            )?;
            Stored::Repaired
        }
        None => {
            tx.execute(
                "INSERT INTO ai_embedding_vectors(space_id,input_sha256,sent_sha256,dimension,
                   vector,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    vector.identity.space_id(),
                    vector.input_sha256,
                    vector.sent_sha256,
                    vector.identity.dimension(),
                    vector.blob,
                    now
                ],
            )?;
            Stored::Inserted
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// [`PreparedVector::new`] と [`store_prepared`] を続けて行う（writer 上で呼ぶとき、検査・正規化も writer の中に
/// なる。writer の外で [`PreparedVector::new`] を済ませたいときは分けて呼ぶ）。
pub fn store_vector(
    conn: &mut Connection,
    identity: &SpaceIdentity,
    input_sha256: &str,
    sent_sha256: &str,
    values: &[f32],
    now: i64,
) -> Result<Stored, StoreError> {
    let prepared = PreparedVector::new(identity, input_sha256, sent_sha256, values)?;
    store_prepared(conn, &prepared, now)
}

// ---------------------------------------------------------------- ジョブのキー

/// 画像 Embedding ジョブの処理版。識別の全項目から決まる（URL・ポリシー・設定全体のハッシュは入れない）。
pub fn image_embedding_processor_version(identity: &SpaceIdentity) -> String {
    format!("image_embedding/v1/{}", identity.fingerprint())
}

/// 画像 Embedding ジョブの冪等キー（空間と入力 SHA 単位）。`target_key` は空間と SHA、`input_hash` は SHA。
/// 後続の reconcile・ワーカー（T4.4）は**この関数**でキーを作る。
pub fn image_embedding_key(identity: &SpaceIdentity, input_sha256: &str) -> JobKey {
    JobKey::new(
        JobKind::ImageEmbedding,
        image_embedding_target_key(identity.space_id(), input_sha256),
        input_sha256,
        image_embedding_processor_version(identity),
    )
}

fn image_embedding_target_key(space_id: &str, input_sha256: &str) -> String {
    format!("{space_id}\u{1f}{input_sha256}")
}

// ---------------------------------------------------------------- 今の対象

/// 今の対象の 1 行（写真と、世代の合う品質特徴量）。`md5`・`mtime` は DB の値のまま持つ（NULL どうしも一致）。
#[derive(Clone, PartialEq)]
pub struct Eligible {
    pub photo_id: i64,
    pub input_sha256: String,
    pub display: String,
    pub md5: Value,
    pub mtime: Value,
}

impl fmt::Debug for Eligible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Eligible").finish_non_exhaustive()
    }
}

/// 今の対象の 1 件（`content_sha256` ごとの代表）と、その空間での状態。
#[derive(Debug, Clone, PartialEq)]
pub struct Classified {
    pub target: Eligible,
    /// その空間に有効（長さが合う）ベクトルがある。
    pub has_vector: bool,
    /// 現在の入力・処理版（[`image_embedding_key`]）のジョブが `failed` のとき、その理由コード。
    pub failed_code: Option<String>,
}

/// 対象の結合（写真と、世代の合う品質特徴量）。
pub(super) const ELIGIBLE_JOIN: &str = "FROM photos p JOIN photo_quality_features f
   ON f.photo_id=p.id AND f.display=p.display
  AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime";
/// 対象の条件。
pub(super) const ELIGIBLE_WHERE: &str = "p.display IS NOT NULL AND p.display!='' AND f.error=''
   AND f.content_sha256 IS NOT NULL AND f.content_sha256!=''";

/// 今の対象を、`content_sha256` ごとの代表（最小の写真 ID）にして返す。`space` があれば、その空間のベクトルの有無と
/// 失敗したジョブを添える（なければ両方なし）。表が無い（旧 DB）ときもエラーにしない。
pub fn classify(
    conn: &Connection,
    space: Option<&SpaceIdentity>,
) -> rusqlite::Result<Vec<Classified>> {
    let has_vectors = object_exists(conn, "table", "ai_embedding_vectors")?;
    let has_jobs = object_exists(conn, "table", "ai_jobs")?;
    let (has_vector_sql, failed_sql) = match space {
        Some(_) => (
            if has_vectors {
                "EXISTS(SELECT 1 FROM ai_embedding_vectors v
                         WHERE v.space_id=?1 AND v.input_sha256=f.content_sha256
                           AND v.dimension=?2 AND length(v.vector)=?2*4)"
            } else {
                "0"
            },
            if has_jobs {
                "(SELECT j.last_error_code FROM ai_jobs j
                   WHERE j.kind='image_embedding' AND j.state='failed'
                     AND j.target_key=?1||char(31)||f.content_sha256
                     AND j.input_hash=f.content_sha256 AND j.processor_version=?3)"
            } else {
                "NULL"
            },
        ),
        None => ("0", "NULL"),
    };
    let sql = format!(
        "SELECT p.id, f.content_sha256, p.display, p.md5, p.mtime, {has_vector_sql}, {failed_sql}
         {ELIGIBLE_JOIN} WHERE {ELIGIBLE_WHERE} ORDER BY p.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Classified> {
        Ok(Classified {
            target: Eligible {
                photo_id: row.get(0)?,
                input_sha256: row.get(1)?,
                display: row.get(2)?,
                md5: row.get(3)?,
                mtime: row.get(4)?,
            },
            has_vector: row.get::<_, i64>(5)? != 0,
            failed_code: row.get(6)?,
        })
    };
    // 使う番号（?1 空間 ID・?2 次元・?3 処理版）は表の有無で変わる。文が持つ数だけを束縛する
    let all: Vec<Value> = match space {
        Some(identity) => vec![
            Value::Text(identity.space_id().to_string()),
            Value::Integer(i64::from(identity.dimension())),
            Value::Text(image_embedding_processor_version(identity)),
        ],
        None => Vec::new(),
    };
    let used = stmt.parameter_count();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(all.into_iter().take(used)), map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen = HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|c| seen.insert(c.target.input_sha256.clone()))
        .collect())
}

// ---------------------------------------------------------------- 状態

/// 有効な検索空間のポインタの状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveState {
    /// 空間の表が無い（旧 DB。新しい空間はない）。
    NoTables,
    /// ポインタが無い（旧結果参照の状態）。
    Unset,
    Active(String),
    /// ポインタが未登録・不正な空間を指している（空間なしとして扱う）。
    Unusable,
}

/// ある空間の構築の進み具合（今の対象に対する数）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// 今の対象（`content_sha256` の種類数）。
    pub eligible: i64,
    /// ベクトルが保存されている数（有効な結合）。
    pub indexed: i64,
    /// ベクトルが無く、現在の入力・処理版の `failed` でもない数（待ち・未登録のジョブを含む）。
    pub pending: i64,
    /// ベクトルが無く、現在の入力・処理版のジョブが `failed` で、対象から除かれる数。
    pub failed_excluded: i64,
    /// `failed_excluded` の理由コードごとの件数（コード順）。
    pub failed_reasons: Vec<(String, i64)>,
}

impl Progress {
    fn from_classified(rows: &[Classified]) -> Self {
        let mut p = Progress {
            eligible: rows.len() as i64,
            ..Progress::default()
        };
        let mut reasons: std::collections::BTreeMap<String, i64> = Default::default();
        for row in rows {
            if row.has_vector {
                p.indexed += 1;
            } else if let Some(code) = &row.failed_code {
                p.failed_excluded += 1;
                *reasons.entry(code.clone()).or_insert(0) += 1;
            } else {
                p.pending += 1;
            }
        }
        p.failed_reasons = reasons.into_iter().collect();
        p
    }
}

/// 状態（件数だけ。写真の値・SHA・ベクトルは持たない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStatus {
    pub active: ActiveState,
    pub progress: Progress,
    /// 検索で読むときに壊れていた数（[`super::search::load_active_index`] だけが数える。ここでは 0）。
    pub corrupt: i64,
    /// active 以外の保存済み空間のベクトルにある `input_sha256` の種類数。
    pub other_space_shas: i64,
    /// 旧 `photo_ai_features`（`error=''`）・`photo_embeddings` にある `content_sha256` の種類数
    /// （`legacy-unverified`。新しい空間には使わない）。
    pub legacy_shas: i64,
    /// `other_space_shas` と `legacy_shas` を合わせた、重複のない `input_sha256` の種類数。
    pub other_shas: i64,
    /// 登録済みの空間の数。
    pub spaces: i64,
}

/// active ポインタの状態と、そのときの識別。
pub(super) fn active_in(
    conn: &Connection,
) -> rusqlite::Result<(ActiveState, Option<SpaceIdentity>)> {
    if !object_exists(conn, "table", "ai_embedding_spaces")? {
        return Ok((ActiveState::NoTables, None));
    }
    let value: Option<Value> = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [ACTIVE_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Ok((ActiveState::Unset, None));
    };
    let Value::Text(space_id) = value else {
        return Ok((ActiveState::Unusable, None));
    };
    match load_space(conn, &space_id) {
        Ok(Some(identity)) => Ok((ActiveState::Active(space_id), Some(identity))),
        Ok(None) | Err(SpaceError::Corrupt) => Ok((ActiveState::Unusable, None)),
        Err(SpaceError::Db(e)) => Err(e),
        Err(_) => Ok((ActiveState::Unusable, None)),
    }
}

/// 状態の本体（ポインタと数を同じ接続・同じ読み取りの中で読む）。
pub(super) fn status_in(
    conn: &Connection,
) -> rusqlite::Result<(IndexStatus, Option<SpaceIdentity>)> {
    let (active, identity) = active_in(conn)?;
    let progress = Progress::from_classified(&classify(conn, identity.as_ref())?);
    let has_vectors = object_exists(conn, "table", "ai_embedding_vectors")?;
    let active_id = identity.as_ref().map(SpaceIdentity::space_id);
    let other_space_shas = if has_vectors {
        conn.query_row(
            "SELECT COUNT(DISTINCT input_sha256) FROM ai_embedding_vectors WHERE space_id IS NOT ?1",
            [active_id],
            |row| row.get(0),
        )?
    } else {
        0
    };
    let mut legacy_sources = Vec::new();
    if object_exists(conn, "table", "photo_ai_features")? {
        legacy_sources.push("SELECT content_sha256 s FROM photo_ai_features WHERE error=''");
    }
    if object_exists(conn, "table", "photo_embeddings")? {
        legacy_sources.push("SELECT content_sha256 s FROM photo_embeddings");
    }
    let legacy_shas = if legacy_sources.is_empty() {
        0
    } else {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM ({})", legacy_sources.join(" UNION ")),
            [],
            |row| row.get(0),
        )?
    };
    let mut other_sources = legacy_sources;
    if has_vectors {
        other_sources
            .push("SELECT input_sha256 s FROM ai_embedding_vectors WHERE space_id IS NOT ?1");
    }
    let other_shas = if other_sources.is_empty() {
        0
    } else if has_vectors {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM ({})", other_sources.join(" UNION ")),
            [active_id],
            |row| row.get(0),
        )?
    } else {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM ({})", other_sources.join(" UNION ")),
            [],
            |row| row.get(0),
        )?
    };
    let spaces = if object_exists(conn, "table", "ai_embedding_spaces")? {
        conn.query_row("SELECT COUNT(*) FROM ai_embedding_spaces", [], |row| {
            row.get(0)
        })?
    } else {
        0
    };
    Ok((
        IndexStatus {
            active,
            progress,
            corrupt: 0,
            other_space_shas,
            legacy_shas,
            other_shas,
            spaces,
        },
        identity,
    ))
}

/// 状態を読む（読み取りだけ。表が無い旧 DB でも書き込みなしで動く）。ポインタと本体は 1 つの読み取り
/// トランザクションの中で読むので、並行する切替の前後が混ざらない。
pub fn status(conn: &Connection) -> rusqlite::Result<IndexStatus> {
    let tx = conn.unchecked_transaction()?;
    let (status, _) = status_in(&tx)?;
    tx.commit()?;
    Ok(status)
}

/// 指定した空間（まだ active でない構築中のものを含む）の進み具合。
pub fn progress(conn: &Connection, identity: &SpaceIdentity) -> rusqlite::Result<Progress> {
    let tx = conn.unchecked_transaction()?;
    let rows = classify(&tx, Some(identity))?;
    tx.commit()?;
    Ok(Progress::from_classified(&rows))
}
