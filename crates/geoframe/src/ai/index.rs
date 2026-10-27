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
//! 画像 Embedding の target は空間と入力 SHA（[`image_embedding_key`]）、実際の入力キーは
//! [`ShaState::job_key`] で全構成員の世代（rotation を含む）も束縛する。
//! URL・送信ポリシー・設定全体のハッシュはキーに入れない。失敗の判定（[`Progress::failed_excluded`]）は
//! `kind`・`target_key`（空間と SHA）・`input_hash`・`processor_version`（識別の全項目から決まる）の 4 つを厳密に
//! 照合するので、他の空間や古い版の失敗では対象を除かない。

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::jobs::{ErrorCode, JobKey, JobKind, object_exists};
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
    let result = register_space_in(&tx, identity, now)?;
    tx.commit()?;
    Ok(result)
}

/// Register inside an existing writer transaction (no I/O).
pub(super) fn register_space_in(
    conn: &Connection,
    identity: &SpaceIdentity,
    now: i64,
) -> Result<Registered, SpaceError> {
    if let Some(existing) = load_space(conn, identity.space_id())? {
        return if existing == *identity {
            Ok(Registered::Existing)
        } else {
            Err(SpaceError::SpaceIdConflict)
        };
    }
    let same_tuple: Option<String> = conn
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
    conn.execute(
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
    let result = store_prepared_in(&tx, vector, now)?;
    tx.commit()?;
    Ok(result)
}

/// Atomic with job success when called from jobs::complete.
pub(super) fn store_prepared_in(
    conn: &Connection,
    vector: &PreparedVector,
    now: i64,
) -> Result<Stored, StoreError> {
    match load_space(conn, vector.identity.space_id()) {
        Ok(Some(registered)) if registered == vector.identity => {}
        Ok(Some(_)) => return Err(StoreError::SpaceMismatch),
        Ok(None) => return Err(StoreError::UnknownSpace),
        Err(SpaceError::Db(e)) => return Err(StoreError::Db(e)),
        Err(_) => return Err(StoreError::Corrupt),
    }
    // 既存の行が有効かは、読み取り・公開と同じ検査（次元の列・型・`space::decode`）で決める
    let existing: Option<bool> = conn
        .query_row(
            "SELECT vector, dimension FROM ai_embedding_vectors WHERE space_id=?1 AND input_sha256=?2",
            params![vector.identity.space_id(), vector.input_sha256],
            |row| {
                Ok(match (row.get_ref(0)?, row.get_ref(1)?) {
                    (ValueRef::Blob(blob), ValueRef::Integer(dim))
                        if dim == i64::from(vector.identity.dimension()) =>
                    {
                        space::decode(&vector.identity, blob).is_ok()
                    }
                    _ => false,
                })
            },
        )
        .optional()?;
    let outcome = match existing {
        Some(true) => return Ok(Stored::Existing),
        Some(false) => {
            conn.execute(
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
            conn.execute(
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
/// 内容の target の組立てに使う。ワーカー・failed 検査は `ShaState::job_key` の世代付きキーを使う。
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
    pub rotation: Value,
}

impl fmt::Debug for Eligible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Eligible").finish_non_exhaustive()
    }
}

/// 今の対象の `content_sha256` 1 種類と、その空間での状態。公開・進捗・検索はすべてこの 1 つの判定
/// ([`scan`]) から数える（件数が食い違う別経路を持たない）。
#[derive(Clone)]
pub struct ShaState {
    /// その SHA の今の写真（世代の合うもの。写真 ID の昇順。先頭が代表）。
    pub members: Vec<Eligible>,
    /// その空間に**検査を通った**ベクトルがある（長さ・次元・有限数・ゼロでないノルム・L2 の単位ノルム・
    /// 型を [`space::decode`] で確かめたもの）。壊れた行は `false`（`corrupt` が立つ）。
    pub has_vector: bool,
    /// ベクトルの行はあるが検査に通らない（再生成の対象。`pending` か `failed` として数える）。
    pub corrupt: bool,
    /// 現在の入力・処理版（[`ShaState::job_key`]）のジョブが `failed` のとき、その理由。保存された文字列は
    /// そのまま持たず、固定の [`ErrorCode`] か `other` に直したもの（[`reason_code`]）。
    pub failed_reason: Option<&'static str>,
    /// 検査を通った単位ベクトル（`scan` を `with_values` で呼んだときだけ）。
    pub(super) unit: Option<Vec<f32>>,
}

impl fmt::Debug for ShaState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShaState")
            .field("members", &self.members.len())
            .field("has_vector", &self.has_vector)
            .finish_non_exhaustive()
    }
}

impl ShaState {
    pub fn input_sha256(&self) -> &str {
        &self.members[0].input_sha256
    }

    /// Content-addressed target, plus all current DB generations. A late response must
    /// match at least one captured member again inside jobs::complete.
    pub fn job_key(&self, identity: &SpaceIdentity) -> JobKey {
        image_embedding_generation_key(identity, self.input_sha256(), &self.members)
    }

    /// 代表（最小の写真 ID）。
    pub fn representative(&self) -> &Eligible {
        &self.members[0]
    }
}

pub(super) fn image_embedding_generation_key(
    identity: &SpaceIdentity,
    input_sha256: &str,
    members: &[Eligible],
) -> JobKey {
    let mut key = image_embedding_key(identity, input_sha256);
    let mut parts = vec!["image-generations/v1".to_string(), input_sha256.to_string()];
    for m in members {
        parts.extend([
            m.photo_id.to_string(),
            m.display.clone(),
            format!("{:?}", m.md5),
            format!("{:?}", m.mtime),
            format!("{:?}", m.rotation),
        ]);
    }
    key.input_hash =
        super::settings::fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>());
    key
}

