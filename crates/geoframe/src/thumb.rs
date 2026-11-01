//! 一覧用サムネイル `GET /thumb/<id>.jpg?v=<gen>`（管理画面のライブラリ・選別の一覧が、長辺 2,560px の
//! 表示用画像でなく小さな画像を読むため）。設計は docs/admin-library/design.md の「`GET /thumb/<id>.jpg`」。
//!
//! # 経路と世代
//!
//! - 正規化した後のパスが 1 段だけで `^[1-9][0-9]{0,17}\.jpg$` に合うものだけ（先頭の 0・`0`・末尾の `/`・
//!   2 段以上は 404）。クライアントの文字列は ID（数字）と `v`（12 桁の小文字 hex）以外使わない。
//! - 世代 `gen` は `display | md5 | mtime | THUMB_VERSION` の SHA-256 の先頭 12 hex（[`thumb_generation`]）。
//!   回転・原本の更新で `display` や `mtime` が変わると世代も変わり、URL が変わる。
//! - **生成するのは `GET` で、`v` が DB から計算した今の世代と一致し、書き込み側（writer。`INGEST=0` でない）が
//!   あるときだけ**。`HEAD`・古い `v`・writer 無しは生成しない。外部のページの `<img>` が連番の ID で生成やディスクの
//!   書き込みを起こせないようにする（`gen` は同一オリジンの管理 API を読まないと分からない）。
//!
//! # 置き場と配信
//!
//! 元は DB の `display`（`derivatives/` か `photos/` で始まる字面のもの）か、無ければ `derivatives/<drive_id>.jpg`。
//! 読む元と返すファイルは領域（`derivatives/`・`photos/`・`thumbs/`）の中に限り、`/derivatives/` と同じ検査
//! （[`crate::statics::resolve_area_file`]）を通す。キャッシュは `thumbs/<id>-<gen>.jpg`（`write_atomic`）。
//!
//! # 資源
//!
//! 写真 ID ごとのロック（同じ写真は 1 つだけ作る）→ 生成の枠 [`THUMB_CONCURRENCY`]。ロックと枠を合わせて待ちは
//! 最長 [`THUMB_WAIT_MAX`]・待つ数は最大 [`THUMB_QUEUE_MAX`]、超えたら 503。枠を取った後と生成の
//! 直前にも書き込みの停止を確かめる。取ったロックと枠は、`tokio::spawn` した所有タスクが、生成・書き込み・
//! 今の世代の引き直し・古い世代の掃除・失敗の記憶が終わるまで持つ（要求が drop されても、同じ ID の次の要求が
//! 先に始まらず、復号が枠を超えて走らない。blocking スレッドから非同期の読み取りを待たない）。
//!
//! # 失敗
//!
//! - 復号できない・上限を超える（`decode`/`limit`）: 404（`no-store`）。同じ世代は件数に上限のある記憶に入れ、
//!   起動中はロックも枠も取らずに 404。
//! - 作れたが書けない（`write`）、または writer が無くてキャッシュも無い: 元の表示用画像（jpg/jpeg/png のとき
//!   だけ）を `no-store` で返す。`write` の失敗後 [`THUMB_WRITE_RETRY`] は全体で生成を試みない。
//! - ログは失敗の初回に 1 行（ID と分類だけ。パス・エラーの文面は出さない）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::response::Response;
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::{OwnedMutexGuard, Semaphore};
use tokio::time::Instant;

use crate::datadir::{self, DataDir, check_drive_id, relative_parts};
use crate::imaging::{ImageError, THUMB_LIMITS, THUMB_PX, make_thumbnail, write_atomic};
use crate::server::AppState;
use crate::statics::{self, Denied};
use crate::webpath::Area;

