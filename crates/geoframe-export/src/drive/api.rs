//! ID で指す Drive 操作（Drive API v3。docs/note-photo-export/drive-api.md 4 章・5 章）。
//!
//! [`DriveApi`] は、フォルダ・ファイルを**Drive の ID で**指して操作する。パスは使わない。操作は次のとおり。
//!
//! | 操作 | API | 備考 |
//! |---|---|---|
//! | [`DriveApi::list_children`] | `GET files?q='<親>' in parents and trashed=false` | 名前で `q` を絞らない。全ページ |
//! | [`DriveApi::list_tree`] | 子の一覧を幅優先で | 相対パスを組み立てる |
//! | [`DriveApi::get`] | `GET files/<ID>` | 親・名前・ゴミ箱の状態も |
//! | [`DriveApi::generate_ids`] | `GET files/generateIds` | 作成の前に ID を取る |
//! | [`DriveApi::create_folder`] | `POST files`（取った ID を本文に） | 409 は「済み」→ 照合 |
//! | [`DriveApi::upload`] | `POST upload/files`（multipart か resumable） | 409 は「済み」→ 照合 |
//! | [`DriveApi::move_to`] | `PATCH files/<ID>?addParents&removeParents` | 再送の前に状態を見る |
//! | [`DriveApi::trash`] | `PATCH files/<ID>`（`trashed: true`） | 再送の前に状態を見る |
//! | [`DriveApi::read`] | `GET files/<ID>?alt=media` | 上限つき |
//!
//! **作らないもの**: 完全な削除（`DELETE files`・`emptyTrash`）と、既存のファイルの中身の更新（`PATCH` の media・`PUT upload`）。
//! 変更系は、新しい ID のファイル・フォルダの作成と、ID を保った移動・改名と、ゴミ箱へ移すことだけ。
//!
//! ## 期限・キャンセル
//!
//! どの操作も、最後の引数に締め切り（[`Deadline`]。呼び出し側が [`DriveApi::deadline`] で作る）を取る。操作の期限（`--drive-timeout`・
//! `--drive-upload-timeout`）とジョブ全体の期限の早いほうで、切れたときに返すエラーが違う（[`ApiError::Timeout`]・
//! [`ApiError::JobDeadline`]）。要求ごとの時間切れは、締め切りの残りと要求 1 回の上限の短いほう。キャンセル（[`Cancel`]）は、要求の前・
//! 再試行の待ち・チャンクの間で見る（要求の最中は見ない。1 回の要求は、最長で要求 1 回の上限〔JSON は 60 秒、チャンクは 10 分〕で終わる）。
//!
//! ## エラーの対応
//!
//! [`ApiError`] は [`crate::remote::RemoteError`] に写せる（`From`）。照会の失敗 → `drive_unavailable`、変更系の失敗 → `rclone_failed`、
//! 操作の期限 → `rclone_timeout`、ジョブの期限 → `timeout`、認可の失効 → `drive_auth_invalid`。

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use serde_json::{Value, json};

use crate::cancel::Cancel;
use crate::remote::{Entry, is_valid_drive_id, normalize_md5, rel_path_ok};

use super::auth::{Credentials, Tokens};
use super::http::{
    ApiError, Ctx, Deadline, Endpoints, Method, Outcome, Reply, Req, Retry, SendOpts, Transport,
    encode,
};

/// フォルダの MIME タイプ（Drive の値。`rclone lsjson` の `inode/directory` ではない）。
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// 1 回の一覧（1 つの親・1 本の木）の件数の上限（rclone 版の `LSJSON_MAX_ENTRIES` と同じ）。
pub const MAX_ENTRIES: usize = 20_000;
/// ページ送りの回数の上限。
pub const MAX_PAGES: u32 = 1000;
/// 木の一覧の深さの上限。
pub const MAX_DEPTH: usize = 32;
/// multipart で送る大きさの上限（これを超えると resumable）。
pub const MULTIPART_MAX: u64 = 8 << 20;
/// resumable のチャンクの大きさ。Google の求める 256KiB の倍数（8MiB）。
pub const CHUNK_SIZE: u64 = 8 << 20;
/// 一覧のページの大きさ（Drive の上限）。
pub const PAGE_SIZE: u32 = 1000;
/// 名前の長さの上限（バイト）。
const MAX_NAME_BYTES: usize = 255;
/// resumable のセッションを始め直す回数の上限。
const MAX_SESSION_RESTARTS: u32 = 3;
/// JSON の要求 1 回の上限。
const JSON_REQUEST_CAP: Duration = Duration::from_secs(60);
/// チャンク 1 回の要求の上限。
const CHUNK_REQUEST_CAP: Duration = Duration::from_secs(10 * 60);
const FILE_FIELDS: &str = "id,name,mimeType,size,md5Checksum,trashed,parents";
const LIST_FIELDS: &str = "nextPageToken,files(id,name,mimeType,size,md5Checksum)";

