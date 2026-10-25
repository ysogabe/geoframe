//! HTTP サーバー。ルートは `GET /healthz`（プロセスが動いていて DB が読める）、
//! `GET /api/v1/photos`（写真フィード。契約は docs/architecture/photo-feed-contract.md）、
//! それ以外（静的ファイル・管理画面のページ・未実装の API）を受ける [`site`]。
//!
//! リクエストは外側から **ログ → ヘッダ付与 → 検査（[`guard`]）→ ルート** の順に通る。検査は
//! 現行 Python の `do_GET`/`do_HEAD`/`do_POST` の入口と同じ順序で、未実装のメソッド（`OPTIONS` を含む。
//! CORS ヘッダは一切返さない）は 501、Host 検査、`POST` は全部 403、の順。仕様と Python との差は
//! docs/rust-migration/server.md §9。
//!
//! 公開するルートが増えても、次の方針は変えない。
//!
//! - ハンドラは [`AppState`] の [`Catalog`] 経由でだけ DB を読む（tokio の worker で SQLite を
//!   直接実行しない）。
//! - 応答の本文にエラーの文字列・パス・件数などの環境の情報を載せない（理由はログにだけ出す）。
//! - リクエストログは [`access_log`] が一手に引き受け、出す項目は
//!   メソッド・ルートのテンプレート・状態コード・所要時間だけ（[`crate::logging`]）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Semaphore};

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, any, on};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

use crate::admin::{self, AdminContext};
use crate::blog::{self, BlogEnv, BlogError};
use crate::cache::Cache;
use crate::config::Config;
use crate::db::Catalog;
use crate::drive::MassDeleteBoard;
use crate::geocode::{self, Geocoder};
use crate::limits::{ConnBusy, ConnLimits, LimitedListener, STATIC_FILE_SLOTS};
use crate::tiles::{self, CacheStatus, TileProxy};
use crate::writer::Writer;
use crate::{csrf, hostcheck, photos, post, pyquery, statics, sysinfo, webpath};

/// `/healthz` が DB の確認を待つ上限。超えたら不健康（503）として答える。`/healthz` 専用の接続の
/// `busy_timeout`（2 秒、`db.rs`）より長く、API の読み取りとは別の接続を使うので、API が混んで
/// いても DB が正常なら間に合う。
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// ディスク使用量（`/api/admin/status` の `disk`）のキャッシュ時間。全走査になるので現行と同じく 60 秒。
const DISK_CACHE_TTL: Duration = Duration::from_secs(60);

/// ハンドラが共有する状態。複製は安い。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub catalog: Catalog,
    /// データ置き場を**リンクを辿って解決した**絶対パス（起動時に1回。静的配信が、要求されたファイルを
    /// 解決した結果の内外判定に使う）。データ置き場自体がシンボリックリンクでもよい。
    pub data_root: Arc<PathBuf>,
    /// データ置き場のファイルを同時に配れる数（[`STATIC_FILE_SLOTS`]）。ファイルを開く前に取り、
    /// 空きが無ければ待たずに 503。ファイル記述子を握る数の上限（`/healthz` などを巻き込まないため）。
    pub static_slots: Arc<Semaphore>,
    /// 管理画面の読み取り API（`admin.rs`）の設定。
    pub admin: Arc<AdminContext>,
    /// `disk` の直近の計測。ロックを持ったまま測るので、同時に来ても全走査は1回で済む。
    disk_cache: Arc<Mutex<Option<(Instant, Value)>>>,
    /// `.cache.json` のメモリキャッシュ。取り込みが有効（`GEOFRAME_INGEST=1`）のときは同期サイクルと同じもの
    /// （保存は保存スレッドに任せる）。無効のときはメモリだけ（ディスクへは書かない。[`Cache::load_memory_only`]）。
    pub cache: Arc<Cache>,
    /// 地名検索（`GET /api/admin/geocode`）。Nominatim の 1 秒 1 件の制限をプロセス全体で持つ。
    pub geocoder: Arc<Geocoder>,
    /// 地図タイルのプロキシ（`GET`/`HEAD /tiles/…`）と、そのキャッシュの掃除。流量の窓と同時数をプロセス全体で持つ。
    pub tiles: Arc<TileProxy>,
    /// 旅行ブログの読み取り（旅程 JSON の置き場・下書き・書き出し。E7〜E10）。
    pub blog: Arc<BlogEnv>,
    /// Drive の大量削除の保留の記憶（同期サイクルの `index_drive` が書く）。取り込みが有効（`GEOFRAME_INGEST=1`）の
    /// `serve` だけが持ち、`/api/admin/status` の `sync.mass_delete_hold` に出す。無いとき（取り込み無効・試験）は
    /// キーを出さない。
    pub mass_delete: Option<Arc<MassDeleteBoard>>,
}

