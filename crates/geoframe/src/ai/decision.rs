//! Decision（local-model-lab の `d3` サービス）のクライアント（P4 / T4.2。設計は p4-design.md §5「Decision」）。
//!
//! 固定の契約（参照した版は `docs/rust-migration/ai-provider.md`）:
//!
//! - サービス ID は `decision`、API は `GET /llm-decision/health`・`POST /llm-decision/v1/systemone`（`GET .../v1/models` は
//!   使わない。モデルの照合は health の `model` で足りる）。
//! - health … `{status: ready|loading, model, max_input_tokens, modalities, authentication}`。`status=ready` かつ
//!   `modalities` に `image` があり、`model` が設定の `location_clue` のモデルと同じときだけ送れる。`loading`・
//!   `modalities` が null は**一時的な待ち**。
//! - systemone … 要求 `{model, state, questions:{名前:{type: choice|noul, instructions, criteria}}, images:[dataURL]}`、
//!   応答 `{model, answers, usage}`。`answers[名前]` は choice なら `{choice, confidence}`、noul なら `{noul}`（0〜1）、
//!   答えられない質問は `{type, error, ...}`。上流は本文 16 MiB・画像 1 枚 8,000,000 バイト・1.6 メガピクセル・応答のヘッダー
//!   120 秒。**要求 JSON は GEOFRAME 側の 8 MiB（base64 込み）を優先**する。
//!
//! # 範囲
//!
//! ここは**入出力の形の検査**まで。質問の文面・clue の閾値・結果の保存（`photo_loc_clues`）・位置候補への接続は T4.6。
//! 画像は [`PreparedImage`]（検証済みの RGB 画素列から新しく作った、メタデータなしのベースライン JPEG）だけを受け取り、
//! 圧縮済みの画像・dataURL・URL・パスは受けない。

use serde_json::{Map, Value, json};

use super::discovery::{self, ContractError, Expect};
use super::http::{Deadline, Endpoint, MAX_REQUEST_BYTES, ServiceId, Session};
use super::image_input::PreparedImage;
use super::jobs::ErrorCode;
use super::provider::{LanClient, Phase, ProviderError};
use super::waits::{WaitReason, WaitScope};
use super::worker::{RunCtx, RunError};

/// Decision のサービス ID（`/llm/services/decision`・接頭辞 `/llm-decision`）。
pub const SERVICE_ID: &str = "decision";
/// catalog に必要な API（method＋接頭辞を除いた path）。
const REQUIRED_APIS: &[(&str, &str)] = &[("POST", "/v1/systemone"), ("GET", "/health")];

/// 1 回の要求の画像の枚数の上限。
pub const MAX_IMAGES: usize = 4;
const MAX_QUESTIONS: usize = 8;
const MAX_CHOICES: usize = 32;
const MAX_TEXT_CHARS: usize = 4000;

fn service_id() -> ServiceId {
    // 固定の文字列（検査を通る）
    ServiceId::parse(SERVICE_ID).expect("decision は有効なサービス ID")
}

// ---------------------------------------------------------------- 要求

/// 要求を作れなかった理由（固定の列挙。恒久の失敗）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// 質問・画像の数や文字の検査に合わない。
    Invalid,
    /// 完成した JSON（base64 込み）が 8 MiB を超える。送らない。
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionKind {
    /// 選択肢（名前→説明）から選ぶ。
    Choice(Vec<(String, String)>),
    /// はい・いいえの度合い（0〜1）。
    Noul,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub instructions: String,
    pub kind: QuestionKind,
}

