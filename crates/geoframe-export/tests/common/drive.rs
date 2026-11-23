//! T4 の試験の共通部品: 偽の Drive の状態を作る・読む・差し替える部品と、呼び出しの記録を中立の操作名で読む部品（`ops`・
//! `mutating_ops`・`ops_used`・`op_count`）。偽の Drive は、試験のプロセスの中の HTTP サーバー（`fake_drive::FakeDrive`。Drive API v3
//! の形で答える。模型の状態は `fake-rclone/model.rs` を共有し、`fake_dir` のファイルに持つ）。`Env::new` がサーバーを立て、
//! `Env::settings`（資格情報・3 つのフォルダの ID・`--drive-endpoint` に当たる通信先）をそれに向ける。
//! rclone の引数・環境変数を見る部品（`rclone_calls`・`assert_rclone_safe_calls`）は rclone 版（`src/rclone.rs`。3 段目で消す）の
//! 試験だけが使う。本物の Google・rclone・ネットワークには触れない。Drive の ID は架空の値（`fid0001` …）。

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use geoframe_export::drive::Layout;
use geoframe_export::error::ExportError;
use geoframe_export::folder::FolderName;
use geoframe_export::image::PublishFile;
use geoframe_export::jobid::JobId;
use geoframe_export::manifest::GENERATOR;
use geoframe_export::publish::{ProgressSink, Timing};
use geoframe_export::published::{PublishedRecord, PublishedStore};
use geoframe_export::recovery::Progress;
use geoframe_export::remote::Remote;
use geoframe_export::request::Destination;
use geoframe_export::settings::{RawSettings, Settings};

use super::fake_drive::{self, FakeDrive};
use serde_json::{Value, json};

#[path = "../../fake-rclone/model.rs"]
pub mod model;

use super::TempDir;

pub const JOB_A: &str = "00112233445566778899aabbccddeeff";
pub const JOB_B: &str = "ffeeddccbbaa99887766554433221100";
pub const FOLDER: &str = "2030-03-10";

/// Drive を変えない操作名（`mutating_ops` が除く）。
const READ_ONLY_OPS: [&str; 5] = ["list", "read", "get", "generate_ids", "token"];

/// 再試行しない方針。
pub fn no_retry() -> geoframe_export::drive::http::Retry {
    geoframe_export::drive::http::Retry {
        max_retries: 0,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(1),
    }
}

/// 待たずに済む再試行の方針（5ms から。3 回まで）。
pub fn fast_retry() -> geoframe_export::drive::http::Retry {
    geoframe_export::drive::http::Retry {
        max_retries: 3,
        base: Duration::from_millis(5),
        cap: Duration::from_millis(200),
    }
}

pub fn fast_timing() -> Timing {
    Timing {
        settle: Duration::from_millis(10),
        rounds: 3,
    }
}

pub struct Env {
    pub root: TempDir,
    pub settings: Settings,
    pub state: PathBuf,
    pub fake_dir: PathBuf,
    pub published: PublishedStore,
    /// 偽の Drive の HTTP サーバー（`Env` が持つ。落とすと止まる）。
    pub fake: FakeDrive,
    /// 資格情報のファイル（架空の値）。
    pub credentials: PathBuf,
    /// 宛先 `yamareco`・`travel-journal` の親フォルダと、作業用のフォルダの ID（模型のノード）。
    pub yamareco_id: String,
    pub travel_journal_id: String,
    pub work_id: String,
}