impl AppState {
    /// 書き込み用の接続（writer）の共有ハンドル。取り込みが有効（`GEOFRAME_INGEST=1`）のときだけある。
    /// 同期サイクルの段と同じ writer（単一接続・専用スレッド）で、更新 API もこれを通す。`None` のときの
    /// 更新 API は、何も書かずに 503（[`post`]）。
    pub fn writer(&self) -> Option<Writer> {
        self.catalog.writer().cloned()
    }

    /// `config.data_dir` を解決して状態を作る。解決できないとき（起動時には DB を開いているので
    /// 起きない。テストで存在しない置き場を渡した場合など）は解決前の絶対パスのままにし、
    /// 静的配信は 404 を返す。
    pub fn new(config: Arc<Config>, catalog: Catalog) -> Self {
        let data_root = std::fs::canonicalize(&config.data_dir).unwrap_or_else(|e| {
            tracing::warn!("データ置き場を解決できません: {e}");
            config.data_dir.clone()
        });
        let admin = Arc::new(AdminContext::new(&config, &data_root));
        let tiles = Arc::new(TileProxy::production(&config, &data_root));
        let blog = Arc::new(BlogEnv::new(config.blog_plans_dir.as_deref(), &data_root));
        Self {
            config,
            catalog,
            data_root: Arc::new(data_root),
            static_slots: Arc::new(Semaphore::new(STATIC_FILE_SLOTS)),
            admin,
            disk_cache: Arc::new(Mutex::new(None)),
            cache: Arc::new(Cache::memory_only()),
            geocoder: Arc::new(Geocoder::production()),
            tiles,
            blog,
            mass_delete: None,
        }
    }

    /// 大量削除の保留の記憶を付ける（取り込みが有効の `serve`。同期サイクルの段と同じもの）。
    pub fn with_mass_delete_board(mut self, board: Arc<MassDeleteBoard>) -> Self {
        self.mass_delete = Some(board);
        self
    }

    /// 地名検索が使うキャッシュを差し替える（起動時。既定は空のメモリだけのキャッシュ）。
    pub fn with_cache(mut self, cache: Arc<Cache>) -> Self {
        self.cache = cache;
        self
    }

    /// 地名検索（通信先・時計）を差し替える。結合テストが、本物の地理院・Nominatim ではなくループバックのスタブへ
    /// 向けるためのもの。本番のバイナリは呼ばない（[`geocode::Endpoints`] を参照）。
    pub fn with_geocoder(mut self, geocoder: Arc<Geocoder>) -> Self {
        self.geocoder = geocoder;
        self
    }

    /// 地図タイル（通信先・時計）を差し替える。結合テストが、本物の OSM・地理院ではなくループバックのスタブへ
    /// 向けるためのもの。本番のバイナリは呼ばない（[`tiles::TileEndpoints`] を参照）。
    pub fn with_tiles(mut self, tiles: Arc<TileProxy>) -> Self {
        self.tiles = tiles;
        self
    }
}

#[derive(Serialize)]
struct Health {
    ok: bool,
}

/// プロセスが動いていて、DB が読める（`photos` を1行読める）なら 200、そうでなければ 503。
/// 本文は `{"ok":true|false}` だけで、理由・件数・パスは載せない（理由はログに出す）。
async fn healthz(State(state): State<AppState>) -> Response {
    let ok = match tokio::time::timeout(HEALTH_TIMEOUT, state.catalog.check_readable()).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::warn!("ヘルス確認: {e}");
            false
        }
        Err(_) => {
            tracing::warn!(
                "ヘルス確認: DB の応答が {} 秒以内に返りません",
                HEALTH_TIMEOUT.as_secs()
            );
            false
        }
    };
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(Health { ok }),
    )
        .into_response()
}