/// どの期限か（[`DriveApi::deadline`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// 一覧・取得・フォルダの作成・移動・ゴミ箱へ移す（`--drive-timeout`）。
    Query,
    /// 写真の送信（`--drive-upload-timeout`）。
    Upload,
}

/// `get` の結果: 一覧の 1 項目の形（`path` は名前）と、ゴミ箱の状態・親。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub entry: Entry,
    pub trashed: bool,
    pub parents: Vec<String>,
}

/// Drive API の操作の入口。`Clone` は安い（接続・トークンは共有。キャンセルの印・期限・上限は複製ごと）。`Send + Sync`。
#[derive(Debug, Clone)]
pub struct DriveApi {
    transport: Transport,
    tokens: Arc<Tokens>,
    cancel: Cancel,
    job_deadline: Option<Instant>,
    query_timeout: Duration,
    upload_timeout: Duration,
    page_size: u32,
    multipart_max: u64,
    chunk_size: u64,
    max_entries: usize,
}

impl DriveApi {
    /// 通信先と資格情報から作る。既定の期限は 5 分（照会・書き込み）と 45 分（送信）。
    pub fn new(credentials: Credentials, endpoints: Endpoints) -> Self {
        Self {
            transport: Transport::new(endpoints, Retry::production()),
            tokens: Arc::new(Tokens::new(credentials)),
            cancel: Cancel::new(),
            job_deadline: None,
            query_timeout: Duration::from_secs(5 * 60),
            upload_timeout: Duration::from_secs(45 * 60),
            page_size: PAGE_SIZE,
            multipart_max: MULTIPART_MAX,
            chunk_size: CHUNK_SIZE,
            max_entries: MAX_ENTRIES,
        }
    }

    /// 操作ごとの期限（送信、それ以外）。
    pub fn with_timeouts(mut self, upload: Duration, query: Duration) -> Self {
        self.upload_timeout = upload;
        self.query_timeout = query;
        self
    }

    /// キャンセルの印を共有する。立つと、要求の前・再試行の待ち・チャンクの間で止まって [`ApiError::Cancelled`] を返す。
    pub fn with_cancel(mut self, cancel: Cancel) -> Self {
        self.cancel = cancel;
        self
    }

    /// ジョブ全体の期限を掛けた複製。
    pub fn with_job_deadline(&self, deadline: Option<Instant>) -> Self {
        let mut c = self.clone();
        c.job_deadline = deadline;
        c
    }

