//! `GET /api/admin/config`（管理画面の設定タブ）。起動時に読んだ設定を、出どころつきで**読み取り専用**に返す。
//!
//! 仕様は `docs/admin-rework/design.md` の「`GET /api/admin/config`」と「安全面」。ユーザーの決定（2026-10-30）で、
//! 鍵のパス・接続先・連絡先・project を含む**全項目を設定どおりの値**で出す（ログ・`ai-jobs status`・`Config` の
//! `Debug` は今までどおり出さない）。出さないのは、`Config` が持たないもの（鍵・認証ファイルの**中身**、トークン）と
//! 内部の値（`embedding_contract`・`config_hash`）だけ。
//!
//! - `items` は [`EnvItem::ALL`] の全項目。値は [`value_of`] の**ワイルドカード無しの `match`** で、`Config` の型つきの
//!   値から項目ごとに文字列にする（`Config` を `Serialize` しない）。項目を足して枝を書き忘れるとコンパイルで落ちる。
//! - ファイルの項目（[`Exposure::File`]）は、値（パス）に加えて `state` を出す。有無は要求のたびに `fs::metadata` で
//!   見る（エラー文は出さない）。`GEOFRAME_AI_CONFIG` は起動時に読んだ結果（`loaded`/`invalid`/`absent`）と `modified_at`。
//! - `today_utc` は読み取り接続で `budget::usage` を呼ぶ。読めない（DB が無い・表が無い・エラー）ときは `null` で、
//!   応答は 200 のまま（設定の表示は DB の状態に依存しない）。

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Map, Value, json};

use crate::ai::budget;
use crate::ai::settings::{AiSettings, CONFIG_VERSION, ProviderKind, TaskKind};
use crate::config::{Config, EnvItem, Exposure, Source};
use crate::server::{AppState, error_response, json_response};
use crate::sysinfo::local_timestamp_at;

/// 用途ごとの今日（UTC 日）の送信数。`None` は読めなかった（`null` で出す）。
pub type TodayUtc = BTreeMap<&'static str, Option<u32>>;

/// ハンドラ。クラウドの用途がなければ DB は読まない。
pub async fn response(state: &AppState) -> Response {
    let today = today_utc(state).await;
    let body = build(&state.config, state.started_at, SystemTime::now(), &today);
    match serde_json::to_vec(&body) {
        Ok(bytes) => json_response(StatusCode::OK, bytes),
        Err(e) => {
            tracing::error!("設定を JSON にできません: {e}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "内部エラーが発生しました",
            )
        }
    }
}

/// クラウドの用途ごとの今日の送信数（`budget::usage` の `day`）。読み取り接続で 1 回だけ読み、用途ごとに失敗を
/// `None` にする（表が無い旧 DB なら全部 `None`）。DB が開けない・順番待ち超過のときも全部 `None`。
async fn today_utc(state: &AppState) -> TodayUtc {
    let cloud: Vec<(TaskKind, u32)> = TaskKind::ALL
        .into_iter()
        .filter(|task| {
            state
                .config
                .ai
                .settings()
                .and_then(|settings| settings.task(*task))
                .is_some_and(|t| t.provider == ProviderKind::Cloud)
        })
        .map(|task| (task, state.config.ai.legacy().cycle(task).1))
        .collect();
    if cloud.is_empty() {
        return TodayUtc::new();
    }
    let now = crate::ai::jobs::unix_now();
    let read = {
        let cloud = cloud.clone();
        state
            .catalog
            .read(move |conn| {
                Ok(cloud
                    .iter()
                    .map(|(task, cycle_secs)| {
                        let day = budget::usage(conn, *task, now, *cycle_secs)
                            .ok()
                            .map(|usage| usage.day);
                        (task.as_str(), day)
                    })
                    .collect::<Vec<_>>())
            })
            .await
    };
    match read {
        Ok(rows) => rows.into_iter().collect(),
        Err(e) => {
            tracing::debug!("今日の送信数を読めません: {e}");
            cloud
                .iter()
                .map(|(task, _)| (task.as_str(), None))
                .collect()
        }
    }
}

