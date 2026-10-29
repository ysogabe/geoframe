//! Gemini 比較の型付き応答。Python `_judge_group` と同じ必須項目・正規化で、列挙・範囲は schema で検査する。
//! 内容（要約・理由）は Debug / error に出さない。
use super::DomainError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_SUMMARY: usize = 2000;
const MAX_ITEM: usize = 300;
const MAX_LIST: usize = 8;
const MAX_PHOTOS: usize = 64;

const SUBJECT: &[&str] = &[
    "person",
    "dog",
    "cat",
    "person_and_pet",
    "landscape",
    "food",
    "vehicle",
    "other",
];
const PET_STATE: &[&str] = &["awake", "sleeping", "active", "uncertain", "not_applicable"];
const EYES: &[&str] = &[
    "open",
    "closed",
    "mixed",
    "not_visible",
    "uncertain",
    "not_applicable",
];
const FACE_CROP: &[&str] = &[
    "good",
    "intentional_closeup",
    "accidental_cut",
    "not_visible",
    "uncertain",
    "not_applicable",
];
const HORIZON: &[&str] = &[
    "level",
    "intentional_tilt",
    "accidental_tilt",
    "uncertain",
    "not_applicable",
];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhotoVerdict {
    index: usize,
    subject: String,
    pet_state: String,
    eyes: String,
    face_crop: String,
    horizon_status: String,
    horizon_score: u8,
    subject_sharpness: u8,
    aesthetic_score: u8,
    keep_score: u8,
    recommended: bool,
    reasons: Vec<String>,
    concerns: Vec<String>,
}
impl PhotoVerdict {
    /// 送信順に対する 1 始まりの番号。適用時は保存した manifest の送信順で写真へ解く。
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn keep_score(&self) -> u8 {
        self.keep_score
    }
    pub fn recommended(&self) -> bool {
        self.recommended
    }
    pub fn aesthetic_score(&self) -> u8 {
        self.aesthetic_score
    }
}
impl std::fmt::Debug for PhotoVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PhotoVerdict(..)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    same_scene: bool,
    summary: String,
    photos: Vec<PhotoVerdict>,
}
impl std::fmt::Debug for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Verdict(..)")
    }
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
fn choice(map: &Map<String, Value>, key: &str, allowed: &[&str]) -> Result<String, DomainError> {
    let v = map
        .get(key)
        .and_then(Value::as_str)
        .ok_or(DomainError::BadResponse)?;
    allowed
        .contains(&v)
        .then(|| v.to_string())
        .ok_or(DomainError::BadResponse)
}
fn score(map: &Map<String, Value>, key: &str) -> Result<u8, DomainError> {
    map.get(key)
        .and_then(Value::as_i64)
        .filter(|v| (0..=100).contains(v))
        .map(|v| v as u8)
        .ok_or(DomainError::BadResponse)
}
fn list(map: &Map<String, Value>, key: &str) -> Result<Vec<String>, DomainError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .take(MAX_LIST)
            .map(|v| {
                v.as_str()
                    .map(|s| truncate(s, MAX_ITEM))
                    .ok_or(DomainError::BadResponse)
            })
            .collect(),
        Some(_) => Err(DomainError::BadResponse),
    }
}

