//! 写真別 LAN Decision → Python 互換 clue。日候補・クラウド・座標化はここでは行わない。
//! 詳細（世代・terminal 投影・画像入力）は docs/rust-migration/ai-location-clue.md。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params};

use super::decision::{self, Answer, DecisionReply, DecisionRequest, Question, QuestionKind};
use super::image_input::{ImageProfile, PreparedImage};
use super::jobs::{self, Claimed, ErrorCode, InputCheck, JobKey, JobKind, Recheck};
use super::provider::LanClient;
use super::waits;
use super::worker::{Handler, Persist, RunCtx, RunError};
use crate::datadir::{DataDir, check_drive_id};
use crate::imaging::{self, ImageError};

pub(super) const TABLES: &[&str] = &["ai_location_clues"];
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_location_clues(
  photo_id INTEGER NOT NULL,
  input_hash TEXT NOT NULL,
  processor_version TEXT NOT NULL,
  source_sha256 TEXT,
  clue REAL,
  kind TEXT NOT NULL,
  model TEXT NOT NULL,
  analyzed_at TEXT NOT NULL,
  error TEXT NOT NULL,
  PRIMARY KEY(photo_id,input_hash,processor_version)
) WITHOUT ROWID;";

pub const CLUE_MIN: f64 = 0.2;
pub const DECISION_STATE: &str = "写真を1枚見て答えてください。";
pub const QUESTION_VERSION: &str = "loc-clue-python-v1";
pub const PREPROCESS_VERSION: &str = "display-exif-once-srgb-1536-1600000-rgb-jpeg-q88-v1";
const CHOICES: &[(&str, &str)] = &[
    ("landmark", "有名な建物・遺跡・寺社・記念碑"),
    ("street", "街並み・市場・看板のある通り・都市の景観"),
    ("nature", "山・海・花・木などの自然の景色"),
    ("indoor", "部屋・店内・館内などの屋内"),
    ("animal_person", "ペットや人物のアップ"),
    ("object", "食べ物・飲み物・小物・展示品のアップ"),
];

pub fn questions() -> Vec<Question> {
    vec![
        Question {
            name: "clue".into(),
            instructions: "この写真には、撮影された国や都市を推定できる手がかり（有名な建物や遺跡、読める看板の文字、その土地特有の街並みや建築）が写っていますか？".into(),
            kind: QuestionKind::Noul,
        },
        Question {
            name: "kind".into(),
            instructions: "この写真の主な被写体はどれですか？".into(),
            kind: QuestionKind::Choice(CHOICES.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()),
        },
    ]
}

