//! 検索空間の切替（`settings.ai_active_space`）と、旧結果参照への復帰（P4 / T4.3。設計は p4-design.md §6.2）。
//!
//! # 流れ
//!
//! 1. 構築を始める時点で [`snapshot_targets`] が**今の対象と各世代**（写真 ID・`content_sha256`・`display`・`md5`・
//!    `mtime`）を候補として持つ（[`BuildSnapshot`]）。
//! 2. 後続のワーカー（T4.4）が、その空間の画像 Embedding を作り、[`super::index::store_vector`] で保存する
//!    （ジョブは [`super::index::image_embedding_key`]）。
//! 3. 切替できそうなとき [`try_activate`] を writer 上で呼ぶ。**1 つの書き込みトランザクションの中で**、候補の
//!    それぞれが**今も同じ世代の対象か**を再検査し、削除・更新・回転された写真は候補から除く（`stale`）。
//!
//! # 公開の条件
//!
//! 残った候補（`valid`）を数える:
//!
//! - ベクトルがあるもの: `indexed`。
//! - ベクトルが無く、現在の入力・処理版・空間のジョブが `failed`: `failed_excluded`（公開を待たせず、対象から除く。
//!   別の空間・古い版・別の SHA の失敗では除かない。T4.4 が手動再試行に成功してベクトルを保存すると、有効な結合で
//!   自動的に検索へ加わる）。
//! - それ以外（ベクトルが無く、ジョブが pending・running・deferred・まだ未登録・`obsolete`）: `pending`。**1 件でも
//!   あれば公開しない**（[`Activation::Waiting`]）。
//!
//! `pending == 0` のとき、`indexed >= 1` かつ `indexed / valid >= min_coverage`（`valid` は失敗を含む）なら
//! `settings.ai_active_space` を `space_id` に書いて公開する（[`Activation::Published`]）。全件失敗・成功 0
//! （[`Activation::NoVectors`]）と閾値未達（[`Activation::BelowThreshold`]）は旧ポインタを変えない。
//! 100 件のうち 1 件だけ `failed` なら 99/100 で公開できる。
//!
//! 構築を始めたあとに追加された写真は候補に無いので、公開を待たせない（有効な結合で、ベクトルが保存された時点で
//! 検索に加わる）。閾値は結果の同一性（識別・ジョブのキー）に入れない。**設定への配線は後続**で、いまは
//! `min_coverage` の引数と [`DEFAULT_ACTIVATION_MIN_COVERAGE`] だけ（`AiSettings` に任意キーを足す場合も、
//! 有限・0〜1 の検査をここの [`valid_coverage`] に合わせる）。
//!
//! # 復帰
//!
//! [`deactivate`] は `settings.ai_active_space` を消すだけ。旧結果・新しいベクトル・空間の表・ジョブは**消さない**
//! （GC は作らない）。サービス停止後だけ使う CLI（`ai-jobs deactivate-space`）から呼ぶ。

use std::fmt;

use rusqlite::{Connection, TransactionBehavior, params};

use super::index::{
    ACTIVE_KEY, DEFAULT_ACTIVATION_MIN_COVERAGE, Eligible, SpaceError, classify, load_space,
};
use super::space::SpaceIdentity;

/// 切替の候補（構築を始めた時点の今の対象）。値（SHA・写真 ID）は `Debug` に出さない。
#[derive(Clone)]
pub struct BuildSnapshot {
    space_id: String,
    targets: Vec<Eligible>,
}

impl fmt::Debug for BuildSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuildSnapshot")
            .field("targets", &self.targets.len())
            .finish_non_exhaustive()
    }
}

impl BuildSnapshot {
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub fn targets(&self) -> &[Eligible] {
        &self.targets
    }
}

/// 今の対象と各世代を候補として持つ（読み取りだけ。構築の開始時に 1 回）。
pub fn snapshot_targets(
    conn: &Connection,
    identity: &SpaceIdentity,
) -> rusqlite::Result<BuildSnapshot> {
    let tx = conn.unchecked_transaction()?;
    let rows = classify(&tx, None)?;
    tx.commit()?;
    Ok(BuildSnapshot {
        space_id: identity.space_id().to_string(),
        targets: rows.into_iter().map(|c| c.target).collect(),
    })
}

/// 切替の判断に使った件数（値は持たない）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Counts {
    /// 候補の数。
    pub snapshot: usize,
    /// 世代が変わった・削除された・対象でなくなった候補（公開の対象から除く）。
    pub stale: usize,
    /// 今も同じ世代の候補（失敗を含む。成功率の分母）。
    pub valid: usize,
    pub indexed: usize,
    pub pending: usize,
    pub failed_excluded: usize,
    /// `indexed / valid`（`valid == 0` なら 0）。
    pub coverage: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Activation {
    /// `ai_active_space` を切り替えた。
    Published(Counts),
    /// 未完了（pending・running・deferred・未登録）があるので旧ポインタのまま。
    Waiting(Counts),
    /// 成功したベクトルが 1 件も無い（全件失敗・有効な対象 0）。旧ポインタのまま。
    NoVectors(Counts),
    /// 成功率が閾値に届かない。旧ポインタのまま（管理確認が要る）。
    BelowThreshold(Counts),
}