/// 応答の本体。`today` に無い用途は `null`。ファイルの有無はここで `fs::metadata` を呼んで見る。
pub fn build(config: &Config, started_at: SystemTime, now: SystemTime, today: &TodayUtc) -> Value {
    json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": local_timestamp_at(started_at),
        "uptime_s": now.duration_since(started_at).map_or(0, |d| d.as_secs()),
        "items": EnvItem::ALL.iter().map(|item| item_json(config, *item)).collect::<Vec<_>>(),
        "ai": ai_json(config, today),
    })
}

fn item_json(config: &Config, item: EnvItem) -> Value {
    let name = item.name();
    let source = config.sources.get(name).copied().unwrap_or(Source::Default);
    let mut out = Map::new();
    out.insert("key".into(), name.into());
    out.insert("value".into(), json!(value_of(config, item)));
    out.insert("source".into(), json!(source));
    match item.exposure() {
        Exposure::Value => {}
        Exposure::File => {
            out.insert("state".into(), file_state(config, item).into());
            if item == EnvItem::AiConfig
                && let Some(modified) = config.ai_config_modified
            {
                out.insert("modified_at".into(), local_timestamp_at(modified).into());
            }
        }
    }
    Value::Object(out)
}

/// ファイルの項目の状態。`GEOFRAME_AI_CONFIG` は起動時に読んだ結果、ほかは要求のたびの `fs::metadata`。
fn file_state(config: &Config, item: EnvItem) -> &'static str {
    match item {
        EnvItem::AiConfig => match (&config.ai_config_path, config.ai.settings()) {
            (None, _) => "absent",
            (Some(_), Some(_)) => "loaded",
            (Some(_), None) => "invalid",
        },
        EnvItem::DriveKey => path_state(&config.drive_key),
        EnvItem::RankModel => path_state(&config.rank_model_path),
        // ファイルの項目ではない（`Exposure::Value`）。ここへは来ない
        EnvItem::DataDir
        | EnvItem::Host
        | EnvItem::Port
        | EnvItem::AllowedHosts
        | EnvItem::Log
        | EnvItem::Blog
        | EnvItem::Ingest
        | EnvItem::DrivePollMin
        | EnvItem::PhotosDir
        | EnvItem::FetchPerCycle
        | EnvItem::QualityPerCycle
        | EnvItem::DriveFolder
        | EnvItem::BlogPlansDir
        | EnvItem::InferMin
        | EnvItem::InferOffsetMin
        | EnvItem::TileUa
        | EnvItem::TileCacheMaxMb
        | EnvItem::TileFetchPerMin
        | EnvItem::AiEnabled
        | EnvItem::AiPollS
        | EnvItem::AiBacklogPollS
        | EnvItem::AiEmbedPerCycle
        | EnvItem::AiJudgePerCycle
        | EnvItem::AiSearchPerMin
        | EnvItem::LocSuggest
        | EnvItem::LocSuggestCluePerCycle
        | EnvItem::LocSuggestGeminiPerCycle
        | EnvItem::EmbedBackend
        | EnvItem::AiTimeWindowMin
        | EnvItem::AiSimilarity
        | EnvItem::AiShortlistMax
        | EnvItem::AiVisualSimilarity => "absent",
    }
}

/// `present`（あるが中身は読まない）/ `absent`（無い）/ `unreadable`（見に行けない。権限など）。
fn path_state(path: &Path) -> &'static str {
    match std::fs::metadata(path) {
        Ok(_) => "present",
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent",
        Err(_) => "unreadable",
    }
}

fn flag(on: bool) -> String {
    String::from(if on { "1" } else { "0" })
}

