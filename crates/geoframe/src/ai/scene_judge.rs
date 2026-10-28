//! 任意の Gemini 比較（photo_judge。T4.5c）。同一場面の shortlist だけを、用途の許可がある場合に送る。
//!
//! 入力は T4.5a/b の現世代の [`super::groups::Group`]。結果は世代別の不変履歴 `ai_scene_judgments` に
//! 保存し、送信集合（写真 ID と各入力世代）・prompt 版・モデル・処理版が一致する限り再送しない。
//! 旧 Python の `photo_ai_*` は読み書きしない（旧判定の由来は `paid_reuse_verified()==false`）。
//! writer 内では DB だけを読み、画像準備・ファイル観測・通信は呼出し側（runtime）で行う。
use serde::Serialize;

use super::cloud::GenerateRequest;
use super::groups::{Group, Overlap, State};
use super::image_input::PreparedImage;
use super::jobs::{JobKey, JobKind};

mod answer;
mod repository;
mod runtime;
pub use answer::{PhotoVerdict, Verdict};
pub(in crate::ai) use repository::count as stored_count;
pub use repository::{Lookup, Saved, lookup, persist};
pub(super) use repository::{SCHEMA, TABLES};
pub(super) use runtime::handler;
#[cfg(test)]
mod tests;

/// 場面の計算が動き、photo_judge がクラウドで許可されている（未許可・ローカル設定では表も handler も作らない）。
pub fn enabled(config: &crate::config::Config) -> bool {
    super::groups::runtime::enabled(config)
        && config
            .ai
            .gate(super::settings::TaskKind::PhotoJudge, config.ingest)
            .is_ok_and(|g| g.settings.provider == super::settings::ProviderKind::Cloud)
}

/// Python `JUDGE_VERSION` と同じ prompt 版（文面は `prompt.txt` が Python の `JUDGE_PROMPT` と一致する）。
pub const PROMPT_VERSION: &str = "judge-v3-horizon";
/// 送信方式の版（画像の前処理・番号ラベル・提示順）。変えると別の由来として再比較する。
pub const PROCESS_VERSION: &str = "scene-judge-v1";
/// 送る画像: 長辺 768px・JPEG q82・RGB から作り直した（EXIF/GPS 無し）画像。
pub const IMAGE_PREPROCESS: &str = "embed-rgb-768-q82-v1";
/// 1 回に送る最大枚数（shortlist 上限と同じ）。
pub const MAX_SEND: usize = 6;
pub const PROMPT: &str = include_str!("scene_judge/prompt.txt");