impl Env {
    /// 偽の Drive に、2 つの宛先の親フォルダ（`yamareco`・`travel-journal`）と作業用のフォルダ（`geoframe-export-work`）だけがある状態。
    pub fn new(name: &str) -> Self {
        let root = TempDir::new(name);
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).expect("state");
        let fake_dir = root.path().join("fake-drive");
        let mut d = model::Drive::default();
        let yamareco_id = d.mkdir_p(&["yamareco"]);
        let travel_journal_id = d.mkdir_p(&["travel-journal"]);
        let work_id = d.mkdir_p(&["geoframe-export-work"]);
        d.save(&fake_dir);
        let fake = FakeDrive::start_in(&fake_dir);
        let credentials = fake_drive::write_credentials(&root.path().join("cred"));
        let settings = Settings::from_raw(&RawSettings {
            state_dir: Some(state.to_string_lossy().into_owned()),
            drive_credentials: Some(credentials.to_string_lossy().into_owned()),
            folder_yamareco: Some(yamareco_id.clone()),
            folder_travel_journal: Some(travel_journal_id.clone()),
            work_folder: Some(work_id.clone()),
            drive_endpoint: Some(fake.url()),
            ..RawSettings::default()
        })
        .expect("settings");
        let published = PublishedStore::in_state_dir(&state);
        Self {
            root,
            settings,
            state,
            fake_dir,
            published,
            fake,
            credentials,
            yamareco_id,
            travel_journal_id,
            work_id,
        }
    }

    /// `--drive-endpoint` 以外の、CLI の設定の引数（偽の Drive へ向ける）。
    pub fn cli_settings_args(&self) -> Vec<String> {
        [
            ("--state-dir", self.state.to_string_lossy().into_owned()),
            (
                "--drive-credentials",
                self.credentials.to_string_lossy().into_owned(),
            ),
            ("--folder-yamareco", self.yamareco_id.clone()),
            ("--folder-travel-journal", self.travel_journal_id.clone()),
            ("--work-folder", self.work_id.clone()),
            ("--drive-endpoint", self.fake.url()),
        ]
        .into_iter()
        .flat_map(|(k, v)| [k.to_owned(), v])
        .collect()
    }

    /// **再試行しない**（失敗した要求はそのまま失敗になる）、期限 30 秒の `Remote`。流れ（公開・復旧）の試験は、API の再試行に
    /// 助けられずに、失敗がそのまま復旧の表へ渡ることを確かめる（API 自身の再試行・「済み」の判定は `drive_api.rs` と、
    /// `remote_retrying` を使うフローの故障注入の試験）。
    pub fn remote(&self) -> Remote {
        Remote::new(&self.settings)
            .expect("remote")
            .with_timeouts(Duration::from_secs(30), Duration::from_secs(30))
            .with_retry(no_retry())
    }

    /// 短い待ちで再試行する（5ms から。3 回まで）`Remote`。
    pub fn remote_retrying(&self) -> Remote {
        self.remote().with_retry(fast_retry())
    }

    pub fn layout(&self, dest: Destination, folder: &str, job: &str) -> Layout {
        Layout::new(
            &self.settings,
            dest,
            FolderName::parse(folder).expect("folder"),
            JobId::parse(job).expect("job"),
        )
        .expect("layout")
    }

    pub fn drive(&self) -> model::Drive {
        model::Drive::load(&self.fake_dir)
    }

    pub fn save(&self, d: &model::Drive) {
        d.save(&self.fake_dir);
    }

    pub fn ctl(&self) -> model::Ctl {
        model::Ctl::load(&self.fake_dir)
    }

    pub fn set_shortcut_mode(&self, mode: &str) {
        let mut c = self.ctl();
        c.shortcut_mode = mode.to_owned();
        c.save(&self.fake_dir);
    }

    /// `op` は中立の操作名。
    pub fn add_rule(&self, op: &str, skip: u64, times: u64, effect: Value) {
        let mut c = self.ctl();
        c.rules.push(model::Rule {
            op: op.to_owned(),
            contains: None,
            skip,
            times,
            seen: 0,
            effect,
        });
        c.save(&self.fake_dir);
    }

    /// 全部の呼び出し（Drive の操作と、段階の記録）を、順番どおりに。
    pub fn calls(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.fake_dir.join("calls.log")).unwrap_or_default();
        let mut lines: Vec<&str> = text.lines().collect();
        // 偽のサーバーが書いている途中の最後の行（改行がまだ無い）は、読み飛ばす（次に読めば、読める）
        if !text.is_empty() && !text.ends_with('\n') {
            lines.pop();
        }
        lines
            .into_iter()
            .map(|l| serde_json::from_str(l).expect("calls.log の行"))
            .collect()
    }

    // ---- 中立の記録（実装に依らない）----
    //
    // 操作名は `list`・`read`・`create_folder`・`upload`・`move`・`trash`。偽の Drive の HTTP サーバー（`fake_drive`）は、同じ名前で
    // calls.log に書く（読むだけの `get`・`generate_ids`・`token` も足す）ので、これらを使う試験は、どちらの偽物でもそのまま通る。

    /// 操作名と、段階の記録（`record:<stage>`）を、順番どおりに。
    pub fn ops(&self) -> Vec<String> {
        self.calls()
            .iter()
            .map(|c| match c["kind"].as_str() {
                Some("hook") => format!("record:{}", c["stage"].as_str().unwrap_or("?")),
                _ => c["op"].as_str().unwrap_or("?").to_owned(),
            })
            .collect()
    }

    /// Drive を変えうる操作と、段階の記録だけ（読むだけの `list`・`read`・`get`・`generate_ids`・`token` を除く）。
    /// `get`・`generate_ids`・`token` は、偽の Drive の HTTP サーバー（`fake_drive`）だけが書く。
    pub fn mutating_ops(&self) -> Vec<String> {
        self.ops()
            .into_iter()
            .filter(|c| !READ_ONLY_OPS.contains(&c.as_str()))
            .collect()
    }

    /// 呼ばれた操作名の集合（重複なし。段階の記録は含まない）。
    pub fn ops_used(&self) -> Vec<String> {
        // `op` を持つ行を全部数える（偽の rclone の行も、偽の Drive の HTTP サーバーの行も。`kind` では絞らない。
        // 数え落とすと、「呼ばれていない」の検査が素通りする）。段階の記録（`hook`）の行は `op` を持たない
        let mut v: Vec<String> = self
            .calls()
            .iter()
            .filter_map(|c| c["op"].as_str().map(str::to_owned))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// その操作が呼ばれた回数。
    pub fn op_count(&self, op: &str) -> usize {
        self.calls().iter().filter(|c| c["op"] == op).count()
    }

    /// 起動時の検査（トークンと、3 つのフォルダとその祖先の `get`）のほかに、Drive への要求が無い（一覧も読み出しも変更も無い）。
    #[track_caller]
    pub fn assert_startup_only(&self) {
        for op in self.ops_used() {
            assert!(op == "token" || op == "get", "起動時の検査のほかの要求: {op}");
        }
    }

    /// 起動時の検査と、読むだけの操作（`list`・`read`）のほかに、Drive への要求が無い（変更が無い）。
    #[track_caller]
    pub fn assert_read_only(&self) {
        for op in self.ops_used() {
            assert!(
                matches!(op.as_str(), "token" | "get" | "list" | "read"),
                "読むだけではない要求: {op}"
            );
        }
    }

    /// Drive を変える操作（`create_folder`・`upload`・`move`・`trash`）も、段階の記録も、ひとつも無い。
    #[track_caller]
    pub fn assert_no_mutations(&self) {
        assert_eq!(self.mutating_ops(), Vec::<String>::new());
    }

    // ---- 偽の Drive の HTTP の要求（API の呼び出しの形）----

    /// HTTP の要求（`kind: "http"`）だけ、順番どおりに。
    pub fn http_calls(&self) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|c| c["kind"] == "http")
            .collect()
    }

    /// ファイルの送信の対象（Drive 上のパス）を、順番どおりに。1 つのファイルにつき 1 つ（multipart の 1 回、または resumable の
    /// 始め。チャンク・位置の問い合わせは数えない）。
    pub fn uploaded_subjects(&self) -> Vec<String> {
        self.http_calls()
            .iter()
            .filter(|c| {
                c["op"] == "upload" && matches!(c["phase"].as_str(), Some("multipart" | "init"))
            })
            .map(|c| c["subject"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    /// 呼び出しの形が、約束どおりであること（rclone の引数の検査〔`assert_rclone_safe_calls`〕の置き換え）:
    /// - Drive へのどの要求にも `Authorization` が付く（トークンの入口と、resumable のチャンク・位置の問い合わせ〔セッションの URL が
    ///   認可する〕を除く）。付いていても無効な値ではない。
    /// - 一覧の `q` は `'<ID>' in parents and trashed=false` だけ（名前で絞らない）。
    /// - 完全な削除（`DELETE`・`emptyTrash`）も、中身の更新（`PATCH /upload/…`）も呼ばない（偽の Drive は 405・`unsupported`）。
    /// - ゴミ箱へは、`trashed: true` の更新（`trash`）で移す。
    pub fn assert_api_calls_safe(&self) {
        let calls = self.http_calls();
        assert!(!calls.is_empty(), "HTTP の要求が 1 つも無い");
        for c in &calls {
            let op = c["op"].as_str().unwrap_or("?");
            assert_ne!(op, "unsupported", "完全な削除か中身の更新を呼んだ: {c}");
            let auth = c["authorization"].as_str().unwrap_or("?");
            let session_put = op == "upload" && matches!(c["phase"].as_str(), Some("chunk" | "status"));
            match (op, auth) {
                ("token", "missing") => {}
                (_, "ok") if op != "token" => {}
                (_, "missing") if session_put => {}
                _ => panic!("Authorization が約束と違う（{auth}）: {c}"),
            }
            if op == "list" {
                let q = c["q"].as_str().unwrap_or("");
                let ok = q
                    .strip_prefix('\'')
                    .and_then(|r| r.strip_suffix("' in parents and trashed=false"))
                    .is_some_and(|id| {
                        !id.is_empty()
                            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    });
                assert!(ok, "一覧の q が ID だけの形ではない（名前で絞っている）: {q}");
            }
        }
    }

    // ---- rclone 固有の記録（rclone 版を消すときに、使う試験と一緒に消す）----

    /// 呼ばれた rclone のコマンド `cmd`（`lsjson` など）の引数。
    pub fn rclone_calls(&self, cmd: &str) -> Vec<Vec<String>> {
        self.calls()
            .iter()
            .filter(|c| c["kind"] == "rclone" && c["cmd"] == cmd)
            .map(|c| {
                c["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap().to_owned())
                    .collect()
            })
            .collect()
    }

    /// Drive の `parent`（なければ作る）の直下に、`name` のファイル（中身は 5 バイト）を足す。ファイルの ID を返す。
    pub fn drive_add_file(&self, parent: &[&str], name: &str) -> String {
        let mut d = self.drive();
        let id = d.add_file(parent, name, 5, None);
        self.save(&d);
        id
    }

    // ---- 公開する一式の用意 ----

    /// 作業領域（絶対パスのディレクトリ）に、`files`（相対パスとサイズ）の写真と `manifest.json` を作る。
    pub fn make_set(
        &self,
        name: &str,
        dest: Destination,
        folder: &str,
        files: &[(&str, u64)],
    ) -> Set {
        let dir = self.root.path().join(format!("ws-{name}"));
        std::fs::create_dir_all(&dir).expect("ws");
        let mut publish = Vec::new();
        for (rel, size) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).expect("ws dir");
            // 写真ごとに中身（と MD5）が違うように、パスの文字を繰り返して `size` バイトにする
            let body: Vec<u8> = rel.bytes().cycle().take(*size as usize).collect();
            std::fs::write(&path, &body).expect("ws file");
            publish.push(PublishFile {
                path: (*rel).to_owned(),
                size: *size,
                md5: model::md5_hex(&body),
            });
        }
        let manifest = manifest_json(dest, folder, files);
        let bytes = format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap());
        std::fs::write(dir.join("manifest.json"), &bytes).expect("ws manifest");
        publish.push(PublishFile {
            path: "manifest.json".to_owned(),
            size: bytes.len() as u64,
            md5: model::md5_hex(bytes.as_bytes()),
        });
        Set {
            dir,
            files: publish,
            manifest: bytes,
        }
    }

    /// 宛先に、このサービスが書いたフォルダ（`set` と同じ中身）を作り、公開の記録に足す。フォルダの ID を返す。
    pub fn seed_managed(&self, dest: Destination, folder: &str, set: &Set) -> String {
        let id = self.seed_folder(dest, folder, set);
        self.published
            .upsert(PublishedRecord {
                destination: dest,
                folder: FolderName::parse(folder).unwrap(),
                folder_id: id.clone(),
                job_id: JobId::parse(JOB_B).unwrap(),
                files: Some(set.recorded()),
                previous: None,
            })
            .expect("published/");
        id
    }

    /// 宛先に `set` と同じ中身のフォルダを作る（公開の記録には足さない）。
    pub fn seed_folder(&self, dest: Destination, folder: &str, set: &Set) -> String {
        self.seed_set_at(&[dest.as_str(), folder], set)
    }

    /// Drive の `path` のフォルダに、`set` と同じ中身（写真と manifest）を作る。フォルダの ID を返す。
    pub fn seed_set_at(&self, path: &[&str], set: &Set) -> String {
        let mut d = self.drive();
        let id = d.mkdir_p(path);
        for f in &set.files {
            let (parent, name) = match f.path.rsplit_once('/') {
                Some((dir, name)) => (path.iter().copied().chain([dir]).collect::<Vec<_>>(), name),
                None => (path.to_vec(), f.path.as_str()),
            };
            let content = (f.path == "manifest.json").then(|| set.manifest.clone());
            d.add_file_md5(&parent, name, f.size, content.as_deref(), &f.md5);
        }
        self.save(&d);
        id
    }
}