/// `GET /api/v1/photos`。Python の `_send_json` と同じ `Content-Type` と `Cache-Control`
/// （`json.dumps(ensure_ascii=False)` と同じく、日本語はエスケープしない UTF-8 のまま）。
/// 読めない・順番待ちが `QUEUE_TIMEOUT` を超えたときは 503 と定型の本文で、理由はログにだけ出す。
async fn api_photos(State(state): State<AppState>) -> Response {
    match state.catalog.read(photos::list_photos).await {
        Ok(list) => match serde_json::to_vec(&list) {
            Ok(body) => json_response(StatusCode::OK, body),
            Err(e) => {
                tracing::error!("写真一覧を JSON にできません: {e}");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "内部エラーが発生しました",
                )
            }
        },
        Err(e) => {
            tracing::warn!("写真一覧を読めません: {e}");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "写真一覧を読み取れません。しばらくしてから再試行してください",
            )
        }
    }
}

/// `GET /api/admin/status`（現行の `api_admin_status`）。DB の読み取りは 503（読めない・順番待ち超過）、
/// ディスクの測定に失敗したときは 500。どちらも本文は定型で、理由はログにだけ出す。
async fn api_admin_status(state: &AppState) -> Response {
    let ctx = Arc::clone(&state.admin);
    let mut status = match state
        .catalog
        .read(move |conn| admin::status(conn, &ctx))
        .await
    {
        Ok(status) => status,
        Err(e) => {
            tracing::warn!("管理画面の状態を読めません: {e}");
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "状態を読み取れません。しばらくしてから再試行してください",
            );
        }
    };
    if let Some(hold) = state
        .mass_delete
        .as_deref()
        .and_then(MassDeleteBoard::current)
    {
        admin::insert_mass_delete_hold(&mut status, &hold);
    }
    match disk_info(state).await {
        Ok(disk) => status.insert("disk".into(), disk),
        Err(e) => {
            tracing::error!("ディスク使用量を測れません: {e}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "内部エラーが発生しました",
            );
        }
    };
    status.insert("server_time".into(), sysinfo::local_timestamp().into());
    match serde_json::to_vec(&status) {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(e) => {
            tracing::error!("状態を JSON にできません: {e}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "内部エラーが発生しました",
            )
        }
    }
}

/// `disk`。60 秒以内の計測があればそれを返す。測定は全走査なので blocking スレッドで行う。
async fn disk_info(state: &AppState) -> std::io::Result<Value> {
    let mut cache = state.disk_cache.lock().await;
    if let Some((at, data)) = cache.as_ref()
        && at.elapsed() < DISK_CACHE_TTL
    {
        return Ok(data.clone());
    }
    let root = Arc::clone(&state.data_root);
    let max_mb = state.config.tile_cache_max_mb;
    let data = tokio::task::spawn_blocking(move || admin::disk_info(&root, max_mb))
        .await
        .map_err(|_| std::io::Error::other("ディスク使用量の測定が異常終了しました"))??;
    *cache = Some((Instant::now(), data.clone()));
    Ok(data)
}

/// `GET /api/admin/photos`（現行の `api_admin_photos` と、`handlers.py` のクエリの解釈）。
/// `page`・`per` が `int()` を通らなければ 400 `bad_params`（クエリの値は応答にもログにも出さない）。
async fn api_admin_photos(state: &AppState, query: Option<&str>) -> Response {
    let params = pyquery::parse_qs_first(query.unwrap_or(""));
    let int = |name: &str, default: &str| pyquery::py_int(params.get(name).map_or(default, |v| v));
    let (Some(page), Some(per)) = (int("page", "1"), int("per", "50")) else {
        return error_response(StatusCode::BAD_REQUEST, "bad_params");
    };
    let filter = admin::Filter::parse(params.get("filter").map_or("all", String::as_str));
    let ctx = Arc::clone(&state.admin);
    match state
        .catalog
        .read(move |conn| admin::photos(conn, &ctx, page, per, filter))
        .await
    {
        Ok(listing) => match serde_json::to_vec(&listing) {
            Ok(body) => json_response(StatusCode::OK, body),
            Err(e) => {
                tracing::error!("管理画面の一覧を JSON にできません: {e}");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "内部エラーが発生しました",
                )
            }
        },
        Err(e) => {
            tracing::warn!("管理画面の一覧を読めません: {e}");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "一覧を読み取れません。しばらくしてから再試行してください",
            )
        }
    }
}