pub fn schema() -> serde_json::Value {
    serde_json::from_str(include_str!("scene_judge/schema.json")).expect("fixed schema")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainError {
    InvalidInput,
    BadResponse,
    TooLarge,
}

fn sha(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn encoded<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("bounded domain")
}
fn valid_hash(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn processor_version(model: &str) -> String {
    sha(format!("{model}\0{PROMPT_VERSION}\0{PROCESS_VERSION}\0{IMAGE_PREPROCESS}").as_bytes())
}

/// 送る集合の識別。グループの結果 ID・順位・URL・timeout・許可は含めない
/// （これらが変わっても、同じ画像・世代・prompt・モデルの有償回答は再利用できる）。
pub(super) fn input_key(model: &str, sent: &[(i64, String)]) -> String {
    let mut ids: Vec<&(i64, String)> = sent.iter().collect();
    ids.sort();
    sha(encoded(&(
        "scene-judge-input-v1",
        PROMPT_VERSION,
        model,
        processor_version(model),
        ids,
    ))
    .as_bytes())
}
fn target_key(sent: &[(i64, String)]) -> String {
    let mut ids: Vec<i64> = sent.iter().map(|s| s.0).collect();
    ids.sort_unstable();
    sha(encoded(&("scene-judge-target-v1", ids)).as_bytes())
}

/// 現世代グループの shortlist から導いた比較の対象。置換候補（LocalRanked かつ Exact / Disjoint）だけが作れる。
/// 比較は shortlist の（写真 ID, 入力世代）の集合で同一視する。Debug は中身を出さない。
#[derive(Clone)]
pub struct Candidate {
    result_id: String,
    overlap: Overlap,
    /// 順位順。送信順もこの順（Python の既定 `rank`）。
    sent: Vec<(i64, String)>,
}
impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Candidate(..)")
    }
}
impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        let sorted = |c: &Self| {
            let mut v = c.sent.clone();
            v.sort();
            v
        };
        sorted(self) == sorted(other)
    }
}
impl Candidate {
    /// LocalRanked で、旧選別と完全一致（Exact）か重ならない（Disjoint）グループの shortlist（2〜6 枚）。
    /// Partial（ReviewOverlap）・Waiting・単独は対象外（管理確認のまま）。
    pub fn from_group(group: &Group) -> Option<Self> {
        if group.state() != State::LocalRanked
            || !matches!(group.overlap(), Overlap::Exact | Overlap::Disjoint)
        {
            return None;
        }
        let sent: Vec<_> = group
            .members()
            .iter()
            .filter(|m| m.shortlisted)
            .map(|m| (m.photo_id, m.generation.clone()))
            .collect();
        (2..=MAX_SEND).contains(&sent.len()).then(|| Self {
            result_id: group.result_id().to_string(),
            overlap: group.overlap(),
            sent,
        })
    }
    /// 新しく送ってよいのは Disjoint だけ。Exact は旧い有償判定が有効なので再課金しない。
    pub fn sendable(&self) -> bool {
        self.overlap == Overlap::Disjoint
    }
    pub fn sent_ids(&self) -> Vec<i64> {
        self.sent.iter().map(|s| s.0).collect()
    }
    pub fn input_key(&self, model: &str) -> String {
        input_key(model, &self.sent)
    }
    pub fn key(&self, model: &str) -> JobKey {
        JobKey::new(
            JobKind::PhotoJudge,
            target_key(&self.sent),
            self.input_key(model),
            processor_version(model),
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Sent {
    photo_id: i64,
    generation: String,
    source_sha256: String,
    jpeg_sha256: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    prompt_version: String,
    model: String,
    processor_version: String,
    /// 監査用。同一性には含めない。
    group_result_id: String,
    sent: Vec<Sent>,
}
impl Manifest {
    fn pairs(&self) -> Vec<(i64, String)> {
        self.sent
            .iter()
            .map(|s| (s.photo_id, s.generation.clone()))
            .collect()
    }
    fn input_key(&self) -> String {
        input_key(&self.model, &self.pairs())
    }
}

/// writer 外で用意した 1 枚。画像の入口は RGB から作り直した `PreparedImage` だけ。
pub struct SendImage {
    pub photo_id: i64,
    pub source_sha256: String,
    pub image: PreparedImage,
}
/// 送信順・実送信画像 SHA まで固定した計画。
#[derive(Clone)]
pub struct Plan {
    candidate: Candidate,
    manifest: Manifest,
    request: GenerateRequest,
}
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Plan(..)")
    }
}
impl Plan {
    pub fn new(
        candidate: Candidate,
        model: &str,
        images: Vec<SendImage>,
    ) -> Result<Self, DomainError> {
        if model.is_empty()
            || model.len() > 256
            || model.chars().any(char::is_control)
            || !images
                .iter()
                .map(|i| i.photo_id)
                .eq(candidate.sent.iter().map(|s| s.0))
        {
            return Err(DomainError::InvalidInput);
        }
        let mut sent = Vec::new();
        let mut numbered = Vec::new();
        for (n, (image, (_, generation))) in images.into_iter().zip(&candidate.sent).enumerate() {
            if !valid_hash(&image.source_sha256) {
                return Err(DomainError::InvalidInput);
            }
            sent.push(Sent {
                photo_id: image.photo_id,
                generation: generation.clone(),
                source_sha256: image.source_sha256,
                jpeg_sha256: image.image.sha256(),
            });
            numbered.push(((n + 1) as u32, image.image));
        }
        let request =
            GenerateRequest::photo_judge(PROMPT, &schema(), &numbered).map_err(|e| match e {
                super::cloud::RequestError::TooLarge => DomainError::TooLarge,
                _ => DomainError::InvalidInput,
            })?;
        let manifest = Manifest {
            prompt_version: PROMPT_VERSION.into(),
            model: model.into(),
            processor_version: processor_version(model),
            group_result_id: candidate.result_id.clone(),
            sent,
        };
        Ok(Self {
            candidate,
            manifest,
            request,
        })
    }
    pub fn key(&self) -> JobKey {
        self.candidate.key(&self.manifest.model)
    }
    pub fn request(&self) -> &GenerateRequest {
        &self.request
    }
    pub fn candidate(&self) -> &Candidate {
        &self.candidate
    }
    pub fn sent_ids(&self) -> Vec<i64> {
        self.candidate.sent_ids()
    }
}