pub fn manifest_json(dest: Destination, folder: &str, files: &[(&str, u64)]) -> Value {
    json!({
        "generator": GENERATOR,
        "destination": dest.as_str(),
        "folder": folder,
        "photos": files.iter().map(|(rel, size)| json!({"filename": rel, "file_size": size})).collect::<Vec<_>>(),
    })
}

pub struct Set {
    pub dir: PathBuf,
    pub files: Vec<PublishFile>,
    pub manifest: String,
}

impl Set {
    /// 公開の記録（`published/`）に書くファイルごとの MD5。
    pub fn recorded(&self) -> Vec<geoframe_export::published::RecordedFile> {
        self.files
            .iter()
            .map(|f| geoframe_export::published::RecordedFile {
                path: f.path.clone(),
                md5: f.md5.clone(),
            })
            .collect()
    }
}

/// 段階の記録の口（T5 の代わり）。記録を偽の rclone と同じ `calls.log` に追記するので、「記録が Drive の操作より先」を
/// 1 本の順番で確かめられる。`mutate` は、その段階の記録の直後（Drive の操作の前）に Drive の状態を書き換える
/// （オーナーが途中で手を入れた、を作る）。
pub struct Hook {
    log: PathBuf,
    pub records: Vec<Progress>,
    fail_at: Option<(geoframe_export::recovery::Stage, usize)>,
    mutations: Vec<(geoframe_export::recovery::Stage, Box<dyn FnMut()>)>,
    counts: std::collections::HashMap<&'static str, usize>,
}

