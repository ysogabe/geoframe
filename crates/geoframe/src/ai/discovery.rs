//! local-model-lab の Discovery（P4 / T4.2。設計は p4-design.md §5「local-model-lab」）。
//!
//! 固定した契約（参照した版は `docs/rust-migration/ai-provider.md`）:
//!
//! - `GET /llm/catalog` … `{schema, revision, revised_at, base_url, docs, notes, services:[{id, title, description, url,
//!   local_url, models, apis:[{method, path, ...}], limits, available, reason}]}`。`apis[].path` は `/llm-<id>` の接頭辞を
//!   除いたパス。
//! - `GET /llm/services/<id>` … `{id, state, endpoint, available, reason, lease_count, generated_at}`。`state` は
//!   `running|starting|stopping|stopped|failed|unknown`。`available` は「running で、lease が 0 件で、lease を変更中でなく、
//!   収集が新しい」ときの true（上流が鮮度も判定する）。`lease_count` は lease を変更中のとき null。未知の id は 404、
//!   lock の競合は 503。
//!
//! # 読み方
//!
//! - **`available` を自分で算出し直さない**。送ってよいのは catalog の `available` と service の `available` が**両方
//!   true**で、かつ `state=running`・`lease_count=0` のときだけ（矛盾する応答は送らない）。
//! - `state`・`lease_count` は、**待ちの範囲**の判定にだけ使う（[`WaitScope`]）: `lease_count` が 0 でない（null を含む）
//!   なら**入口全体**（GPU の評価中に当たる。同じ入口の全サービスを止める根拠）、`state` が running でない・`available`
//!   が false なら**サービス単体**。
//! - `reason` は自由文なので読まず、保存も記録もしない。`url`・`local_url`・`endpoint` は**使わない**（値の比較もしない。
//!   送信先は設定の `local_base_url` に固定のパスを付けたものだけ）。
//! - 未知の `state`・必須項目の欠落・型の違い・id の不一致・catalog に無いサービス・必要な API の欠落は、固定の契約エラー
//!   （[`ContractError`]）。「まだ使えない」は一時的な待ち。

use serde_json::{Map, Value};

use super::http::{Deadline, Endpoint, ServiceId, Session};
use super::pause::{PauseReason, PauseScope};
use super::provider::{Phase, ProviderError};
use super::waits::{WaitReason, WaitScope};

/// 契約に合わない応答の種類（固定のコード。応答の中身は持たない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractError {
    /// catalog の形が違う（`services` が配列でない・必須項目の欠落・型の違い）。
    Catalog,
    /// catalog に期待するサービスが無い。
    ServiceMissing,
    /// catalog のサービスに必要な API（method＋path）が無い。
    ApiMissing,
    /// service の応答の形が違う（必須項目の欠落・型の違い）。
    ServiceDoc,
    /// service の応答の `id` が期待と違う。
    IdMismatch,
    /// 知らない `state`。
    UnknownState,
    /// health の形が違う。
    Health,
    /// health が ready なのに、必要な能力（画像）が無い。
    Capability,
    /// health のモデルが設定のモデルと違う。
    ModelMismatch,
}

impl ContractError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::ServiceMissing => "service_missing",
            Self::ApiMissing => "api_missing",
            Self::ServiceDoc => "service_doc",
            Self::IdMismatch => "id_mismatch",
            Self::UnknownState => "unknown_state",
            Self::Health => "health",
            Self::Capability => "capability",
            Self::ModelMismatch => "model_mismatch",
        }
    }
}

impl From<ContractError> for ProviderError {
    /// catalog そのものの不整合は Provider 全体、サービス固有の不整合は用途の停止。サービス／モデルが無いのは
    /// `model_not_found`。
    fn from(error: ContractError) -> Self {
        match error {
            ContractError::Catalog => Self::contract(true),
            ContractError::ServiceMissing | ContractError::ModelMismatch => {
                Self::Pause(PauseScope::Task, PauseReason::ModelNotFound)
            }
            _ => Self::contract(false),
        }
    }
}

