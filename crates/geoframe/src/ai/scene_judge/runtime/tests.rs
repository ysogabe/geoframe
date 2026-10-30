//! 通信は全て loopback のスタブ。写真・鍵・地名は合成。未許可のときのスタブ受信 0 を確認する。
use super::super::*;
use crate::ai::{
    cloud::CloudClients,
    groups::runtime::Runtime,
    index,
    settings::{Normalization, TaskKind},
    space::SpaceIdentity,
    tests_http::{Stub, respond, stub},
    tests_worker::{Env, env_with},
    worker::{Core, Handler},
};
use crate::{config::Config, stop::StopFlag};
use base64::Engine as _;
use rusqlite::params;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

use super::MAX_QUEUE;

type Hook = Arc<dyn Fn(&str) + Send + Sync>;
struct State {
    cloud_status: u16,
    token_status: u16,
    bad_json: bool,
    /// 推薦する画像の番号（1 始まり）。
    recommend: usize,
    same_scene: bool,
    hook: Option<Hook>,
}
struct Lab {
    _key: crate::testutil::TempDir,
    key_path: std::path::PathBuf,
    env: Env,
    cloud: Stub,
    state: Arc<Mutex<State>>,
    config: Mutex<Arc<Config>>,
}
const CANARY: &[u8] = b"SYNTHETIC-GPS-CANARY-35.000000";

fn identity() -> SpaceIdentity {
    SpaceIdentity::new(
        "judge-a",
        "shared-a",
        "model-a",
        "rev-a",
        "img-a",
        "txt-a",
        2,
        Normalization::Raw,
    )
    .unwrap()
}
fn sha_hex(bytes: &[u8]) -> String {
    sha(bytes)
}
/// 合成 JPEG。`exif` なら位置情報（の代わりの canary）を含む APP1 を差し込む。
fn jpeg(seed: u8, exif: bool) -> Vec<u8> {
    let rgb: Vec<u8> = (0..64 * 48 * 3)
        .map(|i| (i as u8).wrapping_mul(seed | 1))
        .collect();
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 88)
        .encode(&rgb, 64, 48, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    if exif {
        let mut tiff = b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
        tiff.extend_from_slice(CANARY);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = bytes[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&bytes[2..]);
        return out;
    }
    bytes
}

