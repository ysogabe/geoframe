//! サービスアカウントの JWT（RS256）。Drive の認証（T2.4）が、これでトークン交換用の assertion を作る。
//!
//! # 方針
//!
//! - 署名は `ring`（rustls が既に引いている）。`rsa` crate は使わない（純 Rust 実装は署名にタイミング
//!   攻撃の既知の問題があり、修正版が無い）。ring は PKCS#8（`-----BEGIN PRIVATE KEY-----`、Google の
//!   鍵 JSON の `private_key`）と PKCS#1（`-----BEGIN RSA PRIVATE KEY-----`）の秘密鍵を読める。
//! - JWT は `base64url(header).base64url(claims).base64url(signature)`（パディングなし）。
//! - **鍵の中身・トークンをログやエラー文字列に出さない**。[`JwtError`] は定型の種類だけを持つ。
//!   [`Rs256Signer`] の `Debug` は中身を出さない。
//! - 鍵ファイルを読む処理（パスは設定が持つ）と、Google へ送る処理は T2.4。ここは署名と検証だけ。

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{self, RsaKeyPair};
use serde::Serialize;
use serde_json::Value;

const HEADER: &str = r#"{"alg":"RS256","typ":"JWT"}"#;

/// JWT の失敗の種類。値（鍵・トークンの断片）を含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtError {
    /// PEM として読めない（ヘッダ・フッタ・Base64 の誤り）。
    Pem,
    /// RSA の秘密鍵として受理されない（形式違い・2048 ビット未満など）。
    Key,
    /// 署名に失敗した。
    Sign,
    /// JWT の形が違う（3 つに分かれない・Base64 の誤り・alg が RS256 でない）。
    Format,
    /// 署名が合わない。
    Signature,
}

impl fmt::Display for JwtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pem => "秘密鍵を PEM として読めません",
            Self::Key => "RSA の秘密鍵として受理されません",
            Self::Sign => "JWT に署名できません",
            Self::Format => "JWT の形式が不正です",
            Self::Signature => "JWT の署名が一致しません",
        })
    }
}

impl std::error::Error for JwtError {}

/// RS256 で署名する。
pub struct Rs256Signer {
    key: RsaKeyPair,
}

// 鍵の中身を `{:?}` で出さない
impl fmt::Debug for Rs256Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Rs256Signer(..)")
    }
}

impl Rs256Signer {
    /// PEM（PKCS#8 または PKCS#1）の RSA 秘密鍵から作る。
    pub fn from_pem(pem: &str) -> Result<Self, JwtError> {
        let (label, der) = decode_pem(pem)?;
        let key = match label {
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&der),
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(&der),
            _ => return Err(JwtError::Pem),
        }
        .map_err(|_| JwtError::Key)?;
        Ok(Self { key })
    }

    /// `claims` を JSON にして署名し、JWT を返す。
    pub fn sign<T: Serialize>(&self, claims: &T) -> Result<String, JwtError> {
        let claims = serde_json::to_vec(claims).map_err(|_| JwtError::Format)?;
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(HEADER),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let mut signature = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &signature::RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| JwtError::Sign)?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }

    /// 公開鍵（PKCS#1 の `RSAPublicKey` DER）。[`verify`] に渡す。
    pub fn public_key_der(&self) -> &[u8] {
        self.key.public().as_ref()
    }
}

/// RS256 の JWT を検証し、claims（JSON）を返す。`exp` などの意味は見ない（署名と形だけ）。
/// 現在の用途は試験と、署名済みの assertion の自己確認。
pub fn verify(token: &str, public_key_der: &[u8]) -> Result<Value, JwtError> {
    let mut parts = token.split('.');
    let (Some(header), Some(claims), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Format);
    };
    let header: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(header)
            .map_err(|_| JwtError::Format)?,
    )
    .map_err(|_| JwtError::Format)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(JwtError::Format);
    }
    let sig = URL_SAFE_NO_PAD.decode(sig).map_err(|_| JwtError::Format)?;
    // 署名の対象は 2 つ目の `.` より前（`parts` が 3 つだけだったので、最後の `.` がそれ）
    let signing_input = &token[..token.rfind('.').ok_or(JwtError::Format)?];
    signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, public_key_der)
        .verify(signing_input.as_bytes(), &sig)
        .map_err(|_| JwtError::Signature)?;
    serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(claims)
            .map_err(|_| JwtError::Format)?,
    )
    .map_err(|_| JwtError::Format)
}

/// PEM を 1 つだけ読み、(ラベル, DER) を返す。前後の空白・改行（CRLF を含む）は許す。
fn decode_pem(pem: &str) -> Result<(&str, Vec<u8>), JwtError> {
    let pem = pem.trim();
    let rest = pem.strip_prefix("-----BEGIN ").ok_or(JwtError::Pem)?;
    let (label, rest) = rest.split_once("-----").ok_or(JwtError::Pem)?;
    let (body, tail) = rest.split_once("-----END ").ok_or(JwtError::Pem)?;
    if tail.trim_end() != format!("{label}-----") {
        return Err(JwtError::Pem);
    }
    let body: String = body.split_whitespace().collect();
    let der = STANDARD.decode(body).map_err(|_| JwtError::Pem)?;
    Ok((label, der))
}

/// Google のサービスアカウントが OAuth のトークンと交換する assertion の claims
/// （RFC 7523 / Google の JWT bearer）。`exp` は `iat` の 1 時間後（Google の上限）。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct GoogleAssertion<'a> {
    pub iss: &'a str,
    pub scope: &'a str,
    pub aud: &'a str,
    pub iat: u64,
    pub exp: u64,
}

