//! 背景地図タイルのプロキシ（E11 `GET/HEAD /tiles/{osm|gsi}/{z}/{x}/{y}.png`）と、そのファイルキャッシュの掃除
//! （B4）。現行 `tiles.py` の `fetch_tile`・`_tile_fetch_allowed`・`tile_cache_prune` と `handlers.py` の
//! `_serve_tile`。仕様と Python との差は docs/rust-migration/server.md §19。
//!
//! # 規則（現行と同じ）
//!
//! - パスは正規表現 `^/tiles/(osm|gsi)/(\d{1,2})/(\d+)/(\d+)\.png$` と同じ形（[`parse_path`]）。**生のパス**
//!   （クエリを落とし、先頭の `//` を畳んだだけ）で判定し、`webpath::normalize` を通さない。座標が `2^z` 以上、
//!   ズームが最大（OSM 19・GSI 18）超過、形が合わない、は 404。それ以外は**常に 200**: 取得の失敗・流量の超過も
//!   同じ透明 PNG（[`TRANSPARENT_TILE`]。Pillow が作る 256×256 の RGBA と同じバイト列）で返す。
//! - `X-Tile-Cache`: `hit`（キャッシュから）・`miss`（上流から取得して保存。上流の 404 は透明 PNG を保存して
//!   これ）・`error`（上流の失敗。保存しない）・`throttled`（流量の上限。上流へ行かない・保存しない）。
//!   `error`・`throttled` は `Cache-Control: no-store`、他は `public, max-age=86400`。`HEAD` も同じ処理（上流の
//!   取得・保存・窓の消費まで）で、本文だけ省く。
//! - キャッシュは `DATA_DIR/tile_cache/<層>/<z>/<x>/<y>.png`（数値は先頭ゼロなしの十進）。ヒットは窓に数えない。
//!   ヒットのときに上流へは行かない。
//! - 上流の取得は**全体で毎分 [`DEFAULT_FETCH_PER_MIN`] 枚**（`GEOFRAME_TILE_FETCH_PER_MIN`。層をまたいで 1 つの窓。
//!   直近 60 秒）。判定と記録は同じ排他の中で行い、**窓の判定は同時数の枠を待つ前**。上流への同時接続は OSM 2・
//!   GSI 4。上流の応答は、2xx なら本文が PNG のマジック（8 バイト）で始まること（Content-Type は見ない。HTML や
//!   JPEG は失敗）、404 は透明 PNG を保存、それ以外の状態コード・通信の失敗は `error`。
//! - 上流へ送るヘッダは `User-Agent`（`GEOFRAME_TILE_UA`）と `Accept: image/png` だけ（`Accept-Encoding: identity`
//!   は現行の urllib が付けるものと同じ）。1 要求 10 秒。
//!
//! # 現行との違い
//!
//! - 応答の本文は**展開後で** [`MAX_TILE_BYTES`] まで（現行は上限なし）。超えたら `error`。gzip の本文も展開後の
//!   大きさで数える（[`crate::net::read_body_limited`]）。
//! - ヒットは**中身が PNG のマジックで始まる通常ファイル**だけ（現行は何でも返す）。空・壊れたファイル・
//!   リンク・上限を超えるものは、ヒットにせず取り直す。
//! - キャッシュは**リンクを辿らない**: `tile_cache` から各階層が、リンクでない実ディレクトリでなければ使わない
//!   （読めず・書けず、上流から取る）。ファイルは一時ファイル（`.part`、`create_new` + `O_NOFOLLOW`）に書いて
//!   `rename` する。
//! - 流量の窓は単調時計（現行は `time.time()`）。
//! - 掃除は独立のスレッド（[`spawn_pruner`]）で、起動時に 1 回、以後 1 時間ごと。取り込みの有無に関わらず動く。
//!   `tile_cache` の中の通常ファイルだけを数え・消す（リンクは辿らない・消さない）。
//! - ログにタイルの座標・URL を出さない。
//!
//! # ログ
//!
//! 失敗は `[tile] 上流取得失敗（osm）: http 503` の形（層・失敗の種類・状態コード）まで。座標（z/x/y）・URL・
//! User-Agent は出さない。

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use crate::config::Config;
use crate::limits::TILE_FILE_SLOTS;
use crate::net::{self, Failure};
use crate::stop::StopFlag;