/// 有限 0..1 と既存 6 種を検証した判定。Debug は答えを表示しない。
#[derive(Clone, PartialEq)]
pub struct Clue {
    score: f64,
    kind: String,
}
impl std::fmt::Debug for Clue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clue { .. }")
    }
}
impl Clue {
    pub fn new(score: f64, kind: &str) -> Option<Self> {
        (score.is_finite()
            && (0.0..=1.0).contains(&score)
            && CHOICES.iter().any(|(k, _)| *k == kind))
        .then(|| Self {
            score,
            kind: kind.into(),
        })
    }
    pub fn score(&self) -> f64 {
        self.score
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    /// Python passes と同じ。日候補側は別途 hidden / allow_cloud を再検査する。
    pub fn passes(&self) -> bool {
        self.score >= CLUE_MIN || self.kind == "landmark"
    }

    fn from_reply(reply: &DecisionReply, model: &str) -> Option<Self> {
        if reply.model != model {
            return None;
        }
        let score = reply
            .answers
            .iter()
            .find_map(|(k, a)| match (k.as_str(), a) {
                ("clue", Answer::Noul(v)) => Some(*v),
                _ => None,
            })?;
        let kind = reply
            .answers
            .iter()
            .find_map(|(k, a)| match (k.as_str(), a) {
                ("kind", Answer::Choice { choice, .. }) => Some(choice.as_str()),
                _ => None,
            })?;
        Self::new(score, kind)
    }
}

/// Pending は日を待たせる。Failed は判定済み・画像を送らない（passes=false）。
#[derive(Debug, Clone, PartialEq)]
pub enum Readiness {
    Pending,
    Ready(Clue),
    Failed,
}

/// T4.6c の写真集合入口。位置候補の生成・送信は含まない。Debug に写真情報を出さない。
pub struct Target {
    pub photo_id: i64,
    pub taken_at: String,
    pub epoch: i64,
    pub key: JobKey,
    pub readiness: Readiness,
}

fn sha(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn processor_version(model: &str) -> String {
    sha(format!("{model}\0{QUESTION_VERSION}\0{PREPROCESS_VERSION}").as_bytes())
}

// SQLite の NULL と空文字、数値型を同一視しない。自由文をエラー・ログに出さない。
fn hash_values(values: &[Value]) -> String {
    let mut bytes = Vec::new();
    for value in values {
        let (tag, part) = match value {
            Value::Null => (0, vec![]),
            Value::Integer(n) => (1, n.to_be_bytes().to_vec()),
            Value::Real(n) => (2, n.to_bits().to_be_bytes().to_vec()),
            Value::Text(t) => (3, t.as_bytes().to_vec()),
            Value::Blob(b) => (4, b.clone()),
        };
        bytes.push(tag);
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend(part);
    }
    sha(&bytes)
}

#[derive(Clone)]
struct Input {
    id: i64,
    display: Value,
    md5: Value,
    mtime: Value,
    rotation: Value,
    taken_at: String,
    epoch: i64,
    rel: String,
    quality_sha: Option<String>,
    observed_sha: Option<String>,
    quality_failed: bool,
    quality_pending: bool,
}

impl Input {
    /// DB だけを読む。writer の中でファイルに触れない。
    fn load(conn: &Connection, id: i64) -> rusqlite::Result<Option<Self>> {
        Self::load_generation(conn, id, true)
    }
    fn load_generation(
        conn: &Connection,
        id: i64,
        unlocated: bool,
    ) -> rusqlite::Result<Option<Self>> {
        let row = conn
            .query_row(
                "SELECT p.display,p.drive_id,p.md5,p.mtime,p.rotation,p.taken_at,p.hidden,
                    f.photo_id,f.content_sha256,f.error
             FROM photos p LEFT JOIN photo_quality_features f
               ON f.photo_id=p.id AND f.display IS p.display
              AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
             WHERE p.id=?1 AND (?2=0 OR p.lat IS NULL)",
                params![id, unlocated],
                |r| {
                    // hidden は送信直前にも読むが LAN の世代・対象性には含めない。
                    let _: Value = r.get(6)?;
                    Ok((
                        r.get::<_, Value>(0)?,
                        r.get::<_, Value>(1)?,
                        r.get::<_, Value>(2)?,
                        r.get::<_, Value>(3)?,
                        r.get::<_, Value>(4)?,
                        r.get::<_, Value>(5)?,
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((display, drive, md5, mtime, rotation, taken, feature, quality_sha, error)) = row
        else {
            return Ok(None);
        };
        let Value::Text(taken_at) = taken else {
            return Ok(None);
        };
        let Some(epoch) = crate::photos::parse_epoch(&taken_at) else {
            return Ok(None);
        };
        let rel = match &display {
            Value::Text(d) if !d.is_empty() => d.clone(),
            Value::Null | Value::Text(_) => match &drive {
                Value::Text(d) if check_drive_id(d).is_ok() => format!("derivatives/{d}.jpg"),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let quality_failed = error.as_deref().is_some_and(|e| !e.is_empty())
            || quality_sha
                .as_deref()
                .is_some_and(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()));
        let quality_sha = quality_sha.filter(|_| !quality_failed);
        let quality_pending = feature.is_none() || quality_sha.is_none();
        Ok(Some(Self {
            id,
            display,
            md5,
            mtime,
            rotation,
            taken_at,
            epoch,
            rel,
            quality_sha,
            observed_sha: None,
            quality_failed,
            quality_pending,
        }))
    }
    // display 空の fallback は品質 worker の対象外。実 bytes の SHA も世代へ含める。
    // writer 内では呼ばず、current はこの観測値を使い DB 世代だけを再検査する。
    fn observe(mut self, data: &DataDir) -> Self {
        if self.fallback() {
            self.observed_sha = read_source(data, &self).ok().map(|bytes| sha(&bytes));
        }
        self
    }
    fn fallback(&self) -> bool {
        !matches!(&self.display, Value::Text(t) if !t.is_empty())
    }
    fn key(&self, model: &str) -> JobKey {
        JobKey::new(
            JobKind::LocationClue,
            self.id.to_string(),
            hash_values(&[
                Value::Integer(self.id),
                self.display.clone(),
                Value::Text(self.rel.clone()),
                self.md5.clone(),
                self.mtime.clone(),
                self.rotation.clone(),
                Value::Text(self.taken_at.clone()),
                self.quality_sha.clone().map_or(Value::Null, Value::Text),
                self.observed_sha.clone().map_or(Value::Null, Value::Text),
                Value::Integer(i64::from(self.quality_failed)),
            ]),
            processor_version(model),
        )
    }
    fn current(&self, conn: &Connection, model: &str) -> rusqlite::Result<bool> {
        Ok(Self::load(conn, self.id)?.is_some_and(|mut p| {
            p.observed_sha = self.observed_sha.clone();
            p.key(model) == self.key(model)
        }))
    }
    fn projection_rel(&self, data: &DataDir) -> String {
        // Python _image_rel は display が空の場合だけ「存在しない fallback」を空にする。
        if self.fallback() && safe_path(data, &self.rel).is_err() {
            String::new()
        } else {
            self.rel.clone()
        }
    }
}

/// 日候補の画像世代。clue の処理版や GPS/hidden を画像世代と混同しない。
/// fallback の実 SHA 観測は writer 外、DB 再照合は writer 内で行う。
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Generation {
    photo_id: i64,
    input_hash: String,
    source_sha256: Option<String>,
    observed_sha256: Option<String>,
}
impl Generation {
    pub(crate) fn matches_target(&self, key: &JobKey) -> bool {
        self.input_hash == key.input_hash
            && key.target_key == self.photo_id.to_string()
            && key.kind == JobKind::LocationClue
    }
    pub fn photo_id(&self) -> i64 {
        self.photo_id
    }
    pub fn source_sha256(&self) -> Option<&str> {
        self.source_sha256.as_deref()
    }
    pub fn observe(conn: &Connection, data: &DataDir, id: i64) -> rusqlite::Result<Option<Self>> {
        Ok(Input::load_generation(conn, id, false)?.map(|p| Self::from_input(&p.observe(data))))
    }
    fn from_input(p: &Input) -> Self {
        Self {
            photo_id: p.id,
            input_hash: p.key("").input_hash,
            source_sha256: p.quality_sha.clone().or_else(|| p.observed_sha.clone()),
            observed_sha256: p.observed_sha.clone(),
        }
    }
    /// ファイル I/O はしない。呼出し側は直前/応答後の observe も比較すること。
    pub fn matches_db(&self, conn: &Connection) -> rusqlite::Result<bool> {
        Ok(
            Input::load_generation(conn, self.photo_id, false)?.is_some_and(|mut p| {
                p.observed_sha = self.observed_sha256.clone();
                Self::from_input(&p) == *self
            }),
        )
    }
}
impl std::fmt::Debug for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Generation(..)")
    }
}

/// 保存済み sent の実 source bytes と世代を writer 外で検証した証明。
/// DB の品質 SHA だけでは、ファイルだけの更新/削除を未変更とみなさない。
#[derive(Clone)]
pub struct SourceObservation {
    generation: Generation,
}
impl SourceObservation {
    pub fn observe(conn: &Connection, data: &DataDir, id: i64) -> rusqlite::Result<Option<Self>> {
        let Some(mut p) = Input::load_generation(conn, id, false)? else {
            return Ok(None);
        };
        let Ok(bytes) = read_source(data, &p) else {
            return Ok(None);
        };
        let source = sha(&bytes);
        if p.fallback() {
            p.observed_sha = Some(source.clone());
        }
        let generation = Generation::from_input(&p);
        Ok((generation.source_sha256() == Some(source.as_str())).then_some(Self { generation }))
    }
    /// ファイルを開かず、保存時に観測した世代をそのまま使う（DB だけで判定する表示用。観測の証明ではない）。
    pub(in crate::ai) fn stored(generation: &Generation) -> Self {
        Self {
            generation: generation.clone(),
        }
    }
    pub fn generation(&self) -> &Generation {
        &self.generation
    }
    pub fn photo_id(&self) -> i64 {
        self.generation.photo_id()
    }
}

/// 同じ限定読取・向き・sRGB・縮小境界を日候補でも使う。writer 外でのみ呼ぶ。
pub(super) fn prepare_image(
    conn: &Connection,
    data: &DataDir,
    generation: &Generation,
) -> Result<PreparedImage, RunError> {
    #[cfg(test)]
    PREPARED.with(|n| n.set(n.get() + 1));
    let input = Input::load_generation(conn, generation.photo_id(), false)
        .map_err(|_| RunError::Attempt(ErrorCode::Internal))?
        .ok_or(RunError::InputChanged)?
        .observe(data);
    if Generation::from_input(&input) != *generation {
        return Err(RunError::InputChanged);
    }
    let bytes = read_source(data, &input)?;
    if generation.source_sha256() != Some(sha(&bytes).as_str()) {
        return Err(RunError::InputChanged);
    }
    let rgb = imaging::prepare_decision_rgb(&bytes, !input.rel.starts_with("derivatives/"))
        .map_err(image_error)?;
    PreparedImage::encode_jpeg_rgb(
        &rgb.pixels,
        rgb.width as u16,
        rgb.height as u16,
        88,
        ImageProfile::Decision,
    )
    .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))
}
impl std::fmt::Debug for SourceObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceObservation(..)")
    }
}

fn inputs(conn: &Connection) -> rusqlite::Result<Vec<Input>> {
    let ids = conn
        .prepare(
            "SELECT id FROM photos WHERE lat IS NULL AND taken_at IS NOT NULL AND taken_at!=''",
        )?
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(p) = Input::load(conn, id)? {
            out.push(p);
        }
    }
    out.sort_by_key(|p| (p.epoch, p.id));
    Ok(out)
}