/// `GET /api/admin/geocode`（現行の `geocode_search` と、`handlers.py` のクエリの解釈）。`q` は最初の値
/// （`parse_qs` と同じ。空の値は無かったことになる）。2 文字未満は通信せずに空。通信と 1 秒の待ちがあるので
/// blocking スレッドで行い、同時に処理する数に上限を付ける（空きが無ければ 503）。失敗は空の配列で、
/// 応答にも理由を載せない。
async fn api_admin_geocode(state: &AppState, query: Option<&str>) -> Response {
    let params = pyquery::parse_qs_first(query.unwrap_or(""));
    let raw = params.get("q").map_or("", String::as_str);
    let Some(q) = geocode::clean_query(raw) else {
        return geocode_response(Vec::new());
    };
    let Some(permit) = state.geocoder.try_reserve() else {
        tracing::warn!("地名検索: 同時に処理できる数を超えたので断りました");
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "地名検索が混み合っています。しばらくしてから再試行してください",
        );
    };
    let (geocoder, cache, q) = (
        Arc::clone(&state.geocoder),
        Arc::clone(&state.cache),
        q.to_owned(),
    );
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        geocoder.search(&cache, &q)
    })
    .await
    {
        Ok(items) => geocode_response(items),
        Err(_) => {
            tracing::error!("地名検索が異常終了しました");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "内部エラーが発生しました",
            )
        }
    }
}

fn geocode_response(items: Vec<Value>) -> Response {
    match serde_json::to_vec(&serde_json::json!({"ok": true, "items": items})) {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(e) => {
            tracing::error!("地名検索の結果を JSON にできません: {e}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "内部エラーが発生しました",
            )
        }
    }
}

/// 旅行ブログの読み取り API（E7〜E10）のルート。現行 `do_GET` は `/api/blog/` で始まるパスを、完全一致の4つだけ
/// 受け、それ以外は `send_error(404)`（末尾の `/`・大文字・エンコードした名前も 404）。
fn blog_route(path: &str) -> Option<(&'static str, BlogCall)> {
    Some(match path {
        "/api/blog/trips" => ("/api/blog/trips", BlogCall::Trips),
        "/api/blog/trip" => ("/api/blog/trip", BlogCall::Trip),
        "/api/blog/candidates" => ("/api/blog/candidates", BlogCall::Candidates),
        "/api/blog/draft" => ("/api/blog/draft", BlogCall::Draft),
        _ => return None,
    })
}

#[derive(Clone, Copy)]
enum BlogCall {
    Trips,
    Trip,
    Candidates,
    Draft,
}

/// `GET /api/blog/…`（現行の `api_blog_*` と、`handlers.py` のクエリの解釈）。`trip_id` は `parse_qs` の最初の値
/// （空の値は無かったことになる。無ければ `""` で 400）。応答の本文は定型文だけ（旅程の中身・例外の文字列を載せない）。
async fn blog_response(state: &AppState, call: BlogCall, query: Option<&str>) -> Response {
    let params = pyquery::parse_qs_first(query.unwrap_or(""));
    let trip_id = params.get("trip_id").cloned().unwrap_or_default();
    let env = Arc::clone(&state.blog);
    let result = match call {
        BlogCall::Trips => blog::trips(env).await,
        BlogCall::Trip => blog::trip(env, trip_id).await,
        BlogCall::Candidates => blog::candidates(env, &state.catalog, trip_id).await,
        BlogCall::Draft => blog::draft(env, &state.catalog, trip_id).await,
    };
    match result {
        Ok(value) => match serde_json::to_vec(&value) {
            Ok(body) => json_response(StatusCode::OK, body),
            Err(e) => {
                tracing::error!("旅行ブログの応答を JSON にできません: {e}");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "内部エラーが発生しました",
                )
            }
        },
        Err(BlogError::BadRequest(message)) => error_response(StatusCode::BAD_REQUEST, &message),
        Err(BlogError::NotFound(message)) => error_response(StatusCode::NOT_FOUND, &message),
        Err(BlogError::Unavailable(reason)) => {
            tracing::warn!("旅行ブログの写真を読めません: {reason}");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "写真を読み取れません。しばらくしてから再試行してください",
            )
        }
        Err(BlogError::Internal) => {
            tracing::error!("旅行ブログの処理が異常終了しました");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "内部エラーが発生しました",
            )
        }
    }
}

/// アクセスログに出すタイルのルートのテンプレート（座標・層は出さない）。
const TILE_ROUTE: &str = "/tiles/{*path}";

/// `GET`/`HEAD /tiles/…`（現行の `_serve_tile`）。形が合わない・範囲外は 404（`send_error(404)` と同じ本文）、
/// それ以外は常に 200（取得の失敗・流量の超過も透明 PNG）。`HEAD` も同じ処理で、本文だけ省く
/// （`Content-Length` は `GET` と同じ）。
async fn tile_response(state: &AppState, path: &str, method: &Method) -> Response {
    let Some(tile) = tiles::parse_path(path) else {
        return labeled(
            statics::error_page(
                StatusCode::NOT_FOUND,
                "Not Found",
                "Nothing matches the given URI",
            ),
            TILE_ROUTE,
        );
    };
    let (png, status) = state.tiles.fetch(tile).await;
    labeled(tile_ok(png, status, *method == Method::HEAD), TILE_ROUTE)
}