    /// 再試行の方針を替える（試験が待たずに済むように）。
    #[doc(hidden)]
    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.transport = self.transport.with_retry(retry);
        self
    }

    /// 一覧のページの大きさ・multipart の上限・チャンクの大きさを替える（試験用。チャンクは 256KiB の倍数）。
    #[doc(hidden)]
    pub fn with_sizes(mut self, page_size: u32, multipart_max: u64, chunk_size: u64) -> Self {
        assert!((1..=PAGE_SIZE).contains(&page_size));
        assert!(chunk_size > 0 && chunk_size.is_multiple_of(256 << 10));
        self.page_size = page_size;
        self.multipart_max = multipart_max;
        self.chunk_size = chunk_size;
        self
    }

    /// 一覧の件数の上限を替える（試験用。本番は [`MAX_ENTRIES`]）。
    #[doc(hidden)]
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// 今から数えた締め切り（`limit` の期限とジョブ全体の期限の早いほう）。
    pub fn deadline(&self, limit: Limit) -> Deadline {
        let timeout = match limit {
            Limit::Query => self.query_timeout,
            Limit::Upload => self.upload_timeout,
        };
        Deadline::within(timeout, self.job_deadline)
    }

    fn ctx(&self, deadline: Deadline) -> Ctx<'_> {
        Ctx {
            cancel: &self.cancel,
            deadline,
        }
    }

    /// アクセストークンを 1 回取る（起動時の確認。認可の失効は [`ApiError::AuthInvalid`]）。
    pub fn check_credentials(&self, deadline: Deadline) -> Result<(), ApiError> {
        let ctx = self.ctx(deadline);
        self.tokens.get(&self.transport, &ctx).map(|_| ())
    }

    // ---- 送信の下請け ----

    /// トークンつきで送る。401 ならトークンを取り直して 1 回だけやり直す。
    fn authorized(
        &self,
        ctx: &Ctx<'_>,
        req: &Req<'_>,
        opts: &SendOpts<'_>,
    ) -> Result<Outcome, ApiError> {
        for refreshed in [false, true] {
            let token = self.tokens.get(&self.transport, ctx)?;
            match self
                .transport
                .send_with_retry(ctx, req, Some(&token), opts)?
            {
                Outcome::Reply(r) if r.status == 401 => {
                    if refreshed {
                        return Err(ApiError::Status(401));
                    }
                    tracing::debug!(op = opts.op, status = 401, "トークンを取り直します");
                    self.tokens.invalidate(&token);
                }
                other => return Ok(other),
            }
        }
        Err(ApiError::Status(401))
    }

    fn opts(&self, op: &'static str) -> SendOpts<'static> {
        let mut o = SendOpts::new(op);
        o.cap = JSON_REQUEST_CAP;
        o
    }

    /// JSON を返す GET。
    fn get_json(&self, ctx: &Ctx<'_>, op: &'static str, url: String) -> Result<Value, ApiError> {
        let req = Req::get(url);
        match self.authorized(ctx, &req, &self.opts(op))? {
            Outcome::Reply(r) if r.ok() => r.json(),
            _ => Err(ApiError::BadResponse),
        }
    }

    fn files_url(&self, path: &str, query: &[(&str, &str)]) -> String {
        let mut url = format!("{}{path}", self.transport.endpoints().api());
        for (i, (k, v)) in query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(k);
            url.push('=');
            url.push_str(&encode(v));
        }
        url
    }

    // ---- 照会 ----

    /// 親の直下の項目（ゴミ箱の中のものは除く）。**名前で絞らず**全ページを取る（名前の照合は呼び出し側）。`path` は名前。
    /// 件数が [`MAX_ENTRIES`]、ページが [`MAX_PAGES`] を超える・同じ `nextPageToken` が返り続けるときは [`ApiError::TooManyPages`]。
    pub fn list_children(&self, parent: &str, deadline: Deadline) -> Result<Vec<Entry>, ApiError> {
        check_id(parent)?;
        let ctx = self.ctx(deadline);
        let mut out: Vec<Entry> = Vec::new();
        let mut seen_tokens: HashSet<String> = HashSet::new();
        let mut token: Option<String> = None;
        let mut pages = 0u32;
        // `parent` は検査済みの ID（`'` と `\` を含まない）なので、`q` に入れてよい
        let q = format!("'{parent}' in parents and trashed=false");
        let page_size = self.page_size.to_string();
        loop {
            ctx.check()?;
            if pages >= MAX_PAGES {
                return Err(ApiError::TooManyPages);
            }
            let mut query = vec![
                ("q", q.as_str()),
                ("fields", LIST_FIELDS),
                ("pageSize", page_size.as_str()),
                ("spaces", "drive"),
            ];
            if let Some(t) = &token {
                query.push(("pageToken", t.as_str()));
            }
            let json = self.get_json(&ctx, "list", self.files_url("/files", &query))?;
            pages += 1;
            match json.get("files") {
                None | Some(Value::Null) => {}
                Some(Value::Array(items)) => {
                    for item in items {
                        let name = item["name"].as_str().ok_or(ApiError::BadResponse)?;
                        out.push(entry_from(item, name.to_owned())?);
                        if out.len() > self.max_entries {
                            return Err(ApiError::TooManyPages);
                        }
                    }
                }
                Some(_) => return Err(ApiError::BadResponse),
            }
            match json.get("nextPageToken") {
                None | Some(Value::Null) => break,
                Some(Value::String(s)) if s.is_empty() => break,
                Some(Value::String(s)) => {
                    // 同じトークンが返り続けると止まらない（直前と同じものも、前に使ったものも）
                    if !seen_tokens.insert(s.clone()) {
                        return Err(ApiError::TooManyPages);
                    }
                    token = Some(s.clone());
                }
                Some(_) => return Err(ApiError::BadResponse),
            }
        }
        tracing::debug!(op = "list", pages, entries = out.len(), "一覧を取りました");
        Ok(out)
    }

    /// `root` の下の全項目（幅優先）。`path` は `root` からの相対パス（`/` 区切り。[`rel_path_ok`] を通ったものだけ）。
    /// 名前に `/` を含む項目があれば [`ApiError::BadResponse`]（相対パスが曖昧になる）。件数・深さの上限を超えると
    /// [`ApiError::TooManyPages`]。同じフォルダに 2 度入ったら（応答が壊れている）同じく失敗。
    pub fn list_tree(&self, root: &str, deadline: Deadline) -> Result<Vec<Entry>, ApiError> {
        check_id(root)?;
        let mut out: Vec<Entry> = Vec::new();
        let mut visited: HashSet<String> = HashSet::from([root.to_owned()]);
        let mut queue: std::collections::VecDeque<(String, String, usize)> =
            std::collections::VecDeque::from([(root.to_owned(), String::new(), 0)]);
        while let Some((id, prefix, depth)) = queue.pop_front() {
            for mut e in self.list_children(&id, deadline)? {
                if e.name.contains('/') {
                    return Err(ApiError::BadResponse);
                }
                e.path = if prefix.is_empty() {
                    e.name.clone()
                } else {
                    format!("{prefix}/{}", e.name)
                };
                if !rel_path_ok(&e.path) {
                    return Err(ApiError::BadResponse);
                }
                if e.is_dir {
                    if depth + 1 >= MAX_DEPTH || !visited.insert(e.id.clone()) {
                        return Err(ApiError::TooManyPages);
                    }
                    queue.push_back((e.id.clone(), e.path.clone(), depth + 1));
                }
                out.push(e);
                if out.len() > self.max_entries {
                    return Err(ApiError::TooManyPages);
                }
            }
        }
        Ok(out)
    }

    /// ID で 1 件取る。無ければ [`ApiError::NotFound`]。
    pub fn get(&self, id: &str, deadline: Deadline) -> Result<FileMeta, ApiError> {
        check_id(id)?;
        let ctx = self.ctx(deadline);
        self.get_meta(&ctx, id)
    }

    fn get_meta(&self, ctx: &Ctx<'_>, id: &str) -> Result<FileMeta, ApiError> {
        let url = self.files_url(&format!("/files/{id}"), &[("fields", FILE_FIELDS)]);
        let json = self.get_json(ctx, "get", url)?;
        let meta = meta_from(&json)?;
        if meta.entry.id != id {
            return Err(ApiError::BadResponse);
        }
        Ok(meta)
    }

    /// ファイルの中身を読む（`max_bytes` を超えたら [`ApiError::TooLarge`]）。
    pub fn read(&self, id: &str, max_bytes: u64, deadline: Deadline) -> Result<Vec<u8>, ApiError> {
        check_id(id)?;
        let ctx = self.ctx(deadline);
        let req = Req::get(self.files_url(&format!("/files/{id}"), &[("alt", "media")]));
        let mut opts = self.opts("read");
        opts.max_body = max_bytes;
        match self.authorized(&ctx, &req, &opts)? {
            Outcome::Reply(r) if r.ok() => Ok(r.body),
            _ => Err(ApiError::BadResponse),
        }
    }

    /// 作成に使う ID を取る（`count` は 1〜1000）。
    pub fn generate_ids(&self, count: u32, deadline: Deadline) -> Result<Vec<String>, ApiError> {
        if !(1..=1000).contains(&count) {
            return Err(ApiError::Rejected("ID の数が範囲外です"));
        }
        let ctx = self.ctx(deadline);
        self.generate(&ctx, count)
    }

    fn generate(&self, ctx: &Ctx<'_>, count: u32) -> Result<Vec<String>, ApiError> {
        let n = count.to_string();
        let url = self.files_url(
            "/files/generateIds",
            &[("count", &n), ("space", "drive"), ("type", "files")],
        );
        let json = self.get_json(ctx, "generate_ids", url)?;
        let ids: Vec<String> = json["ids"]
            .as_array()
            .ok_or(ApiError::BadResponse)?
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| is_valid_drive_id(s))
                    .map(str::to_owned)
            })
            .collect::<Option<_>>()
            .ok_or(ApiError::BadResponse)?;
        let unique: HashSet<&String> = ids.iter().collect();
        if ids.len() != count as usize || unique.len() != ids.len() {
            return Err(ApiError::BadResponse);
        }
        Ok(ids)
    }

    // ---- 書き込み ----

    /// 親の下にフォルダを作り、その ID を返す。先に取った ID で `POST files` する（再送も同じ ID。応答を失った再送は 409 になり、
    /// `get` で親・名前・種類を照合して「済み」とする。重複したフォルダはできない）。
    pub fn create_folder(
        &self,
        parent: &str,
        name: &str,
        deadline: Deadline,
    ) -> Result<String, ApiError> {
        check_id(parent)?;
        check_name(name)?;
        let ctx = self.ctx(deadline);
        let id = self.generate(&ctx, 1)?.remove(0);
        let body = json!({"id": id, "name": name, "mimeType": FOLDER_MIME, "parents": [parent]})
            .to_string();
        let url = self.files_url("/files", &[("fields", FILE_FIELDS)]);
        let req = Req::with_body(
            Method::Post,
            url,
            "application/json; charset=UTF-8",
            body.as_bytes(),
        );
        let mut opts = self.opts("create_folder");
        opts.passthrough = &[409];
        let matches = |m: &FileMeta| {
            m.entry.id == id
                && m.entry.is_dir
                && m.entry.name == name
                && m.parents == [parent]
                && !m.trashed
        };
        match self.authorized(&ctx, &req, &opts)? {
            Outcome::Reply(r) if r.ok() => {
                if !matches(&meta_from(&r.json()?)?) {
                    return Err(ApiError::Mismatch);
                }
            }
            Outcome::Reply(r) if r.status == 409 => {
                // この ID で、既に作られている（前の送信の応答を失った）
                if !matches(&self.get_meta(&ctx, &id)?) {
                    return Err(ApiError::Mismatch);
                }
            }
            _ => return Err(ApiError::BadResponse),
        }
        Ok(id)
    }

    /// 親の下に `local` を `name` で送り、その ID を返す。`size`・`expected_md5` は、これから送る中身の大きさと MD5（照合に使う）。
    /// `size` が [`MULTIPART_MAX`] 以下なら multipart、超えるなら resumable（チャンクごとに、締め切りとキャンセルを見る。途中で切れたら
    /// 受け取り済みの位置を問い合わせて続きから送り、セッションが失われたら**同じ ID で**始め直す）。どちらも先に取った ID で送り、
    /// 409 は「済み」として `get` で親・名前・大きさ・`md5Checksum` を照合する。**既存のファイルの中身を更新する呼び出しは作らない**。
    pub fn upload(
        &self,
        parent: &str,
        name: &str,
        local: &Path,
        expected_md5: &str,
        size: u64,
        deadline: Deadline,
    ) -> Result<String, ApiError> {
        check_id(parent)?;
        check_name(name)?;
        let md5 = normalize_md5(expected_md5).ok_or(ApiError::Rejected("MD5 の形が違います"))?;
        let ctx = self.ctx(deadline);
        let want = Uploaded {
            parent,
            name,
            size,
            md5: &md5,
        };
        // 手元の中身の確認は、ネットワークに出る前に済ませる（multipart は、送る本文として読んだものをそのまま使う）
        let content = if size <= self.multipart_max {
            let content = read_exact_file(local, size)?;
            if hex_md5(&content) != md5 {
                return Err(ApiError::Rejected("手元のファイルの MD5 が違います"));
            }
            Some(content)
        } else {
            None
        };
        let id = self.generate(&ctx, 1)?.remove(0);
        match content {
            Some(content) => self.upload_multipart(&ctx, &id, &want, &content)?,
            None => self.upload_resumable(&ctx, &id, &want, local)?,
        }
        Ok(id)
    }

    fn upload_multipart(
        &self,
        ctx: &Ctx<'_>,
        id: &str,
        want: &Uploaded<'_>,
        content: &[u8],
    ) -> Result<(), ApiError> {
        let metadata = json!({"id": id, "name": want.name, "parents": [want.parent]}).to_string();
        let boundary = boundary_for(content);
        let mut body = Vec::with_capacity(content.len() + 512);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = format!(
            "{}/files?uploadType=multipart&fields={}",
            self.transport.endpoints().upload(),
            encode(FILE_FIELDS)
        );
        let req = Req::with_body(
            Method::Post,
            url,
            &format!("multipart/related; boundary={boundary}"),
            &body,
        );
        let mut opts = self.opts("upload");
        opts.passthrough = &[409];
        opts.cap = CHUNK_REQUEST_CAP;
        match self.authorized(ctx, &req, &opts)? {
            Outcome::Reply(r) if r.ok() => self.verify_uploaded(&meta_from(&r.json()?)?, id, want),
            Outcome::Reply(r) if r.status == 409 => {
                self.verify_uploaded(&self.get_meta(ctx, id)?, id, want)
            }
            _ => Err(ApiError::BadResponse),
        }
    }

    fn verify_uploaded(
        &self,
        meta: &FileMeta,
        id: &str,
        want: &Uploaded<'_>,
    ) -> Result<(), ApiError> {
        let ok = meta.entry.id == id
            && !meta.entry.is_dir
            && meta.entry.name == want.name
            && meta.parents == [want.parent]
            && !meta.trashed
            && meta.entry.size == i64::try_from(want.size).unwrap_or(-1)
            && meta.entry.md5.as_deref() == Some(want.md5);
        if ok { Ok(()) } else { Err(ApiError::Mismatch) }
    }

    /// resumable のセッションを始める。`Done` は、その ID のファイルが既にある（409）ことを照合できたとき。
    fn start_session(
        &self,
        ctx: &Ctx<'_>,
        id: &str,
        want: &Uploaded<'_>,
    ) -> Result<Session, ApiError> {
        let metadata = json!({"id": id, "name": want.name, "parents": [want.parent]}).to_string();
        let url = format!(
            "{}/files?uploadType=resumable&fields={}",
            self.transport.endpoints().upload(),
            encode(FILE_FIELDS)
        );
        let req = Req::with_body(
            Method::Post,
            url,
            "application/json; charset=UTF-8",
            metadata.as_bytes(),
        )
        .header(
            "X-Upload-Content-Type",
            "application/octet-stream".to_owned(),
        )
        .header("X-Upload-Content-Length", want.size.to_string());
        let mut opts = self.opts("upload");
        opts.passthrough = &[409];
        match self.authorized(ctx, &req, &opts)? {
            Outcome::Reply(r) if r.ok() => {
                let location = r.location.ok_or(ApiError::BadResponse)?;
                // セッションの URL は、設定のアップロードの URL と、スキーム・ホスト・ポートが同じものだけ（別のホストへは送らない）
                if !self.transport.endpoints().same_upload_origin(&location) {
                    tracing::warn!(
                        op = "upload",
                        "セッションの Location が別のホストを指しているので、送りません"
                    );
                    return Err(ApiError::Rejected(
                        "セッションの Location が別のホストを指しています",
                    ));
                }
                Ok(Session::Url(location))
            }
            Outcome::Reply(r) if r.status == 409 => {
                self.verify_uploaded(&self.get_meta(ctx, id)?, id, want)?;
                Ok(Session::Done)
            }
            _ => Err(ApiError::BadResponse),
        }
    }

    fn upload_resumable(
        &self,
        ctx: &Ctx<'_>,
        id: &str,
        want: &Uploaded<'_>,
        local: &Path,
    ) -> Result<(), ApiError> {
        let mut file = File::open(local).map_err(|e| ApiError::Io(e.kind()))?;
        let len = file.metadata().map_err(|e| ApiError::Io(e.kind()))?.len();
        if len != want.size {
            return Err(ApiError::Rejected("手元のファイルの大きさが違います"));
        }
        let retry = self.transport.retry();
        let mut restarts = 0u32;
        'session: loop {
            let url = match self.start_session(ctx, id, want)? {
                Session::Done => return Ok(()),
                Session::Url(url) => url,
            };
            let mut offset = 0u64;
            let mut failures = 0u32;
            loop {
                ctx.check()?;
                let end = (offset + self.chunk_size).min(want.size);
                let chunk_len = usize::try_from(end - offset).map_err(|_| ApiError::TooLarge)?;
                let mut buf = vec![0u8; chunk_len];
                file.seek(SeekFrom::Start(offset))
                    .and_then(|_| file.read_exact(&mut buf))
                    .map_err(|e| ApiError::Io(e.kind()))?;
                let range = format!("bytes {offset}-{}/{}", end - 1, want.size);
                let req =
                    Req::with_body(Method::Put, url.clone(), "application/octet-stream", &buf)
                        .header("Content-Range", range);
                let mut opts = self.opts("upload");
                opts.cap = CHUNK_REQUEST_CAP;
                // セッションの URL（`upload_id`）が認可するので、トークンは付けない
                let sent = self.transport.attempt(ctx, &req, None, &opts);
                let failed = match sent {
                    Ok(reply) if reply.ok() => {
                        return self.verify_uploaded(&meta_from(&reply.json()?)?, id, want);
                    }
                    Ok(reply) => match reply.status {
                        308 => {
                            offset = next_offset(&reply, want.size, offset)?;
                            failures = 0;
                            continue;
                        }
                        404 | 410 => {
                            restarts += 1;
                            if restarts > MAX_SESSION_RESTARTS {
                                return Err(ApiError::status(reply.status));
                            }
                            tracing::debug!(
                                op = "upload",
                                status = reply.status,
                                restart = restarts,
                                "セッションが失われました。同じ ID で始め直します"
                            );
                            continue 'session;
                        }
                        409 => {
                            return self.verify_uploaded(&self.get_meta(ctx, id)?, id, want);
                        }
                        429 | 500..=599 => ApiError::Status(reply.status),
                        s => return Err(ApiError::status(s)),
                    },
                    Err(ApiError::Net(f)) if f != super::http::Failure::Other => ApiError::Net(f),
                    Err(e) => return Err(e),
                };
                // 途中で切れた・一時的な失敗: 少し待って、受け取り済みの位置を問い合わせて続きから送る
                failures += 1;
                if failures > retry.max_retries {
                    return Err(failed);
                }
                tracing::debug!(
                    op = "upload",
                    retry = failures,
                    "チャンクの送信に失敗しました。位置を問い合わせます"
                );
                ctx.sleep(retry_wait(&retry, failures - 1))?;
                match self.query_session(ctx, &url, want.size)? {
                    SessionState::Complete(body) => {
                        return self.verify_uploaded(&meta_from(&body)?, id, want);
                    }
                    SessionState::Received(n) => offset = n,
                    SessionState::Lost => {
                        restarts += 1;
                        if restarts > MAX_SESSION_RESTARTS {
                            return Err(ApiError::NotFound);
                        }
                        continue 'session;
                    }
                }
            }
        }
    }

    /// `Content-Range: bytes */<size>` で、セッションが受け取り済みの位置を問い合わせる。
    fn query_session(&self, ctx: &Ctx<'_>, url: &str, size: u64) -> Result<SessionState, ApiError> {
        let req = Req::with_body(Method::Put, url.to_owned(), "application/octet-stream", &[])
            .header("Content-Range", format!("bytes */{size}"));
        let mut opts = self.opts("upload");
        opts.passthrough = &[308, 404, 410];
        let Outcome::Reply(r) = self.transport.send_with_retry(ctx, &req, None, &opts)? else {
            return Err(ApiError::BadResponse);
        };
        match r.status {
            200 | 201 => Ok(SessionState::Complete(r.json()?)),
            308 => Ok(SessionState::Received(next_offset(&r, size, 0)?)),
            404 | 410 => Ok(SessionState::Lost),
            s => Err(ApiError::status(s)),
        }
    }

    /// `id` を、`from_parent` から `to_parent` へ、`new_name` にして移す（1 回の `PATCH` で親と名前を変える。ID は保たれる）。
    /// 再送の前（と、4xx で断られたとき）に `get` で今の状態を見て、既に目的の状態（親が `to_parent` だけ・名前が `new_name`・
    /// ゴミ箱に入っていない）なら成功とする。
    pub fn move_to(
        &self,
        id: &str,
        from_parent: &str,
        to_parent: &str,
        new_name: &str,
        deadline: Deadline,
    ) -> Result<(), ApiError> {
        check_id(id)?;
        check_id(from_parent)?;
        check_id(to_parent)?;
        check_name(new_name)?;
        if from_parent == to_parent {
            return Err(ApiError::Rejected("移動元と移動先が同じです"));
        }
        let ctx = self.ctx(deadline);
        let done =
            |m: &FileMeta| m.parents == [to_parent] && m.entry.name == new_name && !m.trashed;
        let settled = || Ok(done(&self.get_meta(&ctx, id)?));
        let body = json!({"name": new_name}).to_string();
        let url = self.files_url(
            &format!("/files/{id}"),
            &[
                ("addParents", to_parent),
                ("removeParents", from_parent),
                ("fields", FILE_FIELDS),
            ],
        );
        let req = Req::with_body(
            Method::Patch,
            url,
            "application/json; charset=UTF-8",
            body.as_bytes(),
        );
        let mut opts = self.opts("move");
        opts.settled = Some(&settled);
        match self.authorized(&ctx, &req, &opts)? {
            Outcome::AlreadyDone => Ok(()),
            Outcome::Reply(r) if r.ok() => {
                if done(&meta_from(&r.json()?)?) {
                    Ok(())
                } else {
                    Err(ApiError::Mismatch)
                }
            }
            _ => Err(ApiError::BadResponse),
        }
    }

    /// `id` を Drive のゴミ箱へ移す（完全な削除ではない。ゴミ箱から戻せる）。再送の前（と、4xx で断られたとき）に `get` で見て、
    /// 既にゴミ箱の中なら成功とする。
    pub fn trash(&self, id: &str, deadline: Deadline) -> Result<(), ApiError> {
        check_id(id)?;
        let ctx = self.ctx(deadline);
        let settled = || Ok(self.get_meta(&ctx, id)?.trashed);
        let body = json!({"trashed": true}).to_string();
        let url = self.files_url(&format!("/files/{id}"), &[("fields", FILE_FIELDS)]);
        let req = Req::with_body(
            Method::Patch,
            url,
            "application/json; charset=UTF-8",
            body.as_bytes(),
        );
        let mut opts = self.opts("trash");
        opts.settled = Some(&settled);
        match self.authorized(&ctx, &req, &opts)? {
            Outcome::AlreadyDone => Ok(()),
            Outcome::Reply(r) if r.ok() => {
                if meta_from(&r.json()?)?.trashed {
                    Ok(())
                } else {
                    Err(ApiError::Mismatch)
                }
            }
            _ => Err(ApiError::BadResponse),
        }
    }
}