// 読み取り専用の DataDir 検査。リンクは内部向きも拒否し、FIFO/デバイスを開く前に通常ファイルを確認する。
fn safe_path(data: &DataDir, rel: &str) -> Result<std::path::PathBuf, RunError> {
    let invalid = || RunError::Permanent(ErrorCode::InputInvalid);
    let resolved = data.resolve_existing(rel).map_err(|_| invalid())?;
    let mut path = data.root().to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            std::path::Component::Normal(p) => path.push(p),
            std::path::Component::CurDir => continue,
            _ => return Err(invalid()),
        }
        if fs::symlink_metadata(&path)
            .map_err(|_| invalid())?
            .is_symlink()
        {
            return Err(invalid());
        }
    }
    if path != resolved || !fs::metadata(&resolved).map_err(|_| invalid())?.is_file() {
        return Err(invalid());
    }
    Ok(resolved)
}

fn read_source(data: &DataDir, input: &Input) -> Result<Vec<u8>, RunError> {
    read_image_source(data, &input.rel)
}

#[cfg(test)]
thread_local! {
    static SOURCE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PREPARED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// 試験用: このスレッドが送信用の画像を作り始めた（読み込み・デコード・縮小・JPEG）回数。
#[cfg(test)]
pub(crate) fn prepared_images() -> usize {
    PREPARED.with(std::cell::Cell::get)
}
/// 試験用: このスレッドが画像ファイルを読み始めた回数（読みを伴わない経路の検証に使う）。
#[cfg(test)]
pub(crate) fn source_reads() -> usize {
    SOURCE_READS.with(std::cell::Cell::get)
}

/// Shared bounded, locked read only. Decode and network must happen after returning.
pub(super) fn read_image_source(data: &DataDir, rel: &str) -> Result<Vec<u8>, RunError> {
    #[cfg(test)]
    SOURCE_READS.with(|n| n.set(n.get() + 1));
    let _lock = imaging::lock_derivatives();
    let path = safe_path(data, rel)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
    // open 前に親が差し替わった場合も、実際に開いた fd の所在を読む前に確認する（Linux）。
    let opened = fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
    if !opened.starts_with(data.root()) {
        return Err(RunError::Permanent(ErrorCode::InputInvalid));
    }
    let meta = file
        .metadata()
        .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
    if !meta.is_file() {
        return Err(RunError::Permanent(ErrorCode::InputInvalid));
    }
    if meta.len() > imaging::MAX_INPUT_BYTES {
        return Err(RunError::Permanent(ErrorCode::InputTooLarge));
    }
    imaging::read_opened(file, meta.len()).map_err(image_error)
}
fn image_error(e: ImageError) -> RunError {
    RunError::Permanent(match e {
        ImageError::FileTooLarge | ImageError::TooManyPixels { .. } => ErrorCode::InputTooLarge,
        _ => ErrorCode::InputInvalid,
    })
}

#[derive(Clone)]
struct Stored {
    source_sha: Option<String>,
    clue: Option<f64>,
    kind: String,
    model: String,
    at: String,
    error: String,
}
impl Stored {
    fn load(conn: &Connection, key: &JobKey) -> rusqlite::Result<Option<Self>> {
        if !jobs::object_exists(conn, "table", "ai_location_clues")? {
            return Ok(None);
        }
        conn.query_row(
            "SELECT source_sha256,clue,kind,model,analyzed_at,error FROM ai_location_clues
            WHERE photo_id=?1 AND input_hash=?2 AND processor_version=?3",
            params![key.target_key, key.input_hash, key.processor_version],
            |r| {
                Ok(Self {
                    source_sha: r.get(0)?,
                    clue: r.get(1)?,
                    kind: r.get(2)?,
                    model: r.get(3)?,
                    at: r.get(4)?,
                    error: r.get(5)?,
                })
            },
        )
        .optional()
    }
    fn valid(&self, model: &str) -> bool {
        self.model == model
            && if self.error.is_empty() {
                self.source_sha.as_ref().is_some_and(|s| s.len() == 64)
                    && self.clue.and_then(|v| Clue::new(v, &self.kind)).is_some()
            } else {
                self.clue.is_none()
                    && self.kind.is_empty()
                    && (ErrorCode::parse(&self.error).is_some() || self.error == "other")
            }
    }
    fn compatible(&self, conn: &Connection, input: &Input, rel: &str) -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM photo_loc_clues WHERE photo_id=?1 AND display=?2
            AND source_md5 IS ?3 AND source_mtime IS ?4 AND clue IS ?5 AND kind=?6 AND model=?7
            AND analyzed_at=?8 AND error=?9)",
            params![
                input.id,
                rel,
                input.md5,
                input.mtime,
                self.clue,
                self.kind,
                self.model,
                self.at,
                self.error
            ],
            |r| r.get(0),
        )
    }
    fn project(&self, conn: &Connection, input: &Input, rel: &str) -> rusqlite::Result<()> {
        if self.compatible(conn, input, rel)? {
            return Ok(());
        }
        conn.execute("INSERT INTO photo_loc_clues(photo_id,display,source_md5,source_mtime,clue,kind,model,analyzed_at,error)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(photo_id) DO UPDATE SET
            display=excluded.display,source_md5=excluded.source_md5,source_mtime=excluded.source_mtime,
            clue=excluded.clue,kind=excluded.kind,model=excluded.model,analyzed_at=excluded.analyzed_at,error=excluded.error",
            params![input.id,rel,input.md5,input.mtime,self.clue,self.kind,self.model,self.at,self.error])?;
        Ok(())
    }
    fn save(
        &self,
        conn: &Connection,
        key: &JobKey,
        input: &Input,
        rel: &str,
    ) -> rusqlite::Result<()> {
        conn.execute("INSERT INTO ai_location_clues(photo_id,input_hash,processor_version,source_sha256,clue,kind,model,analyzed_at,error)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(photo_id,input_hash,processor_version) DO UPDATE SET
            source_sha256=excluded.source_sha256,clue=excluded.clue,kind=excluded.kind,model=excluded.model,analyzed_at=excluded.analyzed_at,error=excluded.error",
            params![input.id,key.input_hash,key.processor_version,self.source_sha,self.clue,self.kind,self.model,self.at,self.error])?;
        self.project(conn, input, rel)
    }
}