fn valid_name(name: &str, max: usize) -> bool {
    !name.is_empty()
        && name.len() <= max
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn valid_text(text: &str) -> bool {
    !text.is_empty() && text.chars().count() <= MAX_TEXT_CHARS
}

/// 検査済みの systemone 要求。`Debug` は文面・画像を出さない。
#[derive(Clone)]
pub struct DecisionRequest {
    model: String,
    state: String,
    questions: Vec<Question>,
    images: Vec<PreparedImage>,
}

impl std::fmt::Debug for DecisionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionRequest")
            .field("questions", &self.questions.len())
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

impl DecisionRequest {
    pub fn new(
        model: &str,
        state: &str,
        questions: Vec<Question>,
        images: Vec<PreparedImage>,
    ) -> Result<Self, RequestError> {
        let questions_ok = !questions.is_empty()
            && questions.len() <= MAX_QUESTIONS
            && questions.iter().enumerate().all(|(i, q)| {
                valid_name(&q.name, 32)
                    && valid_text(&q.instructions)
                    && questions[..i].iter().all(|o| o.name != q.name)
                    && match &q.kind {
                        QuestionKind::Noul => true,
                        QuestionKind::Choice(choices) => {
                            !choices.is_empty()
                                && choices.len() <= MAX_CHOICES
                                && choices.iter().enumerate().all(|(j, (key, text))| {
                                    valid_name(key, 64)
                                        && valid_text(text)
                                        && choices[..j].iter().all(|(k, _)| k != key)
                                })
                        }
                    }
            });
        if !questions_ok
            || model.is_empty()
            || !valid_text(state)
            || images.is_empty()
            || images.len() > MAX_IMAGES
        {
            return Err(RequestError::Invalid);
        }
        Ok(Self {
            model: model.to_string(),
            state: state.to_string(),
            questions,
            images,
        })
    }

    /// 完成した JSON（base64 込み）。8 MiB を超えるなら、作る前に断る（メモリも使わない）。
    pub fn to_json(&self) -> Result<Vec<u8>, RequestError> {
        let text: usize = self.state.len()
            + self
                .questions
                .iter()
                .map(|q| {
                    q.name.len()
                        + q.instructions.len()
                        + match &q.kind {
                            QuestionKind::Noul => 0,
                            QuestionKind::Choice(c) => {
                                c.iter().map(|(k, t)| k.len() + t.len() + 16).sum()
                            }
                        }
                        + 64
                })
                .sum::<usize>();
        // dataURL の接頭辞（`data:image/jpeg;base64,`）と JSON の引用符・区切りを多めに見積もる
        let images: usize = self.images.iter().map(|i| i.base64_len() + 40).sum();
        if text.saturating_mul(6) + images + 1024 > MAX_REQUEST_BYTES.saturating_mul(2) {
            // 見積もりが上限の 2 倍を超えるなら、組み立てる前に断る
            return Err(RequestError::TooLarge);
        }
        let mut questions = Map::new();
        for q in &self.questions {
            let mut entry = Map::new();
            match &q.kind {
                QuestionKind::Noul => {
                    entry.insert("type".into(), json!("noul"));
                    entry.insert("instructions".into(), json!(q.instructions));
                }
                QuestionKind::Choice(choices) => {
                    entry.insert("type".into(), json!("choice"));
                    entry.insert("instructions".into(), json!(q.instructions));
                    let criteria: Map<String, Value> =
                        choices.iter().map(|(k, t)| (k.clone(), json!(t))).collect();
                    entry.insert("criteria".into(), Value::Object(criteria));
                }
            }
            questions.insert(q.name.clone(), Value::Object(entry));
        }
        let body = json!({
            "model": self.model,
            "state": self.state,
            "questions": Value::Object(questions),
            "images": self.images.iter().map(PreparedImage::data_url).collect::<Vec<_>>(),
        });
        let bytes = serde_json::to_vec(&body).map_err(|_| RequestError::Invalid)?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(RequestError::TooLarge);
        }
        Ok(bytes)
    }
}

// ---------------------------------------------------------------- 応答

/// 1 つの質問への答え（形の検査済み。意味づけ・閾値は呼び出し側）。
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Choice {
        choice: String,
        confidence: Option<f64>,
    },
    /// 0〜1。
    Noul(f64),
    /// 上流が答えられないと返した質問。
    Unanswerable,
}