/// PNG の先頭 8 バイト（現行の `PNG_MAGIC`）。
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// 256×256 の透明 PNG。現行の `_transparent_tile()`（Pillow の `Image.new("RGBA", (256, 256), (0, 0, 0, 0))` を
/// PNG にしたもの）と同じバイト列。フィクスチャ（`tile-python.json`）と突き合わせる。
pub const TRANSPARENT_TILE: &[u8] = include_bytes!("tiles/transparent_tile.png");

/// 上流取得の毎分の上限の既定（`GEOFRAME_TILE_FETCH_PER_MIN`）。
pub const DEFAULT_FETCH_PER_MIN: i64 = crate::config::DEFAULT_TILE_FETCH_PER_MIN;
/// 流量の窓の長さ。
pub const WINDOW: Duration = Duration::from_secs(60);
/// 上流への 1 要求の時間（現行の `urlopen(timeout=10)`。Rust は名前解決から本文の読み切りまで）。
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);
/// タイル 1 枚の大きさの上限（展開後。実物は数 KB〜数十 KB）。上流の応答にも、キャッシュの読み出しにも使う。
pub const MAX_TILE_BYTES: u64 = 2 << 20;
/// 掃除の間隔。
pub const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);
/// この時間より古い `.part`（書きかけ。異常終了の残り）は掃除で消す。
const STALE_PART_AGE: Duration = Duration::from_secs(3600);
/// 一時ファイルの名前を作り直す回数の上限。
const PART_ATTEMPTS: usize = 8;

const TILE_PREFIX: &str = "/tiles/";
/// Python の `int()` が文字列から読む十進の桁数の上限（`sys.int_info.default_max_str_digits`。先頭ゼロも数える）。
const INT_MAX_STR_DIGITS: usize = 4300;
/// 上流のタイルの URL（現行の `TILE_SOURCES`）。`{z}`・`{x}`・`{y}` を埋めて使う。
const OSM_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
const GSI_URL: &str = "https://cyberjapandata.gsi.go.jp/xyz/pale/{z}/{x}/{y}.png";

/// 地図の層。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    Osm,
    Gsi,
}

impl Layer {
    pub const ALL: [Layer; 2] = [Layer::Osm, Layer::Gsi];

    pub fn name(self) -> &'static str {
        match self {
            Self::Osm => "osm",
            Self::Gsi => "gsi",
        }
    }

    /// 最大ズーム（現行の `TILE_SOURCES` の 2 番目）。
    pub fn max_zoom(self) -> u32 {
        match self {
            Self::Osm => 19,
            Self::Gsi => 18,
        }
    }

    /// 上流への同時接続数（現行の `TILE_SOURCES` の 3 番目）。
    pub fn concurrency(self) -> usize {
        match self {
            Self::Osm => 2,
            Self::Gsi => 4,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Osm => 0,
            Self::Gsi => 1,
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|layer| layer.name() == name)
    }
}

/// 検査を通ったタイルの座標。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub layer: Layer,
    pub z: u32,
    pub x: u64,
    pub y: u64,
}

fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// 現行の `TILE_RE` と `_serve_tile` の範囲検査。形が合わない・座標が `2^z` 以上・ズームが最大を超えるものは
/// `None`（404）。`path` はクエリを落とした生のパス。`\d` は ASCII の数字（リクエスト行は Latin-1 で読むので、
/// 他の 10 進数字は現れない）。`x`・`y` の桁数（先頭ゼロを含む）が 4300 を超えるものは 404（現行は `int()` が
/// `ValueError` になり、応答を返さず接続が切れる。Python 3.11 以降の既定の上限で、4300 桁ちょうどは通る）。
/// `u64` に収まらない値は、最大ズームでも `2^z` 未満になり得ないので 404。
pub fn parse_path(path: &str) -> Option<Tile> {
    let rest = path.strip_prefix(TILE_PREFIX)?;
    let mut parts = rest.split('/');
    let (layer, z, x, file) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let layer = Layer::parse(layer)?;
    let y = file.strip_suffix(".png")?;
    if !(1..=2).contains(&z.len()) || !all_digits(z) || !all_digits(x) || !all_digits(y) {
        return None;
    }
    if x.len() > INT_MAX_STR_DIGITS || y.len() > INT_MAX_STR_DIGITS {
        return None;
    }
    let z: u32 = z.parse().ok()?;
    // 先頭ゼロを足した値も `u64` に収まれば通す（`0` を 4300 個並べた座標は 0）
    let (x, y): (u64, u64) = (x.parse().ok()?, y.parse().ok()?);
    // 範囲 → ズームの順（現行と同じ。どちらも 404 なので順序は見えない）。`2^z` は z ≤ 99 でも u128 に収まる
    let side = 1u128 << z;
    if u128::from(x) >= side || u128::from(y) >= side || z > layer.max_zoom() {
        return None;
    }
    Some(Tile { layer, z, x, y })
}

/// `X-Tile-Cache` の値。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
    Error,
    Throttled,
}

impl CacheStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Error => "error",
            Self::Throttled => "throttled",
        }
    }

    /// `Cache-Control`。`error`・`throttled` は次回に再試行させる（`no-store`）。
    pub fn cache_control(self) -> &'static str {
        match self {
            Self::Error | Self::Throttled => "no-store",
            Self::Hit | Self::Miss => "public, max-age=86400",
        }
    }
}

/// 上流の URL のひな形。
///
/// 本番のバイナリ（`main.rs`・`cli.rs`）は [`TileEndpoints::production`]（定数）しか使わず、設定・環境変数・
/// コマンドラインから通信先を変えられない。結合テストが使う [`TileEndpoints::loopback`] は、**ループバックの
/// アドレス以外を受け付けない**（`None`）。本番のコードが入口を呼ばないことは試験で見る
/// （`geocode::tests::production_code_never_redirects_the_geocoder`）。
#[derive(Debug, Clone)]
pub struct TileEndpoints {
    osm: String,
    gsi: String,
}

impl TileEndpoints {
    pub fn production() -> Self {
        Self {
            osm: OSM_URL.to_string(),
            gsi: GSI_URL.to_string(),
        }
    }

    /// ループバックのスタブへ向ける（結合テスト用。パスは `/<層>/<z>/<x>/<y>.png`）。ループバック以外は `None`。
    pub fn loopback(addr: SocketAddr) -> Option<Self> {
        addr.ip()
            .is_loopback()
            .then(|| Self::at(&format!("http://{addr}")))
    }

    /// 試験用: ループバックのスタブへ向ける。**試験のビルドだけにある**。
    #[cfg(test)]
    pub(crate) fn stub(base: &str) -> Self {
        Self::at(base)
    }

    fn at(base: &str) -> Self {
        Self {
            osm: format!("{base}/osm/{{z}}/{{x}}/{{y}}.png"),
            gsi: format!("{base}/gsi/{{z}}/{{x}}/{{y}}.png"),
        }
    }

    pub fn url(&self, tile: Tile) -> String {
        let template = match tile.layer {
            Layer::Osm => &self.osm,
            Layer::Gsi => &self.gsi,
        };
        template
            .replace("{z}", &tile.z.to_string())
            .replace("{x}", &tile.x.to_string())
            .replace("{y}", &tile.y.to_string())
    }
}

/// 時計（流量の窓）。本番は単調時計、試験は進みを自分で決める偽の時計。
pub(crate) trait Clock: Send + Sync {
    /// 任意の起点からの経過（単調）。
    fn now(&self) -> Duration;
}

