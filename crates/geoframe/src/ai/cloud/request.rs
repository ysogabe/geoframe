//! 固定用途の generateContent 入力。画像は RGB から新規生成された PreparedImage だけ。
use super::super::{
    http::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES},
    image_input::PreparedImage,
    settings::TaskKind,
};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    Invalid,
    TooLarge,
}

/// 文面・画像・schema は Debug に出さない。外部 URL/inlineData/任意 JSON の公開入口は無い。
#[derive(Clone)]
pub struct GenerateRequest {
    pub(super) task: TaskKind,
    pub(super) body: Vec<u8>,
}
impl std::fmt::Debug for GenerateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerateRequest")
            .field("task", &self.task)
            .finish_non_exhaustive()
    }
}

impl GenerateRequest {
    #[cfg(test)]
    pub(crate) fn body_json_for_test(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    // T4.5/T4.6c の crate 内固定 prompt/schema 用。
    #[allow(dead_code)]
    pub(crate) fn photo_judge(
        prompt: &str,
        schema: &Value,
        images: &[(u32, PreparedImage)],
    ) -> Result<Self, RequestError> {
        Self::build(TaskKind::PhotoJudge, prompt, schema, images)
    }
    #[allow(dead_code)]
    pub(crate) fn location_suggest(
        prompt: &str,
        schema: &Value,
        images: &[(u32, PreparedImage)],
    ) -> Result<Self, RequestError> {
        Self::build(TaskKind::LocationSuggest, prompt, schema, images)
    }
    fn build(
        task: TaskKind,
        prompt: &str,
        schema: &Value,
        images: &[(u32, PreparedImage)],
    ) -> Result<Self, RequestError> {
        if prompt.trim().is_empty()
            || !schema.is_object()
            || images.is_empty()
            || images.len() > 64
            || images
                .iter()
                .enumerate()
                .any(|(i, (n, _))| *n == 0 || images[..i].iter().any(|(m, _)| n == m))
        {
            return Err(RequestError::Invalid);
        }
        // 大きい入力は base64 を確保する前に断る。完成後にも再検査する。
        let image_bytes = images.iter().map(|(_, i)| i.base64_len()).sum::<usize>();
        if prompt.len().saturating_add(image_bytes) > MAX_REQUEST_BYTES {
            return Err(RequestError::TooLarge);
        }
        let mut parts = vec![json!({"text": prompt})];
        for (number, image) in images {
            let label = if task == TaskKind::LocationSuggest {
                format!("写真{number}")
            } else {
                format!("Image {number}")
            };
            parts.push(json!({"text": label}));
            parts
                .push(json!({"inlineData": {"mimeType":"image/jpeg", "data":image.jpeg_base64()}}));
        }
        let body = serde_json::to_vec(&json!({
            "contents": [{"role":"user", "parts":parts}],
            "generationConfig": {"temperature":0.1, "responseMimeType":"application/json", "responseJsonSchema":schema}
        }))
        .map_err(|_| RequestError::Invalid)?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(RequestError::TooLarge);
        }
        Ok(Self { task, body })
    }
}

/// envelope から取り出した JSON テキスト。domain の parse は後続。Debug に本文を出さない。
#[derive(Clone, PartialEq, Eq)]
pub struct JsonReply(String);
impl JsonReply {
    pub fn json_text(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for JsonReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JsonReply(..)")
    }
}

pub(super) fn parse_reply(body: &[u8]) -> Option<JsonReply> {
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return None;
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    let candidates = v.get("candidates")?.as_array()?;
    if candidates.len() != 1 {
        return None;
    }
    let c = &candidates[0];
    if c.get("finishReason")?.as_str()? != "STOP" {
        return None;
    }
    let parts = c.get("content")?.get("parts")?.as_array()?;
    if parts.is_empty() {
        return None;
    }
    let mut text = String::new();
    for part in parts {
        let object = part.as_object()?;
        // 単発要求なので署名は再送・保存しない。既知の text 付随メタデータだけ許可。
        if object
            .keys()
            .any(|k| k != "text" && k != "thoughtSignature")
            || object
                .get("thoughtSignature")
                .is_some_and(|s| !s.is_string())
        {
            return None;
        } // URL/function call/inlineData/thought 等は引き続き拒否。
        text.push_str(object.get("text")?.as_str()?);
    }
    if text.is_empty() || text.len() as u64 > MAX_RESPONSE_BYTES {
        return None;
    }
    let json: Value = serde_json::from_str(&text).ok()?;
    if !json.is_object() && !json.is_array() {
        return None;
    }
    Some(JsonReply(text))
}