/// 検査済みの応答。`Debug` は答えの中身を出さない。
#[derive(Clone, PartialEq)]
pub struct DecisionReply {
    pub model: String,
    /// 要求の質問の順。
    pub answers: Vec<(String, Answer)>,
}

impl std::fmt::Debug for DecisionReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionReply")
            .field("answers", &self.answers.len())
            .finish_non_exhaustive()
    }
}

/// 応答の本文を、要求に照らして検査する。形が違えば `None`（ジョブの失敗 1 回）。
pub fn parse_reply(body: &[u8], request: &DecisionRequest) -> Option<DecisionReply> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let doc = value.as_object()?;
    let model = doc.get("model")?.as_str()?.to_string();
    doc.get("usage")?.as_object()?;
    let answers = doc.get("answers")?.as_object()?;
    let mut out = Vec::with_capacity(request.questions.len());
    for question in &request.questions {
        let entry = answers.get(&question.name)?.as_object()?;
        let answer = if entry.contains_key("error") {
            Answer::Unanswerable
        } else {
            match &question.kind {
                QuestionKind::Noul => {
                    let n = entry.get("noul")?.as_f64()?;
                    if !n.is_finite() || !(0.0..=1.0).contains(&n) {
                        return None;
                    }
                    Answer::Noul(n)
                }
                QuestionKind::Choice(choices) => {
                    let choice = entry.get("choice")?.as_str()?;
                    if !choices.iter().any(|(k, _)| k == choice) {
                        return None;
                    }
                    let confidence = match entry.get("confidence") {
                        None | Some(Value::Null) => None,
                        Some(c) => {
                            let c = c.as_f64()?;
                            if !c.is_finite() {
                                return None;
                            }
                            Some(c)
                        }
                    };
                    Answer::Choice {
                        choice: choice.to_string(),
                        confidence,
                    }
                }
            }
        };
        out.push((question.name.clone(), answer));
    }
    Some(DecisionReply {
        model,
        answers: out,
    })
}

// ---------------------------------------------------------------- health・Discovery

/// health の応答を検査する。送れる状態なら `Ok(())`。`loading`・`modalities` が null は一時的な待ち。
pub fn check_health(body: &[u8], configured_model: &str) -> Result<(), ProviderError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| ProviderError::from(ContractError::Health))?;
    let doc = value
        .as_object()
        .ok_or(ProviderError::from(ContractError::Health))?;
    let health = ProviderError::from(ContractError::Health);
    let not_ready = ProviderError::wait(WaitScope::Service, WaitReason::NotReady);
    match doc.get("status").and_then(Value::as_str).ok_or(health)? {
        "loading" => return Err(not_ready),
        "ready" => {}
        _ => return Err(health),
    }
    // ready でも modalities が null（まだ読めていない）なら待つ。配列でなければ契約違反
    let modalities = match doc.get("modalities").ok_or(health)? {
        Value::Null => return Err(not_ready),
        Value::Array(items) => items,
        _ => return Err(health),
    };
    if !modalities.iter().all(Value::is_string) {
        return Err(health);
    }
    if !modalities.iter().any(|m| m.as_str() == Some("image")) {
        return Err(ProviderError::from(ContractError::Capability));
    }
    match doc.get("model").and_then(Value::as_str) {
        Some(model) if model == configured_model => Ok(()),
        Some(_) => Err(ProviderError::from(ContractError::ModelMismatch)),
        None => Err(health),
    }
}

/// 送る前の確認: Discovery（catalog・service）と health。全体で 1 つの Discovery 期限（2 秒）を共有する。
pub fn probe(session: &Session, configured_model: &str) -> Result<(), ProviderError> {
    let deadline = Deadline::discovery();
    probe_until(session, configured_model, &deadline)
}