struct SystemClock {
    origin: Instant,
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

#[derive(Default)]
struct WindowState {
    /// 直近の上流取得を許可した時刻（古い順）。
    times: VecDeque<Duration>,
    /// 抑制中。開始と解除で 1 行ずつだけログを出すために持つ。
    throttled: bool,
}

/// 上流取得のスライディング窓（直近 [`WINDOW`]）。層をまたいで 1 つ（層ごとに分けると、層を交互に叩くだけで
/// 上限が倍になる）。判定と記録は同じ排他の中で行う（分けると、同時のミスが全部通り抜ける）。排他は窓の更新だけに
/// 使い、上流への通信は必ず排他の外で行う。
struct FetchWindow {
    per_min: i64,
    state: Mutex<WindowState>,
    clock: Box<dyn Clock>,
}

impl FetchWindow {
    fn new(per_min: i64, clock: Box<dyn Clock>) -> Self {
        Self {
            per_min,
            state: Mutex::new(WindowState::default()),
            clock,
        }
    }

    /// 上流取得を 1 件許可してよいか。許可したら今を記録する。
    fn allow(&self) -> bool {
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while let Some(&oldest) = state.times.front()
            && now.saturating_sub(oldest) >= WINDOW
        {
            state.times.pop_front();
        }
        if i64::try_from(state.times.len()).is_ok_and(|len| len >= self.per_min) {
            if !state.throttled {
                state.throttled = true;
                tracing::warn!(
                    "[tile] 上流フェッチ抑制 開始: 直近60秒で{}枚に到達（GEOFRAME_TILE_FETCH_PER_MIN で調整可）",
                    self.per_min
                );
            }
            return false;
        }
        if state.throttled {
            state.throttled = false;
            tracing::info!("[tile] 上流フェッチ抑制 解除");
        }
        state.times.push_back(now);
        true
    }
}

/// 上流の取得の結果。
enum Fetched {
    /// PNG の本文。
    Png(Vec<u8>),
    /// 404（提供範囲外は恒久的。透明 PNG を保存して以後問い合わせない）。
    Missing,
}

/// 上流の取得の失敗（ログに出してよい語だけを持つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    Net(Failure),
    /// 2xx・404 以外の状態コード（リダイレクトをたどった後の 3xx を含む）。
    Status(u16),
    /// 応答は届いたが使えない（PNG でない・大きすぎる）。
    Bad,
}

impl Fail {
    fn describe(self) -> String {
        match self {
            Self::Net(failure) => failure.as_str().to_string(),
            Self::Status(status) => format!("http {status}"),
            Self::Bad => "bad_response".to_string(),
        }
    }
}

/// 本文を読む最中の `ureq` のエラー。通信の障害の種類はそのまま、それ以外は本文が読めなかったことにする。
fn body_failure(e: &ureq::Error) -> Failure {
    match Failure::from(e) {
        failure if failure.is_outage() => failure,
        _ => Failure::BodyUnreadable,
    }
}

/// `tile_cache/<層>/<z>/<x>/<y>.png` のファイルキャッシュ。**リンクを辿らない**: `tile_cache` から各階層までが
/// リンクでない実ディレクトリでなければ、読みも書きもしない。
struct TileCache {
    root: PathBuf,
}

/// 書きかけの一時ファイル。`committed` にしないまま捨てると消える。
struct PartFile {
    path: PathBuf,
    committed: bool,
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn part_name(y: u64) -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut random = [0u8; 8];
    // 乱数が取れない環境では 0 のまま（pid と、衝突時の作り直しに任せる）
    let _ = SystemRandom::new().fill(&mut random);
    format!(
        "{y}.{}.{:016x}.part",
        std::process::id(),
        u64::from_be_bytes(random)
    )
}

