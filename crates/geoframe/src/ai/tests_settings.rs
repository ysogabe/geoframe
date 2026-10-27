//! AI 設定（JSON v1・旧環境変数・送信の可否・ハッシュ）の試験。合成の値・予約アドレス・ループバックだけで、
//! 通信・認証ファイルの読み取りはしない。

use std::collections::HashMap;

use super::pause::PauseScope;
use super::settings::*;
use crate::config::{Config, Overrides};
use crate::testutil::TempDir;

/// 全ての用途と `embedding_space` を持つ、合成の設定（例は `docs/rust-migration/ai-config.md` と同じ形）。
pub(super) const SAMPLE: &str = r#"{
  "version": 1,
  "local_base_url": "http://192.0.2.10:8100",
  "embedding_space": {
    "space_id": "example-space-1",
    "model_id": "example-embed",
    "revision": "r1",
    "image_preprocess": "img-v1",
    "text_preprocess": "txt-v1",
    "dimension": 768,
    "normalization": "l2"
  },
  "tasks": {
    "image_embedding": {"provider": "local", "model": "example-embed", "service_id": "example-embed-svc"},
    "query_embedding": {"provider": "local", "model": "example-embed", "service_id": "example-embed-svc"},
    "photo_judge": {"provider": "cloud", "model": "example-judge", "allow_cloud": true,
      "daily_request_limit": 20, "credential_path": "/etc/example/credentials.json",
      "project": "example-project", "region": "example-region1"},
    "location_clue": {"provider": "local", "model": "example-decision"},
    "location_suggest": {"provider": "cloud", "model": "example-suggest",
      "credential_path": "/etc/example/credentials.json",
      "project": "example-project", "region": "example-region1"}
  }
}"#;

pub(super) fn parse(json: &str) -> Result<AiSettings, AiConfigError> {
    AiSettings::parse(json.as_bytes())
}

pub(super) fn sample() -> AiSettings {
    parse(SAMPLE).unwrap()
}

/// `SAMPLE` の一部を置き換えた設定（置き換え元が無ければ試験の誤り）。
pub(super) fn variant(from: &str, to: &str) -> String {
    assert!(SAMPLE.contains(from), "{from}");
    SAMPLE.replacen(from, to, 1)
}

fn env_config(env: &[(&str, &str)]) -> AiConfig {
    let map: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    AiConfig::load(&|k| map.get(k).cloned().filter(|v| !v.trim().is_empty()))
}

/// 設定ファイルを置いて `AiConfig` を作る（`extra` は旧環境変数など）。
fn file_config(json: &str, extra: &[(&str, &str)]) -> (AiConfig, TempDir) {
    let dir = TempDir::new("ai-settings");
    let path = dir.path().join("ai.json");
    std::fs::write(&path, json).unwrap();
    let mut env = vec![("GEOFRAME_AI_CONFIG", path.to_str().unwrap())];
    env.extend_from_slice(extra);
    (env_config(&env), dir)
}

fn ready(extra: &[(&str, &str)]) -> (AiConfig, TempDir) {
    let (config, dir) = file_config(SAMPLE, extra);
    assert_eq!(config.invalid(), None);
    (config, dir)
}

// ---------------------------------------------------------------- 構造の検査

#[test]
fn the_documented_example_parses_and_normalizes() {
    let s = sample();
    assert_eq!(s.local_base_url(), Some("http://192.0.2.10:8100"));
    let space = s.embedding_space().unwrap();
    assert_eq!(
        (space.dimension, space.normalization),
        (768, Normalization::L2)
    );
    let judge = s.task(TaskKind::PhotoJudge).unwrap();
    assert_eq!(judge.provider, ProviderKind::Cloud);
    assert!(judge.allow_cloud && !judge.include_hidden);
    assert_eq!(judge.daily_request_limit, Some(20));
    // 既定: allow_cloud=false・daily 未設定・include_hidden=false
    let suggest = s.task(TaskKind::LocationSuggest).unwrap();
    assert!(!suggest.allow_cloud && !suggest.include_hidden);
    assert_eq!(suggest.daily_request_limit, None);
    // LAN の Decision は service_id を持たない
    assert_eq!(s.task(TaskKind::LocationClue).unwrap().service_id, None);
    assert_eq!(s.config_hash().len(), 64);
}