impl Hook {
    pub fn new(env: &Env) -> Self {
        std::fs::create_dir_all(&env.fake_dir).expect("fake dir");
        Self {
            log: env.fake_dir.join("calls.log"),
            records: Vec::new(),
            fail_at: None,
            mutations: Vec::new(),
            counts: Default::default(),
        }
    }

    /// `stage` の `nth` 回目（1 から）の記録を失敗させる（ジョブの記録に書けなかった）。
    pub fn fail_at(mut self, stage: geoframe_export::recovery::Stage, nth: usize) -> Self {
        self.fail_at = Some((stage, nth));
        self
    }

    pub fn mutate_after(
        mut self,
        stage: geoframe_export::recovery::Stage,
        f: impl FnMut() + 'static,
    ) -> Self {
        self.mutations.push((stage, Box::new(f)));
        self
    }
}

impl ProgressSink for Hook {
    fn record(&mut self, p: &Progress) -> Result<(), ExportError> {
        let n = {
            let c = self.counts.entry(p.stage.as_str()).or_default();
            *c += 1;
            *c
        };
        let line = json!({"kind": "hook", "stage": p.stage.as_str(), "old_id": p.old_id, "incoming_id": p.incoming_id});
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .expect("calls.log");
        writeln!(f, "{line}").expect("log");
        if self.fail_at == Some((p.stage, n)) {
            return Err(ExportError::new(
                geoframe_export::error::ErrorCode::Internal,
                "ジョブの記録を書けません（試験）",
            ));
        }
        self.records.push(p.clone());
        for (stage, f) in &mut self.mutations {
            if *stage == p.stage {
                f();
            }
        }
        Ok(())
    }
}

