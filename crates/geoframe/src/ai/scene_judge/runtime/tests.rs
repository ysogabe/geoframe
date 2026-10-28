//! 通信は全て loopback のスタブ。写真・鍵・地名は合成。未許可のときのスタブ受信 0 を確認する。
use super::super::*;
use crate::ai::{
    cloud::CloudClients,
    groups::runtime::Runtime,
    index,
    jobs::JobState,
    settings::Normalization,
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

type Hook = Arc<dyn Fn(&str) + Send + Sync>;
struct State {
    cloud_status: u16,
    token_status: u16,
    bad_json: bool,
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
        let state = Arc::new(Mutex::new(State {
            cloud_status: 200,
            token_status: 200,
            bad_json: false,
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
                .map(|i| json!({"index":i,"subject":"other","pet_state":"not_applicable","eyes":"not_applicable","face_crop":"not_applicable","horizon_status":"not_applicable","horizon_score":50,"subject_sharpness":70,"aesthetic_score":60,"keep_score":90-i,"recommended":i==1,"reasons":["合成の理由"],"concerns":[]}))
                .collect();
            let text = if s.bad_json {
                "not json".to_string()
            } else {
                json!({"same_scene":true,"summary":"合成の要約","photos":photos}).to_string()
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
        Core::new(
            &self
                .env
                .deps_for(self.config(), self.handlers(), stop, "judge"),
        )
        .cycle();
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
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.paid();
    // 同じ場面に写真が増えても shortlist（上位 6 枚）が変わらなければ再送しない。
    l.add(9, 5, 99, false);
    l.scenes();
    l.env.advance(3600);
    l.cycle();
    let shortlist = l.n("SELECT COUNT(*) FROM ai_scene_members WHERE shortlisted=1");
    assert!(shortlist >= 3);
    assert!(
        l.posts() <= 2,
        "a changed shortlist may be compared once; never repeatedly"
    );
    let posts = l.posts();
    for _ in 0..2 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!(l.posts(), posts);
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

#[test]
fn partial_overlap_appearing_during_the_response_is_not_adopted() {
    let l = Arc::new(Lab::new());
    l.scene(1..=3, 0);
    l.scenes();
    let l2 = l.clone();
    l.hook(move |event| {
        if event == "cloud" {
            // 旧い有効な判定が一部だけ重なる集合になった。
            l2.exec("INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene,judge_model,judge_version) VALUES('old','scene','old','synthetic','judged',1,'m','v')");
            for (rank, id) in [1, 2].iter().enumerate() {
                l2.exec(&format!("INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,analyzed_at,error)
                   SELECT p.id,p.display,p.md5,p.mtime,f.content_sha256,'old-model',2,zeroblob(8),'synthetic','' FROM photos p JOIN photo_quality_features f ON f.photo_id=p.id WHERE p.id={id}"));
                l2.exec(&format!("INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,shortlisted,local_score) VALUES('old',{id},{},{},1,50)", rank + 1, i32::from(rank == 0)));
            }
            l2.scenes();
        }
    });
    l.cycle();
    assert_eq!(l.posts(), 1);
    assert_eq!(l.judgments(), 0);
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
}

#[test]
fn the_schema_is_additive_and_python_rollback_still_reads_and_rebuilds() {
    let l = Lab::new();
    l.scene(1..=3, 0);
    l.paid();
    let tables = [
        "photos",
        "photo_ai_groups",
        "photo_ai_group_members",
        "photo_ai_features",
        "ai_scene_groups",
        "ai_scene_members",
        "ai_scene_active",
        "ai_scene_judgments",
    ];
    let snap = |l: &Lab| -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        tables
            .iter()
            .map(|t| {
                let t = t.to_string();
                l.env
                    .writer()
                    .call(move |c| {
                        let mut q = c.prepare(&format!("SELECT * FROM {t} ORDER BY 1,2"))?;
                        let n = q.column_count();
                        q.query_map([], |r| (0..n).map(|i| r.get(i)).collect())?
                            .collect()
                    })
                    .unwrap()
            })
            .collect()
    };
    let before = snap(&l);
    let script = format!(
        r#"
import os, sys, json
sys.path.insert(0, {app:?})
from geoframe import ai_pipeline, ai_search, photos
feed=photos.api_photos()
status=ai_search.api_ai_status()
loaded=ai_pipeline._load_photos()
ai_pipeline.load_rank_model=lambda *_: None
ai_pipeline._rebuild_groups(loaded)
print(json.dumps(dict(feed=len(feed), status=isinstance(status,dict), loaded=len(loaded))))
"#,
        app = concat!(env!("CARGO_MANIFEST_DIR"), "/../../app")
    );
    let output = std::process::Command::new("python3")
        .args(["-I", "-c", &script])
        .env_clear()
        .env("GEOFRAME_DATA_DIR", &l.config().data_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "synthetic Python rollback failed");
    let result: Value = serde_json::from_str(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], true);
    assert_eq!(
        result["feed"], 3,
        "the feed is not narrowed by a paid judgment"
    );
    // Python が自分の旧表（groups / members / features）を再構築しても、人手の列と新しい表は同一。
    let after = snap(&l);
    for index in [0, 4, 5, 6, 7] {
        assert!(after[index] == before[index], "{} changed", tables[index]);
    }
    assert_eq!(l.judgments(), 1);
    assert_eq!(JobState::parse("succeeded"), Some(JobState::Succeeded));
}