fn tile_ok(png: Vec<u8>, status: CacheStatus, head: bool) -> Response {
    let len = png.len();
    let mut response = Response::new(if head { Body::empty() } else { Body::from(png) });
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(status.cache_control()),
    );
    headers.insert(
        header::HeaderName::from_static("x-tile-cache"),
        HeaderValue::from_static(status.as_str()),
    );
    response
}

pub(crate) fn json_response(status: StatusCode, body: Vec<u8>) -> Response {
    (
        status,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response()
}

/// `{"ok":false,"error":"..."}`。本文は定型文だけで、環境の情報（SQLite のエラー・パス）を載せない。
pub(crate) fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "ok": false, "error": message });
    json_response(status, body.to_string().into_bytes())
}

/// 応答に付ける、アクセスログ用のルートのテンプレート（`/derivatives/{*path}` など）。
/// 実際のパスの代わりにこれだけをログへ出す。応答の拡張で渡す（ネットワークには出ない）。
#[derive(Clone, Copy)]
struct RouteLabel(&'static str);

fn labeled(mut response: Response, label: &'static str) -> Response {
    response.extensions_mut().insert(RouteLabel(label));
    response
}

/// リクエストログ。出す項目は**メソッド・ルートのテンプレート・状態コード・所要時間（ミリ秒）**
/// だけ。実際のパスやクエリ（ファイル名・Drive ID・検索語が入りうる）は出さず、どのルートにも
/// 当たらなかったリクエストは `(unmatched)` と出す。`/healthz` の成功は頻繁に呼ばれるので debug。
/// 引数付きのルート（静的ファイル）は [`RouteLabel`] のテンプレートで出す。
async fn access_log(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let ms = started.elapsed().as_millis();
    let route = response
        .extensions()
        .get::<RouteLabel>()
        .map(|label| label.0)
        .or(route.as_deref())
        .unwrap_or("(unmatched)");
    if status >= 500 {
        tracing::warn!("{method} {route} {status} {ms}ms");
    } else if route == "/healthz" {
        tracing::debug!("{method} {route} {status} {ms}ms");
    } else {
        tracing::info!("{method} {route} {status} {ms}ms");
    }
    response
}

/// 検査（現行 Python の `do_GET`/`do_HEAD`/`do_POST` の入口と同じ順序）:
///
/// 1. `GET`/`HEAD`/`POST` 以外は 501。`OPTIONS` もここ。クロスオリジンの `application/json` の
///    プリフライトを失敗させる（CSRF 対策の一部）ので、**CORS ヘッダは一切返さない**。
/// 2. `Host` ヘッダが複数あれば 400（どれで判定するかが実装次第になり、先頭だけ見ると順序で
///    すり抜けられるため、判定の前に拒否する）。無い場合は従来どおり許可。
/// 3. Host 検査（[`hostcheck`]）。不許可は 403。**Host の値はログに出さない**。
/// 4. 絶対形式の request-target（`GET http://host/...`）は全ルートで 403（現行でもホワイトリストに
///    当たらず 403。`Host` ヘッダと別の宛先を名乗れる形なので、`/healthz`・`/api/v1/photos` にも通さない）。
/// 5. `POST` はルート（[`post::route_of`]）が無ければ 403。あれば [`site`] の `post::handle` が CSRF 対策
///    （Content-Type・Origin・Content-Length・サイズ・JSON）を検査する。
async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let method = request.method();
    if !matches!(*method, Method::GET | Method::HEAD | Method::POST) {
        return statics::unsupported_method(method);
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    // 現行は生のバイトを Latin-1 の文字列として読む（csrf.rs）。UTF-8 として読み直すと、`\xC2\xA0` が NBSP 1 文字になって
    // 空白として取り除かれ、現行が断る値を通してしまう
    let host = hosts.next().map(|v| csrf::latin1(v.as_bytes()));
    if hosts.next().is_some() {
        return statics::bad_request();
    }
    if !hostcheck::host_allowed(
        host.as_deref(),
        state.config.listen.port(),
        &state.config.allowed_hosts,
    ) {
        tracing::warn!(
            "Host ヘッダが許可されていないため拒否しました（値は記録しません。GEOFRAME_ALLOWED_HOSTS で名前を足せます）"
        );
        return statics::forbidden_host();
    }
    if request.uri().scheme().is_some() {
        return statics::forbidden();
    }
    // ルートの無い POST は 403（`/healthz` など GET の API も）。ルートのある POST は CSRF 対策を含めて
    // `post::handle` が検査する
    if *method == Method::POST && post::route_of(request.uri().path()).is_none() {
        return statics::forbidden();
    }
    next.run(request).await
}

/// 現行の `end_headers` と同じヘッダを付ける（全ての応答。403・404・304 にも付く）。
///
/// - HTML と `/static/`・`/vendor/` 配下は `Cache-Control: no-cache`（キャッシュはするが毎回
///   再検証させる。サーバー側のコードを更新しても常時起動のブラウザが古い JS/CSS を使い続ける
///   事故の対策）。`/derivatives/`・`/photos/` は付けない（内容不変で重い）。
/// - HTML には `X-Frame-Options: DENY` と `Content-Security-Policy: frame-ancestors 'none'`
///   （無認証なので、透明な iframe に重ねて誘導クリックさせる攻撃を防ぐ）。
///
/// 判定は**デコード・正規化したあとのパス**（ページの読み替え後）で行う。Python は生のパスで見るため、
/// `/static/admin%2Ehtml` のようにエンコードすると同じ HTML に `X-Frame-Options` が付かなかった。
/// `.htm` も HTML として扱う（`Content-Type` が `text/html` になるため）。
async fn security_headers(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let raw = request.uri().path().to_owned();
    let mut response = next.run(request).await;

    let normalized = webpath::normalize(&raw).map(|n| n.path);
    let mut effective = normalized.clone().unwrap_or(raw);
    if matches!(method, Method::GET | Method::HEAD)
        && let Some(target) = normalized.as_deref().and_then(webpath::page_target)
    {
        effective = format!("/{target}");
    }
    let lower = effective.to_ascii_lowercase();
    let is_html = effective == "/" || lower.ends_with(".html") || lower.ends_with(".htm");
    let headers = response.headers_mut();
    // ディレクトリ自体への要求（`/static`・`/vendor/`。403）にも付ける
    let under_embedded = ["/static/", "/vendor/"]
        .iter()
        .any(|prefix| format!("{effective}/").starts_with(prefix));
    if (is_html || under_embedded) && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    if is_html {
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        );
    }
    response
}