pub(super) fn probe_until(
    session: &Session,
    configured_model: &str,
    deadline: &Deadline,
) -> Result<(), ProviderError> {
    let id = service_id();
    discovery::check(
        session,
        &Expect {
            service: &id,
            apis: REQUIRED_APIS,
        },
        deadline,
    )?;
    let reply = session
        .get(Endpoint::DecisionHealth, deadline)
        .map_err(|e| ProviderError::from_http(e, Phase::Probe))?;
    if !reply.is_success() {
        return Err(ProviderError::from_status(
            reply.status,
            reply.retry_after_secs,
            Phase::Probe,
            false,
        ));
    }
    check_health(&reply.body, configured_model)
}

/// systemone を POST して、応答の形を検査する。呼ぶ前に [`probe`] と `RunCtx::before_send` を済ませること。
/// 要求が 8 MiB を超えるなら、接続せずに恒久の失敗。
pub fn ask(
    session: &Session,
    request: &DecisionRequest,
    deadline: &Deadline,
) -> Result<DecisionReply, ProviderError> {
    let body = request
        .to_json()
        .map_err(|_| ProviderError::Permanent(ErrorCode::InputTooLarge))?;
    let reply = session
        .post_json(Endpoint::DecisionSystemOne, &body, deadline)
        .map_err(|e| ProviderError::from_http(e, Phase::Send))?;
    if !reply.is_success() {
        return Err(ProviderError::from_status(
            reply.status,
            reply.retry_after_secs,
            Phase::Send,
            false,
        ));
    }
    parse_reply(&reply.body, request).ok_or(ProviderError::BadResponse)
}

/// handler（T4.6）が呼ぶ入口。1 回の判定を、次の順で行う:
///
/// 1. 要求の JSON を組み立てて 8 MiB を検査（超えるなら接続せず恒久の失敗）。
/// 2. 設定・停止・Provider 停止を確かめ（[`RunCtx::before_probe`]。クラウドの送信枠は使わない）、背景の枠を取る。
/// 3. Discovery と health（[`probe`]）。使えなければ**送らず**、範囲つきの待ち（`RunError::ProviderWait`）。
/// 4. [`RunCtx::before_send`]（送信の直前の確認）→ POST。
/// 5. POST が通信失敗・5xx なら、Discovery をやり直す。使えなければ待ち（ジョブの失敗に数えない）、
///    使えるならそのジョブだけの反復失敗（`RunError::Repeated`）。
///
/// 停止の合図があれば新しい要求は出さないが、送信済みの要求の応答は待って返す（有効なら確定される）。
pub fn run(
    ctx: &RunCtx<'_>,
    lan: &LanClient,
    request: &DecisionRequest,
) -> Result<DecisionReply, RunError> {
    run_checked(ctx, lan, request, || Ok(()))
}