impl<'a> GoogleAssertion<'a> {
    pub fn new(iss: &'a str, scope: &'a str, aud: &'a str, iat: u64) -> Self {
        Self {
            iss,
            scope,
            aud,
            iat,
            exp: iat + 3600,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // テスト専用の使い捨ての RSA 鍵（2048 ビット。どのサービスにも登録していない）。PEM のヘッダ行を
    // 付けずに Base64 だけをファイルに置き、ここで PEM に包む（鍵の検出ツールの誤検知を避けるため）。
    // 作り直すとき: openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048
    const PKCS8_B64: &str = include_str!("../tests/fixtures/jwt-test-pkcs8.b64");
    const PKCS1_B64: &str = include_str!("../tests/fixtures/jwt-test-pkcs1.b64");

    fn pem(label: &str, b64: &str) -> String {
        let lines: Vec<&str> = b64
            .trim()
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            lines.join("\n")
        )
    }

    fn signer() -> Rs256Signer {
        Rs256Signer::from_pem(&pem("PRIVATE KEY", PKCS8_B64)).unwrap()
    }

    #[test]
    fn signs_and_verifies_with_the_public_key() {
        let signer = signer();
        let claims = GoogleAssertion::new(
            "sa@example.invalid",
            "https://www.googleapis.com/auth/drive.readonly",
            "https://oauth2.googleapis.com/token",
            1_700_000_000,
        );
        let token = signer.sign(&claims).unwrap();
        // header.claims.signature の 3 つ。パディングなしの URL 安全な Base64
        assert_eq!(token.split('.').count(), 3);
        assert!(!token.contains('=') && !token.contains('+') && !token.contains('/'));
        let got = verify(&token, signer.public_key_der()).unwrap();
        assert_eq!(
            got,
            json!({
                "iss": "sa@example.invalid",
                "scope": "https://www.googleapis.com/auth/drive.readonly",
                "aud": "https://oauth2.googleapis.com/token",
                "iat": 1_700_000_000u64,
                "exp": 1_700_003_600u64,
            })
        );
        // 署名は 2048 ビット = 256 バイト
        let sig = token.rsplit('.').next().unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(sig).unwrap().len(), 256);
        // RSASSA-PKCS1-v1_5 は決定的なので、同じ入力なら同じ JWT
        assert_eq!(token, signer.sign(&claims).unwrap());
    }

    #[test]
    fn header_is_rs256_jwt() {
        let token = signer().sign(&json!({"a": 1})).unwrap();
        let header = URL_SAFE_NO_PAD
            .decode(token.split('.').next().unwrap())
            .unwrap();
        assert_eq!(header, HEADER.as_bytes());
    }

    #[test]
    fn pkcs1_pem_is_accepted_and_gives_the_same_signature() {
        let a = signer().sign(&json!({"a": 1})).unwrap();
        let b = Rs256Signer::from_pem(&pem("RSA PRIVATE KEY", PKCS1_B64))
            .unwrap()
            .sign(&json!({"a": 1}))
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn crlf_and_surrounding_whitespace_in_pem_are_fine() {
        let text = format!(
            "\n  {}  \n",
            pem("PRIVATE KEY", PKCS8_B64).replace('\n', "\r\n")
        );
        assert!(Rs256Signer::from_pem(&text).is_ok());
    }

    #[test]
    fn tampering_is_detected() {
        let signer = signer();
        let token = signer.sign(&json!({"sub": "a"})).unwrap();
        let mut parts: Vec<&str> = token.split('.').collect();
        // claims を差し替える
        let forged = URL_SAFE_NO_PAD.encode(br#"{"sub":"b"}"#);
        parts[1] = &forged;
        assert_eq!(
            verify(&parts.join("."), signer.public_key_der()),
            Err(JwtError::Signature)
        );
        // 署名の 1 文字を変える
        let mut broken = token.clone();
        let last = broken.pop().unwrap();
        broken.push(if last == 'A' { 'B' } else { 'A' });
        assert!(verify(&broken, signer.public_key_der()).is_err());
        // 別の鍵では通らない（PKCS#8 の別鍵が無いので、公開鍵を壊して代用）
        let mut other = signer.public_key_der().to_vec();
        let n = other.len() - 8;
        other[n] ^= 0xff;
        assert!(verify(&token, &other).is_err());
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        let key = signer();
        let public = key.public_key_der();
        for bad in ["", "a.b", "a.b.c.d", "!!.!!.!!"] {
            assert_eq!(verify(bad, public), Err(JwtError::Format), "{bad:?}");
        }
        // alg が RS256 でない
        let none = format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode("{}"),
            URL_SAFE_NO_PAD.encode("x")
        );
        assert_eq!(verify(&none, public), Err(JwtError::Format));
    }

    #[test]
    fn bad_keys_are_rejected_without_leaking_the_input() {
        for bad in [
            "",
            "not a pem",
            "-----BEGIN PRIVATE KEY-----\n!!!!\n-----END PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----",
        ] {
            let e = Rs256Signer::from_pem(bad).unwrap_err();
            assert!(!e.to_string().contains("AAAA"));
        }
        // Base64 は正しいが RSA の鍵ではない
        let e = Rs256Signer::from_pem(&pem("PRIVATE KEY", "AAAAAAAA")).unwrap_err();
        assert_eq!(e, JwtError::Key);
        // Debug に鍵の中身を出さない
        assert_eq!(format!("{:?}", signer()), "Rs256Signer(..)");
    }
}