/// 成功の由来を検査。品質 SHA の無い fallback は実 bytes を照合し、古いメタデータだけで trusted にしない。
fn source_matches(input: &Input, stored: &Stored) -> bool {
    if !stored.error.is_empty() {
        return true;
    }
    if input.quality_failed {
        return false;
    }
    match &input.quality_sha {
        Some(s) => stored.source_sha.as_ref() == Some(s),
        None if input.fallback() => {
            input.observed_sha.is_some() && stored.source_sha == input.observed_sha
        }
        None => false,
    }
}

/// 保存された判定が `input`（`observed_sha` を含む今の世代）に合っているか。`rel` は旧 Python の投影の `display`。
fn judge(conn: &Connection, input: &Input, model: &str, rel: &str) -> rusqlite::Result<Readiness> {
    let key = input.key(model);
    Ok(match Stored::load(conn, &key)? {
        Some(s)
            if s.valid(model) && source_matches(input, &s) && s.compatible(conn, input, rel)? =>
        {
            if s.error.is_empty() {
                Readiness::Ready(Clue::new(s.clue.unwrap(), &s.kind).unwrap())
            } else if job_state(conn, &key)?.is_some_and(|(state, _)| state == "failed") {
                Readiness::Failed
            } else {
                Readiness::Pending
            }
        }
        _ => Readiness::Pending,
    })
}