/// 保存された失敗の理由を、固定の理由コードに直す（未知の文字列・空・改行入りは `other`）。
/// [`super::ops`] の理由の集計と同じ規則。
pub fn reason_code(stored: &str) -> &'static str {
    ErrorCode::parse(stored).map_or("other", ErrorCode::as_str)
}

/// 対象の結合（写真と、世代の合う品質特徴量）。
pub(super) const ELIGIBLE_JOIN: &str = "FROM photos p JOIN photo_quality_features f
   ON f.photo_id=p.id AND f.display=p.display
  AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime";
/// 対象の条件。
pub(super) const ELIGIBLE_WHERE: &str = "p.display IS NOT NULL AND p.display!='' AND f.error=''
   AND f.content_sha256 IS NOT NULL AND f.content_sha256!=''";

/// 今の対象を `content_sha256` ごとにまとめ（写真 ID の昇順に最初に現れる順）、`space` があれば、その空間の
/// ベクトルの検査結果と失敗したジョブを添える（なければベクトルなし・失敗なし）。表が無い（旧 DB）ときも
/// エラーにしない。呼び出し側の読み取り（または書き込み）トランザクションの中で呼ぶ。
///
/// `with_values` なら、検査を通した単位ベクトルも持つ（検索用）。それ以外は検査だけして捨てる。
pub(super) fn scan(
    conn: &Connection,
    space: Option<&SpaceIdentity>,
    with_values: bool,
) -> rusqlite::Result<Vec<ShaState>> {
    let has_jobs = space.is_some() && object_exists(conn, "table", "ai_jobs")?;
    let sql = format!(
        "SELECT p.id, f.content_sha256, p.display, p.md5, p.mtime, p.rotation
         {ELIGIBLE_JOIN} WHERE {ELIGIBLE_WHERE} ORDER BY p.id"
    );
    let rows = conn
        .prepare(&sql)?
        .query_map([], |row| {
            Ok(Eligible {
                photo_id: row.get(0)?,
                input_sha256: row.get(1)?,
                display: row.get(2)?,
                md5: row.get(3)?,
                mtime: row.get(4)?,
                rotation: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut order: Vec<ShaState> = Vec::new();
    let mut by_sha: HashMap<String, usize> = HashMap::new();
    for member in rows {
        match by_sha.get(&member.input_sha256) {
            Some(&i) => order[i].members.push(member),
            None => {
                by_sha.insert(member.input_sha256.clone(), order.len());
                order.push(ShaState {
                    members: vec![member],
                    has_vector: false,
                    corrupt: false,
                    failed_reason: None,
                    unit: None,
                });
            }
        }
    }
    if let Some(identity) = space.filter(|_| has_jobs) {
        let mut failed_stmt = conn.prepare(
            "SELECT COALESCE(last_error_code,'') FROM ai_jobs
            WHERE kind='image_embedding' AND target_key=?1 AND input_hash=?2
            AND processor_version=?3 AND state='failed'",
        )?;
        for state in &mut order {
            let key = state.job_key(identity);
            let failed: Option<String> = failed_stmt
                .query_row(
                    params![key.target_key, key.input_hash, key.processor_version],
                    |r| r.get(0),
                )
                .optional()?;
            state.failed_reason = failed.as_deref().map(reason_code);
        }
    }
    if let Some(identity) = space
        && object_exists(conn, "table", "ai_embedding_vectors")?
    {
        let mut stmt = conn.prepare(
            "SELECT input_sha256, dimension, vector FROM ai_embedding_vectors WHERE space_id=?1",
        )?;
        let mut rows = stmt.query([identity.space_id()])?;
        while let Some(row) = rows.next()? {
            let sha: String = match row.get_ref(0)? {
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                _ => continue,
            };
            let Some(&i) = by_sha.get(&sha) else {
                continue; // 今の対象でない（古い世代の SHA）。読まない
            };
            let state = &mut order[i];
            // 次元の列・型・長さ・値は、検索と同じ検査（`space::decode` / `unit`）を通す
            let checked = match (row.get_ref(1)?, row.get_ref(2)?) {
                (ValueRef::Integer(dim), ValueRef::Blob(blob))
                    if dim == i64::from(identity.dimension()) =>
                {
                    space::decode(identity, blob).and_then(|values| space::unit(identity, &values))
                }
                _ => Err(VectorError::Dimension),
            };
            match checked {
                Ok(unit) => {
                    state.has_vector = true;
                    if with_values {
                        state.unit = Some(unit);
                    }
                }
                Err(_) => state.corrupt = true,
            }
        }
    }
    Ok(order)
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
    /// 固定の理由コード（[`reason_code`]）ごとの件数（コード順。未知の保存値は `other` にまとめる）。
    pub failed_reasons: Vec<(&'static str, i64)>,
}

impl Progress {
    pub(super) fn from_states(states: &[ShaState]) -> Self {
        let mut p = Progress {
            eligible: states.len() as i64,
            ..Progress::default()
        };
        let mut reasons: BTreeMap<&'static str, i64> = BTreeMap::new();
        for state in states {
            if state.has_vector {
                p.indexed += 1;
            } else if let Some(reason) = state.failed_reason {
                p.failed_excluded += 1;
                *reasons.entry(reason).or_insert(0) += 1;
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
    /// 今の対象のうち、ベクトルの行が検査に通らない `content_sha256` の数（`pending`/`failed_excluded` に数え、
    /// 再生成の対象。検索・公開と同じ検査）。
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
    with_values: bool,
) -> rusqlite::Result<(IndexStatus, Option<SpaceIdentity>, Vec<ShaState>)> {
    let (active, identity) = active_in(conn)?;
    let states = scan(conn, identity.as_ref(), with_values)?;
    let progress = Progress::from_states(&states);
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
            corrupt: states.iter().filter(|s| s.corrupt).count() as i64,
            other_space_shas,
            legacy_shas,
            other_shas,
            spaces,
        },
        identity,
        states,
    ))
}

/// 状態を読む（読み取りだけ。表が無い旧 DB でも書き込みなしで動く）。ポインタと本体は 1 つの読み取り
/// トランザクションの中で読むので、並行する切替の前後が混ざらない。
pub fn status(conn: &Connection) -> rusqlite::Result<IndexStatus> {
    let tx = conn.unchecked_transaction()?;
    let (status, _, _) = status_in(&tx, false)?;
    tx.commit()?;
    Ok(status)
}

/// 指定した空間（まだ active でない構築中のものを含む）の進み具合。
pub fn progress(conn: &Connection, identity: &SpaceIdentity) -> rusqlite::Result<Progress> {
    let tx = conn.unchecked_transaction()?;
    let states = scan(&tx, Some(identity), false)?;
    tx.commit()?;
    Ok(Progress::from_states(&states))
}