/// まだ Rust 版に無い機能（AI）。P4 で実装する。現行の「未知の API」の 403 とは区別して、**503 と定型の JSON** を返す
/// （「禁止」ではなく「まだ無い」と分かるように）。
fn not_yet_implemented(label: &'static str) -> Response {
    labeled(
        error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "この機能は Rust 版ではまだ使えません",
        ),
        label,
    )
}

/// `/healthz`・`/api/v1/photos` 以外の `GET`/`HEAD`（`POST` と未実装のメソッドは [`guard`] が返す）。
/// 現行の `do_GET`/`do_HEAD` と同じ順に:
///
/// 1. `GET` の `/api/admin/status`・`/api/admin/photos`（T1.4）は管理画面の読み取り API。
///    現行と同じく**生のパスの前方一致**（`/api/admin/photosX` も、クエリ付きも当たる）。
///    `GET` の `/api/admin/geocode`（地名検索。T3.3）も同じ前方一致。`/tiles/`（地図タイル。T3.4。`GET`・`HEAD` とも、
///    他のどの判定よりも先に、生のパスで）は [`tile_response`]。`GET` の `/api/blog/`（旅行ブログの読み取り。T3.5。完全一致の4つ以外は 404）は [`blog_response`]、`/api/ai/` は 503。`/api/admin/` の残り（`setloc` などの更新ルートへの `GET` を含む）は現行でも
///    ホワイトリストに当たらず 403 なので、下の静的配信の判定へ回す。
/// 2. 管理画面のページ（`/`・`/admin`・`/blog-admin`・`/ai-search`）は埋め込みの HTML。
/// 3. 静的ファイル。許可は `/favicon.ico` と5つの接頭辞だけで、それ以外・ドット始まりは 403。
///    `/static/`・`/vendor/` は埋め込み、`/derivatives/`・`/photos/`・`/blog_exports/` はデータ置き場から。
///
/// `HEAD` も `GET` と同じ判定を通る（`HEAD /api/...` は静的のホワイトリストに当たらず 403。現行と同じ）。
async fn site(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = &parts.method;
    let raw = parts.uri.path();
    if *method == Method::POST {
        // 入口の検査（`guard`）がルートの有無を済ませている。現行は行の読み込みで断る大きさ（414・431）を、
        // hyper の上限は緩いのでここで断る
        if let Some(response) = post::head_limits(&parts.uri, &parts.headers) {
            return response;
        }
        return match post::route_of(raw) {
            Some(route) => {
                // 書き出し（E16）などの長い処理の間は、無通信の期限で接続を切らない（`serve` が付けた接続の情報。
                // 付いていない起動のしかた（試験）では何もしない）。止めるのは**本文を読み終えて検査したあと**の
                // 処理の間だけで、本文の受信中は期限を働かせたままにする（`post::handle`）
                let busy = parts
                    .extensions
                    .get::<axum::extract::ConnectInfo<ConnInfo>>()
                    .map(|info| info.0.busy.clone());
                labeled(
                    post::handle(&state, route, &parts.headers, body, busy.as_ref()).await,
                    route.label(),
                )
            }
            None => statics::forbidden(),
        };
    }
    // 現行の `http.server` は、先頭の `//` を1つの `/` にしてから `do_GET` に渡す。接頭辞の判定は
    // その後のパスで行う（`//api/admin/status` も当たる）。静的配信の正規化（`webpath::normalize`）は
    // 元のパスを受けて自分で同じ畳み込みをする
    let collapsed;
    let path = if raw.starts_with("//") {
        collapsed = format!("/{}", raw.trim_start_matches('/'));
        collapsed.as_str()
    } else {
        raw
    };

    // 現行の `do_GET`/`do_HEAD` は、`/tiles/` を他のどの判定よりも先に（生のパスで）見る
    if path.starts_with("/tiles/") {
        return tile_response(&state, path, method).await;
    }
    if *method == Method::GET {
        // axum のルートは完全一致なので、`//api/v1/photos` はここへ来る（現行は200）
        if path == "/api/v1/photos" {
            return labeled(api_photos(State(state)).await, "/api/v1/photos");
        }
        if path.starts_with("/api/admin/status") {
            return labeled(api_admin_status(&state).await, "/api/admin/status");
        }
        if path.starts_with("/api/admin/photos") {
            return labeled(
                api_admin_photos(&state, parts.uri.query()).await,
                "/api/admin/photos",
            );
        }
        if path.starts_with("/api/admin/geocode") {
            return labeled(
                api_admin_geocode(&state, parts.uri.query()).await,
                "/api/admin/geocode",
            );
        }
        if path.starts_with("/api/blog/") {
            return match blog_route(path) {
                Some((label, call)) => {
                    labeled(blog_response(&state, call, parts.uri.query()).await, label)
                }
                None => labeled(
                    statics::error_page(
                        StatusCode::NOT_FOUND,
                        "Not Found",
                        "Nothing matches the given URI",
                    ),
                    "/api/blog/{*path}",
                ),
            };
        }
        if path.starts_with("/api/ai/") {
            return not_yet_implemented("/api/ai/{*path}");
        }
    }
    let Some(normalized) = webpath::normalize(raw) else {
        return statics::forbidden();
    };
    let trailing_slash = normalized.trailing_slash;
    if let Some(key) = webpath::page_target(&normalized.path) {
        let label = match normalized.path.as_str() {
            "/blog-admin" => "/blog-admin",
            "/ai-search" => "/ai-search",
            "/admin" => "/admin",
            _ => "/",
        };
        return labeled(
            statics::serve_embedded_key(key, false, method, &parts.headers),
            label,
        );
    }
    let Some(target) = webpath::classify(&normalized.path) else {
        // ホワイトリストの内側のドット始まりなどは、ログにはテンプレートで出す
        let response = statics::forbidden();
        return match webpath::area_of(&normalized.path) {
            Some(area) => labeled(response, area.route()),
            None => response,
        };
    };
    let response = match target.area {
        webpath::Area::Static | webpath::Area::Vendor => {
            statics::serve_embedded(&target, trailing_slash, method, &parts.headers)
        }
        webpath::Area::Favicon => statics::not_found(),
        _ => {
            statics::serve_data(
                &state.data_root,
                &state.static_slots,
                &target,
                trailing_slash,
                method,
                &parts.headers,
            )
            .await
        }
    };
    labeled(response, target.area.route())
}