/// 現在の写真を時刻/id 順に読む。旧 clue の自動 trusted 移行はしない。
pub fn load_targets(
    conn: &Connection,
    data: &DataDir,
    model: &str,
) -> rusqlite::Result<Vec<Target>> {
    let mut targets = Vec::new();
    for input in inputs(conn)?.into_iter().map(|p| p.observe(data)) {
        let readiness = judge(conn, &input, model, &input.projection_rel(data))?;
        targets.push(Target {
            photo_id: input.id,
            taken_at: input.taken_at.clone(),
            epoch: input.epoch,
            key: input.key(model),
            readiness,
        });
    }
    Ok(targets)
}

/// STATUS 用の待ちの件数（[`wait_counts`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Waits {
    /// Rust の判定が今の入力の世代に対して保存されていない写真の数（`db.loc.clue_pending`）。
    pub pending: usize,
    /// そのうち、Provider が止まっていなければ今 Decision が動けば進むもの（`db.loc.clue_runnable` の素）。
    pub runnable: usize,
}

/// STATUS 用: `display` の無い fallback の写真の判定を、画像を読まずに調べる。世代（`input_hash`）は実 bytes の
/// SHA を含むので、保存された最新の行がその SHA で作った世代と、今の DB の入力から組み直した世代が一致するかで
/// 決める。ファイルだけが変わった場合は、ワーカーが次の周期で観測して新しい世代のジョブを積むまで気づけない
/// ので、現世代と違うジョブが生きているとき（`pending`／`running`／`deferred`）は待ちに戻す。
fn recorded_fallback(conn: &Connection, input: &Input, model: &str) -> rusqlite::Result<Readiness> {
    if !jobs::object_exists(conn, "table", "ai_location_clues")? {
        return Ok(Readiness::Pending);
    }
    let version = processor_version(model);
    let latest: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT input_hash,source_sha256 FROM ai_location_clues WHERE photo_id=?1
            AND processor_version=?2 ORDER BY analyzed_at DESC,input_hash LIMIT 1",
            params![input.id, version],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((hash, source)) = latest else {
        return Ok(Readiness::Pending);
    };
    let mut observed = vec![source.filter(|s| s.len() == 64), None];
    observed.dedup();
    for observed_sha in observed {
        let mut seen = input.clone();
        seen.observed_sha = observed_sha;
        if seen.key(model).input_hash != hash {
            continue;
        }
        // 投影の `display` は、ファイルがあれば rel、無ければ空（`projection_rel`）。どちらも許す
        for rel in [seen.rel.as_str(), ""] {
            let readiness = judge(conn, &seen, model, rel)?;
            if readiness == Readiness::Pending {
                continue;
            }
            let other_live: bool = jobs::object_exists(conn, "table", "ai_jobs")?
                && conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM ai_jobs WHERE kind='location_clue'
                    AND target_key=?1 AND processor_version=?2 AND input_hash!=?3
                    AND state IN ('pending','running','deferred'))",
                    params![input.id.to_string(), version, hash],
                    |r| r.get(0),
                )?;
            return Ok(if other_live {
                Readiness::Pending
            } else {
                readiness
            });
        }
    }
    Ok(Readiness::Pending)
}