/// サムネイルの作り方（`THUMB_PX`・品質・上限）を変えたときに上げる。古いファイルと ETag が残らない。
pub const THUMB_VERSION: u32 = 1;
/// 同時に作る数。
pub const THUMB_CONCURRENCY: usize = 2;
/// ID のロックと生成の枠を待つ時間の上限（合わせて）。
pub const THUMB_WAIT_MAX: Duration = Duration::from_secs(10);
/// 待っていてよい要求の数。
pub const THUMB_QUEUE_MAX: usize = 32;
/// 失敗の記憶の件数の上限。超えたら古いものから捨てる。
pub const THUMB_FAILED_MAX: usize = 10_000;
/// 書き込みに失敗してから、全体で生成を試みない時間。
pub const THUMB_WRITE_RETRY: Duration = Duration::from_secs(60);
/// 応答の `Cache-Control`（URL に世代があるので長くてよい）。
const CACHE_CONTROL: &str = "private, max-age=604800";

// ---------------------------------------------------------------------------
// 世代

/// 写真の世代（`display`・`md5`・`mtime`・[`THUMB_VERSION`] の SHA-256 の先頭 48 bit）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gen([u8; 6]);

impl Gen {
    /// 12 桁の小文字 hex。
    pub fn hex(self) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(12);
        for b in self.0 {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    /// 12 桁の小文字 hex だけを受ける。
    pub fn parse(text: &str) -> Option<Self> {
        if !datadir::is_thumb_gen(text) {
            return None;
        }
        let mut out = [0u8; 6];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(out))
    }
}

/// 世代を作る。入力は `display | md5 | mtime | THUMB_VERSION` を `|` で連結したもの（NULL は空）。
/// API の `thumb_url`（`/thumb/<id>.jpg?v=<世代>`）も同じ関数で作る。
pub fn thumb_generation(display: Option<&str>, md5: Option<&str>, mtime: Option<f64>) -> Gen {
    let input = format!(
        "{}|{}|{}|{THUMB_VERSION}",
        display.unwrap_or(""),
        md5.unwrap_or(""),
        mtime.map(|m| m.to_string()).unwrap_or_default(),
    );
    let digest = ring::digest::digest(&ring::digest::SHA256, input.as_bytes());
    let mut out = [0u8; 6];
    out.copy_from_slice(&digest.as_ref()[..6]);
    Gen(out)
}

/// `thumbs/` の中のファイル名 `<id>-<gen>.jpg`。
fn cache_name(id: i64, generation: Gen) -> String {
    format!("{id}-{}.jpg", generation.hex())
}

/// 古い世代の掃除の対象の名前か（`^<id>-[0-9a-f]{12}\.jpg$`）。
fn is_cache_name_of(name: &str, id: i64) -> bool {
    name.strip_prefix(&format!("{id}-"))
        .and_then(|rest| rest.strip_suffix(".jpg"))
        .is_some_and(datadir::is_thumb_gen)
}

/// `rest`（`/thumb/` より後ろ）が `^[1-9][0-9]{0,17}\.jpg$` に合うなら ID。
fn parse_id(rest: &str) -> Option<i64> {
    let digits = rest.strip_suffix(".jpg")?;
    let ok = (1..=18).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0');
    if ok { digits.parse().ok() } else { None }
}

/// クエリの `v`（最初のもの）。
fn query_v(query: Option<&str>) -> Option<&str> {
    query?.split('&').find_map(|pair| pair.strip_prefix("v="))
}

// ---------------------------------------------------------------------------
// 状態

/// サムネイルを作る関数（元ファイルのパス → JPEG）。既定は [`make_thumbnail`]。試験が差し替える。
pub type Renderer = Arc<dyn Fn(&Path) -> Result<Vec<u8>, ImageError> + Send + Sync>;

/// 失敗の記憶: `<id>-<gen>` の集合。件数に上限があり、超えたら古いものから捨てる。
struct Failed {
    order: VecDeque<(i64, Gen)>,
    set: HashSet<(i64, Gen)>,
    max: usize,
}

impl Failed {
    fn new(max: usize) -> Self {
        Self {
            order: VecDeque::new(),
            set: HashSet::new(),
            max,
        }
    }

    fn contains(&self, key: &(i64, Gen)) -> bool {
        self.set.contains(key)
    }