pub fn path_of(env: &Env, id: &str) -> Option<String> {
    env.drive().path_of(id)
}

// ---- 挙動の差し替え（効果）の作り方 ----

pub fn fail(code: i64) -> Value {
    json!({"kind": "fail", "code": code, "stderr": "fake failure"})
}

pub fn do_then_fail() -> Value {
    json!({"kind": "do_then_fail", "code": 1, "stderr": "fake: done, but the response was lost"})
}

pub fn sleep_ms(ms: u64) -> Value {
    json!({"kind": "sleep", "ms": ms})
}

pub fn garbage(text: &str) -> Value {
    json!({"kind": "garbage", "stdout": text})
}

/// 渡された環境変数・引数が、約束どおりであること（呼ばれた rclone の全部に対して）。
pub fn assert_rclone_safe_calls(env: &Env) {
    let config = env
        .state
        .join("rclone/rclone.conf")
        .to_string_lossy()
        .into_owned();
    let mut expected_keys: Vec<&str> = geoframe_export::rclone::ENV_KEYS.to_vec();
    expected_keys.sort_unstable();
    for call in env.calls().iter().filter(|c| c["kind"] == "rclone") {
        let mut keys: Vec<&str> = call["env"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys, expected_keys,
            "渡した環境変数がこの 3 つだけではありません: {call}"
        );
        assert_eq!(call["env"]["PATH"], geoframe_export::rclone::PATH_VALUE);
        let state = env.state.to_string_lossy().into_owned();
        for k in ["HOME", "XDG_CACHE_HOME"] {
            let v = call["env"][k].as_str().unwrap();
            assert!(v.starts_with(&format!("{state}/rclone/")), "{k}={v}");
        }
        let args: Vec<&str> = call["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        let dashes = args.iter().position(|a| *a == "--").expect("-- がある");
        let flags = &args[..dashes];
        let ci = flags
            .iter()
            .position(|a| *a == "--config")
            .expect("--config");
        assert_eq!(flags[ci + 1], config);
        for f in flags {
            for bad in [
                "--drive-client",
                "--drive-token",
                "--drive-service",
                "--dump",
                "--verbose",
                "--rc",
            ] {
                assert!(!f.starts_with(bad), "{f}");
            }
            assert!(*f != "-v" && *f != "-vv" && *f != "-vvv", "{f}");
            assert!(!f.starts_with("RCLONE_"), "{f}");
        }
        assert!(
            args[dashes + 1..].iter().all(|a| !a.starts_with('-')),
            "位置の引数が flag に見える: {args:?}"
        );
    }
}