/// 待ちの写真に、今 Decision を動かして進められる仕事があるか。次は数えない:
/// 品質の解析待ち（画像は送らず後回しになる）・品質の解析に失敗した写真（Decision を使わず恒久失敗になる）・
/// 期限（`next_attempt_at`）が先の後回しや再試行待ちだけのもの・失敗の履歴だけのもの（明示の再試行まで動かない）。
/// ジョブがまだ無い・期限が来ている・実行中なら数える。
fn runnable(conn: &Connection, input: &Input, model: &str, now: i64) -> rusqlite::Result<bool> {
    if input.quality_failed || (input.quality_pending && !input.fallback()) {
        return Ok(false);
    }
    if !jobs::object_exists(conn, "table", "ai_jobs")? {
        return Ok(true);
    }
    // fallback の世代はファイルの SHA を含み、読まずには組み立てられない。ジョブは対象の全世代を見る
    let any_hash = input.fallback();
    let mut stmt = conn.prepare_cached(
        "SELECT state,next_attempt_at FROM ai_jobs WHERE kind='location_clue' AND target_key=?1
        AND processor_version=?2 AND (?3 OR input_hash=?4)",
    )?;
    let rows = stmt
        .query_map(
            params![
                input.id.to_string(),
                processor_version(model),
                any_hash,
                input.key(model).input_hash
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let live = |state: &str| matches!(state, "pending" | "running" | "deferred");
    if rows.iter().any(|(state, _)| live(state)) {
        return Ok(rows
            .iter()
            .any(|(state, at)| state == "running" || (live(state) && *at <= now)));
    }
    // 生きたジョブが無い。現世代の失敗の履歴だけがあるなら、明示の再試行まで動かない
    Ok(any_hash || !rows.iter().any(|(state, _)| state == "failed"))
}

/// STATUS 用: 待ちの写真の数と、そのうち今 Decision が動けば進むものの数。**画像ファイルは開かない**（DB だけ）。
/// Provider の停止は見ない（呼び出し側が [`crate::ai::ops::task_pause`] と合わせる）。
pub fn wait_counts(conn: &Connection, model: &str, now: i64) -> rusqlite::Result<Waits> {
    let mut waits = Waits::default();
    for input in inputs(conn)? {
        let readiness = if input.fallback() {
            recorded_fallback(conn, &input, model)?
        } else {
            judge(conn, &input, model, &input.rel)?
        };
        if readiness == Readiness::Pending {
            waits.pending += 1;
            if runnable(conn, &input, model, now)? {
                waits.runnable += 1;
            }
        }
    }
    Ok(waits)
}

fn job_state(conn: &Connection, key: &JobKey) -> rusqlite::Result<Option<(String, String)>> {
    if !jobs::object_exists(conn, "table", "ai_jobs")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT state,last_error_code FROM ai_jobs WHERE kind='location_clue' AND target_key=?1
        AND input_hash=?2 AND processor_version=?3",
        params![key.target_key, key.input_hash, key.processor_version],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

pub struct LocationClue {
    data: DataDir,
    lan: LanClient,
    model: String,
    retry_observations: Mutex<Option<HashMap<i64, Option<String>>>>,
}
impl LocationClue {
    pub fn new(data: DataDir, lan: LanClient, model: String) -> Self {
        Self {
            data,
            lan,
            model,
            retry_observations: Mutex::new(None),
        }
    }
    fn input(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Option<Input>> {
        let Some(id) = key.target_key.parse().ok() else {
            return Ok(None);
        };
        Ok(Input::load(conn, id)?
            .map(|p| p.observe(&self.data))
            .filter(|p| p.key(&self.model) == *key))
    }
}
impl Handler for LocationClue {
    fn kind(&self) -> JobKind {
        JobKind::LocationClue
    }
    fn reconcile(&self, read: &Connection, _now: i64) -> rusqlite::Result<Vec<JobKey>> {
        // 全現世代を渡すので、成功済みでも旧非成功世代の失効・terminal 投影の修復を行える。
        Ok(inputs(read)?
            .into_iter()
            .map(|p| p.observe(&self.data).key(&self.model))
            .collect())
    }
    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError> {
        let db_error = |_| RunError::Attempt(ErrorCode::Internal);
        if ctx.is_stopping() {
            return Err(RunError::Stopping);
        }
        let input = self
            .input(ctx.read(), &job.key)
            .map_err(db_error)?
            .ok_or(RunError::InputChanged)?;
        let bytes = read_source(&self.data, &input)?;
        let source_sha = sha(&bytes);
        if input.quality_sha.as_ref().is_some_and(|s| s != &source_sha)
            || input
                .observed_sha
                .as_ref()
                .is_some_and(|s| s != &source_sha)
        {
            return Err(RunError::InputChanged);
        }
        if input.quality_failed {
            return Err(RunError::Permanent(ErrorCode::InputInvalid));
        }
        // 画像無し・不正な入力は品質を永遠に待たせない。デコードの不正は先に恒久失敗へ。
        let rgb = imaging::prepare_decision_rgb(&bytes, !input.rel.starts_with("derivatives/"))
            .map_err(image_error)?;
        drop(bytes);
        if ctx.is_stopping() {
            return Err(RunError::Stopping);
        }
        if input.quality_pending && !input.fallback() {
            return Err(RunError::InputPending);
        }
        // ai_jobs が失われても現世代の検証済み結果を再利用できる。
        if let Some(stored) = Stored::load(ctx.read(), &job.key).map_err(db_error)?
            && stored.valid(&self.model)
            && stored.error.is_empty()
            && stored.source_sha.as_ref() == Some(&source_sha)
        {
            return Ok(persist(input, job.key.clone(), stored, self.model.clone()));
        }
        let image = PreparedImage::encode_jpeg_rgb(
            &rgb.pixels,
            rgb.width as u16,
            rgb.height as u16,
            88,
            ImageProfile::Decision,
        )
        .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
        let request = DecisionRequest::new(&self.model, DECISION_STATE, questions(), vec![image])
            .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
        let reply = decision::run_checked(ctx, &self.lan, &request, || {
            if !input.current(ctx.read(), &self.model).map_err(db_error)? {
                return Err(RunError::InputChanged);
            }
            // Discovery 待ち中の回転・置換も検出。読んでロックを解放した bytes の SHA を比べる。
            let current = read_source(&self.data, &input).map_err(|_| RunError::InputChanged)?;
            if sha(&current) != source_sha {
                return Err(RunError::InputChanged);
            }
            Ok(())
        })?;
        if !input.current(ctx.read(), &self.model).map_err(db_error)?
            || read_source(&self.data, &input).map_or(true, |b| sha(&b) != source_sha)
        {
            return Err(RunError::InputChanged);
        }
        let clue = Clue::from_reply(&reply, &self.model)
            .ok_or(RunError::Attempt(ErrorCode::BadResponse))?;
        let stored = Stored {
            source_sha: Some(source_sha),
            clue: Some(clue.score),
            kind: clue.kind,
            model: self.model.clone(),
            at: waits::utc_iso(ctx.now()),
            error: String::new(),
        };
        Ok(persist(input, job.key.clone(), stored, self.model.clone()))
    }
    fn reconcile_cleanup(&self, read: &Connection, now: i64) -> rusqlite::Result<Vec<Persist>> {
        let mut stmt = read.prepare(
            "SELECT target_key,input_hash,processor_version FROM ai_jobs
            WHERE kind='location_clue' AND state IN ('pending','running','deferred','failed')",
        )?;
        let keys = stmt
            .query_map([], |r| {
                Ok(JobKey::new(
                    JobKind::LocationClue,
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut actions: Vec<Persist> = Vec::new();
        for key in keys {
            if self.input(read, &key)?.is_some() {
                continue;
            }
            let model = self.model.clone();
            let observed = key
                .target_key
                .parse()
                .ok()
                .map(|id| Input::load(read, id))
                .transpose()?
                .flatten()
                .map(|p| p.observe(&self.data))
                .and_then(|p| p.observed_sha);
            actions.push(Box::new(move |conn| {
                let current=key.target_key.parse().ok().map(|id| Input::load(conn,id)).transpose()?.flatten();
                if current.is_none_or(|mut p| { p.observed_sha = observed; p.key(&model)!=key }) {
                    conn.execute("UPDATE ai_jobs SET state='obsolete',claim_token=NULL,updated_at=?4
                        WHERE kind='location_clue' AND target_key=?1 AND input_hash=?2 AND processor_version=?3
                          AND state IN ('pending','running','deferred','failed')",
                        params![key.target_key,key.input_hash,key.processor_version,now])?;
                }
                Ok(InputCheck::Current)
            }));
        }
        Ok(actions)
    }
    fn projection(
        &self,
        read: &Connection,
        key: &JobKey,
        now: i64,
    ) -> rusqlite::Result<Option<Persist>> {
        let Some(input) = self.input(read, key)? else {
            return Ok(Some(Box::new(|_| Ok(InputCheck::Changed))));
        };
        let (key, model) = (key.clone(), self.model.clone());
        let rel = input.projection_rel(&self.data);
        let stored = Stored::load(read, &key)?;
        let success_valid = stored
            .as_ref()
            .is_some_and(|s| s.valid(&model) && s.error.is_empty() && source_matches(&input, s));
        Ok(Some(Box::new(move |conn| {
            if !input.current(conn, &model)? {
                return Ok(InputCheck::Changed);
            }
            let Some((state, code)) = job_state(conn, &key)? else {
                return Ok(InputCheck::Current);
            };
            if state == "failed" {
                let error = ErrorCode::parse(&code)
                    .map_or("other", ErrorCode::as_str)
                    .to_string();
                // 既に同じ投影なら日時も書き換えない。
                if let Some(s) = Stored::load(conn, &key)?
                    && s.valid(&model)
                    && s.error == error
                {
                    s.project(conn, &input, &rel)?;
                } else {
                    Stored {
                        source_sha: input.observed_sha.clone(),
                        clue: None,
                        kind: String::new(),
                        model,
                        at: waits::utc_iso(now),
                        error,
                    }
                    .save(conn, &key, &input, &rel)?;
                }
            } else if state == "succeeded"
                && success_valid
                && let Some(s) = stored
            {
                // Python が互換行だけを変更しても、その内容を trusted とせず保存済み Rust 結果を復元する。
                s.project(conn, &input, &rel)?;
            }
            Ok(InputCheck::Current)
        })))
    }
    fn prepare_retry(&self) -> rusqlite::Result<()> {
        let read = crate::db::open_read_only(&self.data.root().join(crate::config::DB_FILE_NAME))
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let observed = inputs(&read)?
            .into_iter()
            .filter(|p| p.fallback())
            .map(|p| {
                let input = p.observe(&self.data);
                (input.id, input.observed_sha)
            })
            .collect();
        *self
            .retry_observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observed);
        Ok(())
    }
    fn recheck(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck> {
        let stored = Stored::load(conn, key)?;
        let input = key
            .target_key
            .parse()
            .ok()
            .map(|id| Input::load(conn, id))
            .transpose()?
            .flatten();
        let Some(mut input) = input else {
            return Ok(Recheck::Obsolete);
        };
        // prepare_retry が writer 外で得た実 SHA と、ここで読み直す DB 世代を照合する。
        if input.fallback() {
            let prepared = self
                .retry_observations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            input.observed_sha = match prepared.as_ref() {
                Some(map) => map.get(&input.id).cloned().flatten(),
                None => stored.as_ref().and_then(|s| s.source_sha.clone()),
            };
        }
        if input.key(&self.model) != *key {
            return Ok(Recheck::Obsolete);
        }
        if let Some(s) = Stored::load(conn, key)?
            && s.valid(&self.model)
            && s.error.is_empty()
            && input.quality_sha.is_some()
            && input.quality_sha == s.source_sha
            && s.compatible(conn, &input, &input.rel)?
        {
            return Ok(Recheck::Satisfied);
        }
        // 同じ世代の failed 投影だけ無効化。同じ tx の jobs::retry_failed が pending へ戻す。
        if let Some(s) = Stored::load(conn, key)?
            && s.valid(&self.model)
            && !s.error.is_empty()
        {
            conn.execute(
                "DELETE FROM photo_loc_clues WHERE photo_id=?1 AND model=?2 AND error=?3
                AND clue IS NULL AND kind='' AND source_md5 IS ?4 AND source_mtime IS ?5
                AND analyzed_at=?6 AND (display=?7 OR (?8 AND display=''))",
                params![
                    input.id,
                    self.model,
                    s.error,
                    input.md5,
                    input.mtime,
                    s.at,
                    input.rel,
                    input.fallback()
                ],
            )?;
            conn.execute("DELETE FROM ai_location_clues WHERE photo_id=?1 AND input_hash=?2 AND processor_version=?3 AND error!=''",
                params![input.id,key.input_hash,key.processor_version])?;
        }
        Ok(Recheck::Requeue)
    }
}

fn persist(input: Input, key: JobKey, stored: Stored, model: String) -> Persist {
    Box::new(move |conn| {
        if !input.current(conn, &model)? {
            return Ok(InputCheck::Changed);
        }
        stored.save(conn, &key, &input, &input.rel)?;
        Ok(InputCheck::Current)
    })
}

pub(super) fn handler(data: DataDir, lan: LanClient, model: String) -> Arc<dyn Handler> {
    Arc::new(LocationClue::new(data, lan, model))
}

#[cfg(test)]
mod tests;