    /// 覚える。初めてなら `true`。
    fn insert(&mut self, key: (i64, Gen)) -> bool {
        if !self.set.insert(key) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > self.max {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

type LockTable = Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>;

fn plain<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `AppState` に載せる、サムネイルの共有状態。
pub struct ThumbState {
    /// 写真 ID ごとのロック。表からの削除は、待っている要求が無い（`Arc::strong_count == 1`）ときだけ。
    locks: Arc<LockTable>,
    /// 生成の枠（[`THUMB_CONCURRENCY`]）。
    slots: Arc<Semaphore>,
    /// ロックか枠を待っている要求の数。
    waiting: Arc<AtomicUsize>,
    failed: Mutex<Failed>,
    /// 書き込みの失敗の後、この時刻までは全体で生成を試みない。
    write_failed_until: Mutex<Option<Instant>>,
    renderer: Renderer,
    /// 生成関数を呼んだ回数（試験の観測用。本番の処理は使わない）。
    renders: AtomicUsize,
    wait_max: Duration,
    queue_max: usize,
}

impl Default for ThumbState {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbState {
    pub fn new() -> Self {
        Self::with_renderer(Arc::new(|src: &Path| {
            make_thumbnail(src, THUMB_PX, &THUMB_LIMITS)
        }))
    }

    /// 生成関数を差し替える。結合試験が、本物を呼び出し回数の計測や待ちで包むためのもの。
    pub fn with_renderer(renderer: Renderer) -> Self {
        Self {
            locks: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(Semaphore::new(THUMB_CONCURRENCY)),
            waiting: Arc::new(AtomicUsize::new(0)),
            failed: Mutex::new(Failed::new(THUMB_FAILED_MAX)),
            write_failed_until: Mutex::new(None),
            renderer,
            renders: AtomicUsize::new(0),
            wait_max: THUMB_WAIT_MAX,
            queue_max: THUMB_QUEUE_MAX,
        }
    }

    /// 本物の生成関数をそのまま呼び、呼んだ回数を数える（結合試験用）。
    pub fn renders(&self) -> usize {
        self.renders.load(Ordering::SeqCst)
    }

    /// ID のロック表の大きさ（試験用。待つ要求も持つ要求も無ければ 0）。
    pub fn lock_table_len(&self) -> usize {
        plain(&self.locks).len()
    }

    fn is_failed(&self, key: (i64, Gen)) -> bool {
        plain(&self.failed).contains(&key)
    }

    fn write_blocked(&self) -> bool {
        plain(&self.write_failed_until).is_some_and(|until| Instant::now() < until)
    }
}

/// 待ち数の予約。落とされたら（完了・切断・期限）減らす。
struct WaitTicket(Arc<AtomicUsize>);

impl WaitTicket {
    fn reserve(waiting: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        if waiting.fetch_add(1, Ordering::SeqCst) >= max {
            waiting.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Arc::clone(waiting)))
    }
}

impl Drop for WaitTicket {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 写真 ID のロック。表にある `Arc` の写しを持ち、落とされたとき（切断で future が捨てられた場合も）
/// ロックを放してから、待っている要求が無ければ表の項目を消す。
struct IdLock {
    table: Arc<LockTable>,
    id: i64,
    arc: Option<Arc<tokio::sync::Mutex<()>>>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl IdLock {
    fn new(table: &Arc<LockTable>, id: i64) -> Self {
        let arc = Arc::clone(plain(table).entry(id).or_default());
        Self {
            table: Arc::clone(table),
            id,
            arc: Some(arc),
            guard: None,
        }
    }

    /// ロックを取る（`lock_owned`）。待っている間に落とされても、`Drop` が後始末をする。
    async fn acquire(&mut self) {
        if let Some(arc) = &self.arc {
            self.guard = Some(Arc::clone(arc).lock_owned().await);
        }
    }
}

impl Drop for IdLock {
    fn drop(&mut self) {
        // 先にロックと自分の写しを手放す。持ったまま数えると常に 2 以上で消えない
        self.guard = None;
        self.arc = None;
        let mut table = plain(&self.table);
        if table
            .get(&self.id)
            .is_some_and(|arc| Arc::strong_count(arc) == 1)
        {
            table.remove(&self.id);
        }
    }
}

// ---------------------------------------------------------------------------
// 生成

/// 1 枚の生成の依頼。
pub(crate) struct Job {
    pub id: i64,
    pub generation: Gen,
    /// 領域の検査を済ませた元ファイル（解決後の絶対パス）。
    pub src: PathBuf,
    /// 解決済みのデータ置き場。
    pub data_root: PathBuf,
    /// 今の DB の世代（古い世代を消す直前に、非同期側で引き直す）。引けなければ `None`。
    pub current_gen: CurrentGen,
}

/// 今の DB の世代を引く関数（呼ぶたびに新しい future を返す）。
pub(crate) type CurrentGen =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Gen>> + Send>> + Send + Sync>;

/// 失敗の分類（ログと記憶）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    Decode,
    Limit,
    Write,
}

impl Fail {
    fn label(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Limit => "limit",
            Self::Write => "write",
        }
    }
}

/// 生成の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 作って置いた。
    Generated,
    /// 待っているあいだに他の要求が作っていた。
    Cached,
    /// 復号できない・上限超過（記憶済みを含む）。
    Rejected,
    /// 元ファイルを読めなかった（入出力。覚えない）。
    Unreadable,
    /// 書けなかった（直近の失敗で試みなかった場合を含む）。元の画像を返す。
    WriteFailed,
    /// 待ちの数か時間の上限を超えた。
    Busy,
}

impl ThumbState {
    /// 同じ写真を 1 つだけ作る。ID のロック → 再確認（キャッシュ・失敗の記憶・書き込みの停止）→ 生成の枠 →
    /// 枠を取った後にもう一度、書き込みの停止を確かめる → 所有タスク。ロックと枠は所有タスクが、
    /// 生成・世代の引き直し・古い世代の掃除が終わるまで持つ。
    pub(crate) async fn produce(self: &Arc<Self>, job: Job) -> Outcome {
        let key = (job.id, job.generation);
        let Some(ticket) = WaitTicket::reserve(&self.waiting, self.queue_max) else {
            return Outcome::Busy;
        };
        let deadline = Instant::now() + self.wait_max;
        let mut lock = IdLock::new(&self.locks, job.id);
        if tokio::time::timeout_at(deadline, lock.acquire())
            .await
            .is_err()
        {
            return Outcome::Busy;
        }
        // 待っているあいだに、他の要求が作った・失敗した・書けなくなった
        if self.is_failed(key) {
            return Outcome::Rejected;
        }
        if cache_exists(&job.data_root, job.id, job.generation).await {
            return Outcome::Cached;
        }
        if self.write_blocked() {
            return Outcome::WriteFailed;
        }
        let permit = match tokio::time::timeout_at(
            deadline,
            Arc::clone(&self.slots).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            _ => return Outcome::Busy,
        };
        // 枠を待っているあいだに、先行の生成が書けなくなったかもしれない
        if self.write_blocked() {
            return Outcome::WriteFailed;
        }
        drop(ticket);
        let this = Arc::clone(self);
        // 要求の future が捨てられても、始まった生成は最後まで走る。ロック（`OwnedMutexGuard`）と枠
        // （`OwnedSemaphorePermit`）は所有タスクが持ち、掃除が終わってから放す
        let task = tokio::spawn(async move {
            let _held = (lock, permit);
            this.generate_and_sweep(Arc::new(job)).await
        });
        match task.await {
            Ok(outcome) => outcome,
            // 所有タスクのパニック。覚えず、元の画像を返す
            Err(_) => Outcome::WriteFailed,
        }
    }

    /// 所有タスクの中。生成と書き込み（blocking）→ 今の世代の引き直し（非同期）→ 古い世代の掃除（blocking）。
    async fn generate_and_sweep(self: Arc<Self>, job: Arc<Job>) -> Outcome {
        let (this, blocking_job) = (Arc::clone(&self), Arc::clone(&job));
        match tokio::task::spawn_blocking(move || this.render_and_write(&blocking_job)).await {
            Ok(Ok(())) => {}
            Ok(Err(outcome)) => return outcome,
            // 生成の中のパニック。覚えず、元の画像を返す
            Err(_) => return Outcome::WriteFailed,
        }
        let current = (job.current_gen)().await;
        let _ = tokio::task::spawn_blocking(move || sweep_old(&job, current)).await;
        Outcome::Generated
    }

    /// `spawn_blocking` の中。書けたら `Ok(())`、そうでなければ返す結果。
    fn render_and_write(&self, job: &Job) -> Result<(), Outcome> {
        // blocking pool で順番を待つあいだに、他の生成が書けなくなったかもしれない
        if self.write_blocked() {
            return Err(Outcome::WriteFailed);
        }
        self.renders.fetch_add(1, Ordering::SeqCst);
        let bytes = match (self.renderer)(&job.src) {
            Ok(bytes) => bytes,
            Err(ImageError::Io(_)) => return Err(Outcome::Unreadable),
            Err(ImageError::FileTooLarge | ImageError::TooManyPixels { .. }) => {
                return Err(self.remember_failure(job, Fail::Limit));
            }
            Err(_) => return Err(self.remember_failure(job, Fail::Decode)),
        };
        let dest = DataDir::open(&job.data_root)
            .and_then(|data| data.thumb_dest(job.id, &job.generation.hex()));
        match dest {
            Ok(dest) if write_atomic(&dest, &bytes).is_ok() => Ok(()),
            _ => Err(self.remember_write_failure(job)),
        }
    }

    fn remember_failure(&self, job: &Job, kind: Fail) -> Outcome {
        if plain(&self.failed).insert((job.id, job.generation)) {
            tracing::warn!(
                "[thumb] サムネイルを作れません（id={} 分類={}）",
                job.id,
                kind.label()
            );
        }
        Outcome::Rejected
    }

    fn remember_write_failure(&self, job: &Job) -> Outcome {
        let mut until = plain(&self.write_failed_until);
        let first = until.is_none_or(|t| Instant::now() >= t);
        *until = Some(Instant::now() + THUMB_WRITE_RETRY);
        if first {
            tracing::warn!(
                "[thumb] サムネイルを書けません（id={} 分類={}）。{}秒は生成を試みません",
                job.id,
                Fail::Write.label(),
                THUMB_WRITE_RETRY.as_secs()
            );
        }
        Outcome::WriteFailed
    }
}

async fn cache_exists(data_root: &Path, id: i64, generation: Gen) -> bool {
    let path = data_root.join("thumbs").join(cache_name(id, generation));
    tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|meta| meta.is_file())
}

/// 古い世代の掃除。`thumbs/` の実体がそのディレクトリ自身であること、通常のファイルであること、名前が
/// `^<id>-[0-9a-f]{12}\.jpg$` で今の世代でないことを確かめ、**自分の世代が今の世代のときだけ**消す
/// （回転の直前に始めた生成が、回転後の世代を消さない）。`current` は生成のあとに非同期側で引き直した
/// DB の世代（引けなければ `None` で、消さない）。
fn sweep_old(job: &Job, current_gen: Option<Gen>) {
    let Ok(dir) = DataDir::open(&job.data_root).and_then(|data| data.thumbs_dir()) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let current = cache_name(job.id, job.generation);
    let stale: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            name.to_str()
                .is_some_and(|name| name != current && is_cache_name_of(name, job.id))
        })
        .filter(|entry| fs::symlink_metadata(entry.path()).is_ok_and(|meta| meta.is_file()))
        .map(|entry| entry.path())
        .collect();
    if stale.is_empty() || current_gen != Some(job.generation) {
        return;
    }
    for path in stale {
        let _ = fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// 要求の処理

/// DB から引く列。
struct PhotoRow {
    display: Option<String>,
    drive_id: Option<String>,
    md5: Option<String>,
    mtime: Option<f64>,
}

impl PhotoRow {
    fn generation(&self) -> Gen {
        thumb_generation(self.display.as_deref(), self.md5.as_deref(), self.mtime)
    }
}

fn lookup(conn: &Connection, id: i64) -> rusqlite::Result<Option<PhotoRow>> {
    conn.query_row(
        "SELECT display, drive_id, md5, mtime FROM photos WHERE id=?1",
        [id],
        |row| {
            Ok(PhotoRow {
                display: row.get(0)?,
                drive_id: row.get(1)?,
                md5: row.get(2)?,
                mtime: row.get(3)?,
            })
        },
    )
    .optional()
}

/// 元ファイルの置き場所（領域とその中の要素）。
#[derive(Debug, PartialEq, Eq)]
struct Source {
    area: Area,
    rel: Vec<String>,
}

/// `thumb_for`（管理 API）と同じ規則で元ファイルを決める。`display` は `derivatives/` か `photos/` で始まり、
/// 字面の検査（`..`・絶対パス・NUL・ドット始まりの要素）を通るものだけ。`display` が無いときは
/// `drive_id` から `derivatives/<drive_id>.jpg`。決められなければ `None`（404）。
fn source_of(row: &PhotoRow) -> Option<Source> {
    if let Some(display) = row.display.as_deref().filter(|d| !d.is_empty()) {
        let parts = relative_parts(display).ok()?;
        let area = match *parts.first()? {
            "derivatives" => Area::Derivatives,
            "photos" => Area::Photos,
            _ => return None,
        };
        let rel: Vec<String> = parts[1..].iter().map(|part| (*part).to_owned()).collect();
        if rel.is_empty() || rel.iter().any(|part| part.starts_with('.')) {
            return None;
        }
        return Some(Source { area, rel });
    }
    let drive_id = row.drive_id.as_deref().filter(|d| !d.is_empty())?;
    check_drive_id(drive_id).ok()?;
    Some(Source {
        area: Area::Derivatives,
        rel: vec![format!("{drive_id}.jpg")],
    })
}

/// 404（`no-store`。作れなかった結果を、ブラウザの経路に覚えさせない）。
fn miss() -> Response {
    no_store(statics::not_found())
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// 成功した応答に付ける、同一オリジン限定の印。
fn same_origin(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response
}

fn cached_headers(mut response: Response, etag: &str) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("ETag は ASCII"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    same_origin(response)
}

/// キャッシュの検索の結果。
enum Lookup {
    Hit(Response),
    Miss,
    /// 返して終わり（領域の外を指すリンク 403・配信の枠が無い 503）。
    Stop(Response),
}

async fn serve_cached(
    state: &AppState,
    id: i64,
    generation: Gen,
    method: &Method,
    headers: &HeaderMap,
) -> Lookup {
    let name = cache_name(id, generation);
    let real =
        match statics::resolve_area_file(&state.data_root, Area::Thumbs, &[name.as_str()], false)
            .await
        {
            Ok(real) => real,
            Err(Denied::NotFound) => return Lookup::Miss,
            Err(Denied::Forbidden) => return Lookup::Stop(statics::forbidden()),
        };
    let etag = format!("\"{}\"", generation.hex());
    if statics::etag_matches(headers, &etag) {
        return Lookup::Hit(cached_headers(statics::not_modified(), &etag));
    }
    let Ok(permit) = Arc::clone(&state.static_slots).try_acquire_owned() else {
        return Lookup::Stop(no_store(statics::busy()));
    };
    let Ok(file) = tokio::fs::File::open(&real).await else {
        return Lookup::Miss;
    };
    let Ok(meta) = file.metadata().await else {
        return Lookup::Miss;
    };
    Lookup::Hit(cached_headers(
        statics::file_response(
            method,
            "image/jpeg",
            meta.len(),
            statics::file_body(file, permit),
            meta.modified().ok(),
            None,
        ),
        &etag,
    ))
}

/// サムネイルを返せないとき、元の表示用画像（jpg/jpeg/png だけ）をそのまま返す。`no-store`。`Last-Modified`・304 は付けない。
async fn serve_original(
    state: &AppState,
    source: &Source,
    real: &Path,
    method: &Method,
) -> Response {
    let name = source.rel.last().map_or("", String::as_str);
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    if !matches!(ext.as_deref(), Some("jpg" | "jpeg" | "png")) {
        return miss();
    }
    let Ok(permit) = Arc::clone(&state.static_slots).try_acquire_owned() else {
        return no_store(statics::busy());
    };
    let Ok(file) = tokio::fs::File::open(real).await else {
        return miss();
    };
    let Ok(meta) = file.metadata().await else {
        return miss();
    };
    no_store(same_origin(statics::file_response(
        method,
        statics::content_type(name),
        meta.len(),
        statics::file_body(file, permit),
        None,
        None,
    )))
}

/// `GET`/`HEAD /thumb/<rest>?v=<gen>`。`rest` は正規化したパスの `/thumb/` より後ろ。
pub(crate) async fn respond(
    state: &AppState,
    rest: &str,
    trailing_slash: bool,
    query: Option<&str>,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    // DB を引く前に断れるもの（形・版・Fetch Metadata）
    let Some(id) = parse_id(rest).filter(|_| !trailing_slash) else {
        return miss();
    };
    let Some(requested) = query_v(query).and_then(Gen::parse) else {
        return miss();
    };
    let cross_site = headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| matches!(v, "cross-site" | "same-site"));
    if cross_site {
        return no_store(statics::forbidden());
    }

    let row = match state.catalog.read(move |conn| lookup(conn, id)).await {
        Ok(Some(row)) => row,
        Ok(None) => return miss(),
        Err(_) => return no_store(statics::busy()),
    };
    let Some(source) = source_of(&row) else {
        return miss();
    };
    let generation = row.generation();
    let rel: Vec<&str> = source.rel.iter().map(String::as_str).collect();
    let src = match statics::resolve_area_file(&state.data_root, source.area, &rel, false).await {
        Ok(real) => real,
        Err(Denied::NotFound) => return miss(),
        Err(Denied::Forbidden) => return no_store(statics::forbidden()),
    };

    match serve_cached(state, id, generation, method, headers).await {
        Lookup::Hit(response) | Lookup::Stop(response) => return response,
        Lookup::Miss => {}
    }

    // ここから先は「作る」話。`HEAD`・古い `v` は作らない（キャッシュが無ければ 404）
    if *method != Method::GET || requested != generation {
        return miss();
    }
    let thumbs = &state.thumbs;
    if thumbs.is_failed((id, generation)) {
        return miss();
    }
    if state.catalog.writer().is_none() || thumbs.write_blocked() {
        return serve_original(state, &source, &src, method).await;
    }

    let catalog = state.catalog.clone();
    let job = Job {
        id,
        generation,
        src: src.clone(),
        data_root: state.data_root.as_ref().clone(),
        // 所有タスク（非同期側）が、生成のあとに呼ぶ
        current_gen: Arc::new(move || {
            let catalog = catalog.clone();
            Box::pin(async move {
                catalog
                    .read(move |conn| lookup(conn, id))
                    .await
                    .ok()
                    .flatten()
                    .map(|row| row.generation())
            })
        }),
    };
    match thumbs.produce(job).await {
        Outcome::Generated | Outcome::Cached => {
            match serve_cached(state, id, generation, method, headers).await {
                Lookup::Hit(response) | Lookup::Stop(response) => response,
                Lookup::Miss => miss(),
            }
        }
        Outcome::Rejected | Outcome::Unreadable => miss(),
        Outcome::WriteFailed => serve_original(state, &source, &src, method).await,
        Outcome::Busy => no_store(statics::busy()),
    }
}

#[cfg(test)]
mod tests;
