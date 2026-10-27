use super::DomainError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_TEXT: usize = 512;
const MAX_EVIDENCE: usize = 64;
const MAX_PHOTOS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    Landmark,
    District,
    City,
    Region,
    Country,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
    Unknown,
}
impl Precision {
    fn parse(v: Option<&Value>) -> Option<Self> {
        serde_json::from_value(v?.clone()).ok()
    }
}
impl Confidence {
    fn parse(v: Option<&Value>) -> Self {
        v.and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(Self::Unknown)
    }
}

/// 正規化済みの地名。内容を Debug/error に出さない。query は URL ではなく地名検索の文字列。
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PhotoAnswer {
    index: usize,
    place_name: String,
    geocode_query: String,
    confidence: Confidence,
    precision: Precision,
}
impl PhotoAnswer {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn place_name(&self) -> &str {
        &self.place_name
    }
    pub fn geocode_query(&self) -> &str {
        &self.geocode_query
    }
    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
    pub fn precision(&self) -> Precision {
        self.precision
    }
}
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Answer {
    country: String,
    country_code: String,
    city: String,
    place_name: String,
    geocode_query: String,
    confidence: Confidence,
    precision: Precision,
    evidence: Vec<String>,
    photos: Vec<PhotoAnswer>,
}
macro_rules! redacted {
    ($t:ty, $s:literal) => {
        impl std::fmt::Debug for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str($s)
            }
        }
    };
}
redacted!(Answer, "Answer(..)");
redacted!(PhotoAnswer, "PhotoAnswer(..)");

fn text(v: Option<&Value>, required: bool, max: usize) -> Result<String, DomainError> {
    let v = match v {
        Some(Value::String(s)) => s,
        None if !required => return Ok(String::new()),
        _ => return Err(DomainError::BadResponse),
    };
    if v.len() > max || v.chars().any(char::is_control) {
        return Err(DomainError::BadResponse);
    }
    Ok(v.clone())
}
// Python str(e) 互換。JSON が表現できる有限値だけ、再帰深さ/件数/結果長を制限する。
fn python_string(v: &Value, depth: usize, nested: bool) -> Result<String, DomainError> {
    if depth > 8 {
        return Err(DomainError::BadResponse);
    }
    let s = match v {
        Value::String(s) if !nested => s.clone(),
        Value::String(s) => {
            // Python repr chooses double quotes when that avoids escaping a single quote.
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::from(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) if n.is_f64() => {
            let repr = format!("{:?}", n.as_f64().ok_or(DomainError::BadResponse)?);
            if let Some((mantissa, exp)) = repr.split_once('e') {
                let exp: i32 = exp.parse().map_err(|_| DomainError::BadResponse)?;
                format!("{mantissa}e{exp:+03}")
            } else {
                repr
            }
        }
        Value::Number(n) => n.to_string(),
        Value::Array(a) => {
            if a.len() > MAX_EVIDENCE {
                return Err(DomainError::BadResponse);
            }
            format!(
                "[{}]",
                a.iter()
                    .map(|e| python_string(e, depth + 1, true))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            )
        }
        Value::Object(o) => {
            if o.len() > MAX_EVIDENCE {
                return Err(DomainError::BadResponse);
            }
            let mut parts = Vec::new();
            for (k, v) in o {
                parts.push(format!(
                    "{}: {}",
                    python_string(&Value::String(k.clone()), depth + 1, true)?,
                    python_string(v, depth + 1, true)?
                ));
            }
            format!("{{{}}}", parts.join(", "))
        }
    };
    if s.len() > 2048 || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(DomainError::BadResponse);
    }
    Ok(s)
}
impl Answer {
    /// Cloud envelope は CloudClient が解く。この入口は JSON object のみ。
    /// 不正型/サイズは固定 BadResponse。未知 enum は Python と同じ unknown/day。
    pub fn parse(json: &str, sent_count: usize) -> Result<Self, DomainError> {
        if json.len() > MAX_BYTES || !(1..=super::MAX_SEND).contains(&sent_count) {
            return Err(DomainError::BadResponse);
        }
        let value: Value = serde_json::from_str(json).map_err(|_| DomainError::BadResponse)?;
        let o = value.as_object().ok_or(DomainError::BadResponse)?;
        let precision = Precision::parse(o.get("precision")).unwrap_or(Precision::Unknown);
        let evidence = match o.get("evidence") {
            Some(Value::Array(a)) => {
                if a.len() > MAX_EVIDENCE {
                    return Err(DomainError::BadResponse);
                }
                a.iter()
                    .map(|v| python_string(v, 0, false))
                    .collect::<Result<_, _>>()?
            }
            _ => Vec::new(),
        };
        let mut photos = Vec::new();
        if let Some(v) = o.get("photos")
            && !v.is_null()
        {
            let a = v.as_array().ok_or(DomainError::BadResponse)?;
            if a.len() > MAX_PHOTOS {
                return Err(DomainError::BadResponse);
            }
            for v in a {
                let Some(o) = v.as_object() else {
                    continue;
                };
                let Some(index) = o.get("index") else {
                    continue;
                };
                let index = index.as_i64().ok_or(DomainError::BadResponse)?;
                if index < 1
                    || index > sent_count as i64
                    || !o.get("place_name").is_some_and(Value::is_string)
                {
                    continue;
                }
                if photos
                    .iter()
                    .any(|p: &PhotoAnswer| p.index == index as usize)
                {
                    return Err(DomainError::BadResponse);
                }
                photos.push(PhotoAnswer {
                    index: index as usize,
                    place_name: text(o.get("place_name"), true, MAX_TEXT)?,
                    geocode_query: text(o.get("geocode_query"), false, MAX_TEXT)?,
                    confidence: Confidence::parse(o.get("confidence")),
                    precision: Precision::parse(o.get("precision")).unwrap_or(precision),
                });
            }
        }
        Ok(Self {
            country: text(o.get("country"), false, MAX_TEXT)?,
            // Keep string semantics (Python accepts an empty country code); never use as URL/path.
            country_code: text(o.get("country_code"), false, 16)?,
            city: text(o.get("city"), false, MAX_TEXT)?,
            place_name: text(o.get("place_name"), true, MAX_TEXT)?,
            geocode_query: text(o.get("geocode_query"), false, MAX_TEXT)?,
            confidence: Confidence::parse(o.get("confidence")),
            precision,
            evidence,
            photos,
        })
    }
    pub fn normalized_json(&self) -> String {
        super::encoded(self)
    }
    pub fn country(&self) -> &str {
        &self.country
    }
    pub fn country_code(&self) -> &str {
        &self.country_code
    }
    pub fn city(&self) -> &str {
        &self.city
    }
    pub fn place_name(&self) -> &str {
        &self.place_name
    }
    pub fn geocode_query(&self) -> &str {
        &self.geocode_query
    }
    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
    pub fn precision(&self) -> Precision {
        self.precision
    }
    pub fn evidence(&self) -> &[String] {
        &self.evidence
    }
    pub fn photos(&self) -> &[PhotoAnswer] {
        &self.photos
    }
}