impl Activation {
    pub fn counts(&self) -> &Counts {
        match self {
            Self::Published(c)
            | Self::Waiting(c)
            | Self::NoVectors(c)
            | Self::BelowThreshold(c) => c,
        }
    }

    pub fn published(&self) -> bool {
        matches!(self, Self::Published(_))
    }
}

#[derive(Debug)]
pub enum ActivateError {
    /// 閾値が有限の 0〜1 でない。
    InvalidThreshold,
    /// `identity` の空間が未登録、または登録済みの識別と違う。
    UnknownSpace,
    SpaceMismatch,
    Corrupt,
    /// 候補が別の空間の構築のものだった。
    WrongSnapshot,
    Db(rusqlite::Error),
}

impl fmt::Display for ActivateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidThreshold => f.write_str("成功率の閾値が 0〜1 の有限の値ではありません"),
            Self::UnknownSpace => f.write_str("その空間は登録されていません"),
            Self::SpaceMismatch => f.write_str("空間の識別が登録済みのものと違います"),
            Self::Corrupt => f.write_str("保存された空間の識別が不正です"),
            Self::WrongSnapshot => f.write_str("候補が別の空間のものです"),
            Self::Db(e) => write!(f, "DB のエラー: {e}"),
        }
    }
}

impl std::error::Error for ActivateError {}

impl From<rusqlite::Error> for ActivateError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

/// 成功率の閾値が有限の 0〜1 か。
pub fn valid_coverage(min_coverage: f64) -> bool {
    min_coverage.is_finite() && (0.0..=1.0).contains(&min_coverage)
}

/// 切替を試みる（writer 上で呼ぶ。1 つの IMMEDIATE トランザクションで再検査から書き込みまで行う）。
/// 呼び出し側は外部通信・重い計算をここに持ち込まない。
pub fn try_activate(
    conn: &mut Connection,
    snapshot: &BuildSnapshot,
    identity: &SpaceIdentity,
    min_coverage: f64,
) -> Result<Activation, ActivateError> {
    if !valid_coverage(min_coverage) {
        return Err(ActivateError::InvalidThreshold);
    }
    if snapshot.space_id != identity.space_id() {
        return Err(ActivateError::WrongSnapshot);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match load_space(&tx, identity.space_id()) {
        Ok(Some(registered)) if registered == *identity => {}
        Ok(Some(_)) => return Err(ActivateError::SpaceMismatch),
        Ok(None) => return Err(ActivateError::UnknownSpace),
        Err(SpaceError::Db(e)) => return Err(ActivateError::Db(e)),
        Err(_) => return Err(ActivateError::Corrupt),
    }
    // 今の対象（SHA ごとの代表）。候補は、代表の写真・世代まで同じものだけが有効
    let current: std::collections::HashMap<String, super::index::Classified> =
        classify(&tx, Some(identity))?
            .into_iter()
            .map(|c| (c.target.input_sha256.clone(), c))
            .collect();
    let mut counts = Counts {
        snapshot: snapshot.targets.len(),
        stale: 0,
        valid: 0,
        indexed: 0,
        pending: 0,
        failed_excluded: 0,
        coverage: 0.0,
    };
    for target in &snapshot.targets {
        match current.get(&target.input_sha256) {
            Some(now) if now.target == *target => {
                counts.valid += 1;
                if now.has_vector {
                    counts.indexed += 1;
                } else if now.failed_code.is_some() {
                    counts.failed_excluded += 1;
                } else {
                    counts.pending += 1;
                }
            }
            _ => counts.stale += 1,
        }
    }
    if counts.valid > 0 {
        counts.coverage = counts.indexed as f64 / counts.valid as f64;
    }
    if counts.pending > 0 {
        return Ok(Activation::Waiting(counts));
    }
    if counts.indexed == 0 {
        return Ok(Activation::NoVectors(counts));
    }
    if counts.coverage < min_coverage {
        return Ok(Activation::BelowThreshold(counts));
    }
    tx.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![ACTIVE_KEY, identity.space_id()],
    )?;
    tx.commit()?;
    Ok(Activation::Published(counts))
}

/// [`try_activate`] の既定の閾値（[`DEFAULT_ACTIVATION_MIN_COVERAGE`]）版。
pub fn try_activate_default(
    conn: &mut Connection,
    snapshot: &BuildSnapshot,
    identity: &SpaceIdentity,
) -> Result<Activation, ActivateError> {
    try_activate(conn, snapshot, identity, DEFAULT_ACTIVATION_MIN_COVERAGE)
}

/// 有効な検索空間のポインタを外し、旧結果参照へ戻す。ポインタがあった（外した）かを返す。旧結果・空間・
/// ベクトル・ジョブには触れない。
pub fn deactivate(conn: &mut Connection) -> rusqlite::Result<bool> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let removed = tx.execute("DELETE FROM settings WHERE key=?1", [ACTIVE_KEY])?;
    tx.commit()?;
    Ok(removed > 0)
}