/// 期待するサービスと、必要な API（method＋catalog 上の path）。
#[derive(Debug, Clone, Copy)]
pub struct Expect<'a> {
    pub service: &'a ServiceId,
    pub apis: &'a [(&'static str, &'static str)],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Starting,
    Stopping,
    Stopped,
    Failed,
    Unknown,
}

fn object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// catalog の応答を検査し、期待するサービスの `available` を返す。
pub fn parse_catalog(body: &[u8], expect: &Expect<'_>) -> Result<bool, ContractError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ContractError::Catalog)?;
    let services = object(&value)
        .and_then(|o| o.get("services"))
        .and_then(Value::as_array)
        .ok_or(ContractError::Catalog)?;
    let mut found = None;
    for service in services {
        let service = object(service).ok_or(ContractError::Catalog)?;
        let id = service
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ContractError::Catalog)?;
        if id == expect.service.as_str() && found.replace(service).is_some() {
            return Err(ContractError::Catalog);
        }
    }
    let service = found.ok_or(ContractError::ServiceMissing)?;
    let available = service
        .get("available")
        .and_then(Value::as_bool)
        .ok_or(ContractError::Catalog)?;
    let apis = service
        .get("apis")
        .and_then(Value::as_array)
        .ok_or(ContractError::Catalog)?;
    let mut listed = Vec::with_capacity(apis.len());
    for api in apis {
        let api = object(api).ok_or(ContractError::Catalog)?;
        let method = api
            .get("method")
            .and_then(Value::as_str)
            .ok_or(ContractError::Catalog)?;
        let path = api
            .get("path")
            .and_then(Value::as_str)
            .ok_or(ContractError::Catalog)?;
        listed.push((method, path));
    }
    for (method, path) in expect.apis {
        if !listed.iter().any(|(m, p)| m == method && p == path) {
            return Err(ContractError::ApiMissing);
        }
    }
    Ok(available)
}

/// service の状態（検査済み）。`reason`・`endpoint` は読まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStatus {
    state: State,
    available: bool,
    lease_count: Option<u32>,
}

/// service の応答を検査する。
pub fn parse_service(body: &[u8], expect: &Expect<'_>) -> Result<ServiceStatus, ContractError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ContractError::ServiceDoc)?;
    let doc = object(&value).ok_or(ContractError::ServiceDoc)?;
    let id = doc
        .get("id")
        .and_then(Value::as_str)
        .ok_or(ContractError::ServiceDoc)?;
    if id != expect.service.as_str() {
        return Err(ContractError::IdMismatch);
    }
    let state = match doc
        .get("state")
        .and_then(Value::as_str)
        .ok_or(ContractError::ServiceDoc)?
    {
        "running" => State::Running,
        "starting" => State::Starting,
        "stopping" => State::Stopping,
        "stopped" => State::Stopped,
        "failed" => State::Failed,
        "unknown" => State::Unknown,
        _ => return Err(ContractError::UnknownState),
    };
    let available = doc
        .get("available")
        .and_then(Value::as_bool)
        .ok_or(ContractError::ServiceDoc)?;
    // lease_count は必須のキー。lease を変更中は null
    let lease_count = match doc.get("lease_count").ok_or(ContractError::ServiceDoc)? {
        Value::Null => None,
        Value::Number(n) => Some(
            n.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(ContractError::ServiceDoc)?,
        ),
        _ => return Err(ContractError::ServiceDoc),
    };
    Ok(ServiceStatus {
        state,
        available,
        lease_count,
    })
}

/// 送ってよいか。送れないときは、待ちの範囲と理由。
pub fn judge(
    catalog_available: bool,
    status: &ServiceStatus,
) -> Result<(), (WaitScope, WaitReason)> {
    // lease があるか数えられない間は、GPU の評価中に当たる。同じ入口の全サービスを止める根拠
    if status.lease_count != Some(0) {
        return Err((WaitScope::Entrance, WaitReason::Leased));
    }
    // running でない（矛盾する `available=true` でも送らない）・上流が `available=false` と言っている
    if status.state != State::Running || !status.available || !catalog_available {
        return Err((WaitScope::Service, WaitReason::NotReady));
    }
    Ok(())
}