impl Verdict {
    /// `sent` は実際に送った枚数。写真番号 1..=sent が重複なく全て揃うことを要求する
    /// （範囲外の番号は Python と同じく無視する）。
    pub fn parse(text: &str, sent: usize) -> Result<Self, DomainError> {
        if text.len() > MAX_BYTES || sent == 0 || sent > MAX_PHOTOS {
            return Err(DomainError::BadResponse);
        }
        let value: Value = serde_json::from_str(text).map_err(|_| DomainError::BadResponse)?;
        let root = value.as_object().ok_or(DomainError::BadResponse)?;
        let same_scene = root
            .get("same_scene")
            .and_then(Value::as_bool)
            .ok_or(DomainError::BadResponse)?;
        let summary = truncate(
            root.get("summary")
                .and_then(Value::as_str)
                .ok_or(DomainError::BadResponse)?,
            MAX_SUMMARY,
        );
        let items = root
            .get("photos")
            .and_then(Value::as_array)
            .filter(|a| a.len() <= MAX_PHOTOS)
            .ok_or(DomainError::BadResponse)?;
        let mut found: Vec<Option<PhotoVerdict>> = vec![None; sent];
        for item in items {
            let map = item.as_object().ok_or(DomainError::BadResponse)?;
            let index = map
                .get("index")
                .and_then(Value::as_i64)
                .ok_or(DomainError::BadResponse)?;
            let Some(slot) = usize::try_from(index)
                .ok()
                .filter(|i| (1..=sent).contains(i))
            else {
                continue;
            };
            if found[slot - 1].is_some() {
                return Err(DomainError::BadResponse);
            }
            found[slot - 1] = Some(PhotoVerdict {
                index: slot,
                subject: choice(map, "subject", SUBJECT)?,
                pet_state: choice(map, "pet_state", PET_STATE)?,
                eyes: choice(map, "eyes", EYES)?,
                face_crop: choice(map, "face_crop", FACE_CROP)?,
                horizon_status: choice(map, "horizon_status", HORIZON)?,
                horizon_score: score(map, "horizon_score")?,
                subject_sharpness: score(map, "subject_sharpness")?,
                aesthetic_score: score(map, "aesthetic_score")?,
                keep_score: score(map, "keep_score")?,
                recommended: map
                    .get("recommended")
                    .and_then(Value::as_bool)
                    .ok_or(DomainError::BadResponse)?,
                reasons: list(map, "reasons")?,
                concerns: list(map, "concerns")?,
            });
        }
        let mut photos = found
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(DomainError::BadResponse)?;
        // 同一場面なのに推薦が 0 枚なら、keep_score が最大（同点は送信順で先）の 1 枚を推薦にする。
        if same_scene && !photos.iter().any(|p| p.recommended) {
            let mut best = 0;
            for (i, p) in photos.iter().enumerate() {
                if p.keep_score > photos[best].keep_score {
                    best = i;
                }
            }
            photos[best].recommended = true;
        }
        Ok(Self {
            same_scene,
            summary,
            photos,
        })
    }

    /// 保存済み JSON を読み戻す。正規化済みの形と完全に一致しなければ破損として拒否する。
    pub fn from_stored(text: &str, sent: usize) -> Option<Self> {
        if text.len() > MAX_BYTES {
            return None;
        }
        let v: Self = serde_json::from_str(text).ok()?;
        let in_set = |s: &str, allowed: &[&str]| allowed.contains(&s);
        let valid = v.photos.len() == sent
            && v.summary.chars().count() <= MAX_SUMMARY
            && v.photos.iter().enumerate().all(|(i, p)| {
                p.index == i + 1
                    && in_set(&p.subject, SUBJECT)
                    && in_set(&p.pet_state, PET_STATE)
                    && in_set(&p.eyes, EYES)
                    && in_set(&p.face_crop, FACE_CROP)
                    && in_set(&p.horizon_status, HORIZON)
                    && [
                        p.horizon_score,
                        p.subject_sharpness,
                        p.aesthetic_score,
                        p.keep_score,
                    ]
                    .iter()
                    .all(|s| *s <= 100)
                    && p.reasons.len() <= MAX_LIST
                    && p.concerns.len() <= MAX_LIST
                    && p.reasons
                        .iter()
                        .chain(&p.concerns)
                        .all(|s| s.chars().count() <= MAX_ITEM)
            })
            && (!v.same_scene || v.photos.iter().any(|p| p.recommended));
        (valid && v.normalized_json() == text).then_some(v)
    }

    pub fn normalized_json(&self) -> String {
        serde_json::to_string(self).expect("bounded verdict")
    }
    pub fn same_scene(&self) -> bool {
        self.same_scene
    }
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn photos(&self) -> &[PhotoVerdict] {
        &self.photos
    }
    /// 送信された写真（番号は 1 始まり）の推薦。Python: 同一場面なら Gemini の `recommended`、
    /// 別場面なら全員を推薦（抑止しない）。送信されなかった構成員は同一場面なら非推薦。
    pub fn recommended(&self, sent_position: Option<usize>) -> bool {
        match (self.same_scene, sent_position) {
            (true, Some(i)) => self.photos.get(i).is_some_and(PhotoVerdict::recommended),
            (same, _) => !same,
        }
    }
}