/// `path` がリンクでない実ディレクトリであること。無ければ（`create` なら）作る。
fn check_dir(path: &Path, create: bool) -> std::io::Result<()> {
    for attempt in 0..2 {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_dir() => return Ok(()),
            Ok(_) => return Err(std::io::ErrorKind::NotADirectory.into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create && attempt == 0 => {
                match fs::create_dir(path) {
                    Ok(()) => {}
                    // 同時に別の要求が作った。もう一度確かめる
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::ErrorKind::NotADirectory.into())
}

impl TileCache {
    /// `tile_cache` の根から `<層>/<z>/<x>` までを確かめて（`create` なら作って）、その絶対パスを返す。
    fn dir_of(&self, tile: Tile, create: bool) -> std::io::Result<PathBuf> {
        check_dir(&self.root, create)?;
        let mut dir = self.root.clone();
        for part in [
            tile.layer.name().to_string(),
            tile.z.to_string(),
            tile.x.to_string(),
        ] {
            dir.push(part);
            check_dir(&dir, create)?;
        }
        Ok(dir)
    }

    /// キャッシュのタイル。通常のファイルで、上限以下で、PNG のマジックで始まるものだけ。
    fn read(&self, tile: Tile) -> Option<Vec<u8>> {
        let path = self
            .dir_of(tile, false)
            .ok()?
            .join(format!("{}.png", tile.y));
        // `O_NOFOLLOW`: 最後の要素がリンクなら開かない。`O_NONBLOCK`: FIFO を開いて固まらない
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_TILE_BYTES {
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).ok()?);
        file.take(MAX_TILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 <= MAX_TILE_BYTES && bytes.starts_with(PNG_MAGIC)).then_some(bytes)
    }

    /// 一時ファイルに書いて `rename` で差し替える。同じタイルへの同時の書き込みは、それぞれ別名の一時ファイルを
    /// 経由するので、最後に差し替えたものが残り、書きかけは見えない。失敗しても自分の一時ファイルは残さない。
    fn write(&self, tile: Tile, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::ErrorKind;
        let dir = self.dir_of(tile, true)?;
        let dest = dir.join(format!("{}.png", tile.y));
        let mut last = ErrorKind::AlreadyExists;
        for _ in 0..PART_ATTEMPTS {
            let path = dir.join(part_name(tile.y));
            let mut file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    last = e.kind();
                    continue;
                }
                Err(e) => return Err(e),
            };
            let mut part = PartFile {
                path,
                committed: false,
            };
            file.write_all(bytes)?;
            drop(file);
            fs::rename(&part.path, &dest)?;
            part.committed = true;
            return Ok(());
        }
        Err(last.into())
    }
}

/// 地図タイルのプロキシ。プロセスで 1 つ（流量の窓と同時数をプロセス全体で持つ）。
pub struct TileProxy {
    endpoints: TileEndpoints,
    agent: ureq::Agent,
    ua: String,
    cache: TileCache,
    window: FetchWindow,
    /// 層ごとの上流への同時接続の枠（[`Layer::index`] の順）。
    upstream_slots: [Arc<Semaphore>; 2],
    /// キャッシュのファイルを同時に読む数の枠（記述子を握る数の上限。待たせるだけで断らない）。
    read_slots: Arc<Semaphore>,
    cache_max_mb: i64,
    /// 上流への 1 要求の時間（本番は [`UPSTREAM_TIMEOUT`]。試験は短くする）。
    timeout: Duration,
}

impl TileProxy {
    pub fn production(config: &Config, data_root: &Path) -> Self {
        Self::with_parts(
            config,
            data_root,
            TileEndpoints::production(),
            Box::new(SystemClock {
                origin: Instant::now(),
            }),
        )
    }

    /// 通信先だけを差し替える（実時計。結合テスト用。[`TileEndpoints`] を参照）。
    pub fn with_endpoints(config: &Config, data_root: &Path, endpoints: TileEndpoints) -> Self {
        Self::with_parts(
            config,
            data_root,
            endpoints,
            Box::new(SystemClock {
                origin: Instant::now(),
            }),
        )
    }