/// catalog と service を読み、使えるときだけ `Ok`。使えない・契約に合わないときは [`ProviderError`]。
/// Discovery 全体の期限（[`Deadline::discovery`]）を共有する。
pub fn check(
    session: &Session,
    expect: &Expect<'_>,
    deadline: &Deadline,
) -> Result<(), ProviderError> {
    let reply = session
        .get(Endpoint::Catalog, deadline)
        .map_err(|e| ProviderError::from_http(e, Phase::Probe))?;
    if !reply.is_success() {
        return Err(ProviderError::from_status(
            reply.status,
            reply.retry_after_secs,
            Phase::Probe,
            true,
        ));
    }
    let catalog_available = parse_catalog(&reply.body, expect)?;

    let reply = session
        .get(Endpoint::Service(expect.service), deadline)
        .map_err(|e| ProviderError::from_http(e, Phase::Probe))?;
    if !reply.is_success() {
        return Err(ProviderError::from_status(
            reply.status,
            reply.retry_after_secs,
            Phase::Probe,
            false,
        ));
    }
    let status = parse_service(&reply.body, expect)?;
    judge(catalog_available, &status).map_err(|(scope, reason)| ProviderError::wait(scope, reason))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(text: &str) -> ServiceId {
        ServiceId::parse(text).unwrap()
    }

    const APIS: &[(&str, &str)] = &[("POST", "/v1/systemone"), ("GET", "/health")];

    fn catalog(available: Value, extra_services: Vec<Value>) -> Vec<u8> {
        let mut services = vec![json!({
            "id": "svc", "title": "t", "description": "d",
            "url": "https://example.invalid/not-used", "local_url": "http://10.0.0.9:1/not-used",
            "models": [], "limits": [], "reason": "free text",
            "apis": [
                {"method": "POST", "path": "/v1/systemone", "model": "m"},
                {"method": "GET", "path": "/health"}
            ],
            "available": available
        })];
        services.extend(extra_services);
        serde_json::to_vec(&json!({"schema": 1, "revision": "r", "services": services})).unwrap()
    }

    fn service_doc(state: &str, available: Value, lease: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "id": "svc", "state": state, "endpoint": "127.0.0.1:1", "available": available,
            "reason": "x", "lease_count": lease, "generated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn catalog_requires_the_service_the_apis_and_a_boolean_available() {
        let sid = id("svc");
        let expect = Expect {
            service: &sid,
            apis: APIS,
        };
        assert_eq!(
            parse_catalog(&catalog(json!(true), vec![]), &expect),
            Ok(true)
        );
        assert_eq!(
            parse_catalog(&catalog(json!(false), vec![]), &expect),
            Ok(false)
        );
        // available が bool でない
        for bad in [json!("true"), json!(1), json!(null)] {
            assert_eq!(
                parse_catalog(&catalog(bad, vec![]), &expect),
                Err(ContractError::Catalog)
            );
        }
        // 別のサービスだけ
        let other = id("other");
        assert_eq!(
            parse_catalog(
                &catalog(json!(true), vec![]),
                &Expect {
                    service: &other,
                    apis: APIS
                }
            ),
            Err(ContractError::ServiceMissing)
        );
        // 必要な API が無い
        assert_eq!(
            parse_catalog(
                &catalog(json!(true), vec![]),
                &Expect {
                    service: &sid,
                    apis: &[("POST", "/v1/other")]
                }
            ),
            Err(ContractError::ApiMissing)
        );
        assert_eq!(
            parse_catalog(
                &catalog(json!(true), vec![]),
                &Expect {
                    service: &sid,
                    apis: &[("GET", "/v1/systemone")]
                }
            ),
            Err(ContractError::ApiMissing)
        );
        // 同じ id が 2 つ・services が配列でない・JSON でない
        assert_eq!(
            parse_catalog(
                &catalog(
                    json!(true),
                    vec![json!({"id": "svc", "available": true, "apis": []})]
                ),
                &expect
            ),
            Err(ContractError::Catalog)
        );
        for body in [&b"{}"[..], b"[]", b"not json", br#"{"services": {}}"#] {
            assert_eq!(parse_catalog(body, &expect), Err(ContractError::Catalog));
        }
    }

    #[test]
    fn service_doc_is_validated_strictly() {
        let sid = id("svc");
        let expect = Expect {
            service: &sid,
            apis: APIS,
        };
        let ok = parse_service(&service_doc("running", json!(true), json!(0)), &expect).unwrap();
        assert_eq!(judge(true, &ok), Ok(()));
        assert_eq!(
            parse_service(&service_doc("dancing", json!(true), json!(0)), &expect),
            Err(ContractError::UnknownState)
        );
        // lease_count は必須のキー。null は可（変更中）。負・小数・文字列は不可
        let mut missing: Value =
            serde_json::from_slice(&service_doc("running", json!(true), json!(0))).unwrap();
        missing.as_object_mut().unwrap().remove("lease_count");
        assert_eq!(
            parse_service(&serde_json::to_vec(&missing).unwrap(), &expect),
            Err(ContractError::ServiceDoc)
        );
        for bad in [json!(-1), json!(1.5), json!("0")] {
            assert_eq!(
                parse_service(&service_doc("running", json!(true), bad), &expect),
                Err(ContractError::ServiceDoc)
            );
        }
        assert_eq!(
            parse_service(&service_doc("running", json!("yes"), json!(0)), &expect),
            Err(ContractError::ServiceDoc)
        );
        let mut other: Value =
            serde_json::from_slice(&service_doc("running", json!(true), json!(0))).unwrap();
        other["id"] = json!("other");
        assert_eq!(
            parse_service(&serde_json::to_vec(&other).unwrap(), &expect),
            Err(ContractError::IdMismatch)
        );
    }

    /// `available` を自己流に上書きしない。lease が 0 でない・null なら入口全体、サービスの不調ならサービス単体。
    #[test]
    fn judge_respects_available_and_separates_entrance_from_service_scope() {
        let sid = id("svc");
        let expect = Expect {
            service: &sid,
            apis: APIS,
        };
        let status = |state, available, lease| {
            parse_service(&service_doc(state, available, lease), &expect).unwrap()
        };
        let entrance = Err((WaitScope::Entrance, WaitReason::Leased));
        let service = Err((WaitScope::Service, WaitReason::NotReady));
        assert_eq!(
            judge(true, &status("running", json!(true), json!(0))),
            Ok(())
        );
        // 入口全体: lease あり・lease 変更中（null）。available=true と言っていても送らない
        assert_eq!(
            judge(true, &status("running", json!(false), json!(2))),
            entrance
        );
        assert_eq!(
            judge(true, &status("running", json!(true), json!(1))),
            entrance
        );
        assert_eq!(
            judge(true, &status("running", json!(false), json!(null))),
            entrance
        );
        assert_eq!(
            judge(true, &status("running", json!(true), json!(null))),
            entrance
        );
        // サービス単体
        for state in ["starting", "stopping", "stopped", "failed", "unknown"] {
            assert_eq!(
                judge(true, &status(state, json!(false), json!(0))),
                service,
                "{state}"
            );
            // 矛盾（available=true だが running でない）も送らない
            assert_eq!(
                judge(true, &status(state, json!(true), json!(0))),
                service,
                "{state}"
            );
        }
        // 上流が available=false（収集が古いなど）と言うなら、running・lease 0 でも送らない
        assert_eq!(
            judge(true, &status("running", json!(false), json!(0))),
            service
        );
        // catalog 側が false なら送らない
        assert_eq!(
            judge(false, &status("running", json!(true), json!(0))),
            service
        );
    }

    #[test]
    fn contract_errors_map_to_pauses_with_the_right_scope() {
        assert_eq!(
            ProviderError::from(ContractError::Catalog),
            ProviderError::contract(true)
        );
        assert_eq!(
            ProviderError::from(ContractError::ServiceMissing),
            ProviderError::Pause(PauseScope::Task, PauseReason::ModelNotFound)
        );
        for e in [
            ContractError::ApiMissing,
            ContractError::ServiceDoc,
            ContractError::IdMismatch,
            ContractError::UnknownState,
            ContractError::Health,
            ContractError::Capability,
        ] {
            assert_eq!(
                ProviderError::from(e),
                ProviderError::contract(false),
                "{e:?}"
            );
        }
    }
}