#[test]
fn the_smallest_valid_config_has_no_tasks() {
    let s = parse(r#"{"version":1}"#).unwrap();
    assert!(TaskKind::ALL.iter().all(|t| s.task(*t).is_none()));
    assert_eq!(s.local_base_url(), None);
}

#[test]
fn unknown_keys_are_rejected_at_every_level() {
    // 秘密に見えるキーも未知のキー（秘密を埋め込ませない）
    let cases = [
        (r#"{"version":1,"extra":1}"#.to_string(), "top"),
        (
            r#"{"version":1,"tasks":{"geocode":{"provider":"local","model":"m"}}}"#.into(),
            "tasks",
        ),
        (
            variant(
                r#""model": "example-judge","#,
                r#""model": "example-judge", "api_key": "x","#,
            ),
            "task",
        ),
        (
            variant(
                r#""dimension": 768,"#,
                r#""dimension": 768, "pooling": "mean","#,
            ),
            "space",
        ),
    ];
    for (json, level) in cases {
        assert_eq!(
            parse(&json).unwrap_err(),
            AiConfigError::UnknownKey,
            "{level}"
        );
    }
}

#[test]
fn duplicate_keys_are_rejected_at_every_level() {
    let task = r#"{"provider":"local","model":"m","service_id":"s"}"#;
    let cases = [
        (r#"{"version":1,"version":1}"#.to_string(), "top"),
        (
            r#"{"version":1,"local_base_url":"http://192.0.2.10","local_base_url":"http://192.0.2.11"}"#.into(),
            "top-url",
        ),
        (r#"{"version":1,"tasks":{},"tasks":{}}"#.to_string(), "tasks-object"),
        (
            format!(
                r#"{{"version":1,"local_base_url":"http://192.0.2.10","tasks":{{"photo_judge":{task},"photo_judge":{task}}}}}"#
            ),
            "task-name",
        ),
        (
            r#"{"version":1,"local_base_url":"http://192.0.2.10","tasks":{"image_embedding":{"provider":"local","provider":"cloud","model":"m","service_id":"s"}}}"#
                .into(),
            "task-field",
        ),
        (variant(r#""dimension": 768,"#, r#""dimension": 768, "dimension": 4096,"#), "space"),
    ];
    for (json, level) in cases {
        assert_eq!(
            parse(&json).unwrap_err(),
            AiConfigError::DuplicateKey,
            "{level}"
        );
    }
}

#[test]
fn required_keys_are_enforced() {
    let cases = [
        (r#"{}"#.to_string(), "version"),
        (r#"{"version":1,"tasks":{"photo_judge":{"model":"m"}}}"#.into(), "provider"),
        (r#"{"version":1,"local_base_url":"http://192.0.2.10","tasks":{"image_embedding":{"provider":"local"}}}"#.into(), "model"),
        // LAN の用途には入口の URL が要る
        (r#"{"version":1,"tasks":{"location_clue":{"provider":"local","model":"m"}}}"#.into(), "base-url"),
        // local の画像 Embedding には service_id が要る（Decision だけは要らない）
        (r#"{"version":1,"local_base_url":"http://192.0.2.10","tasks":{"image_embedding":{"provider":"local","model":"m"}}}"#.into(), "service_id"),
        (variant(r#""credential_path": "/etc/example/credentials.json",
      "project": "example-project", "region": "example-region1"},
    "location_clue""#, r#""project": "example-project", "region": "example-region1"},
    "location_clue""#), "credential_path"),
        (variant(r#""revision": "r1","#, ""), "space"),
    ];
    for (json, what) in cases {
        assert_eq!(
            parse(&json).unwrap_err(),
            AiConfigError::MissingKey,
            "{what}"
        );
    }
    assert_eq!(
        parse(r#"{"version":2}"#).unwrap_err(),
        AiConfigError::UnsupportedVersion
    );
}

#[test]
fn bad_values_and_conflicts_are_rejected_with_fixed_codes() {
    let bad_value = [
        r#"{"version":"1"}"#.to_string(),
        r#"[]"#.into(),
        r#"null"#.into(),
        r#"{"version":1,"tasks":null}"#.into(),
        variant(r#""dimension": 768"#, r#""dimension": 0"#),
        variant(r#""dimension": 768"#, r#""dimension": 4097"#),
        variant(r#""dimension": 768"#, r#""dimension": 7.5"#),
        variant(r#""normalization": "l2""#, r#""normalization": "l1""#),
        variant(
            r#""space_id": "example-space-1""#,
            r#""space_id": "bad id""#,
        ),
        variant(
            r#""provider": "cloud", "model": "example-judge""#,
            r#""provider": "gpu", "model": "example-judge""#,
        ),
        variant(r#""model": "example-judge""#, r#""model": "a/b""#),
        variant(r#""model": "example-judge""#, r#""model": """#),
        variant(
            r#""daily_request_limit": 20"#,
            r#""daily_request_limit": -1"#,
        ),
        variant(
            r#""daily_request_limit": 20"#,
            r#""daily_request_limit": 1000001"#,
        ),
        variant(r#""allow_cloud": true"#, r#""allow_cloud": "yes""#),
        variant(
            r#"/etc/example/credentials.json"#,
            r#"relative/credentials.json"#,
        ),
        variant(
            r#""project": "example-project""#,
            r#""project": "Example-Project""#,
        ),
        variant(
            r#""region": "example-region1""#,
            r#""region": "example.region""#,
        ),
        variant(
            r#""local_base_url": "http://192.0.2.10:8100""#,
            r#""local_base_url": "http://192.0.2.10:8100/x""#,
        ),
    ];
    for json in &bad_value {
        assert_eq!(parse(json).unwrap_err(), AiConfigError::BadValue, "{json}");
    }
    let conflicts = [
        // LAN の用途にクラウドの項目
        r#"{"version":1,"local_base_url":"http://192.0.2.10","tasks":{"image_embedding":{"provider":"local","model":"m","service_id":"s","allow_cloud":false}}}"#.to_string(),
        r#"{"version":1,"local_base_url":"http://192.0.2.10","tasks":{"image_embedding":{"provider":"local","model":"m","service_id":"s","daily_request_limit":5}}}"#.into(),
        r#"{"version":1,"local_base_url":"http://192.0.2.10","tasks":{"image_embedding":{"provider":"local","model":"m","service_id":"s","project":"p"}}}"#.into(),
        // location_clue は LAN の Decision に限る
        variant(r#""location_clue": {"provider": "local", "model": "example-decision"}"#,
            r#""location_clue": {"provider": "cloud", "model": "example-decision", "allow_cloud": true, "daily_request_limit": 5, "credential_path": "/etc/example/credentials.json", "project": "example-project", "region": "example-region1"}"#),
        variant(r#""location_clue": {"provider": "local", "model": "example-decision"}"#,
            r#""location_clue": {"provider": "local", "model": "example-decision", "service_id": "s"}"#),
        // クラウドに service_id
        variant(r#""model": "example-judge","#, r#""model": "example-judge", "service_id": "s","#),
    ];
    for json in &conflicts {
        assert_eq!(parse(json).unwrap_err(), AiConfigError::Conflict, "{json}");
    }
    // 構文・UTF-8・末尾のゴミ
    for body in [
        &b""[..],
        b"{",
        b"not json",
        b"{\"version\":1} x",
        b"{\"version\":1,\"local_base_url\":\"\xff\"}",
    ] {
        assert_eq!(AiSettings::parse(body).unwrap_err(), AiConfigError::Syntax);
    }
}

#[test]
fn error_codes_never_carry_the_offending_values() {
    for json in [
        r#"{"version":1,"local_base_url":"http://user:topsecret@192.0.2.10"}"#,
        r#"{"version":1,"topsecret":1}"#,
        r#"{"version":"topsecret"}"#,
        r#"{"version":1,"tasks":{"photo_judge":{"provider":"topsecret","model":"m"}}}"#,
    ] {
        let err = parse(json).unwrap_err();
        let text = format!("{err} {err:?} {}", err.code());
        assert!(!text.contains("topsecret"), "{text}");
    }
}

#[test]
fn base_urls_are_checked_as_strings_without_resolving_anything() {
    let ok = [
        ("http://192.0.2.10", "http://192.0.2.10"),
        ("http://192.0.2.10:8100", "http://192.0.2.10:8100"),
        ("https://lab.example.test", "https://lab.example.test"),
        ("http://LAB.Example.TEST:80/", "http://lab.example.test:80"),
        ("http://frame", "http://frame"),
        ("http://localhost:8080/", "http://localhost:8080"),
        ("http://[2001:db8::1]:8100", "http://[2001:db8::1]:8100"),
        ("http://[::1]", "http://[::1]"),
        ("http://203.0.113.5", "http://203.0.113.5"),
    ];
    for (raw, expected) in ok {
        let json = format!(r#"{{"version":1,"local_base_url":"{raw}"}}"#);
        assert_eq!(
            parse(&json).unwrap().local_base_url(),
            Some(expected),
            "{raw}"
        );
    }
    let bad = [
        "ftp://192.0.2.10",
        "file:///etc/passwd",
        "HTTP://192.0.2.10",
        "//192.0.2.10",
        "192.0.2.10",
        "http://",
        "http:///",
        "http://user@192.0.2.10",
        "http://user:pw@192.0.2.10",
        "http://192.0.2.10?x=1",
        "http://192.0.2.10#frag",
        "http://192.0.2.10/path",
        "http://192.0.2.10:0",
        "http://192.0.2.10:65536",
        "http://192.0.2.10:",
        "http://192.0.2.10:80a",
        "http://192.0.2.10:8100:9",
        "http://host name",
        "http://host%41",
        "http://-bad.example.test",
        "http://bad-.example.test",
        "http://a..b",
        "http://192.0.2.10.5",
        "http://999.1.1.1",
        "http://[::1",
        "http://[::1]x",
        "http://::1",
        "http://[not-an-ip]",
        "http://日本.example.test",
        "http://host\\evil",
    ];
    for raw in bad {
        let json = format!(
            r#"{{"version":1,"local_base_url":{}}}"#,
            serde_json::to_string(raw).unwrap()
        );
        assert_eq!(parse(&json).unwrap_err(), AiConfigError::BadValue, "{raw}");
    }
    // 制御文字（JSON のエスケープ経由）と長すぎる値
    for raw in ["http://192.0.2.10\n", "http://192.0.2.10\u{0}"] {
        let json = format!(
            r#"{{"version":1,"local_base_url":{}}}"#,
            serde_json::to_string(raw).unwrap()
        );
        assert_eq!(parse(&json).unwrap_err(), AiConfigError::BadValue);
    }
    let long = format!(
        r#"{{"version":1,"local_base_url":"http://{}.example.test"}}"#,
        "a".repeat(250)
    );
    assert_eq!(parse(&long).unwrap_err(), AiConfigError::BadValue);
}

// ---------------------------------------------------------------- ファイル

#[test]
fn files_must_be_regular_utf8_json_up_to_64_kib() {
    let dir = TempDir::new("ai-files");
    let p = |name: &str| dir.path().join(name);

    assert_eq!(
        AiSettings::load_file(&p("missing.json")).unwrap_err(),
        AiConfigError::Unreadable
    );
    std::fs::create_dir(p("a-directory")).unwrap();
    assert_eq!(
        AiSettings::load_file(&p("a-directory")).unwrap_err(),
        AiConfigError::NotRegularFile
    );
    // FIFO は開いて止まらず、通常のファイルでないものとして断る
    let fifo = p("a-fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        AiSettings::load_file(&fifo).unwrap_err(),
        AiConfigError::NotRegularFile
    );

    // ちょうど 64KiB は読める。1 バイト超えると、中身を見ずに断る
    let mut exact = String::from(r#"{"version":1}"#);
    exact.push_str(&" ".repeat(MAX_CONFIG_BYTES as usize - exact.len()));
    assert_eq!(exact.len() as u64, MAX_CONFIG_BYTES);
    std::fs::write(p("exact.json"), &exact).unwrap();
    assert!(AiSettings::load_file(&p("exact.json")).is_ok());
    std::fs::write(p("over.json"), format!("{exact} ")).unwrap();
    assert_eq!(
        AiSettings::load_file(&p("over.json")).unwrap_err(),
        AiConfigError::TooLarge
    );
    std::fs::write(p("binary.json"), b"\xff\xfe\x00").unwrap();
    assert_eq!(
        AiSettings::load_file(&p("binary.json")).unwrap_err(),
        AiConfigError::Syntax
    );
}

// ---------------------------------------------------------------- Config への配線・秘匿

fn config_env(map: &HashMap<String, String>) -> Result<Config, crate::config::ConfigError> {
    Config::load(|k| map.get(k).cloned(), &Overrides::default())
}

#[test]
fn an_unset_config_disables_new_calls_and_changes_nothing_else() {
    let c = config_env(&HashMap::new()).unwrap();
    assert_eq!(c.ai.invalid(), None);
    assert!(c.ai.settings().is_none());
    assert_eq!(c.ai.summary(), "unset");
    for task in TaskKind::ALL {
        assert_eq!(c.ai.gate(task, true).unwrap_err(), Disabled::Unset);
    }
    assert!(c.ai.enabled_tasks(true).is_empty());
    // 旧設定の既定は config.py と同じ（旧設定だけでは何も起動しない）
    let l = c.ai.legacy();
    assert!(l.enabled && l.loc_suggest);
    assert_eq!(
        (
            l.poll_secs,
            l.embed_per_cycle,
            l.judge_per_cycle,
            l.search_per_min
        ),
        (60.0, 12, 2, 30)
    );
    assert_eq!((l.clue_per_cycle, l.gemini_per_cycle), (60, 2));
    assert!(c.log_summary().contains("ai=unset"));
}

#[test]
fn a_bad_ai_config_keeps_a_kind_code_and_the_rest_of_the_config_loads() {
    let dir = TempDir::new("ai-bad-config");
    // パス・中身に、ログに出てはいけない目印を入れる
    let path = dir.path().join("sentinel-path-9f3a.json");
    std::fs::write(
        &path,
        r#"{"version":1 "sentinel-key-77c1":"sentinel-value-31b8""#,
    )
    .unwrap();
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("GEOFRAME_AI_CONFIG".into(), path.to_str().unwrap().into());
    env.insert("GEOFRAME_PORT".into(), "18081".into());
    let c = config_env(&env).unwrap();
    assert_eq!(c.listen.port(), 18081);
    assert_eq!(c.ai.invalid(), Some(AiConfigError::Syntax));
    for task in TaskKind::ALL {
        assert_eq!(c.ai.gate(task, true).unwrap_err(), Disabled::ConfigInvalid);
    }
    let text = format!("{c:?} {} {:?} {}", c.log_summary(), c.ai, c.ai.summary());
    assert!(text.contains("ai=invalid:syntax"), "{text}");
    for secret in [
        "sentinel-path-9f3a",
        "sentinel-key-77c1",
        "sentinel-value-31b8",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
    // 未知のキー・重複したキーでも種類のコードだけ
    std::fs::write(&path, r#"{"version":1,"sentinel-key-77c1":1}"#).unwrap();
    assert_eq!(
        config_env(&env).unwrap().ai.invalid(),
        Some(AiConfigError::UnknownKey)
    );
    // 相対パスはカレントディレクトリ基準（存在しなければ読めない）。値は出ない
    env.insert(
        "GEOFRAME_AI_CONFIG".into(),
        "sentinel-relative-6d2e.json".into(),
    );
    let c = config_env(&env).unwrap();
    assert_eq!(c.ai.invalid(), Some(AiConfigError::Unreadable));
    assert!(!format!("{c:?}").contains("sentinel-relative"));
}

#[test]
fn a_bad_legacy_env_value_disables_only_ai_and_never_echoes_the_value() {
    let cases = [
        ("GEOFRAME_AI_POLL_S", "sentinel-9a1"),
        ("GEOFRAME_AI_POLL_S", "nan"),
        ("GEOFRAME_AI_POLL_S", "inf"),
        ("GEOFRAME_AI_POLL_S", "-5"),
        ("GEOFRAME_AI_POLL_S", "-0.5"),
        ("GEOFRAME_AI_EMBED_PER_CYCLE", "-1"),
        ("GEOFRAME_AI_EMBED_PER_CYCLE", "1.5"),
        ("GEOFRAME_AI_JUDGE_PER_CYCLE", "sentinel-9a1"),
        ("GEOFRAME_AI_SEARCH_PER_MIN", "99999999999"),
        ("GEOFRAME_AI_VISUAL_SIMILARITY", "sentinel-9a1"),
        ("GEOFRAME_AI_VISUAL_SIMILARITY", "nan"),
        ("GEOFRAME_AI_VISUAL_SIMILARITY", "inf"),
        ("GEOFRAME_AI_VISUAL_SIMILARITY", "-0.1"),
        ("GEOFRAME_AI_VISUAL_SIMILARITY", "1.1"),
        ("GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE", "sentinel-9a1"),
        ("GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE", "sentinel-9a1"),
    ];
    for (key, value) in cases {
        let dir = TempDir::new("ai-bad-env");
        let path = dir.path().join("ai.json");
        std::fs::write(&path, SAMPLE).unwrap();
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert(key.into(), value.into());
        env.insert("GEOFRAME_AI_CONFIG".into(), path.to_str().unwrap().into());
        env.insert("GEOFRAME_PORT".into(), "18082".into());
        let c = config_env(&env).unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(c.listen.port(), 18082, "{key}={value}");
        assert_eq!(
            c.ai.invalid(),
            Some(AiConfigError::LegacyEnv),
            "{key}={value}"
        );
        // 正しい JSON があっても使わない（旧設定が不正な間は AI だけ無効）
        assert!(c.ai.settings().is_none());
        assert_eq!(
            c.ai.gate(TaskKind::PhotoJudge, true).unwrap_err(),
            Disabled::ConfigInvalid
        );
        let text = format!("{c:?} {}", c.log_summary());
        assert!(!text.contains(value) || value.len() < 4, "{text}");
        assert!(!text.contains("sentinel-9a1"), "{text}");
    }
}

#[test]
fn visual_similarity_is_validated_without_preventing_startup_when_ai_is_off() {
    let c = config_env(&HashMap::from([
        (
            "GEOFRAME_AI_VISUAL_SIMILARITY".into(),
            "sentinel-visual".into(),
        ),
        ("GEOFRAME_AI_ENABLED".into(), "0".into()),
    ]))
    .expect("AI settings must not prevent serving");
    assert_eq!(c.ai.invalid(), Some(AiConfigError::LegacyEnv));
    assert!(!c.ai.legacy().enabled);
    assert!(!format!("{c:?} {}", c.log_summary()).contains("sentinel-visual"));

    for (backend, expected) in [("gemini", 0.90), ("local", 0.91)] {
        let mut env = HashMap::from([("GEOFRAME_EMBED_BACKEND".into(), backend.into())]);
        assert_eq!(config_env(&env).unwrap().ai_visual_similarity, expected);
        for value in ["0", "1", "0.8_5"] {
            env.insert("GEOFRAME_AI_VISUAL_SIMILARITY".into(), value.into());
            let c = config_env(&env).unwrap();
            assert!(c.ai.invalid().is_none());
            assert_eq!(
                c.ai_visual_similarity,
                crate::pyquery::py_float(value).unwrap()
            );
        }
    }
}

#[test]
fn legacy_env_values_map_to_cycle_limits() {
    let c = env_config(&[
        ("GEOFRAME_AI_POLL_S", "45.5"),
        ("GEOFRAME_AI_EMBED_PER_CYCLE", "7"),
        ("GEOFRAME_AI_JUDGE_PER_CYCLE", "3"),
        ("GEOFRAME_AI_SEARCH_PER_MIN", "11"),
        ("GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE", "40"),
        ("GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE", "5"),
    ]);
    let l = c.legacy();
    assert_eq!(l.cycle(TaskKind::ImageEmbedding), (7, 46));
    assert_eq!(l.cycle(TaskKind::QueryEmbedding), (11, 60));
    assert_eq!(l.cycle(TaskKind::PhotoJudge), (3, 46));
    assert_eq!(l.cycle(TaskKind::LocationClue), (40, 46));
    assert_eq!(l.cycle(TaskKind::LocationSuggest), (5, 46));
    assert_eq!(l.poll().as_secs_f64(), 45.5);
    // 周期は 30 秒未満を 30 に切り上げる（現行の max(30, …)）。負は切り上げずに不正（上の試験）
    for raw in ["1", "0", "0.5", "29.9"] {
        assert_eq!(
            env_config(&[("GEOFRAME_AI_POLL_S", raw)])
                .legacy()
                .poll_secs,
            30.0
        );
    }
    assert_eq!(
        env_config(&[("GEOFRAME_AI_POLL_S", "999999")])
            .legacy()
            .poll_secs,
        86_400.0
    );
    // フラグは現行と同じく `1` だけが有効。空文字は未設定（既定の有効）
    assert!(!env_config(&[("GEOFRAME_AI_ENABLED", "0")]).legacy().enabled);
    assert!(
        !env_config(&[("GEOFRAME_AI_ENABLED", "true")])
            .legacy()
            .enabled
    );
    assert!(env_config(&[("GEOFRAME_AI_ENABLED", "")]).legacy().enabled);
    assert!(
        !env_config(&[("GEOFRAME_LOC_SUGGEST", "0")])
            .legacy()
            .loc_suggest
    );
}

// ---------------------------------------------------------------- 送信の可否

#[test]
fn local_tasks_are_enabled_cloud_tasks_need_allow_and_a_daily_limit() {
    let (config, _dir) = ready(&[]);
    // LAN の用途は設定どおり。writer があれば全て使える（クラウドの位置候補だけ allow_cloud が無い）
    for task in [
        TaskKind::ImageEmbedding,
        TaskKind::QueryEmbedding,
        TaskKind::LocationClue,
    ] {
        let gate = config.gate(task, true).unwrap();
        assert!(gate.budget.is_none(), "{task:?}");
        assert_eq!(gate.local_base_url, Some("http://192.0.2.10:8100"));
    }
    // allow_cloud=true と daily=20 の写真比較は、予算つきで使える（周期は JUDGE_PER_CYCLE=2・窓は周期）
    let judge = config.gate(TaskKind::PhotoJudge, true).unwrap();
    let budget = judge.budget.unwrap();
    assert_eq!((budget.daily, budget.cycle, budget.cycle_secs), (20, 2, 60));
    // 既定の allow_cloud=false は policy で止まる（provider=cloud でも送らない）
    assert_eq!(
        config.gate(TaskKind::LocationSuggest, true).unwrap_err(),
        Disabled::CloudNotAllowed
    );
    assert_eq!(config.enabled_tasks(true).len(), 4);

    // 許可があっても daily が未設定・0 なら送らない
    for daily in ["", r#", "daily_request_limit": 0"#] {
        let json = variant(
            r#""model": "example-suggest","#,
            &format!(r#""model": "example-suggest", "allow_cloud": true{daily},"#),
        );
        let (c, _d) = file_config(&json, &[]);
        assert_eq!(
            c.gate(TaskKind::LocationSuggest, true).unwrap_err(),
            Disabled::NoDailyLimit,
            "{daily}"
        );
    }
    // 周期の上限 0 はクラウドもローカルも止まる
    let (c, _d) = ready(&[
        ("GEOFRAME_AI_JUDGE_PER_CYCLE", "0"),
        ("GEOFRAME_AI_EMBED_PER_CYCLE", "0"),
    ]);
    assert_eq!(
        c.gate(TaskKind::PhotoJudge, true).unwrap_err(),
        Disabled::CycleLimitZero
    );
    assert_eq!(
        c.gate(TaskKind::ImageEmbedding, true).unwrap_err(),
        Disabled::CycleLimitZero
    );
    assert!(c.gate(TaskKind::QueryEmbedding, true).is_ok());
}

#[test]
fn legacy_flags_gate_the_tasks_and_a_missing_task_or_space_is_disabled() {
    let (c, _d) = ready(&[("GEOFRAME_AI_ENABLED", "0")]);
    for task in TaskKind::ALL {
        assert_eq!(c.gate(task, true).unwrap_err(), Disabled::AiDisabled);
    }
    let (c, _d) = ready(&[("GEOFRAME_LOC_SUGGEST", "0")]);
    assert_eq!(
        c.gate(TaskKind::LocationClue, true).unwrap_err(),
        Disabled::LocSuggestOff
    );
    assert_eq!(
        c.gate(TaskKind::LocationSuggest, true).unwrap_err(),
        Disabled::LocSuggestOff
    );
    assert!(c.gate(TaskKind::ImageEmbedding, true).is_ok());

    // 用途が無い・embedding_space が無い
    let (c, _d) = file_config(r#"{"version":1}"#, &[]);
    assert_eq!(
        c.gate(TaskKind::PhotoJudge, true).unwrap_err(),
        Disabled::TaskNotConfigured
    );
    let no_space = r#"{"version":1,"local_base_url":"http://192.0.2.10","tasks":{
        "image_embedding":{"provider":"local","model":"m","service_id":"s"},
        "location_clue":{"provider":"local","model":"m"}}}"#;
    let (c, _d) = file_config(no_space, &[]);
    assert_eq!(
        c.gate(TaskKind::ImageEmbedding, true).unwrap_err(),
        Disabled::NoEmbeddingSpace
    );
    assert!(c.gate(TaskKind::LocationClue, true).is_ok());
}

#[test]
fn without_a_writer_cloud_never_sends_but_a_lan_query_embedding_still_works() {
    let json = variant(
        r#""query_embedding": {"provider": "local", "model": "example-embed", "service_id": "example-embed-svc"}"#,
        r#""query_embedding": {"provider": "cloud", "model": "example-q", "allow_cloud": true,
          "daily_request_limit": 5, "credential_path": "/etc/example/credentials.json",
          "project": "example-project", "region": "example-region1"}"#,
    );
    let (cloud_query, _d) = file_config(&json, &[]);
    // writer があればクラウドの検索語は毎分の周期窓つきで使える
    let budget = cloud_query
        .gate(TaskKind::QueryEmbedding, true)
        .unwrap()
        .budget
        .unwrap();
    assert_eq!((budget.daily, budget.cycle, budget.cycle_secs), (5, 30, 60));
    // INGEST=0（writer なし）: クラウドの検索語は予算を迂回して送らない
    assert_eq!(
        cloud_query
            .gate(TaskKind::QueryEmbedding, false)
            .unwrap_err(),
        Disabled::NoWriter
    );
    // LAN の検索語 Embedding は writer なしで使える。ジョブの用途は writer が要る
    let (lan, _d) = ready(&[]);
    assert!(lan.gate(TaskKind::QueryEmbedding, false).is_ok());
    assert_eq!(
        lan.gate(TaskKind::ImageEmbedding, false).unwrap_err(),
        Disabled::NoWriter
    );
    assert_eq!(
        lan.gate(TaskKind::PhotoJudge, false).unwrap_err(),
        Disabled::NoWriter
    );
    assert_eq!(lan.enabled_tasks(false), [TaskKind::QueryEmbedding]);
}

// ---------------------------------------------------------------- ハッシュ・停止の識別

#[test]
fn fingerprints_are_deterministic_and_unambiguous() {
    // 固定の値: 再起動をまたいで同じ（DefaultHasher ではない）。値を変えるなら永続した識別が無効になる
    assert_eq!(
        fingerprint(&["a"]),
        "ae6121c88ba555f64c3d812123eb799d128015541f850c5e9bf1d54c08ad8481"
    );
    // 区切りの曖昧さが無い
    assert_ne!(fingerprint(&["ab", "c"]), fingerprint(&["a", "bc"]));
    assert_eq!(
        fingerprint(&["ab", "c"]),
        "43ee655579de01ca739b3f95c1c2d3f46d353b2c0df818064ea594506cdb2617"
    );
    assert_eq!(sample().config_hash(), sample().config_hash());
}

#[test]
fn config_and_policy_hashes_follow_what_they_audit() {
    let base = sample();
    // 設定のどこを変えても設定全体のハッシュは変わる
    for (from, to) in [
        (r#""allow_cloud": true"#, r#""allow_cloud": false"#),
        (
            r#""daily_request_limit": 20"#,
            r#""daily_request_limit": 21"#,
        ),
        (r#"http://192.0.2.10:8100"#, r#"http://192.0.2.11:8100"#),
        (
            r#""model": "example-judge""#,
            r#""model": "example-judge-2""#,
        ),
    ] {
        assert_ne!(
            parse(&variant(from, to)).unwrap().config_hash(),
            base.config_hash(),
            "{from}"
        );
    }
    // 用途のポリシーのハッシュは、その用途の送信ポリシーだけで変わる
    let judge = base.policy_hash(TaskKind::PhotoJudge).unwrap();
    let other_allow = parse(&variant(
        r#""model": "example-suggest","#,
        r#""model": "example-suggest", "allow_cloud": true, "daily_request_limit": 9,"#,
    ))
    .unwrap();
    assert_eq!(
        other_allow.policy_hash(TaskKind::PhotoJudge).unwrap(),
        judge
    );
    assert_ne!(
        other_allow.policy_hash(TaskKind::LocationSuggest),
        base.policy_hash(TaskKind::LocationSuggest)
    );
    let changed = parse(&variant(
        r#""daily_request_limit": 20"#,
        r#""daily_request_limit": 21"#,
    ))
    .unwrap();
    assert_ne!(changed.policy_hash(TaskKind::PhotoJudge).unwrap(), judge);
    assert_eq!(base.policy_hash(TaskKind::LocationClue).unwrap().len(), 64);
    assert!(
        parse(r#"{"version":1}"#)
            .unwrap()
            .policy_hash(TaskKind::PhotoJudge)
            .is_none()
    );
}

#[test]
fn pause_keys_depend_only_on_the_destination_and_the_model() {
    let base = sample();
    let provider = |s: &AiSettings| s.provider_pause_key(TaskKind::PhotoJudge).unwrap();
    let task = |s: &AiSettings| s.task_pause_key(TaskKind::PhotoJudge).unwrap();
    assert_eq!(provider(&base).scope(), PauseScope::Provider);
    assert_eq!(task(&base).scope(), PauseScope::Task);

    // 関係しない変更（許可・上限・include_hidden・別の用途・別の用途のモデル・LAN の入口）では変わらない
    for (from, to) in [
        (r#""allow_cloud": true"#, r#""allow_cloud": false"#),
        (
            r#""daily_request_limit": 20"#,
            r#""daily_request_limit": 5"#,
        ),
        (
            r#""allow_cloud": true,"#,
            r#""allow_cloud": true, "include_hidden": true,"#,
        ),
        (
            r#""model": "example-suggest""#,
            r#""model": "example-suggest-2""#,
        ),
        (
            r#""text_preprocess": "txt-v1""#,
            r#""text_preprocess": "txt-v2""#,
        ),
        (r#"http://192.0.2.10:8100"#, r#"http://192.0.2.99:8100"#),
    ] {
        let v = parse(&variant(from, to)).unwrap();
        assert_eq!(provider(&v), provider(&base), "{from}");
        assert_eq!(task(&v), task(&base), "{from}");
    }
    // 認証・接続先が変わると Provider の識別が変わる（用途の識別も、その接続先を含むので変わる）
    for (from, to) in [
        (
            r#""project": "example-project", "region": "example-region1"},
    "location_clue""#,
            r#""project": "example-project-2", "region": "example-region1"},
    "location_clue""#,
        ),
        (
            r#""project": "example-project", "region": "example-region1"},
    "location_clue""#,
            r#""project": "example-project", "region": "example-region2"},
    "location_clue""#,
        ),
    ] {
        let v = parse(&variant(from, to)).unwrap();
        assert_ne!(provider(&v), provider(&base), "{to}");
        assert_ne!(task(&v), task(&base), "{to}");
    }
    // モデルを変えると用途の識別だけが変わる（Provider は同じ）
    let model = parse(&variant(
        r#""model": "example-judge""#,
        r#""model": "example-judge-2""#,
    ))
    .unwrap();
    assert_eq!(provider(&model), provider(&base));
    assert_ne!(task(&model), task(&base));
    // LAN の入口が変わると、LAN の用途の Provider の識別が変わる
    let moved = parse(&variant(
        r#"http://192.0.2.10:8100"#,
        r#"http://192.0.2.11:8100"#,
    ))
    .unwrap();
    assert_ne!(
        moved.provider_pause_key(TaskKind::ImageEmbedding),
        base.provider_pause_key(TaskKind::ImageEmbedding)
    );
    // Debug に識別の hash は出ない
    let text = format!(
        "{:?} {:?} {:?}",
        provider(&base),
        base,
        base.task(TaskKind::PhotoJudge)
    );
    for secret in [
        "example-project",
        "credentials",
        "192.0.2.10",
        "example-judge",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
}

#[test]
fn debug_output_never_contains_urls_paths_or_model_names() {
    let s = sample();
    let (config, dir) = ready(&[]);
    let mut text = format!("{s:?} {:?} {:?}", s.embedding_space(), config);
    for task in TaskKind::ALL {
        text.push_str(&format!("{:?}", s.task(task)));
        if let Ok(gate) = config.gate(task, true) {
            text.push_str(&format!("{gate:?} {:?}", gate.settings));
        }
    }
    text.push_str(&format!("{:?}", config.summary()));
    let secrets = [
        "192.0.2.10",
        "credentials.json",
        "example-project",
        "example-embed",
        "example-judge",
        "example-decision",
        "example-region1",
        "example-space-1",
    ];
    for secret in secrets {
        assert!(!text.contains(secret), "{secret}: {text}");
    }
    assert!(!text.contains(dir.path().to_str().unwrap()), "{text}");
}