pub fn router(state: AppState) -> Router {
    Router::new()
        // `any`: メソッドの絞り込みは `guard`（GET/HEAD/POST 以外は 501、POST は 403）が済ませている。
        // `get` にすると axum が 405 用の `Allow` を付ける
        .route("/healthz", any(healthz))
        // GET だけ。HEAD は axum が GET へ自動で回さないよう `site` を明示し、静的のホワイトリストに
        // 当たらず 403（現行と同じ）
        .route(
            "/api/v1/photos",
            on(MethodFilter::GET, api_photos)
                .on(MethodFilter::HEAD, site)
                .fallback(site),
        )
        // 既定のフォールバックには layer が掛からないので、明示して全ての応答をログに載せる
        .fallback(site)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(access_log))
        .with_state(state)
}

/// SIGINT / SIGTERM（systemd の停止）を待つ。
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM を登録できません");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    // 実行中の旅行ブログの書き出しは、次の画像の前で作業領域を片付けて止まる（前の書き出しは残る）
    crate::blog_write::request_stop();
}

/// 接続ごとの付随情報（`axum::extract::ConnectInfo`）。長い処理を持つ `POST` が、その間だけ無通信の期限を
/// 止めるために使う（[`ConnBusy`]）。
#[derive(Clone, Debug)]
pub struct ConnInfo {
    pub busy: ConnBusy,
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, LimitedListener>>
    for ConnInfo
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, LimitedListener>) -> Self {
        Self {
            busy: stream.io().busy().clone(),
        }
    }
}