/// これから送るファイルの、期待する姿。
struct Uploaded<'a> {
    parent: &'a str,
    name: &'a str,
    size: u64,
    md5: &'a str,
}

enum Session {
    Url(String),
    /// その ID のファイルが既にあり、期待どおりだった。
    Done,
}

enum SessionState {
    Complete(Value),
    Received(u64),
    Lost,
}

/// 308 の `Range: bytes=0-N` から、次に送る位置（N + 1。見出しが無ければ 0）。`size` を超える・戻りすぎる値は断る。
fn next_offset(reply: &Reply, size: u64, current: u64) -> Result<u64, ApiError> {
    let received = match reply.range.as_deref() {
        None => 0,
        Some(r) => {
            let last = r
                .trim()
                .strip_prefix("bytes=0-")
                .and_then(|n| n.parse::<u64>().ok())
                .ok_or(ApiError::BadResponse)?;
            last.checked_add(1).ok_or(ApiError::BadResponse)?
        }
    };
    // 全部受け取ったのに 200 でない、は決められた形ではない
    if received >= size {
        return Err(ApiError::BadResponse);
    }
    let _ = current;
    Ok(received)
}

fn retry_wait(retry: &Retry, attempt: u32) -> Duration {
    retry
        .base
        .checked_mul(1u32 << attempt.min(16))
        .unwrap_or(retry.cap)
        .min(retry.cap)
}

