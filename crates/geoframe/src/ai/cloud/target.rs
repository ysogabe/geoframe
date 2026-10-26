//! Google の固定ホストと単一 path segment の検査。設定の検査を信用せず再検査する。
use super::super::settings::CloudTarget;

pub(super) const TOKEN_HOST: &str = "oauth2.googleapis.com";
pub(super) const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub(super) const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub host: String,
    pub path: String,
}

fn label(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

impl Target {
    pub fn generate(cloud: &CloudTarget, model: &str) -> Option<Self> {
        if !label(&cloud.project, 63)
            || !label(&cloud.region, 32)
            || model.is_empty()
            || model.len() > 64
            || model.contains("..")
            || !model.as_bytes()[0].is_ascii_alphanumeric()
            || !model.as_bytes().last()?.is_ascii_alphanumeric()
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || !cloud.credential_path.is_absolute()
            || cloud.credential_path.as_os_str().len() > 4096
            || cloud
                .credential_path
                .to_str()?
                .bytes()
                .any(|b| b < 0x20 || b == 0x7f)
        {
            return None;
        }
        let host = match cloud.region.as_str() {
            "global" => "aiplatform.googleapis.com".to_string(),
            "us" | "eu" => format!("aiplatform.{}.rep.googleapis.com", cloud.region),
            _ => format!("{}-aiplatform.googleapis.com", cloud.region),
        };
        Some(Self {
            host,
            path: format!(
                "/v1/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
                cloud.project, cloud.region, model
            ),
        })
    }
}