    pub(crate) fn with_parts(
        config: &Config,
        data_root: &Path,
        endpoints: TileEndpoints,
        clock: Box<dyn Clock>,
    ) -> Self {
        Self {
            endpoints,
            agent: net::agent(),
            ua: config.tile_ua.clone(),
            cache: TileCache {
                root: data_root.join("tile_cache"),
            },
            window: FetchWindow::new(config.tile_fetch_per_min, clock),
            upstream_slots: Layer::ALL.map(|layer| Arc::new(Semaphore::new(layer.concurrency()))),
            read_slots: Arc::new(Semaphore::new(TILE_FILE_SLOTS)),
            cache_max_mb: config.tile_cache_max_mb,
            timeout: UPSTREAM_TIMEOUT,
        }
    }

    /// 現行の `fetch_tile`。本文（PNG）と `X-Tile-Cache` の値を返す。失敗は透明 PNG で返す。
    pub async fn fetch(self: &Arc<Self>, tile: Tile) -> (Vec<u8>, CacheStatus) {
        // キャッシュのヒットは窓に数えない（正常な再描画・リロードを妨げない）
        if let Some(bytes) = self.read_cached(tile).await {
            return (bytes, CacheStatus::Hit);
        }
        // 窓の判定は同時数の枠を待つ前（現行と同じ順）。上限超過は 404 にせず 200 + 透明 PNG で返しきる
        // （404 だと MapLibre がリトライを連発する）
        if !self.window.allow() {
            return (TRANSPARENT_TILE.to_vec(), CacheStatus::Throttled);
        }
        let Ok(permit) = Arc::clone(&self.upstream_slots[tile.layer.index()])
            .acquire_owned()
            .await
        else {
            return (TRANSPARENT_TILE.to_vec(), CacheStatus::Error);
        };
        let proxy = Arc::clone(self);
        // 取得と保存は、クライアントが切断しても最後まで進める（枠は取得が終わるまで持つ）
        match tokio::task::spawn_blocking(move || proxy.fetch_blocking(tile, permit)).await {
            Ok(result) => result,
            Err(_) => {
                tracing::error!("[tile] 取得が異常終了しました");
                (TRANSPARENT_TILE.to_vec(), CacheStatus::Error)
            }
        }
    }

    async fn read_cached(self: &Arc<Self>, tile: Tile) -> Option<Vec<u8>> {
        let permit = Arc::clone(&self.read_slots).acquire_owned().await.ok()?;
        let proxy = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            proxy.cache.read(tile)
        })
        .await
        .ok()
        .flatten()
    }

    /// 上流から取り、保存する（ブロッキング）。上流の枠は取得が終わったら放す（保存では持たない）。
    fn fetch_blocking(
        &self,
        tile: Tile,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> (Vec<u8>, CacheStatus) {
        let fetched = self.fetch_upstream(tile);
        drop(permit);
        let bytes = match fetched {
            Ok(Fetched::Png(bytes)) => bytes,
            // 提供範囲外は恒久的 → 透明タイルを永続化して以後問い合わせない
            Ok(Fetched::Missing) => TRANSPARENT_TILE.to_vec(),
            Err(fail) => {
                tracing::warn!(
                    "[tile] 上流取得失敗（{}）: {}",
                    tile.layer.name(),
                    fail.describe()
                );
                return (TRANSPARENT_TILE.to_vec(), CacheStatus::Error);
            }
        };
        if let Err(e) = self.cache.write(tile, &bytes) {
            tracing::warn!("[tile] キャッシュ書込失敗: {:?}", e.kind());
        }
        (bytes, CacheStatus::Miss)
    }

    fn fetch_upstream(&self, tile: Tile) -> Result<Fetched, Fail> {
        let url = self.endpoints.url(tile);
        let mut response = self
            .agent
            .get(&url)
            .header("User-Agent", self.ua.as_str())
            .header("Accept", "image/png")
            // 現行の urllib が付ける値。圧縮された応答を求めない
            .header("Accept-Encoding", "identity")
            .config()
            .timeout_global(Some(self.timeout))
            .build()
            .call()
            .map_err(|e| Fail::Net(Failure::from(&e)))?;
        let status = response.status().as_u16();
        if status == 404 {
            return Ok(Fetched::Missing);
        }
        if !(200..300).contains(&status) {
            return Err(Fail::Status(status));
        }
        // 上限は展開後の大きさに効かせる（gzip の本文は圧縮後の大きさでは測れない）
        let bytes =
            net::read_body_limited(response.body_mut(), MAX_TILE_BYTES).map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => Fail::Bad,
                other => Fail::Net(body_failure(&other)),
            })?;
        // キャプティブポータル等の HTML を拒否する（osm/gsi は PNG で配信される）
        if !bytes.starts_with(PNG_MAGIC) {
            return Err(Fail::Bad);
        }
        Ok(Fetched::Png(bytes))
    }

    /// 現行の `tile_cache_prune`（B4）。ログを出して結果を返す。`should_stop` が真になったら中断する。
    pub fn prune(&self, should_stop: &dyn Fn() -> bool) -> PruneReport {
        match prune_dir(&self.cache.root, self.cache_max_mb, should_stop) {
            Ok(report) => {
                if report.deleted > 0 {
                    let mb = 1024 * 1024;
                    tracing::info!(
                        "[tile] キャッシュ整理: {}枚削除 {}MB → {}MB",
                        report.deleted,
                        report.total / mb,
                        (report.total - report.freed) / mb
                    );
                }
                if report.stale_parts > 0 {
                    tracing::info!(
                        "[tile] キャッシュ整理: 書きかけの一時ファイル {} 件を削除",
                        report.stale_parts
                    );
                }
                report
            }
            Err(e) => {
                tracing::warn!(
                    "[tile] キャッシュ整理を飛ばしました（tile_cache を読めません、またはリンク）: {:?}",
                    e.kind()
                );
                PruneReport::default()
            }
        }
    }
}