pub async fn serve(state: AppState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(state.config.listen).await?;
    tracing::info!("{} で待ち受けます", listener.local_addr()?);
    tracing::info!(
        "{}",
        hostcheck::startup_note(state.config.listen.port(), &state.config.allowed_hosts)
    );
    // 接続の総数・送信の停滞・無通信の上限（limits.rs）
    let listener = LimitedListener::new(listener, ConnLimits::default());
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<ConnInfo>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    tracing::info!("停止しました");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;
    use crate::testutil::{TempDir, synthetic_catalog};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn state(catalog: Catalog) -> AppState {
        let config = Config::load(|_| None, &Overrides::default()).unwrap();
        let state = AppState::new(Arc::new(config), catalog)
            .with_geocoder(crate::geocode::stub::deny_geocoder());
        let proxy = TileProxy::with_endpoints(
            &state.config,
            &state.data_root,
            crate::tiles::stub::deny_endpoints(),
        );
        state.with_tiles(Arc::new(proxy))
    }

    /// 127.0.0.1 の空きポートで router を起動し、(状態行, ヘッダ, 本文) を返す GET を用意する。
    async fn spawn(state: AppState) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        addr
    }

    async fn get(addr: std::net::SocketAddr, target: &str) -> (String, String, String) {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).await.unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let (status, headers) = head.split_once("\r\n").unwrap();
        (status.to_string(), headers.to_lowercase(), body.to_string())
    }

    #[tokio::test]
    async fn healthz_is_ok_when_the_db_is_readable() {
        let dir = TempDir::new("health-ok");
        let (path, _writer) = synthetic_catalog(dir.path(), 3);
        let addr = spawn(state(Catalog::open(&path).unwrap())).await;

        let (status, headers, body) = get(addr, "/healthz").await;
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        assert!(headers.contains("cache-control: no-store"), "{headers}");
        // 件数などは出さない
        assert_eq!(body, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn healthz_is_503_when_the_db_cannot_be_read() {
        let dir = TempDir::new("health-ng");
        let (path, writer) = synthetic_catalog(dir.path(), 3);
        let catalog = Catalog::open(&path).unwrap();
        let addr = spawn(state(catalog)).await;
        // 起動後に DB の中身が壊れた・入れ替わった状況
        writer.execute_batch("DROP TABLE photos").unwrap();

        let (status, _, body) = get(addr, "/healthz").await;
        assert!(status.starts_with("HTTP/1.1 503"), "{status}");
        assert_eq!(body, r#"{"ok":false}"#);
        assert!(!body.contains("photos"), "{body}");
    }

    #[tokio::test]
    async fn unknown_paths_are_403_like_python() {
        let dir = TempDir::new("health-403");
        let (path, _writer) = synthetic_catalog(dir.path(), 0);
        let addr = spawn(state(Catalog::open(&path).unwrap())).await;
        // 現行の静的配信はホワイトリスト方式で、未知のパスは 404 ではなく 403
        let (status, _, _) = get(addr, "/nope").await;
        assert!(status.starts_with("HTTP/1.1 403"), "{status}");
    }
}