impl Lab {
    fn new() -> Self {
        Self::configured(|_| {})
    }
    fn doc(key: &std::path::Path, change: impl FnOnce(&mut Value)) -> String {
        let mut doc: Value = serde_json::from_str(crate::ai::tests_settings::SAMPLE).unwrap();
        doc["tasks"]["photo_judge"]["allow_cloud"] = json!(true);
        doc["tasks"]["photo_judge"]["daily_request_limit"] = json!(100);
        doc["tasks"]["photo_judge"]["credential_path"] = json!(key);
        change(&mut doc);
        doc.to_string()
    }
    fn configured(change: impl FnOnce(&mut Value)) -> Self {
        let key = crate::testutil::TempDir::new("judge-runtime-key");
        let key_path = key.path().join("synthetic.json");
        std::fs::write(&key_path,serde_json::to_vec(&json!({"client_email":"synthetic@example.invalid","private_key":format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",include_str!("../../../../tests/fixtures/jwt-test-pkcs8.b64").trim())})).unwrap()).unwrap();
        std::fs::set_permissions(
            &key_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        let state = Arc::new(Mutex::new(State {
            cloud_status: 200,
            token_status: 200,
            bad_json: false,
            recommend: 1,
            same_scene: true,
            hook: None,
        }));
        let s = state.clone();
        let cloud = stub(move |stream, req| {
            let token = req.request_line.starts_with("POST /token ");
            let hook = s.lock().unwrap().hook.clone();
            if let Some(h) = hook {
                h(if token { "token" } else { "cloud" });
            }
            let s = s.lock().unwrap();
            if token {
                return if s.token_status == 200 {
                    respond(stream,"200 OK","",br#"{"access_token":"synthetic-token","token_type":"Bearer","expires_in":3600}"#)
                } else {
                    respond(stream, &format!("{} X", s.token_status), "", b"")
                };
            }
            let n = String::from_utf8_lossy(&req.body)
                .matches("inlineData")
                .count();
            let photos: Vec<Value> = (1..=n)
                .map(|i| json!({"index":i,"subject":"other","pet_state":"not_applicable","eyes":"not_applicable","face_crop":"not_applicable","horizon_status":"not_applicable","horizon_score":50,"subject_sharpness":70,"aesthetic_score":60,"keep_score":90-i,"recommended":i==s.recommend,"reasons":["合成の理由"],"concerns":[]}))
                .collect();
            let text = if s.bad_json {
                "not json".to_string()
            } else {
                json!({"same_scene":s.same_scene,"summary":"合成の要約","photos":photos})
                    .to_string()
            };
            let v =
                json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":text}]}}]});
            respond(
                stream,
                &format!("{} X", s.cloud_status),
                "",
                &serde_json::to_vec(&v).unwrap(),
            );
        });
        let env = env_with("judge-runtime", &Self::doc(&key_path, change), true);
        env.writer()
            .call(|c| {
                c.execute_batch("DELETE FROM photos")?;
                let s = identity();
                index::register_space(c, &s, 0).unwrap();
                c.execute(
                    "INSERT INTO settings VALUES('ai_active_space',?1)",
                    [s.space_id()],
                )?;
                Ok(())
            })
            .unwrap();
        std::fs::create_dir_all(env.config.data_dir.join("photos")).unwrap();
        let config = Mutex::new(env.config.clone());
        Self {
            _key: key,
            key_path,
            env,
            cloud,
            state,
            config,
        }
    }
    fn config(&self) -> Arc<Config> {
        self.config.lock().unwrap().clone()
    }
    /// 設定を変えて再起動した状態（同じ DB・データ）にする。
    fn reconfigure(&self, change: impl FnOnce(&mut Value)) {
        let doc = Self::doc(&self.key_path, change);
        *self.config.lock().unwrap() = self.env.reconfigure(&doc);
    }
    fn add(&self, id: i64, seconds: i64, seed: u8, exif: bool) {
        let bytes = jpeg(seed, exif);
        let rel = format!("photos/synthetic-{id}.jpg");
        std::fs::write(self.env.config.data_dir.join(&rel), &bytes).unwrap();
        let (sha, time) = (
            sha_hex(&bytes),
            format!(
                "2026-01-01 {:02}:{:02}:{:02}",
                seconds / 3600,
                (seconds / 60) % 60,
                seconds % 60
            ),
        );
        self.env
            .writer()
            .call(move |c| {
                c.execute("INSERT INTO photos(id,source,name,display,md5,mtime,rotation,taken_at,lat,lon,hidden,favorite) VALUES(?1,'local','synthetic',?2,'m',1,0,?3,1,2,0,0)",params![id,rel,time])?;
                c.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,phash,sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at)
                   VALUES(?1,?2,'m',1,?3,'0',5,128,90,0,0,100,'','synthetic')",params![id,rel,sha])?;
                index::store_vector(c, &identity(), &sha, &"f".repeat(64), &[1., 0.], 0).unwrap();
                Ok(())
            })
            .unwrap();
    }
    fn scene(&self, ids: std::ops::RangeInclusive<i64>, base: i64) {
        for id in ids {
            self.add(id, base + id * 10, id as u8 + 3, id == 1);
        }
    }
    /// ローカルの場面計算 1 周期（Provider・クラウドとは無関係）。
    fn scenes(&self) -> usize {
        let read = crate::db::open_read_only(&self.config().db_path()).unwrap();
        Runtime::production(self.config())
            .unwrap()
            .cycle(&read, &self.env.writer(), &StopFlag::new())
            .unwrap()
    }
    fn handlers(&self) -> Vec<Arc<dyn Handler>> {
        let config = self.config();
        let clients = CloudClients::loopback(&config.ai, self.cloud.addr, self.cloud.addr);
        handler(
            &config,
            crate::datadir::DataDir::open(&config.data_dir).unwrap(),
            Some(&clients),
        )
        .into_iter()
        .collect()
    }
    fn cycle(&self) {
        self.cycle_stop(&StopFlag::new());
    }
    fn cycle_stop(&self, stop: &StopFlag) {
        self.cycle_with(self.handlers(), stop);
    }
    fn cycle_with(&self, handlers: Vec<Arc<dyn Handler>>, stop: &StopFlag) {
        Core::new(&self.env.deps_for(self.config(), handlers, stop, "judge")).cycle();
    }
    fn posts(&self) -> usize {
        self.cloud
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| !r.request_line.starts_with("POST /token "))
            .count()
    }
    fn tokens(&self) -> usize {
        let posts = self.posts();
        self.cloud.requests.lock().unwrap().len() - posts
    }
    fn n(&self, sql: &str) -> i64 {
        let sql = sql.to_string();
        self.env
            .writer()
            .call(move |c| c.query_row(&sql, [], |r| r.get(0)))
            .unwrap()
    }
    fn exec(&self, sql: &str) {
        let sql = sql.to_string();
        self.env
            .writer()
            .call(move |c| c.execute_batch(&sql))
            .unwrap();
    }
    fn hook(&self, h: impl Fn(&str) + Send + Sync + 'static) {
        self.state.lock().unwrap().hook = Some(Arc::new(h));
    }
    fn job(&self) -> Option<(String, i64, String)> {
        self.env
            .writer()
            .call(|c| {
                rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT state,attempts,last_error_code FROM ai_jobs WHERE kind='photo_judge' ORDER BY id DESC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                ))
            })
            .unwrap()
    }
    fn judgments(&self) -> i64 {
        self.n("SELECT COUNT(*) FROM ai_scene_judgments")
    }
    fn paid(&self) {
        assert_eq!(self.scenes(), 1);
        self.cycle();
        assert_eq!(self.judgments(), 1);
    }
    fn primary(&self) -> Vec<Vec<rusqlite::types::Value>> {
        self.env
            .writer()
            .call(|c| {
                let mut q = c.prepare("SELECT * FROM photos ORDER BY id")?;
                let n = q.column_count();
                q.query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
                    .collect()
            })
            .unwrap()
    }
}

#[test]
fn unauthorized_variants_send_nothing_but_local_groups_still_form() {
    type Change = fn(&mut Value);
    let variants: [(&str, Change); 4] = [
        ("cloud not allowed", |d| {
            d["tasks"]["photo_judge"]["allow_cloud"] = json!(false)
        }),
        ("no daily limit", |d| {
            d["tasks"]["photo_judge"]["daily_request_limit"] = json!(0)
        }),
        ("task absent", |d| {
            d["tasks"].as_object_mut().unwrap().remove("photo_judge");
        }),
        (
            "local provider",
            |d| {
                d["tasks"]["photo_judge"] =
                    json!({"provider":"local","model":"m","service_id":"svc"})
            },
        ),
    ];
    for (label, change) in variants {
        let l = Lab::configured(change);
        l.scene(1..=3, 0);
        assert_eq!(l.scenes(), 1, "{label}");
        assert!(
            l.handlers().is_empty(),
            "{label}: no handler without permission"
        );
        l.cycle();
        assert_eq!(
            l.n("SELECT COUNT(*) FROM ai_scene_groups WHERE state='local_ranked'"),
            1,
            "{label}"
        );
        assert_eq!(
            l.cloud
                .connections
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "{label}"
        );
        assert_eq!(
            l.n("SELECT COUNT(*) FROM ai_jobs WHERE kind='photo_judge'"),
            0,
            "{label}"
        );
        assert_eq!(
            l.n("SELECT COUNT(*) FROM sqlite_master WHERE name='ai_scene_judgments'"),
            0,
            "{label}: the history table exists only for a permitted task"
        );
    }
}

#[test]
fn authorized_send_strips_exif_gps_caps_images_and_keeps_primary_columns() {
    let l = Lab::new();
    l.scene(1..=8, 0);
    let before = l.primary();
    l.paid();
    assert_eq!(l.posts(), 1);
    assert_eq!(l.tokens(), 1);
    let req = l
        .cloud
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|r| !r.request_line.starts_with("POST /token "))
        .cloned()
        .unwrap();
    let body: Value = serde_json::from_slice(&req.body).unwrap();
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    let images: Vec<Vec<u8>> = parts
        .iter()
        .filter_map(|p| p.get("inlineData"))
        .map(|d| {
            base64::engine::general_purpose::STANDARD
                .decode(d["data"].as_str().unwrap())
                .unwrap()
        })
        .collect();
    assert_eq!(images.len(), 6, "the shortlist cap is the image cap");
    for image in &images {
        assert!(
            !image.windows(CANARY.len()).any(|w| w == CANARY),
            "GPS canary leaked"
        );
        assert!(!image.windows(4).any(|w| w == b"Exif"));
        assert!(!image.windows(2).any(|w| w == [0xFF, 0xE1]), "APP1 kept");
        assert!(image.starts_with(&[0xFF, 0xD8]));
    }
    assert_eq!(body["generationConfig"]["temperature"], 0.1);
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(l.primary(), before, "photos columns are untouched");
    assert_eq!(l.job().unwrap().0, "succeeded");
    // 旧 Python の選別表には何も書かない（フィード抑止の根拠にしない）。
    assert_eq!(l.n("SELECT COUNT(*) FROM photo_ai_groups"), 0);
    assert_eq!(l.n("SELECT COUNT(*) FROM photo_ai_features"), 0);
    let admin = crate::admin::status(
        &crate::db::open_read_only(&l.config().db_path()).unwrap(),
        &crate::admin::AdminContext::new(&l.config(), &l.config().data_dir),
    )
    .unwrap();
    assert_eq!(admin["ai_scenes"]["paid_judgments"], 1);
}

#[test]
fn a_saved_answer_is_reused_across_cycles_restarts_and_unrelated_settings() {
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.paid();
    for _ in 0..3 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!(l.posts(), 1);
    // 再起動（別の URL 相当・timeout 相当・他用途の許可だけの変更）。同じ画像・世代・prompt・モデルなので再送 0。
    l.reconfigure(|d| {
        d["tasks"]["location_suggest"]["allow_cloud"] = json!(true);
        d["tasks"]["location_suggest"]["daily_request_limit"] = json!(7);
        d["tasks"]["photo_judge"]["daily_request_limit"] = json!(3);
    });
    l.cycle();
    l.env.advance(3600);
    l.cycle();
    assert_eq!((l.posts(), l.judgments()), (1, 1));
    // 別モデルは別の由来: 比較をやり直す（過去の履歴は残る）。
    l.reconfigure(|d| d["tasks"]["photo_judge"]["model"] = json!("other-judge-model"));
    l.cycle();
    assert_eq!((l.posts(), l.judgments()), (2, 2));
}

#[test]
fn a_reused_answer_survives_membership_changes_outside_the_shortlist_only() {
    // 7 枚（同点は ID 順なので shortlist は 1..=6）。shortlist の外（ID 9）が増えても再送 0。
    let l = Lab::new();
    l.scene(1..=7, 0);
    l.paid();
    assert_eq!(
        l.n("SELECT COUNT(*) FROM ai_scene_members WHERE shortlisted=1"),
        6
    );
    l.add(9, 5, 99, false);
    assert_eq!(l.scenes(), 1, "the group is re-saved with its new member");
    assert_eq!(l.n("SELECT COUNT(*) FROM ai_scene_active"), 8);
    assert_eq!(
        l.n(
            "SELECT COUNT(*) FROM ai_scene_members m JOIN ai_scene_active a
             ON a.result_id=m.result_id AND a.photo_id=m.photo_id
             WHERE m.shortlisted=1 AND m.photo_id=9"
        ),
        0
    );
    for _ in 0..2 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!((l.posts(), l.judgments()), (1, 1));
    // shortlist に入る写真（ID 0）が増えれば送信集合が変わるので、別の由来として 1 回だけ再比較する。
    let l = Lab::new();
    l.scene(1..=7, 0);
    l.paid();
    l.add(0, 5, 98, false);
    l.scenes();
    for _ in 0..3 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!((l.posts(), l.judgments()), (2, 2));
}

#[test]
fn per_post_budget_cycle_limit_and_failure_cap_are_kept() {
    // 日の上限 1: 2 つの場面のうち 1 つだけ送り、もう一方は予算切れで待つ（POST 1）。
    let l = Lab::configured(|d| d["tasks"]["photo_judge"]["daily_request_limit"] = json!(1));
    l.scene(1..=2, 0);
    l.scene(11..=12, 36_000);
    assert_eq!(l.scenes(), 2);
    l.cycle();
    assert_eq!(l.posts(), 1);
    assert_eq!(l.judgments(), 1);
    assert_eq!(
        l.n("SELECT COUNT(*) FROM ai_jobs WHERE kind='photo_judge' AND last_error_code='budget_exhausted'"),
        1
    );
    l.env.advance(3600);
    l.cycle();
    assert_eq!(
        l.posts(),
        1,
        "the daily budget is not refilled within the day"
    );
    // 応答が不正 JSON のときの失敗は 5 回で打ち切る（毎 POST 1 枠を数える）。
    let l = Lab::new();
    l.scene(1..=2, 0);
    l.state.lock().unwrap().bad_json = true;
    l.scenes();
    for _ in 0..8 {
        l.env.advance(7200);
        l.cycle();
    }
    assert_eq!(l.posts(), 5);
    assert_eq!(
        l.job().unwrap(),
        ("failed".into(), 5, "bad_response".into())
    );
    assert_eq!(l.judgments(), 0);
    assert_eq!(
        l.n("SELECT COALESCE(SUM(count),0) FROM ai_request_usage WHERE task='photo_judge' AND scope='day'"),
        5
    );
}

#[test]
fn posts_per_cycle_never_exceed_the_cycle_limit_even_with_more_sendable_scenes() {
    // 4 つの場面が同時に送れる。1 周期の POST は周期の上限（GEOFRAME_AI_JUDGE_PER_CYCLE。既定 2）以下。
    // 送信後の時間切れを有償の試行に数えることと、中断 3 回の扱いは共通層（worker/jobs/cloud）の試験で確かめる。
    let l = Lab::new();
    for (ids, base) in [
        (1..=2, 0),
        (11..=12, 20_000),
        (21..=22, 40_000),
        (31..=32, 60_000),
    ] {
        l.scene(ids, base);
    }
    assert_eq!(l.scenes(), 4);
    let limit = l.config().ai.legacy().cycle(TaskKind::PhotoJudge).0 as usize;
    assert_eq!(limit, 2, "the default per-cycle limit");
    l.cycle();
    assert!(l.posts() <= limit, "{} posts in one cycle", l.posts());
    assert_eq!(l.posts(), limit);
    assert_eq!(l.judgments(), limit as i64);
    // 同じ周期の窓の中の再実行でも上限を超えない。窓が進めば残りが順に送られ、全部で 1 場面 1 回。
    l.cycle();
    assert!(l.posts() <= limit);
    for _ in 0..4 {
        let before = l.posts();
        l.env.advance(3600);
        l.cycle();
        assert!(l.posts() - before <= limit);
    }
    assert_eq!((l.posts(), l.judgments()), (4, 4));
}

#[test]
fn exact_partial_waiting_and_hidden_are_never_sent() {
    // Exact: 旧い有償判定が有効な集合は再課金しない。
    let legacy = |l: &Lab, ids: &[i64]| {
        l.exec("INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene,judge_model,judge_version) VALUES('old','scene','old','synthetic','judged',1,'synthetic-model','old-prompt')");
        for (rank, id) in ids.iter().enumerate() {
            l.exec(&format!("INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,analyzed_at,error)
               SELECT p.id,p.display,p.md5,p.mtime,f.content_sha256,'old-model',2,zeroblob(8),'synthetic','' FROM photos p JOIN photo_quality_features f ON f.photo_id=p.id WHERE p.id={id}"));
            l.exec(&format!("INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,shortlisted,local_score) VALUES('old',{id},{},{},1,50)", rank + 1, i32::from(rank == 0)));
        }
    };
    let exact = Lab::new();
    exact.scene(1..=3, 0);
    legacy(&exact, &[1, 2, 3]);
    exact.scenes();
    assert_eq!(
        exact.n("SELECT COUNT(*) FROM ai_scene_groups WHERE overlap='exact'"),
        1
    );
    exact.cycle();
    assert_eq!((exact.posts(), exact.tokens()), (0, 0));
    let partial = Lab::new();
    partial.scene(1..=3, 0);
    legacy(&partial, &[1, 2]);
    partial.scenes();
    assert_eq!(
        partial.n("SELECT COUNT(*) FROM ai_scene_groups WHERE state='review_overlap'"),
        1
    );
    partial.cycle();
    assert_eq!((partial.posts(), partial.tokens()), (0, 0));
    // Waiting: 場面の窓に未解析の写真があるあいだは送らない。
    let waiting = Lab::new();
    waiting.scene(1..=3, 0);
    waiting.exec("INSERT INTO photos(id,source,name,display,md5,mtime,rotation,taken_at) VALUES(99,'local','synthetic','photos/missing.jpg','m',1,0,'2026-01-01 00:00:30')");
    waiting.scenes();
    waiting.cycle();
    assert_eq!((waiting.posts(), waiting.tokens()), (0, 0));
    // hidden を含む shortlist は、hidden を許可していない用途では送らない。
    let hidden = Lab::new();
    hidden.scene(1..=3, 0);
    hidden.exec("UPDATE photos SET hidden=1 WHERE id=2");
    hidden.scenes();
    hidden.cycle();
    assert_eq!((hidden.posts(), hidden.tokens()), (0, 0));
    let allowed = Lab::configured(|d| d["tasks"]["photo_judge"]["include_hidden"] = json!(true));
    allowed.scene(1..=3, 0);
    allowed.exec("UPDATE photos SET hidden=1 WHERE id=2");
    allowed.scenes();
    allowed.cycle();
    assert_eq!(allowed.posts(), 1);
    // 単独の写真は比較しない。
    let single = Lab::new();
    single.scene(1..=1, 0);
    single.scenes();
    single.cycle();
    assert_eq!(single.posts(), 0);
}

#[test]
fn input_changed_during_the_response_is_not_adopted() {
    type Change = fn(&Lab);
    let changes: [(&str, Change); 5] = [
        ("rotation", |l| {
            l.exec("UPDATE photos SET rotation=90 WHERE id=2")
        }),
        ("delete", |l| l.exec("DELETE FROM photos WHERE id=2")),
        ("hidden", |l| {
            l.exec("UPDATE photos SET hidden=1 WHERE id=2")
        }),
        ("md5", |l| {
            l.exec("UPDATE photos SET md5='changed' WHERE id=2")
        }),
        ("file", |l| {
            std::fs::write(
                l.env.config.data_dir.join("photos/synthetic-2.jpg"),
                jpeg(200, false),
            )
            .unwrap()
        }),
    ];
    for (label, change) in changes {
        let l = Arc::new(Lab::new());
        l.scene(1..=3, 0);
        assert_eq!(l.scenes(), 1);
        let (l2, fired) = (
            l.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        l.hook(move |event| {
            if event == "cloud" && !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                change(&l2);
            }
        });
        l.cycle();
        assert_eq!(l.posts(), 1, "{label}");
        assert_eq!(l.judgments(), 0, "{label}: a stale answer is not stored");
        assert!(
            matches!(l.job(), Some((ref s, _, _)) if s == "obsolete"),
            "{label}: {:?}",
            l.job()
        );
        assert_eq!(l.n("SELECT COUNT(*) FROM photo_ai_groups"), 0, "{label}");
    }
}

/// 本物の handler を包み、保存（writer の IMMEDIATE tx の中の `persist`）の直前に、同じ tx で SQL を実行する。
/// 応答後の `check_input`（writer の外）を通り抜けた後で入力が変わった場合を作る。
struct MutateBeforePersist {
    inner: Arc<dyn Handler>,
    sql: &'static str,
}
impl Handler for MutateBeforePersist {
    fn kind(&self) -> JobKind {
        self.inner.kind()
    }
    fn reconcile(&self, read: &rusqlite::Connection, now: i64) -> rusqlite::Result<Vec<JobKey>> {
        self.inner.reconcile(read, now)
    }
    fn run(
        &self,
        ctx: &crate::ai::worker::RunCtx<'_>,
        job: &crate::ai::jobs::Claimed,
    ) -> Result<crate::ai::worker::Persist, crate::ai::worker::RunError> {
        let persist = self.inner.run(ctx, job)?;
        let sql = self.sql;
        Ok(Box::new(move |tx| {
            assert!(!tx.is_autocommit(), "persist runs inside the writer tx");
            tx.execute_batch(sql)?;
            persist(tx)
        }))
    }
    fn recheck(
        &self,
        conn: &rusqlite::Connection,
        key: &JobKey,
    ) -> rusqlite::Result<crate::ai::jobs::Recheck> {
        self.inner.recheck(conn, key)
    }
}

#[test]
fn the_cas_inside_the_writer_tx_rejects_a_sent_photo_changed_after_the_pre_commit_check() {
    // check_input は writer の外で通り抜けている。その後・保存の直前（同じ tx）に写真が変わった場合、
    // `persist` 自身の `input_current` が不採用にする。
    let cases: [(&str, &'static str, bool); 5] = [
        ("unchanged (control)", "SELECT 1", true),
        (
            "rotation",
            "UPDATE photos SET rotation=90 WHERE id=2",
            false,
        ),
        ("delete", "DELETE FROM photos WHERE id=2", false),
        ("hidden", "UPDATE photos SET hidden=1 WHERE id=2", false),
        ("md5", "UPDATE photos SET md5='changed' WHERE id=2", false),
    ];
    for (label, sql, stored) in cases {
        let l = Lab::new();
        l.scene(1..=3, 0);
        assert_eq!(l.scenes(), 1);
        let handlers: Vec<Arc<dyn Handler>> = l
            .handlers()
            .into_iter()
            .map(|inner| Arc::new(MutateBeforePersist { inner, sql }) as Arc<dyn Handler>)
            .collect();
        assert_eq!(handlers.len(), 1);
        l.cycle_with(handlers, &StopFlag::new());
        assert_eq!(l.posts(), 1, "{label}: the answer was paid for");
        assert_eq!(l.judgments(), i64::from(stored), "{label}");
        let state = l.job().unwrap().0;
        assert_eq!(
            state,
            if stored { "succeeded" } else { "obsolete" },
            "{label}"
        );
    }
}

/// 旧い有効な判定が一部だけ重なる（Partial になる）ようにする。
fn make_partial(l: &Lab) {
    l.exec("INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene,judge_model,judge_version) VALUES('old','scene','old','synthetic','judged',1,'m','v')");
    for (rank, id) in [1, 2].iter().enumerate() {
        l.exec(&format!("INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,analyzed_at,error)
           SELECT p.id,p.display,p.md5,p.mtime,f.content_sha256,'old-model',2,zeroblob(8),'synthetic','' FROM photos p JOIN photo_quality_features f ON f.photo_id=p.id WHERE p.id={id}"));
        l.exec(&format!("INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,shortlisted,local_score) VALUES('old',{id},{},{},1,50)", rank + 1, i32::from(rank == 0)));
    }
}
fn shown(view: &Value) -> bool {
    view["groups"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g.get("judge_source").is_some())
}

#[test]
fn a_paid_answer_survives_the_scene_turning_partial_or_waiting_and_is_reused_when_it_forms_again() {
    // Partial: 応答待ちに旧判定と一部重なった。送った入力（写真 ID・世代）は同じなので履歴に残るが、
    // Partial の場面には表示も再送もしない。
    let l = Arc::new(Lab::new());
    l.scene(1..=3, 0);
    l.scenes();
    let l2 = l.clone();
    l.hook(move |event| {
        if event == "cloud" {
            make_partial(&l2);
            l2.scenes();
        }
    });
    l.cycle();
    assert_eq!(l.posts(), 1);
    assert_eq!(l.judgments(), 1, "the paid answer is kept in the history");
    assert_eq!(l.job().unwrap().0, "succeeded");
    assert_eq!(
        l.n("SELECT COUNT(*) FROM ai_scene_groups WHERE state='review_overlap'"),
        1
    );
    assert!(!shown(&view(&l)), "a partial scene shows no comparison");
    l.env.advance(3600);
    l.cycle();
    assert_eq!(l.posts(), 1, "a partial scene is not sent");
    // 旧判定が外れて同じ shortlist の Disjoint の場面ができ直す: 再課金せず、保存済みの回答が表示に使われる。
    l.exec("DELETE FROM photo_ai_group_members; DELETE FROM photo_ai_groups");
    assert_eq!(l.scenes(), 1);
    for _ in 0..2 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!((l.posts(), l.judgments()), (1, 1), "no second charge");
    assert!(shown(&view(&l)));

    // Waiting: 応答待ちに、場面の窓へ解析前の写真が入った。履歴には残り、窓が戻ると再課金なしで表示に使う。
    let l = Arc::new(Lab::new());
    l.scene(1..=3, 0);
    l.scenes();
    let l2 = l.clone();
    l.hook(move |event| {
        if event == "cloud" {
            l2.exec("INSERT INTO photos(id,source,name,display,md5,mtime,rotation,taken_at) VALUES(99,'local','synthetic','photos/missing.jpg','m',1,0,'2026-01-01 00:00:30')");
            l2.scenes();
        }
    });
    l.cycle();
    assert_eq!((l.posts(), l.judgments()), (1, 1));
    assert_eq!(l.job().unwrap().0, "succeeded");
    assert_eq!(
        l.n("SELECT COUNT(*) FROM ai_scene_groups WHERE state='waiting'"),
        1
    );
    assert!(!shown(&view(&l)));
    l.exec("DELETE FROM photos WHERE id=99");
    assert_eq!(l.scenes(), 1);
    for _ in 0..2 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!((l.posts(), l.judgments()), (1, 1), "no second charge");
    assert!(shown(&view(&l)));
}

#[test]
fn config_changes_after_enqueue_are_rechecked_before_any_token_or_post() {
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.scenes();
    // 認証トークンの段階で失敗 → 推論 POST 0 のままジョブが待つ。
    l.state.lock().unwrap().token_status = 503;
    l.cycle();
    assert_eq!(l.posts(), 0);
    let tokens = l.tokens();
    assert!(tokens >= 1);
    assert_eq!(
        l.n("SELECT COUNT(*) FROM ai_jobs WHERE kind='photo_judge' AND state!='succeeded'"),
        1
    );
    // 許可を外して再起動: 未送信ジョブに許可の判定がやり直され、token も POST も 0。
    l.reconfigure(|d| d["tasks"]["photo_judge"]["allow_cloud"] = json!(false));
    l.env.advance(7200);
    l.cycle();
    assert_eq!((l.tokens(), l.posts()), (tokens, 0));
    // 許可を戻して復旧すれば同じジョブが進む。
    l.reconfigure(|_| {});
    l.state.lock().unwrap().token_status = 200;
    l.env.advance(7200);
    l.cycle();
    assert_eq!((l.posts(), l.judgments()), (1, 1));
}

#[test]
fn stop_before_claim_sends_nothing_and_a_response_in_flight_is_committed() {
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.scenes();
    let stop = StopFlag::new();
    stop.stop();
    l.cycle_stop(&stop);
    assert_eq!((l.tokens(), l.posts()), (0, 0));
    // 送信中に停止が来ても、有効な応答は確定される（以降の claim は 0）。
    let stop = StopFlag::new();
    let s2 = stop.clone();
    l.hook(move |event| {
        if event == "cloud" {
            s2.stop();
        }
    });
    l.cycle_stop(&stop);
    assert_eq!((l.posts(), l.judgments()), (1, 1));
}

#[test]
fn damaged_history_holds_without_billing_and_manual_retry_requeues_only_unpaid_input() {
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.paid();
    l.exec("UPDATE ai_scene_judgments SET verdict_json=replace(verdict_json,'true','false')");
    for _ in 0..2 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!(
        l.posts(),
        1,
        "an unverifiable record is held, never billed again"
    );
    let read = crate::db::open_read_only(&l.config().db_path()).unwrap();
    let key: String = read
        .query_row("SELECT input_key FROM ai_scene_judgments", [], |r| r.get(0))
        .unwrap();
    assert!(matches!(lookup(&read, &key).unwrap(), Lookup::Unverified));
    assert!(matches!(
        lookup(&read, &"0".repeat(64)).unwrap(),
        Lookup::Absent
    ));
    // 手動の再試行（ai-jobs retry）も、破損した履歴の入力を成功とも未送信とも見なさず、再送しない。
    l.exec("UPDATE ai_jobs SET state='failed',terminal=1,attempts=5,last_error_code='bad_response' WHERE kind='photo_judge'");
    let report = crate::ai::ops::retry(
        &l.env.writer(),
        &l.config().ai,
        &l.handlers(),
        crate::ai::ops::RetryTarget::Kind(JobKind::PhotoJudge),
        &l.env.clock(),
    )
    .unwrap();
    assert_eq!(report.requeued, 0);
    l.env.advance(3600);
    l.cycle();
    assert_eq!(l.posts(), 1);
}

#[test]
fn scenes_stopped_by_the_failure_cap_do_not_starve_the_ones_behind_them_until_a_manual_retry() {
    // MAX_QUEUE + 1 の送れる場面（同じ大きさ）。先頭の MAX_QUEUE 件が失敗の上限に達して回答が無い。
    let l = Lab::new();
    for scene in 0..=MAX_QUEUE as i64 {
        l.scene(2 * scene + 1..=2 * scene + 2, scene * 2_400);
    }
    // 場面の計算は 1 周期に GROUPS_PER_CYCLE 件までなので、全部できるまで回す。
    let built: usize = (0..3).map(|_| l.scenes()).sum();
    assert_eq!(built, MAX_QUEUE + 1);
    let handler = l.handlers().remove(0);
    let reconcile = |h: &Arc<dyn Handler>| {
        let read = crate::db::open_read_only(&l.config().db_path()).unwrap();
        h.reconcile(&read, crate::ai::tests_worker::T0).unwrap()
    };
    let first = reconcile(&handler);
    assert_eq!(first.len(), MAX_QUEUE);
    let queued = first.clone();
    l.env
        .writer()
        .call(move |c| {
            crate::ai::jobs::enqueue_batch(c, &queued, crate::ai::tests_worker::T0)?;
            c.execute_batch(
                "UPDATE ai_jobs SET state='failed',terminal=1,attempts=5,last_error_code='bad_response'
                 WHERE kind='photo_judge'",
            )
        })
        .unwrap();
    // 失敗で止まったキーは候補から外れ、後ろの 1 場面だけが残る（直す前は同じ 32 件が返り続けた）。
    let next = reconcile(&handler);
    assert_eq!(next.len(), 1);
    assert!(first.iter().all(|k| *k != next[0]));
    // 実際の周期でも、後ろの場面が送られ、失敗の履歴（上限・試行数）はそのまま残る。
    l.cycle();
    assert_eq!(l.posts(), 1);
    assert_eq!(l.judgments(), 1);
    assert_eq!(
        l.n("SELECT COUNT(*) FROM ai_jobs WHERE kind='photo_judge' AND state='failed' AND terminal=1 AND attempts=5"),
        MAX_QUEUE as i64
    );
    // 手動の再試行で失敗の履歴が消えたキーは、また候補に戻る（回答のある後ろの場面は戻らない）。
    let report = crate::ai::ops::retry(
        &l.env.writer(),
        &l.config().ai,
        &l.handlers(),
        crate::ai::ops::RetryTarget::Kind(JobKind::PhotoJudge),
        &l.env.clock(),
    )
    .unwrap();
    assert_eq!(report.requeued, MAX_QUEUE);
    assert_eq!(
        l.n("SELECT COUNT(*) FROM ai_jobs WHERE kind='photo_judge' AND state='pending' AND terminal=0 AND attempts=0"),
        MAX_QUEUE as i64
    );
    assert_eq!(reconcile(&handler).len(), MAX_QUEUE);
}

fn view(l: &Lab) -> Value {
    view_with(l, &l.config())
}
fn view_with(l: &Lab, config: &Config) -> Value {
    let read = crate::db::open_read_only(&l.config().db_path()).unwrap();
    crate::ai::search_view::search(
        &read,
        config,
        "",
        30,
        false,
        None,
        &crate::ai::http::Deadline::interactive(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn an_adopted_answer_is_shown_for_its_disjoint_scene_only_and_never_hides_a_photo() {
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.scenes();
    let before = view(&l);
    assert_eq!(before["groups"][0]["kind"], "scene");
    assert!(before["groups"][0].get("judge_source").is_none());
    let feed = |l: &Lab| {
        crate::photos::list_photos(&crate::db::open_read_only(&l.config().db_path()).unwrap())
            .unwrap()
            .len()
    };
    assert_eq!(feed(&l), 3);
    l.cycle();
    assert_eq!(l.judgments(), 1);
    let after = view(&l);
    let group = &after["groups"][0];
    assert_eq!(group["judge_source"], "gemini");
    assert_eq!(group["summary"], "合成の要約");
    assert_eq!(group["same_scene"], true);
    assert_eq!(group["scene_overlap"], "disjoint");
    let photos = group["photos"].as_array().unwrap();
    assert_eq!(
        photos
            .iter()
            .filter(|p| p["gemini_recommended"] == true)
            .count(),
        1
    );
    assert!(photos.iter().all(|p| p["gemini_keep_score"].is_number()));
    // 旧い意味の項目・並び・フィードは変えない（Gemini の推薦で何も隠さない）。
    assert!(photos.iter().all(|p| p["recommended"].is_null()));
    assert_eq!(feed(&l), 3);
    assert_eq!(l.n("SELECT COUNT(*) FROM photo_ai_groups"), 0);
    // 送った写真が変われば（世代が変わり）回答はもう表示に使わない。履歴は残る。
    l.exec("UPDATE photos SET rotation=90 WHERE id=2");
    l.scenes();
    let changed = view(&l);
    assert!(changed["groups"][0].get("judge_source").is_none());
    assert_eq!(l.judgments(), 1);
    // 許可を外した設定（task が無い）では表示にも使わない。
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.paid();
    l.reconfigure(|d| {
        d["tasks"].as_object_mut().unwrap().remove("photo_judge");
    });
    assert!(view(&l)["groups"][0].get("judge_source").is_none());
}

/// 同じ DB・同じ AI 設定で、AI 全体（`GEOFRAME_AI_ENABLED=0`）だけを止めた設定。
fn config_ai_off(l: &Lab) -> Config {
    let path = l._key.path().join("ai-off.json");
    std::fs::write(&path, Lab::doc(&l.key_path, |_| {})).unwrap();
    let vars = std::collections::HashMap::from([
        ("GEOFRAME_AI_CONFIG", path.to_str().unwrap().to_string()),
        ("GEOFRAME_INGEST", "1".to_string()),
        ("GEOFRAME_AI_ENABLED", "0".to_string()),
    ]);
    let overrides = crate::config::Overrides {
        listen: None,
        data_dir: Some(l.config().data_dir.clone()),
    };
    Config::load(|k| vars.get(k).cloned(), &overrides).unwrap()
}

fn ids(view: &Value, group: usize) -> Vec<i64> {
    view["groups"][group]["photos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_i64().unwrap())
        .collect()
}

#[test]
fn an_adopted_recommendation_picks_the_representative_and_order_inside_the_group_only() {
    // 合成の応答は 2 番目の画像を推薦する（ローカルの順位 1 位とは別の写真）。
    let l = Lab::new();
    l.state.lock().unwrap().recommend = 2;
    l.scene(1..=3, 0);
    l.scene(11..=13, 20_000);
    l.scenes();
    let before = view(&l);
    let local_first: Vec<i64> = (0..2)
        .map(|g| {
            before["groups"][g]["photos"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["local_representative"] == true)
                .unwrap()["id"]
                .as_i64()
                .unwrap()
        })
        .collect();
    assert!(
        (0..2).all(|g| ids(&before, g)[0] == local_first[g]),
        "without a comparison the local representative leads"
    );
    let order: Vec<_> = before["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["group_id"].clone())
        .collect();
    let feed = |l: &Lab| {
        crate::photos::list_photos(&crate::db::open_read_only(&l.config().db_path()).unwrap())
            .unwrap()
            .len()
    };
    l.env.advance(3600);
    l.cycle();
    assert_eq!(l.judgments(), 2);
    let after = view(&l);
    for g in 0..2 {
        let photos = after["groups"][g]["photos"].as_array().unwrap();
        let gemini = photos
            .iter()
            .find(|p| p["gemini_recommended"] == true)
            .unwrap();
        // 群の中の先頭（クラスタの代表）は Gemini の推薦。局所の代表の表示はそのまま。
        assert_eq!(photos[0]["id"], gemini["id"], "group {g}");
        assert_ne!(gemini["id"].as_i64().unwrap(), local_first[g]);
        assert_eq!(photos[0]["visual_representative"], true);
        let local = photos.iter().find(|p| p["local_rank"] == 1).unwrap();
        assert_eq!(local["local_representative"], true);
        assert_eq!(local["id"].as_i64().unwrap(), local_first[g]);
        assert_eq!(photos.len(), 3, "nothing is dropped");
    }
    // 群どうしの並びと件数は変わらず、フィードの抑止にも使わない。
    assert_eq!(
        after["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["group_id"].clone())
            .collect::<Vec<_>>(),
        order
    );
    assert_eq!(feed(&l), 6);
    // 許可を外した設定では、並びにも Gemini の推薦を使わない（回答前と同じ並びに戻る）。履歴は残る。
    l.reconfigure(|d| d["tasks"]["photo_judge"]["allow_cloud"] = json!(false));
    let off = view(&l);
    assert!(!shown(&off));
    for g in 0..2 {
        assert_eq!(ids(&off, g), ids(&before, g), "group {g}: the unpaid order");
    }
    assert_eq!(l.judgments(), 2);
    l.reconfigure(|_| {});
    assert!(shown(&view(&l)));
    // favorite は Gemini の推薦より前に来る。
    l.exec("UPDATE photos SET favorite=1 WHERE id=3");
    let favored = view(&l);
    let first_group = (0..2).find(|g| ids(&favored, *g).contains(&3)).unwrap();
    assert_eq!(ids(&favored, first_group)[0], 3);
    l.exec("UPDATE photos SET favorite=0 WHERE id=3");
    // 旧い群（今は有効でない）から残った写真ごとの recommended=1 も、Gemini の推薦より前に来る。
    // Gemini の推薦でも局所の代表でもない写真に付けて、並びの先頭がそれになることを見る。
    let g = (0..2).find(|g| ids(&after, *g).contains(&1)).unwrap();
    let gemini = after["groups"][g]["photos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["gemini_recommended"] == true)
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let other = [1, 2, 3]
        .into_iter()
        .find(|id| *id != gemini && *id != local_first[g])
        .unwrap();
    l.exec(&format!("INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,analyzed_at,error,recommended)
       SELECT p.id,p.display,p.md5,p.mtime,f.content_sha256,'old-model',2,zeroblob(8),'synthetic','',1 FROM photos p JOIN photo_quality_features f ON f.photo_id=p.id WHERE p.id={other}"));
    let legacy = view(&l);
    assert!(shown(&legacy), "the answer is still adopted");
    assert_eq!(legacy["groups"][g]["photos"][0]["id"], other);
    assert_eq!(legacy["groups"][g]["photos"][0]["recommended"], true);
    assert_eq!(legacy["groups"][g]["photos"][1]["id"], gemini);
    assert_eq!(legacy["groups"][g]["photos"][1]["gemini_recommended"], true);
    assert_eq!(ids(&legacy, g).len(), 3, "nothing is dropped");
    // 別場面（同一場面でない）の応答は全員が推薦扱いで差がつかず、群の中の並びを変えない。
    let none = Lab::new();
    none.state.lock().unwrap().same_scene = false;
    none.state.lock().unwrap().recommend = 2;
    none.scene(1..=3, 0);
    none.scenes();
    let unpaid = ids(&view(&none), 0);
    none.cycle();
    assert_eq!(none.judgments(), 1);
    let paid = view(&none);
    assert!(shown(&paid));
    assert_eq!(ids(&paid, 0), unpaid);
}

#[test]
fn the_stored_answer_is_displayed_only_while_the_current_setting_allows_the_cloud_judge() {
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.paid();
    assert!(shown(&view(&l)));
    // 許可を外した設定・ローカル・別のモデル・用途なし: 表示しない。履歴は消さない。
    type Change = fn(&mut Value);
    let variants: [(&str, Change); 4] = [
        ("cloud not allowed", |d| {
            d["tasks"]["photo_judge"]["allow_cloud"] = json!(false)
        }),
        (
            "local provider",
            |d| {
                d["tasks"]["photo_judge"] =
                    json!({"provider":"local","model":"m","service_id":"svc"})
            },
        ),
        ("other model", |d| {
            d["tasks"]["photo_judge"]["model"] = json!("other-judge-model")
        }),
        ("task absent", |d| {
            d["tasks"].as_object_mut().unwrap().remove("photo_judge");
        }),
    ];
    for (label, change) in variants {
        l.reconfigure(change);
        assert!(!shown(&view(&l)), "{label}");
        assert_eq!(l.judgments(), 1, "{label}: the history stays");
    }
    // AI 全体を止めた設定（GEOFRAME_AI_ENABLED=0）でも表示に使わない。履歴は消さない。
    let off = config_ai_off(&l);
    assert!(!off.ai.legacy().enabled);
    assert!(!shown(&view_with(&l, &off)), "AI disabled");
    assert_eq!(l.judgments(), 1, "AI disabled: the history stays");
    // 許可を戻せば、同じ回答がまた表示に使われる（再課金なし）。
    l.reconfigure(|_| {});
    assert!(shown(&view(&l)));
    assert_eq!(l.posts(), 1);
    // 日の上限 0 は送信の止めであって表示の許可ではない（保存済みの回答は表示できる）。
    l.reconfigure(|d| d["tasks"]["photo_judge"]["daily_request_limit"] = json!(0));
    assert!(shown(&view(&l)));
}

thread_local! {
    static CATALOG_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
fn count_catalog_read(event: rusqlite::trace::TraceEvent<'_>) {
    if let rusqlite::trace::TraceEvent::Stmt(stmt, _) = event
        && stmt.sql().contains("sqlite_master")
    {
        CATALOG_READS.with(|n| n.set(n.get() + 1));
    }
}

#[test]
fn the_history_table_is_looked_up_once_per_request_however_many_scenes_are_returned() {
    let reads = |scenes: i64| {
        let l = Lab::new();
        for n in 0..scenes {
            l.scene(n * 10 + 1..=n * 10 + 3, n * 20_000);
        }
        assert_eq!(l.scenes(), scenes as usize);
        l.env.advance(3600);
        for _ in 0..scenes {
            l.env.advance(3600);
            l.cycle();
        }
        assert_eq!(l.judgments(), scenes);
        let read = crate::db::open_read_only(&l.config().db_path()).unwrap();
        CATALOG_READS.with(|n| n.set(0));
        read.trace_v2(
            rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
            Some(count_catalog_read),
        );
        let out = crate::ai::search_view::search(
            &read,
            &l.config(),
            "",
            30,
            false,
            None,
            &crate::ai::http::Deadline::interactive(),
        )
        .unwrap()
        .unwrap();
        read.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
        assert_eq!(
            out["groups"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|g| g.get("judge_source").is_some())
                .count() as i64,
            scenes
        );
        CATALOG_READS.with(Cell::get)
    };
    use std::cell::Cell;
    let (one, three) = (reads(1), reads(3));
    assert!(one > 0);
    assert_eq!(
        one, three,
        "the lookup does not grow with the scenes returned"
    );
}

#[test]
fn a_response_for_a_claim_that_was_returned_meanwhile_is_not_saved() {
    let l = Arc::new(Lab::new());
    l.scene(1..=3, 0);
    l.scenes();
    let (l2, stop) = (l.clone(), StopFlag::new());
    let s2 = stop.clone();
    l.hook(move |event| {
        if event == "cloud" {
            s2.stop();
            // 起動回収・最後の戻し相当: claim token を失効させて deferred に戻す。
            l2.exec("UPDATE ai_jobs SET state='deferred',claim_token=NULL,last_error_code='shutdown' WHERE kind='photo_judge'");
        }
    });
    l.cycle_stop(&stop);
    assert_eq!(l.posts(), 1);
    assert_eq!(
        l.judgments(),
        0,
        "a late answer of an invalidated claim is never stored"
    );
    assert_eq!(l.job().unwrap().0, "deferred");
}

/// Measurement only (`--ignored --nocapture`): time inside the single writer for the paid answer's CAS,
/// with and without 6500 photos in the catalog. "full" is the earlier CAS (the whole catalog read through
/// `Prepared::load_in` in the writer transaction); "persist" is the CAS in the writer now (the sent
/// photos' rows only). The other photos have quality features and a (2-dimensional) vector but no
/// capture time, so they belong to no scene; the vectors are smaller than the real 768 dimensions, which
/// makes "full" a lower bound.
#[test]
#[ignore = "measurement: synthetic 6500-photo catalog, writer time of the paid answer's CAS"]
fn synthetic_6500_photo_catalog_measures_the_writer_time_of_the_paid_answer_cas() {
    use std::time::{Duration, Instant};
    fn median(mut v: Vec<Duration>) -> f64 {
        v.sort();
        v[v.len() / 2].as_secs_f64() * 1000.
    }
    for others in [0_i64, 6500] {
        let l = Lab::new();
        for (ids, base) in [
            (1..=2, 0),
            (11..=12, 20_000),
            (21..=22, 40_000),
            (31..=32, 60_000),
            (41..=42, 80_000),
        ] {
            l.scene(ids, base);
        }
        let space = identity().space_id().to_string();
        let blob = crate::ai::space::encode(&[1., 0.]);
        l.env
            .writer()
            .call(move |c| {
                let tx = c.transaction()?;
                for n in 0..others {
                    let (photo, sha) = (1000 + n, format!("{:064x}", 1000 + n));
                    let display = format!("photos/synthetic-{photo}.jpg");
                    tx.execute("INSERT INTO photos(id,source,name,display,md5,mtime,rotation,lat,lon,hidden,favorite) VALUES(?1,'local','synthetic',?2,'m',1,0,1,2,0,0)", params![photo, display])?;
                    tx.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,phash,sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at)
                       VALUES(?1,?2,'m',1,?3,'0',5,128,90,0,0,100,'','synthetic')", params![photo, display, sha])?;
                    tx.execute("INSERT INTO ai_embedding_vectors VALUES(?1,?2,?3,2,?4,0)", params![space, sha, "f".repeat(64), blob])?;
                }
                tx.commit()
            })
            .unwrap();
        assert_eq!(l.scenes(), 5, "the other photos form no scene");
        for _ in 0..4 {
            l.env.advance(3600);
            l.cycle();
        }
        assert_eq!(l.judgments(), 5);
        // 他の試験と同時に動いても、この Lab の DB の分だけを数える。
        let db = l.config().db_path();
        let persist: Vec<Duration> = super::PERSIST_TIMES
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.as_deref().map(std::path::Path::new) == Some(db.as_path()))
            .map(|(_, took)| *took)
            .collect();
        assert_eq!(persist.len(), 5);
        let config = l.config();
        let full: Vec<Duration> = (0..5)
            .map(|_| {
                let config = config.clone();
                l.env
                    .writer()
                    .call(move |c| {
                        let started = Instant::now();
                        let tx = c.unchecked_transaction()?;
                        let prepared = crate::ai::groups::runtime::Prepared::read(&tx, &config)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?;
                        let loaded = prepared
                            .load_in(&tx)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?;
                        let wanted = loaded
                            .groups()
                            .iter()
                            .find_map(Candidate::from_group)
                            .unwrap();
                        let found = repository::candidate_current(&tx, &prepared, &wanted)?;
                        assert!(found.is_some());
                        tx.commit()?;
                        Ok(started.elapsed())
                    })
                    .unwrap()
            })
            .collect();
        eprintln!(
            "PERSIST_CAS photos={} full_catalog_cas_ms_median={:.2} persist_ms_median={:.3} persist_ms_max={:.3}",
            others + 10,
            median(full),
            median(persist.clone()),
            persist.iter().max().unwrap().as_secs_f64() * 1000.
        );
        // E6 with the five stored answers shown, against the same read when the setting no longer allows them.
        let e6 = |l: &Lab| {
            median(
                (0..10)
                    .map(|_| {
                        let started = Instant::now();
                        assert!(!view(l)["groups"].as_array().unwrap().is_empty());
                        started.elapsed()
                    })
                    .collect(),
            )
        };
        assert!(shown(&view(&l)));
        let with = e6(&l);
        l.reconfigure(|d| d["tasks"]["photo_judge"]["allow_cloud"] = json!(false));
        eprintln!(
            "PERSIST_CAS photos={} e6_with_5_answers_ms_median={with:.1} e6_without_ms_median={:.1}",
            others + 10,
            e6(&l)
        );
    }
}