/// 掃除の結果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneReport {
    /// 数えた `.png` の枚数と合計バイト数。
    pub files: usize,
    pub total: u64,
    /// 消した枚数とバイト数。
    pub deleted: usize,
    pub freed: u64,
    /// 消した古い一時ファイル（`.part`）の件数。
    pub stale_parts: usize,
    /// 中断した（停止の合図）。
    pub interrupted: bool,
}

/// 掃除が数える 1 枚。パスは `<層>/<z>/<x>/<y>.png` から作り直す（百万枚でも収まるよう、文字列を持たない）。
struct Entry {
    mtime: (i64, i64),
    size: u64,
    layer: Layer,
    z: u32,
    x: u64,
    y: u64,
}

impl Entry {
    fn path(&self, root: &Path) -> PathBuf {
        root.join(self.layer.name())
            .join(self.z.to_string())
            .join(self.x.to_string())
            .join(format!("{}.png", self.y))
    }

    /// Python の `Path` の比較と同じ並び（成分ごとの文字列比較は、`/` 区切りの文字列比較と一致する）。
    fn rel(&self) -> String {
        format!("{}/{}/{}/{}.png", self.layer.name(), self.z, self.x, self.y)
    }
}

/// 先頭ゼロのない十進（現行が書く名前）だけを数値にする。
fn canonical_number(name: &str) -> Option<u64> {
    let n: u64 = name.parse().ok()?;
    (n.to_string() == name).then_some(n)
}