/// 項目の値（設定どおり）。未設定で既定も無い項目は `None`。**ワイルドカード無し**: 項目を足すと、ここを書くまで
/// コンパイルが通らない。
fn value_of(config: &Config, item: EnvItem) -> Option<String> {
    let legacy = config.ai.legacy();
    let scene = config.ai.scene_options();
    let path = |p: &Path| Some(p.display().to_string());
    match item {
        EnvItem::DataDir => path(&config.data_dir),
        EnvItem::Host => Some(config.listen.ip().to_string()),
        EnvItem::Port => Some(config.listen.port().to_string()),
        EnvItem::AllowedHosts => Some(
            config
                .allowed_hosts
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        EnvItem::Log => Some(config.log_level.as_str().into()),
        EnvItem::Blog => Some(flag(config.blog)),
        EnvItem::Ingest => Some(flag(config.ingest)),
        EnvItem::DrivePollMin => Some(config.drive_poll_min.to_string()),
        EnvItem::PhotosDir => path(config.photos_dir_setting()),
        EnvItem::FetchPerCycle => Some(config.fetch_per_cycle.to_string()),
        EnvItem::QualityPerCycle => Some(config.quality_per_cycle.to_string()),
        EnvItem::DriveKey => path(&config.drive_key),
        EnvItem::DriveFolder => Some(config.drive_folder.clone()),
        EnvItem::BlogPlansDir => config.blog_plans_dir.as_deref().and_then(path),
        EnvItem::InferMin => Some(config.infer_min.to_string()),
        EnvItem::InferOffsetMin => Some(config.infer_offset_min.to_string()),
        EnvItem::TileUa => Some(config.tile_ua.clone()),
        EnvItem::TileCacheMaxMb => Some(config.tile_cache_max_mb.to_string()),
        EnvItem::TileFetchPerMin => Some(config.tile_fetch_per_min.to_string()),
        EnvItem::AiConfig => config.ai_config_path.as_deref().and_then(path),
        EnvItem::AiEnabled => Some(flag(legacy.enabled)),
        EnvItem::AiPollS => Some(legacy.poll_secs.to_string()),
        EnvItem::AiBacklogPollS => Some(legacy.backlog_poll_secs.to_string()),
        EnvItem::AiEmbedPerCycle => Some(legacy.embed_per_cycle.to_string()),
        EnvItem::AiJudgePerCycle => Some(legacy.judge_per_cycle.to_string()),
        EnvItem::AiSearchPerMin => Some(legacy.search_per_min.to_string()),
        EnvItem::LocSuggest => Some(flag(legacy.loc_suggest)),
        EnvItem::LocSuggestCluePerCycle => Some(legacy.clue_per_cycle.to_string()),
        EnvItem::LocSuggestGeminiPerCycle => Some(legacy.gemini_per_cycle.to_string()),
        EnvItem::EmbedBackend => Some(config.embed_backend.as_str().into()),
        // 窓は秒（切り捨て済み）で持つので、分に戻す（既定の 15 分は "15"）
        EnvItem::AiTimeWindowMin => Some((scene.window_s() as f64 / 60.0).to_string()),
        EnvItem::AiSimilarity => Some(scene.similarity().to_string()),
        EnvItem::AiShortlistMax => Some(scene.shortlist().to_string()),
        EnvItem::AiVisualSimilarity => Some(config.ai_visual_similarity.to_string()),
        EnvItem::RankModel => path(&config.rank_model_path),
    }
}

fn ai_json(config: &Config, today: &TodayUtc) -> Value {
    let settings = config.ai.settings();
    let tasks: Vec<Value> = settings
        .map(|settings| {
            TaskKind::ALL
                .into_iter()
                .filter_map(|task| task_json(config, settings, task, today))
                .collect()
        })
        .unwrap_or_default();
    let space = settings.and_then(AiSettings::embedding_space).map(|s| {
        json!({
            "space_id": s.space_id,
            "model_id": s.model_id,
            "revision": s.revision,
            "image_preprocess": s.image_preprocess,
            "text_preprocess": s.text_preprocess,
            "dimension": s.dimension,
            "normalization": s.normalization.as_str(),
        })
    });
    json!({
        "configured": config.ai_config_path.is_some(),
        "invalid": config.ai.invalid().map(|code| code.code()),
        "enabled": config.ai.legacy().enabled,
        "version": settings.map(|_| CONFIG_VERSION),
        "local_base_url": settings.and_then(AiSettings::local_base_url),
        "embedding_space": space,
        "tasks": tasks,
    })
}

/// 用途 1 つ。AI 設定 JSON に無い用途は出さない。`disabled` は `ops.rs` の `gate` と同じ計算（writer がある起動として
/// 評価する。`INGEST=0` の `no_writer` は状況タブの AI の欄が示す）。
fn task_json(
    config: &Config,
    settings: &AiSettings,
    task: TaskKind,
    today: &TodayUtc,
) -> Option<Value> {
    let t = settings.task(task)?;
    let disabled = config.ai.gate(task, true).err().map(|d| d.as_str());
    let mut out = Map::new();
    out.insert("task".into(), task.as_str().into());
    out.insert("provider".into(), t.provider.as_str().into());
    out.insert("model".into(), t.model.clone().into());
    match t.provider {
        ProviderKind::Local => {
            out.insert("service_id".into(), json!(t.service_id));
        }
        ProviderKind::Cloud => {
            out.insert("allow_cloud".into(), t.allow_cloud.into());
            out.insert("include_hidden".into(), t.include_hidden.into());
            out.insert("daily_limit".into(), json!(t.daily_request_limit));
            out.insert(
                "today_utc".into(),
                json!(today.get(task.as_str()).copied().flatten()),
            );
            if let Some(cloud) = &t.cloud {
                out.insert(
                    "credential_path".into(),
                    cloud.credential_path.display().to_string().into(),
                );
                out.insert(
                    "credential".into(),
                    path_state(&cloud.credential_path).into(),
                );
                out.insert("project".into(), cloud.project.clone().into());
                out.insert("region".into(), cloud.region.clone().into());
            }
        }
    }
    out.insert("source".into(), "ai_config".into());
    out.insert("disabled".into(), json!(disabled));
    Some(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;
    use crate::testutil::TempDir;
    use std::collections::HashMap;
    use std::time::Duration;

    /// 鍵の中身（応答のどこにも出てはいけない）。
    const SECRET: &str = "SECRET-mark";

    /// 検査に通る形の ASCII の目印を全項目に入れた、合成の設定（`Config::load` で作る）。鍵のパスが指す合成ファイルの
    /// 中身は [`SECRET`]。作業ディレクトリ名に目印を含めない（`data_dir` は起動ログの要約に載るため）。
    struct Marked {
        dir: TempDir,
        config: Config,
        /// 応答に値として含まれるべき目印（部分文字列）。
        marks: Vec<String>,
    }

    fn ai_json(dir: &Path, allow_cloud: bool) -> String {
        let key = dir.join("mark-key-1.json");
        format!(
            r#"{{
  "version": 1,
  "local_base_url": "http://mark-host.invalid:8100",
  "embedding_space": {{
    "space_id": "mark-space-id", "model_id": "mark-space-model", "revision": "mark-space-rev",
    "image_preprocess": "mark-space-img", "text_preprocess": "mark-space-txt",
    "dimension": 768, "normalization": "l2"
  }},
  "tasks": {{
    "image_embedding": {{"provider": "local", "model": "mark-model-embed", "service_id": "mark-svc"}},
    "query_embedding": {{"provider": "local", "model": "mark-model-embed", "service_id": "mark-svc"}},
    "photo_judge": {{"provider": "cloud", "model": "mark-model-judge", "allow_cloud": {allow_cloud},
      "daily_request_limit": 20, "credential_path": "{key}",
      "project": "mark-project", "region": "mark-region1"}},
    "location_clue": {{"provider": "local", "model": "mark-model-decision"}},
    "location_suggest": {{"provider": "cloud", "model": "mark-model-suggest", "allow_cloud": true,
      "daily_request_limit": 5, "include_hidden": true, "credential_path": "{key}",
      "project": "mark-project", "region": "mark-region1"}}
  }}
}}"#,
            key = key.display()
        )
    }

    fn marked_with(allow_cloud: bool, overrides: &Overrides) -> Marked {
        let dir = TempDir::new("cfgapi");
        let root = dir.path();
        std::fs::create_dir_all(root.join("mark-dir")).unwrap();
        let key1 = root.join("mark-key-1.json");
        let key2 = root.join("mark-key-2.json");
        std::fs::write(&key1, SECRET).unwrap();
        std::fs::write(&key2, SECRET).unwrap();
        let ai_path = root.join("mark-dir/ai.json");
        std::fs::write(&ai_path, ai_json(root, allow_cloud)).unwrap();
        let path = |p: &Path| p.display().to_string();
        let env: HashMap<String, String> = [
            ("GEOFRAME_DATA_DIR", path(root)),
            ("GEOFRAME_ALLOWED_HOSTS", "mark-host.example.test".into()),
            ("GEOFRAME_TILE_UA", "mark@example.test".into()),
            ("GEOFRAME_DRIVE_KEY", path(&key2)),
            ("GEOFRAME_DRIVE_FOLDER", "mark-folder".into()),
            (
                "GEOFRAME_RANK_MODEL",
                path(&root.join("mark-dir/rank.json")),
            ),
            (
                "GEOFRAME_BLOG_PLANS_DIR",
                path(&root.join("mark-dir/plans")),
            ),
            ("GEOFRAME_AI_CONFIG", path(&ai_path)),
            ("GEOFRAME_INGEST", "1".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let config = Config::load(|k| env.get(k).cloned(), overrides).unwrap();
        // 前提: AI 設定が ready で、用途が 5 つある
        let settings = config.ai.settings().expect("ready");
        assert_eq!(
            TaskKind::ALL
                .into_iter()
                .filter(|t| settings.task(*t).is_some())
                .count(),
            5
        );
        let marks = [
            path(&key1),
            path(&key2),
            "mark-key-1".into(),
            "mark-key-2".into(),
            "http://mark-host.invalid:8100".into(),
            "mark-space-id".into(),
            "mark-space-model".into(),
            "mark-space-rev".into(),
            "mark-space-img".into(),
            "mark-space-txt".into(),
            "mark-svc".into(),
            "mark@example.test".into(),
            "mark-project".into(),
            "mark-region1".into(),
            "mark-model-embed".into(),
            "mark-model-judge".into(),
            "mark-model-decision".into(),
            "mark-model-suggest".into(),
            "mark-folder".into(),
            path(&root.join("mark-dir/rank.json")),
            path(&root.join("mark-dir/plans")),
            path(&ai_path),
            "mark-host.example.test".into(),
        ]
        .into();
        Marked { dir, config, marks }
    }

    fn marked() -> Marked {
        marked_with(true, &Overrides::default())
    }

    fn body(config: &Config) -> Value {
        let started = SystemTime::now() - Duration::from_secs(100);
        build(config, started, SystemTime::now(), &TodayUtc::new())
    }

    fn item<'a>(body: &'a Value, key: &str) -> &'a Value {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == key)
            .unwrap_or_else(|| panic!("{key} が無い"))
    }

    fn task<'a>(body: &'a Value, name: &str) -> &'a Value {
        body["ai"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["task"] == name)
            .unwrap_or_else(|| panic!("{name} が無い"))
    }

    #[test]
    fn the_response_carries_every_item_and_every_marker_as_a_value_but_never_a_key_body() {
        let m = marked();
        let body = body(&m.config);
        // items は EnvItem::ALL の全項目を、その順で
        let keys: Vec<&str> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        let expected: Vec<&str> = EnvItem::ALL.iter().map(|i| i.name()).collect();
        assert_eq!(keys, expected);
        assert!(
            body["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["source"].is_string())
        );
        // 目印は全部、値として含まれる。鍵の中身は無い
        let text = serde_json::to_string(&body).unwrap();
        for mark in &m.marks {
            assert!(text.contains(mark.as_str()), "{mark} が無い: {text}");
        }
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains("embedding_contract") && !text.contains("config_hash"));
        // 目印は、その項目・用途の値として入っている（どこかに偶然あるのではない）
        let root = m.dir.path();
        assert_eq!(
            item(&body, "GEOFRAME_TILE_UA")["value"],
            "mark@example.test"
        );
        assert_eq!(item(&body, "GEOFRAME_DRIVE_FOLDER")["value"], "mark-folder");
        assert_eq!(
            item(&body, "GEOFRAME_ALLOWED_HOSTS")["value"],
            "localhost, mark-host.example.test"
        );
        assert_eq!(
            item(&body, "GEOFRAME_DRIVE_KEY")["value"],
            root.join("mark-key-2.json").display().to_string()
        );
        assert_eq!(
            item(&body, "GEOFRAME_BLOG_PLANS_DIR")["value"],
            root.join("mark-dir/plans").display().to_string()
        );
        let ai = &body["ai"];
        assert_eq!(ai["local_base_url"], "http://mark-host.invalid:8100");
        assert_eq!(ai["embedding_space"]["space_id"], "mark-space-id");
        assert_eq!(ai["embedding_space"]["model_id"], "mark-space-model");
        assert_eq!(ai["embedding_space"]["revision"], "mark-space-rev");
        assert_eq!(ai["embedding_space"]["image_preprocess"], "mark-space-img");
        assert_eq!(ai["embedding_space"]["text_preprocess"], "mark-space-txt");
        assert_eq!(ai["embedding_space"]["dimension"], 768);
        assert_eq!(ai["embedding_space"]["normalization"], "l2");
        assert_eq!(ai["tasks"].as_array().unwrap().len(), 5);
        let judge = task(&body, "photo_judge");
        assert_eq!(judge["provider"], "cloud");
        assert_eq!(judge["model"], "mark-model-judge");
        assert_eq!(judge["project"], "mark-project");
        assert_eq!(judge["region"], "mark-region1");
        assert_eq!(
            judge["credential_path"],
            root.join("mark-key-1.json").display().to_string()
        );
        assert_eq!(judge["credential"], "present");
        assert_eq!(judge["source"], "ai_config");
        assert_eq!(task(&body, "image_embedding")["service_id"], "mark-svc");
        assert_eq!(task(&body, "location_clue")["service_id"], Value::Null);
    }

    #[test]
    fn debug_and_log_summary_stay_free_of_markers_model_names_and_regions() {
        let m = marked();
        for text in [format!("{:?}", m.config), m.config.log_summary()] {
            for needle in [SECRET, "mark-", "mark@", "mark-model", "mark-region1"] {
                assert!(!text.contains(needle), "{needle}: {text}");
            }
            assert!(text.contains("ai=ready:tasks=5"), "{text}");
        }
    }

    #[test]
    fn files_are_reported_by_state_and_ai_config_by_what_was_loaded() {
        let m = marked();
        let body = body(&m.config);
        assert_eq!(item(&body, "GEOFRAME_DRIVE_KEY")["state"], "present");
        // 順位モデルは置いていない
        assert_eq!(item(&body, "GEOFRAME_RANK_MODEL")["state"], "absent");
        let ai = item(&body, "GEOFRAME_AI_CONFIG");
        assert_eq!(ai["state"], "loaded");
        assert_eq!(ai["source"], "env");
        assert_eq!(ai["modified_at"].as_str().unwrap().len(), 19);
        // 値の項目には state を付けない
        assert!(item(&body, "GEOFRAME_INGEST").get("state").is_none());
        // 鍵が消えれば、要求のたびに absent になる
        std::fs::remove_file(m.dir.path().join("mark-key-2.json")).unwrap();
        std::fs::remove_file(m.dir.path().join("mark-key-1.json")).unwrap();
        let body = super::tests::body(&m.config);
        assert_eq!(item(&body, "GEOFRAME_DRIVE_KEY")["state"], "absent");
        assert_eq!(task(&body, "photo_judge")["credential"], "absent");
        // 値（パス）は変わらない
        assert!(
            item(&body, "GEOFRAME_DRIVE_KEY")["value"]
                .as_str()
                .unwrap()
                .ends_with("mark-key-2.json")
        );
    }

    #[test]
    fn sources_follow_the_rules() {
        let m = marked();
        let body = body(&m.config);
        for key in [
            "GEOFRAME_DATA_DIR",
            "GEOFRAME_ALLOWED_HOSTS",
            "GEOFRAME_TILE_UA",
            "GEOFRAME_DRIVE_KEY",
            "GEOFRAME_DRIVE_FOLDER",
            "GEOFRAME_RANK_MODEL",
            "GEOFRAME_BLOG_PLANS_DIR",
            "GEOFRAME_AI_CONFIG",
            "GEOFRAME_INGEST",
        ] {
            assert_eq!(item(&body, key)["source"], "env", "{key}");
        }
        for key in [
            "GEOFRAME_HOST",
            "GEOFRAME_PORT",
            "GEOFRAME_LOG",
            "GEOFRAME_BLOG",
        ] {
            assert_eq!(item(&body, key)["source"], "default", "{key}");
        }
        // 起動引数は arg（環境変数があっても）
        let overrides = Overrides {
            listen: Some("192.0.2.7:9000".parse().unwrap()),
            data_dir: Some(m.dir.path().to_path_buf()),
        };
        let args = marked_with(true, &overrides);
        let body = self::body(&args.config);
        for key in ["GEOFRAME_HOST", "GEOFRAME_PORT", "GEOFRAME_DATA_DIR"] {
            assert_eq!(item(&body, key)["source"], "arg", "{key}");
        }
        assert_eq!(item(&body, "GEOFRAME_HOST")["value"], "192.0.2.7");
        assert_eq!(item(&body, "GEOFRAME_PORT")["value"], "9000");
    }

    #[test]
    fn defaults_are_shown_as_values_and_unset_optional_items_as_null() {
        let config = Config::load(|_| None, &Overrides::default()).unwrap();
        let body = body(&config);
        assert_eq!(item(&body, "GEOFRAME_BLOG_PLANS_DIR")["value"], Value::Null);
        assert_eq!(item(&body, "GEOFRAME_AI_CONFIG")["value"], Value::Null);
        assert_eq!(item(&body, "GEOFRAME_AI_CONFIG")["state"], "absent");
        assert!(
            item(&body, "GEOFRAME_AI_CONFIG")
                .get("modified_at")
                .is_none()
        );
        let expected = [
            ("GEOFRAME_HOST", "0.0.0.0"),
            ("GEOFRAME_PORT", "8080"),
            ("GEOFRAME_ALLOWED_HOSTS", "localhost"),
            ("GEOFRAME_LOG", "info"),
            ("GEOFRAME_INGEST", "0"),
            ("GEOFRAME_BLOG", "0"),
            ("GEOFRAME_DRIVE_POLL_MIN", "10"),
            ("GEOFRAME_PHOTOS_DIR", "photos"),
            ("GEOFRAME_FETCH_PER_CYCLE", "25"),
            ("GEOFRAME_QUALITY_PER_CYCLE", "25"),
            ("GEOFRAME_DRIVE_FOLDER", "GeoFrame"),
            ("GEOFRAME_INFER_MIN", "15"),
            ("GEOFRAME_INFER_OFFSET_MIN", "0"),
            ("GEOFRAME_TILE_UA", "GeoFrame/0.1 (personal photo frame)"),
            ("GEOFRAME_TILE_CACHE_MAX_MB", "16384"),
            ("GEOFRAME_TILE_FETCH_PER_MIN", "240"),
            ("GEOFRAME_AI_ENABLED", "1"),
            ("GEOFRAME_AI_POLL_S", "60"),
            ("GEOFRAME_AI_BACKLOG_POLL_S", "5"),
            ("GEOFRAME_AI_EMBED_PER_CYCLE", "12"),
            ("GEOFRAME_AI_JUDGE_PER_CYCLE", "2"),
            ("GEOFRAME_AI_SEARCH_PER_MIN", "30"),
            ("GEOFRAME_LOC_SUGGEST", "1"),
            ("GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE", "10"),
            ("GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE", "2"),
            ("GEOFRAME_EMBED_BACKEND", "gemini"),
            ("GEOFRAME_AI_TIME_WINDOW_MIN", "15"),
            ("GEOFRAME_AI_SIMILARITY", "0.82"),
            ("GEOFRAME_AI_SHORTLIST_MAX", "6"),
            ("GEOFRAME_AI_VISUAL_SIMILARITY", "0.9"),
        ];
        for (key, value) in expected {
            let got = item(&body, key);
            assert_eq!(got["value"], value, "{key}");
            assert_eq!(got["source"], "default", "{key}");
        }
        // 既定でパスを持つファイルの項目: 値は絶対パス、置いていないので absent
        assert!(
            item(&body, "GEOFRAME_DRIVE_KEY")["value"]
                .as_str()
                .unwrap()
                .ends_with("drive_key.json")
        );
        assert!(
            item(&body, "GEOFRAME_RANK_MODEL")["value"]
                .as_str()
                .unwrap()
                .ends_with("models/rank_model.json")
        );
        // AI 設定が無い: 用途は空で、版・接続先・空間は null
        let ai = &body["ai"];
        assert_eq!(ai["configured"], false);
        assert_eq!(ai["invalid"], Value::Null);
        assert_eq!(ai["enabled"], true);
        assert_eq!(ai["version"], Value::Null);
        assert_eq!(ai["local_base_url"], Value::Null);
        assert_eq!(ai["embedding_space"], Value::Null);
        assert_eq!(ai["tasks"], json!([]));
    }

    #[test]
    fn an_invalid_ai_config_is_reported_by_code_with_no_tasks() {
        let dir = TempDir::new("cfgapi");
        let path = dir.path().join("ai.json");
        std::fs::write(&path, "{ not json").unwrap();
        let env: HashMap<String, String> =
            [("GEOFRAME_AI_CONFIG".to_string(), path.display().to_string())].into();
        let config = Config::load(|k| env.get(k).cloned(), &Overrides::default()).unwrap();
        let body = body(&config);
        assert_eq!(body["ai"]["configured"], true);
        assert_eq!(body["ai"]["invalid"], "syntax");
        assert_eq!(body["ai"]["tasks"], json!([]));
        let ai = item(&body, "GEOFRAME_AI_CONFIG");
        assert_eq!(ai["state"], "invalid");
        assert_eq!(ai["value"], path.display().to_string());
        assert!(ai["modified_at"].is_string());
        // 旧設定の環境変数の誤りは legacy_env
        let env: HashMap<String, String> =
            [("GEOFRAME_AI_POLL_S".to_string(), "x".to_string())].into();
        let config = Config::load(|k| env.get(k).cloned(), &Overrides::default()).unwrap();
        let body = self::body(&config);
        assert_eq!(body["ai"]["invalid"], "legacy_env");
        assert_eq!(body["ai"]["configured"], false);
    }

    #[test]
    fn tasks_report_the_gate_reason_and_todays_count() {
        let m = marked();
        let started = SystemTime::now() - Duration::from_secs(5);
        let mut today = TodayUtc::new();
        today.insert("photo_judge", Some(12));
        today.insert("location_suggest", None);
        let body = build(&m.config, started, SystemTime::now(), &today);
        let judge = task(&body, "photo_judge");
        assert_eq!(judge["daily_limit"], 20);
        assert_eq!(judge["today_utc"], 12);
        assert_eq!(judge["allow_cloud"], true);
        assert_eq!(judge["include_hidden"], false);
        assert_eq!(judge["disabled"], Value::Null);
        let suggest = task(&body, "location_suggest");
        assert_eq!(suggest["today_utc"], Value::Null);
        assert_eq!(suggest["include_hidden"], true);
        // ローカルの用途に送信数は無い
        assert!(task(&body, "image_embedding").get("today_utc").is_none());
        assert_eq!(task(&body, "image_embedding")["disabled"], Value::Null);
        // クラウド許可が無ければ、固定のコードで理由が出る
        let off = marked_with(false, &Overrides::default());
        let body = self::body(&off.config);
        assert_eq!(task(&body, "photo_judge")["disabled"], "cloud_not_allowed");
        assert_eq!(task(&body, "photo_judge")["allow_cloud"], false);
        assert_eq!(task(&body, "location_suggest")["disabled"], Value::Null);
    }

    #[test]
    fn version_start_time_and_uptime() {
        let config = Config::load(|_| None, &Overrides::default()).unwrap();
        let now = SystemTime::now();
        let body = build(
            &config,
            now - Duration::from_secs(47520),
            now,
            &TodayUtc::new(),
        );
        assert_eq!(body["ok"], true);
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["uptime_s"], 47520);
        assert_eq!(body["started_at"].as_str().unwrap().len(), 19);
        // 起動時刻が未来（時計が戻った）でも 0
        let body = build(
            &config,
            now + Duration::from_secs(60),
            now,
            &TodayUtc::new(),
        );
        assert_eq!(body["uptime_s"], 0);
    }
}
