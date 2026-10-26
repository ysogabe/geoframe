//! ベクトル空間の識別 [`SpaceIdentity`] と、ベクトルの検査・正規化・BLOB の形式（P4 / T4.3。設計は
//! p4-design.md §6.1・§6.2）。
//!
//! # SpaceIdentity
//!
//! 画像と検索語が**同じ空間**にあることを、名前や次元だけで判断しない。識別は次の全項目で、1 つでも違えば
//! 別の空間（混ぜて比較しない）:
//!
//! - `space_id`: この DB 内の空間の名前（`ai_embedding_spaces` の主キー）。
//! - `provider_space_id`: **Provider が保証する**画像・文に共通の空間 ID。設定ファイルの
//!   [`EmbeddingSpace`]（形式を検査しただけのメタデータ）には無く、Provider の応答（接続契約が確定した後の
//!   T4.4）から得る。
//! - `model_id`・`revision`・`image_preprocess`・`text_preprocess`・`dimension`（1〜4096）・`normalization`。
//!
//! [`EmbeddingSpace`] だけからは識別を作れない（[`SpaceIdentity::from_declared`] は Provider が証明した
//! `provider_space_id` を別に要る）。旧 `photo_ai_features` / `photo_embeddings` の `embedding_model` の文字列からも
//! 作れない（旧ベクトルは版が分からない `legacy-unverified` で、新しい空間には輸入しない）。
//!
//! # ベクトル
//!
//! 検査は次元・有限数（NaN・±Infinity を拒否）・ゼロでないノルム。`normalization = l2` なら単位ベクトルに
//! 直して保存し、`none` なら受け取った値をそのまま保存する。L2 は最大要素で割ってから f64 で足すので、
//! 極端に大きい・小さい値でもオーバーフロー・アンダーフローで 0 や無限大にならない。保存の形式は
//! float32 リトルエンディアンの BLOB（長さは `dimension * 4`）。DB から読む BLOB も同じ検査を通し、
//! 壊れていれば**エラーを返す**（パニックしない）。

use std::fmt;

use super::settings::{EmbeddingSpace, MAX_EMBEDDING_DIMENSION, Normalization, fingerprint};

/// 識別子 1 つの最大の長さ（`settings` の `embedding_space` と同じ）。
const MAX_TOKEN_BYTES: usize = 64;
/// L2 で正規化したベクトルを読むとき、ノルムが 1 からこれ以上離れていれば壊れているとみなす。
/// 保存は f32 なので本来の誤差は 1e-6 程度。
const NORM_TOLERANCE: f64 = 1e-3;

/// 識別の項目が不正（固定のコードだけ。値は持たない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    /// 識別子が空・長すぎる・使えない文字を含む。
    BadToken,
    /// 次元が 1〜4096 の範囲外。
    BadDimension,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadToken => "空間の識別子が不正です",
            Self::BadDimension => "空間の次元が範囲外です",
        })
    }
}

impl std::error::Error for IdentityError {}

/// 識別子: 英数字で始まり、英数字と `.` `_` `-` `:` だけ。
fn is_token(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_TOKEN_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
}

/// 画像と検索語に共通のベクトル空間の識別（検査済み。全項目が一致して初めて同じ空間）。
#[derive(Clone, PartialEq, Eq)]
pub struct SpaceIdentity {
    space_id: String,
    provider_space_id: String,
    model_id: String,
    revision: String,
    image_preprocess: String,
    text_preprocess: String,
    dimension: u32,
    normalization: Normalization,
}

impl fmt::Debug for SpaceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpaceIdentity")
            .field("dimension", &self.dimension)
            .field("normalization", &self.normalization)
            .finish_non_exhaustive()
    }
}

impl SpaceIdentity {
    /// 全項目を検査して作る。版（`revision`・前処理）を補う・既定値にすることはしない（空なら不正）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        space_id: &str,
        provider_space_id: &str,
        model_id: &str,
        revision: &str,
        image_preprocess: &str,
        text_preprocess: &str,
        dimension: u32,
        normalization: Normalization,
    ) -> Result<Self, IdentityError> {
        let tokens = [
            space_id,
            provider_space_id,
            model_id,
            revision,
            image_preprocess,
            text_preprocess,
        ];
        if !tokens.iter().all(|t| is_token(t)) {
            return Err(IdentityError::BadToken);
        }
        if !(1..=MAX_EMBEDDING_DIMENSION).contains(&dimension) {
            return Err(IdentityError::BadDimension);
        }
        Ok(Self {
            space_id: space_id.to_string(),
            provider_space_id: provider_space_id.to_string(),
            model_id: model_id.to_string(),
            revision: revision.to_string(),
            image_preprocess: image_preprocess.to_string(),
            text_preprocess: text_preprocess.to_string(),
            dimension,
            normalization,
        })
    }

    /// 設定の宣言（[`EmbeddingSpace`]）に、**Provider が保証した**共通の空間 ID を添えて識別にする。
    /// `provider_space_id` は Provider の応答から得たもの（T4.4）。設定の文字列だけでは作れない。
    pub fn from_declared(
        declared: &EmbeddingSpace,
        provider_space_id: &str,
    ) -> Result<Self, IdentityError> {
        Self::new(
            &declared.space_id,
            provider_space_id,
            &declared.model_id,
            &declared.revision,
            &declared.image_preprocess,
            &declared.text_preprocess,
            declared.dimension,
            declared.normalization,
        )
    }

    pub fn space_id(&self) -> &str {
        &self.space_id
    }

    pub fn provider_space_id(&self) -> &str {
        &self.provider_space_id
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn image_preprocess(&self) -> &str {
        &self.image_preprocess
    }

    pub fn text_preprocess(&self) -> &str {
        &self.text_preprocess
    }

    pub fn dimension(&self) -> u32 {
        self.dimension
    }

    pub fn normalization(&self) -> Normalization {
        self.normalization
    }

    /// 全項目の SHA-256（16 進）。ジョブの処理版（[`super::index::image_embedding_processor_version`]）に使う。
    pub fn fingerprint(&self) -> String {
        fingerprint(&[
            "space-identity/v1",
            &self.space_id,
            &self.provider_space_id,
            &self.model_id,
            &self.revision,
            &self.image_preprocess,
            &self.text_preprocess,
            &self.dimension.to_string(),
            self.normalization.as_str(),
        ])
    }
}