/// 現行の `tile_cache_prune`: `root` の `.png` の合計が `max_mb` MB を超えたら、`mtime` の古い順に、合計が
/// 上限の 90% 以下になるまで消す。数えるのは `<層>/<z>/<x>/<y>.png` の形の通常ファイルだけ（リンクは辿らず・
/// 数えず・消さない）。`root` がリンクなら何もせずエラー。無ければ何もしない。古い一時ファイルも消す。
pub fn prune_dir(
    root: &Path,
    max_mb: i64,
    should_stop: &dyn Fn() -> bool,
) -> std::io::Result<PruneReport> {
    match fs::symlink_metadata(root) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return Err(std::io::ErrorKind::NotADirectory.into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PruneReport::default()),
        Err(e) => return Err(e),
    }
    let mut report = PruneReport::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut stale: Vec<PathBuf> = Vec::new();
    let now = std::time::SystemTime::now();
    'scan: for layer in Layer::ALL {
        for (z, z_dir) in sub_dirs(&root.join(layer.name())) {
            for (x, x_dir) in sub_dirs(&z_dir) {
                if should_stop() {
                    report.interrupted = true;
                    break 'scan;
                }
                let Ok(read) = fs::read_dir(&x_dir) else {
                    continue;
                };
                for entry in read.flatten() {
                    // 1 つのディレクトリに何万枚あっても、合図から次の 1 件で止まる
                    if should_stop() {
                        report.interrupted = true;
                        break 'scan;
                    }
                    // `DirEntry::metadata` はリンクを辿らない
                    let Ok(meta) = entry.metadata() else { continue };
                    if !meta.file_type().is_file() {
                        continue;
                    }
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if let Some(stem) = name.strip_suffix(".png") {
                        let Some(y) = canonical_number(stem) else {
                            continue;
                        };
                        let Ok(z) = u32::try_from(z) else { continue };
                        report.files += 1;
                        report.total += meta.len();
                        entries.push(Entry {
                            mtime: (meta.mtime(), meta.mtime_nsec()),
                            size: meta.len(),
                            layer,
                            z,
                            x,
                            y,
                        });
                    } else if name.ends_with(".part")
                        && meta
                            .modified()
                            .ok()
                            .and_then(|m| now.duration_since(m).ok())
                            .is_some_and(|age| age >= STALE_PART_AGE)
                    {
                        stale.push(entry.path());
                    }
                }
            }
        }
    }
    // 走査の途中で止まったら、集めた分も消さずに戻る（停止の合図のあとに削除を始めない）
    if report.interrupted {
        return Ok(report);
    }
    for path in stale {
        if should_stop() {
            report.interrupted = true;
            return Ok(report);
        }
        if fs::remove_file(path).is_ok() {
            report.stale_parts += 1;
        }
    }
    // 現行は `MB * 1024 * 1024`（整数）と、`limit * 0.9`（浮動小数）の比較
    let limit = i128::from(max_mb) * 1024 * 1024;
    if i128::from(report.total) <= limit {
        return Ok(report);
    }
    entries.sort_by(|a, b| {
        (a.mtime, a.size)
            .cmp(&(b.mtime, b.size))
            .then_with(|| a.rel().cmp(&b.rel()))
    });
    let target = limit as f64 * 0.9;
    for entry in &entries {
        if (report.total - report.freed) as f64 <= target {
            break;
        }
        if should_stop() {
            report.interrupted = true;
            break;
        }
        if fs::remove_file(entry.path(root)).is_err() {
            continue;
        }
        report.freed += entry.size;
        report.deleted += 1;
    }
    Ok(report)
}

/// `dir` の直下のうち、名前が先頭ゼロのない十進の実ディレクトリ（リンクは辿らない）。
fn sub_dirs(dir: &Path) -> Vec<(u64, PathBuf)> {
    // `dir` 自体がリンクなら降りない
    if !fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()) {
        return Vec::new();
    }
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    read.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let n = canonical_number(e.file_name().to_str()?)?;
            Some((n, e.path()))
        })
        .collect()
}

/// 掃除のスレッド: 起動時に 1 回、以後 `interval` ごと。取り込み（同期サイクル）が無効でも動く。
/// `stop` が来たら、待ちも走査も中断して終わる。
pub fn spawn_pruner(proxy: Arc<TileProxy>, stop: StopFlag, interval: Duration) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("tile-pruner".into())
        .spawn(move || {
            loop {
                if stop.is_stopped() {
                    break;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    proxy.prune(&|| stop.is_stopped());
                }));
                if result.is_err() {
                    tracing::error!("[tile] キャッシュ整理が異常終了しました");
                }
                if stop.wait(interval) {
                    break;
                }
            }
        })
        .expect("スレッドを起動できません")
}

#[cfg(test)]
pub(crate) mod stub;
#[cfg(test)]
mod tests;