/// Discovery / health 待ちの後、推論 POST の直前にも写真の世代・実入力を照合する。
/// callback は writer / transaction / 画像 lock を保持せずに戻すこと。
pub fn run_checked(
    ctx: &RunCtx<'_>,
    lan: &LanClient,
    request: &DecisionRequest,
    before_post: impl FnOnce() -> Result<(), RunError>,
) -> Result<DecisionReply, RunError> {
    request
        .to_json()
        .map_err(|_| RunError::Permanent(ErrorCode::InputTooLarge))?;
    let gate = ctx.before_probe()?;
    let model = gate.settings.model.as_str();
    let session = lan
        .background(ctx.stop())
        .map_err(|e| e.into_run_error(false))?;
    probe(&session, model).map_err(|e| e.into_run_error(false))?;
    ctx.before_send()?;
    before_post()?;
    let deadline = Deadline::for_task(ctx.task());
    match ask(&session, request, &deadline) {
        Ok(reply) => Ok(reply),
        Err(ProviderError::Transient(code)) => {
            // 確認は予約ではない。lease の開始などで落ちたのなら、ジョブの不良ではない
            match probe(&session, model) {
                Ok(()) => Err(ProviderError::Transient(code).into_run_error(false)),
                Err(other) => Err(other.into_run_error(false)),
            }
        }
        Err(other) => Err(other.into_run_error(false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::image_input::{ImageProfile, fixtures};

    fn image() -> PreparedImage {
        fixtures::image(32, 24)
    }

    pub(crate) fn questions() -> Vec<Question> {
        vec![
            Question {
                name: "clue".into(),
                instructions: "合成の質問 1".into(),
                kind: QuestionKind::Noul,
            },
            Question {
                name: "kind".into(),
                instructions: "合成の質問 2".into(),
                kind: QuestionKind::Choice(vec![
                    ("a".into(), "選択肢 A".into()),
                    ("b".into(), "選択肢 B".into()),
                ]),
            },
        ]
    }

    pub(crate) fn request() -> DecisionRequest {
        DecisionRequest::new("example-decision", "合成の状況", questions(), vec![image()]).unwrap()
    }

    #[test]
    fn request_json_has_the_contract_shape_and_only_data_urls() {
        let json: Value = serde_json::from_slice(&request().to_json().unwrap()).unwrap();
        assert_eq!(json["model"], "example-decision");
        assert_eq!(json["questions"]["clue"]["type"], "noul");
        assert_eq!(json["questions"]["kind"]["type"], "choice");
        assert_eq!(json["questions"]["kind"]["criteria"]["a"], "選択肢 A");
        let images = json["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert!(
            images[0]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        // 質問の順が保たれる
        let keys: Vec<_> = json["questions"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["clue", "kind"]);
    }

    #[test]
    fn request_validation_rejects_bad_shapes() {
        let q = questions;
        let img = || vec![image()];
        assert!(DecisionRequest::new("m", "s", q(), vec![]).is_err());
        assert!(DecisionRequest::new("m", "s", vec![], img()).is_err());
        assert!(DecisionRequest::new("", "s", q(), img()).is_err());
        assert!(DecisionRequest::new("m", "", q(), img()).is_err());
        assert!(DecisionRequest::new("m", "s", q(), vec![image(); MAX_IMAGES + 1]).is_err());
        let mut dup = q();
        dup.push(dup[0].clone());
        assert!(DecisionRequest::new("m", "s", dup, img()).is_err());
        let mut bad_name = q();
        bad_name[0].name = "a/b".into();
        assert!(DecisionRequest::new("m", "s", bad_name, img()).is_err());
        let mut no_choices = q();
        no_choices[1].kind = QuestionKind::Choice(vec![]);
        assert!(DecisionRequest::new("m", "s", no_choices, img()).is_err());
    }

    #[test]
    fn oversized_request_json_is_refused_before_it_is_sent() {
        // 清浄な（画素から作った）JPEG だけで 8 MiB を超える: 圧縮しにくい 1024×1024 を 1 回だけエンコードし、
        // 1 枚（base64 で約 5.8MB）は収まり、2 枚は収まらない（上流の 16 MiB より厳しい 8 MiB のほうを優先）
        let big = PreparedImage::encode_jpeg_rgb(
            &fixtures::noise_rgb(1024, 1024),
            1024,
            1024,
            100,
            ImageProfile::Decision,
        )
        .unwrap();
        assert!(big.len() < 8_000_000);
        let one = DecisionRequest::new("m", "s", questions(), vec![big.clone()]).unwrap();
        assert!(one.to_json().is_ok(), "1 枚なら 8 MiB に収まる");
        let two = DecisionRequest::new("m", "s", questions(), vec![big; 2]).unwrap();
        assert_eq!(two.to_json().unwrap_err(), RequestError::TooLarge);
    }

    #[test]
    fn health_requires_ready_image_and_the_configured_model() {
        let health = |v: Value| check_health(&serde_json::to_vec(&v).unwrap(), "m");
        let ok = json!({"status": "ready", "model": "m", "max_input_tokens": 1, "modalities": ["text", "image"], "authentication": false});
        assert_eq!(health(ok.clone()), Ok(()));
        let not_ready = Err(ProviderError::wait(
            WaitScope::Service,
            WaitReason::NotReady,
        ));
        assert_eq!(
            health(
                json!({"status": "loading", "model": null, "max_input_tokens": null, "modalities": null, "authentication": false})
            ),
            not_ready
        );
        assert_eq!(
            health(json!({"status": "ready", "model": "m", "modalities": null})),
            not_ready
        );
        // 画像が無い（text のみ）は契約違反（能力）
        assert_eq!(
            health(json!({"status": "ready", "model": "m", "modalities": ["text"]})),
            Err(ProviderError::contract(false))
        );
        // 別のモデル名は model_not_found
        assert_eq!(
            health(json!({"status": "ready", "model": "other", "modalities": ["image"]})),
            Err(ProviderError::Pause(
                crate::ai::pause::PauseScope::Task,
                crate::ai::pause::PauseReason::ModelNotFound
            ))
        );
        // 形が違う
        for bad in [
            json!({"status": "weird", "modalities": ["image"]}),
            json!({"status": "ready", "model": "m"}),
            json!({"status": "ready", "model": "m", "modalities": "image"}),
            json!({"status": "ready", "model": "m", "modalities": [1]}),
            json!({"status": "ready", "modalities": ["image"]}),
            json!([]),
        ] {
            assert_eq!(
                health(bad.clone()),
                Err(ProviderError::contract(false)),
                "{bad}"
            );
        }
        assert_eq!(
            check_health(b"nope", "m"),
            Err(ProviderError::contract(false))
        );
    }

    #[test]
    fn reply_shape_is_checked_against_the_request() {
        let request = request();
        let good = json!({
            "model": "served", "usage": {"tokens": 1},
            "answers": {"clue": {"noul": 0.75}, "kind": {"choice": "a", "confidence": 0.9}}
        });
        let parsed = parse_reply(&serde_json::to_vec(&good).unwrap(), &request).unwrap();
        assert_eq!(parsed.model, "served");
        assert_eq!(
            parsed.answers,
            vec![
                ("clue".to_string(), Answer::Noul(0.75)),
                (
                    "kind".to_string(),
                    Answer::Choice {
                        choice: "a".into(),
                        confidence: Some(0.9)
                    }
                )
            ]
        );
        // 答えられない質問は Unanswerable
        let unanswerable = json!({
            "model": "m", "usage": {},
            "answers": {"clue": {"type": "noul", "error": "x"}, "kind": {"choice": "b"}}
        });
        let parsed = parse_reply(&serde_json::to_vec(&unanswerable).unwrap(), &request).unwrap();
        assert_eq!(parsed.answers[0].1, Answer::Unanswerable);
        for bad in [
            json!({"usage": {}, "answers": {}}),
            json!({"model": "m", "answers": {"clue": {"noul": 0.5}, "kind": {"choice": "a"}}}),
            json!({"model": "m", "usage": {}, "answers": {"clue": {"noul": 1.5}, "kind": {"choice": "a"}}}),
            json!({"model": "m", "usage": {}, "answers": {"clue": {"noul": "0.5"}, "kind": {"choice": "a"}}}),
            json!({"model": "m", "usage": {}, "answers": {"clue": {"noul": 0.5}, "kind": {"choice": "zzz"}}}),
            json!({"model": "m", "usage": {}, "answers": {"clue": {"noul": 0.5}}}),
            json!({"model": "m", "usage": {}, "answers": []}),
        ] {
            assert!(
                parse_reply(&serde_json::to_vec(&bad).unwrap(), &request).is_none(),
                "{bad}"
            );
        }
        assert!(parse_reply(b"{", &request).is_none());
    }
}