// ---------------------------------------------------------------- ベクトル

/// ベクトルが不正（固定のコードだけ。値は持たない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorError {
    /// 次元が空間と違う（BLOB は長さが `dimension * 4` でない）。
    Dimension,
    /// NaN・Infinity を含む。
    NotFinite,
    /// ノルムが 0（全要素が 0）。
    ZeroNorm,
    /// L2 の空間なのに保存された値が単位ベクトルでない（壊れている）。
    NotNormalized,
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dimension => "ベクトルの次元が空間と違います",
            Self::NotFinite => "ベクトルに有限でない値があります",
            Self::ZeroNorm => "ベクトルのノルムが 0 です",
            Self::NotNormalized => "ベクトルが正規化されていません",
        })
    }
}

impl std::error::Error for VectorError {}

/// L2 ノルム。最大の絶対値で割ってから 2 乗和を f64 で取る（要素が極端に大きくても小さくても 0・無限大に
/// ならない）。全要素 0 なら `None`。呼び出し側が有限性を確かめてから呼ぶ。
fn scaled_norm(values: &[f32]) -> Option<(f64, f64)> {
    let max = values
        .iter()
        .fold(0.0_f64, |m, v| m.max(f64::from(*v).abs()));
    if max == 0.0 {
        return None;
    }
    let sum: f64 = values
        .iter()
        .map(|v| {
            let scaled = f64::from(*v) / max;
            scaled * scaled
        })
        .sum();
    Some((max, sum.sqrt()))
}

fn check_basic(identity: &SpaceIdentity, values: &[f32]) -> Result<(), VectorError> {
    if values.len() != identity.dimension as usize {
        return Err(VectorError::Dimension);
    }
    if values.iter().any(|v| !v.is_finite()) {
        return Err(VectorError::NotFinite);
    }
    Ok(())
}

/// Provider から受け取った値を検査して、保存する形にする。`l2` は単位ベクトルに、`none` は受け取った値のまま。
pub fn prepare(identity: &SpaceIdentity, values: &[f32]) -> Result<Vec<f32>, VectorError> {
    check_basic(identity, values)?;
    let (max, norm) = scaled_norm(values).ok_or(VectorError::ZeroNorm)?;
    match identity.normalization {
        Normalization::Raw => Ok(values.to_vec()),
        Normalization::L2 => Ok(values
            .iter()
            .map(|v| ((f64::from(*v) / max) / norm) as f32)
            .collect()),
    }
}

/// 単位ベクトルにして返す（検索の計算用。`none` の空間でも向きだけを比べる）。
pub fn unit(identity: &SpaceIdentity, values: &[f32]) -> Result<Vec<f32>, VectorError> {
    check_basic(identity, values)?;
    let (max, norm) = scaled_norm(values).ok_or(VectorError::ZeroNorm)?;
    Ok(values
        .iter()
        .map(|v| ((f64::from(*v) / max) / norm) as f32)
        .collect())
}

/// float32 リトルエンディアンの BLOB にする。
pub fn encode(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// DB から読んだ BLOB を、空間の識別で検査して値にする。長さ・有限数・ノルム（L2 は単位ベクトル）を確かめ、
/// 壊れていればエラー（パニックしない）。
pub fn decode(identity: &SpaceIdentity, blob: &[u8]) -> Result<Vec<f32>, VectorError> {
    let dimension = identity.dimension as usize;
    if blob.len() != dimension * 4 {
        return Err(VectorError::Dimension);
    }
    let values: Vec<f32> = blob
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    check_basic(identity, &values)?;
    let (max, norm) = scaled_norm(&values).ok_or(VectorError::ZeroNorm)?;
    if identity.normalization == Normalization::L2 && ((max * norm) - 1.0).abs() > NORM_TOLERANCE {
        return Err(VectorError::NotNormalized);
    }
    Ok(values)
}

/// 同じ次元の単位ベクトルどうしの内積（コサイン類似度）。
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    // `+ 0.0` は -0.0 を 0.0 にそろえる（同点の並びを `total_cmp` で安定させる）
    (a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum::<f64>()
        + 0.0) as f32
}