// ---- 検査・写し ----

fn check_id(id: &str) -> Result<(), ApiError> {
    if is_valid_drive_id(id) {
        Ok(())
    } else {
        Err(ApiError::Rejected("Drive の ID の形ではありません"))
    }
}

/// 名前（フォルダ名・ファイル名）: 1〜255 バイト、制御文字と `/` を含まない。
fn check_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name.contains('/')
        || name.chars().any(char::is_control)
    {
        Err(ApiError::Rejected("名前の形が違います"))
    } else {
        Ok(())
    }
}

/// 一覧・取得の 1 項目を [`Entry`] にする。`size` が無いもの（Google 文書・フォーム・フォルダ・ショートカット）は -1。
/// `md5Checksum` が無い・形が違うものは MD5 無し。
fn entry_from(item: &Value, path: String) -> Result<Entry, ApiError> {
    let id = item["id"]
        .as_str()
        .filter(|s| is_valid_drive_id(s))
        .ok_or(ApiError::BadResponse)?;
    let name = item["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(ApiError::BadResponse)?;
    let mime = item["mimeType"].as_str().ok_or(ApiError::BadResponse)?;
    let size = match item.get("size") {
        None | Some(Value::Null) => -1,
        // Drive の int64 は文字列で返る
        Some(Value::String(s)) => s
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or(ApiError::BadResponse)?,
        Some(_) => return Err(ApiError::BadResponse),
    };
    let is_dir = mime == FOLDER_MIME;
    Ok(Entry {
        path,
        name: name.to_owned(),
        size: if is_dir { -1 } else { size },
        mime: mime.to_owned(),
        is_dir,
        id: id.to_owned(),
        shortcut_id: None,
        md5: item["md5Checksum"].as_str().and_then(normalize_md5),
    })
}

fn meta_from(item: &Value) -> Result<FileMeta, ApiError> {
    let name = item["name"]
        .as_str()
        .ok_or(ApiError::BadResponse)?
        .to_owned();
    let trashed = item["trashed"].as_bool().ok_or(ApiError::BadResponse)?;
    let parents: Vec<String> = match item.get("parents") {
        // 親が無い（マイドライブの根など）ことはある
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|p| {
                p.as_str()
                    .filter(|s| is_valid_drive_id(s))
                    .map(str::to_owned)
            })
            .collect::<Option<_>>()
            .ok_or(ApiError::BadResponse)?,
        Some(_) => return Err(ApiError::BadResponse),
    };
    Ok(FileMeta {
        entry: entry_from(item, name)?,
        trashed,
        parents,
    })
}

/// `local` の中身を、ちょうど `size` バイト読む（大きさが違えば断る）。
fn read_exact_file(local: &Path, size: u64) -> Result<Vec<u8>, ApiError> {
    let mut file = File::open(local).map_err(|e| ApiError::Io(e.kind()))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| ApiError::Io(e.kind()))?;
    if bytes.len() as u64 != size {
        return Err(ApiError::Rejected("手元のファイルの大きさが違います"));
    }
    Ok(bytes)
}

fn hex_md5(bytes: &[u8]) -> String {
    Md5::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// multipart の境界。中身に含まれない文字列にする。
fn boundary_for(content: &[u8]) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let candidate = format!("geoframe-export-{nanos:x}-{n:x}-{:x}", std::process::id());
        if !contains(content, candidate.as_bytes()) {
            return candidate;
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
