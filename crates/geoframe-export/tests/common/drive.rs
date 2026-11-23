//! T4 の試験の共通部品: 偽の Drive（今は偽の rclone。`fake-rclone/`）の状態を作る・読む・差し替える部品と、呼び出しの記録を
//! 中立の操作名で読む部品（`ops`・`mutating_ops`・`ops_used`・`op_count`）。rclone の引数・環境変数を見る部品
//! （`rclone_calls`・`assert_rclone_safe_calls`）は rclone 固有で、rclone 版を消すときに一緒に消す。
//! 本物の rclone・Drive・ネットワークには触れない。Drive の ID は架空の値（`fid0001` …）。

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
use serde_json::{Value, json};

#[path = "../../fake-rclone/model.rs"]
pub mod model;

use super::TempDir;

pub const JOB_A: &str = "00112233445566778899aabbccddeeff";
pub const JOB_B: &str = "ffeeddccbbaa99887766554433221100";
pub const FOLDER: &str = "2030-03-10";

/// Drive を変えない操作名（`mutating_ops` が除く）。
const READ_ONLY_OPS: [&str; 5] = ["list", "read", "get", "generate_ids", "token"];

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
}

impl Env {
    /// 偽の Drive に、2 つの接頭辞（`yamareco`・`travel-journal`）と作業用のフォルダ（`geoframe-export-work`）だけがある状態。
    pub fn new(name: &str) -> Self {
        let root = TempDir::new(name);
        let state = root.path().join("state");
        std::fs::create_dir_all(state.join("rclone")).expect("state");
        let settings = Settings::from_raw(&RawSettings {
            rclone: Some(env!("CARGO_BIN_EXE_fake-rclone").to_owned()),
            rclone_config: Some(
                state
                    .join("rclone/rclone.conf")
                    .to_string_lossy()
                    .into_owned(),
            ),
            state_dir: Some(state.to_string_lossy().into_owned()),
            ..RawSettings::default()
        })
        .expect("settings");
        let fake_dir = state.join("rclone/fake-drive");
        let mut d = model::Drive::default();
        d.mkdir_p(&["yamareco"]);
        d.mkdir_p(&["travel-journal"]);
        d.mkdir_p(&["geoframe-export-work"]);
        d.save(&fake_dir);
        let published = PublishedStore::in_state_dir(&state);
        Self {
            root,
            settings,
            state,
            fake_dir,
            published,
        }
    }

    pub fn remote(&self) -> Remote {
        Remote::new(&self.settings).with_timeouts(Duration::from_secs(30), Duration::from_secs(30))
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
        std::fs::read_to_string(self.fake_dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
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

    /// Drive を変える操作（`create_folder`・`upload`・`move`・`trash`）も、段階の記録も、ひとつも無い。
    #[track_caller]
    pub fn assert_no_mutations(&self) {
        assert_eq!(self.mutating_ops(), Vec::<String>::new());
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

/// 移動を、`after_lists` 回の一覧のあとに反映させ、呼び出しは失敗で終わらせる。
pub fn late_then_fail(after_lists: u64) -> Value {
    json!({"kind": "late_then_fail", "code": 1, "after_lists": after_lists})
}

pub fn filewise() -> Value {
    json!({"kind": "filewise"})
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
