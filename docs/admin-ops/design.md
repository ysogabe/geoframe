# 運用コンソール（C） 設計

作成日: 2026-11-01。状態: 下書き（レビュー前）。PRD: [prd.md](prd.md)。行番号は 2026-11-01（main 37ea68b）時点。

## 概要

[PRD](prd.md) の 5 つの運用操作（同期をいま回す・品質解析の一括・Drive の大量削除の保留の解除・AI ジョブの再試行・
AI の停止の解除）を、稼働中の `serve` の中で行い、進み具合を「運用」タブで見せる。A の設計書
（[../admin-rework/design.md](../admin-rework/design.md)）と B の設計書（[../admin-library/design.md](../admin-library/design.md)。
どちらも末尾の「実装との差」が実装の正）の上に足す。

## 今の作り（調査結果）

### 同期サイクル

- `serve` は取り込みが有効なとき `sync::spawn_worker`（`crates/geoframe/src/sync.rs:438-459`）で 1 本のスレッドを起こす
  （`cli.rs:463-470`）。起動直後に 1 回、以後 `stop.wait(poll)`（`sync.rs:455`、`poll = config.drive_poll()`）で待って
  `one_cycle`（`sync.rs:464-492`）を回す。段の一覧は `default_stages_with(drive)`（`sync.rs:300`）で 1 回作り、閉包に move する。
- `StopFlag`（`stop.rs`）は `Condvar` と `Option<Instant>` だけで、**停止しか伝えられない**（`wait` は `stop.rs:84` 付近）。
  要求で起こす仕組みは無い。
- `run_cycle`（`sync.rs:368-432`）は段ごとにパニックを隔離し、`CycleReport { stages: Vec<StageReport{name, result}>, photos }`
  を返す。`StageResult` は `Done(Counts)`・`Skipped(&'static str)`・`Failed(String)`・`Panicked`・`NotRun`（`sync.rs:322-334`）。
  **`one_cycle` はこの戻り値を捨てる**（ログだけ。`sync.rs:489`）。`AppState` には何も残らない。
- `Stage` trait（`sync.rs:169-172`）は `name()` と `run(&self, ctx: &StageCtx)`。`StageCtx`（`sync.rs:69-82`）は
  `mode`・`config`・`read`・`writer: Option<&Writer>`・`cache`・`http`・`stop`。
- 段の失敗の文面 `StageError`（`sync.rs:127-139`）は SQLite のメッセージ（表名・列名まで）を含みうる。
- `.ingest.lock`（`ingest.rs:153-170`）は取り込み有効の `serve` が起動から終了まで持つ。CLI の書き込み系は
  `start_existing` で `Locked` になり「サービスを止めてから」と出る（`cli.rs:904-908`、`:1190`）。

### 品質解析

- 段 `quality`（`quality.rs:537-545` の `Quality::run` → `apply`）は 1 サイクル `quality_per_cycle` 件だけ解析し、
  残りが 0 のときだけ再構築する。CLI `quality-scan`（`cli.rs:771`）は `quality::scan(ctx)`（`quality.rs:548-591`）で、
  未解析が無くなるまで回る一括版。使うのは `ctx.read`・`ctx.writer()`・`ctx.config`・`ctx.stop` だけ（`cache`・`http` は
  構造体の都合で要る）。`one_off`（`cli.rs:797-879`）は `StopFlag::new()` を新しく作るので、CLI は停止の合図で止まらない。

### Drive の大量削除の保留

- `HoldReason`（`drive/index.rs:63-69`）: `Ratio`（消す行 > 50 件かつ Drive の行の 20% 超）・`EmptyListing`。
  `MassDeleteHold`（`index.rs:82-93`）: `held`・`reason`・`drive_rows`・`since`・`checked_at`（`YYYY-MM-DD HH:MM:SS`）。
- `MassDeleteBoard`（`index.rs:104-134`）は `Mutex<Option<MassDeleteHold>>`。プロセスの中だけ（DB には書かない。
  再起動で消え、次のサイクルでまた付く）。`DriveShared.board`（`drive.rs:276-299`）に入り、`serve` は
  `state.with_mass_delete_board`（`cli.rs:480`、`server.rs:155`）で `/api/admin/status` の `sync.mass_delete_hold` に出す
  （`admin.rs:254-270`）。
- `IndexDrive { shared, allow_mass_delete: bool }`（`index.rs:139-157`）。`plan()`（`index.rs:340-410`）は `allow` のとき
  `doomed` を全部 `deletes` にし（**一覧が空のときの全削除も通る**。`index.rs:398-401`）、`allow` でなければ空の一覧
  （`deletes_withheld`）と比率（`mass_delete_withheld`）で保留する。`run` は一覧を取って計画を作った直後、書き込みの前に
  `board.record(plan.hold(), now)` を呼ぶ（`index.rs:541-547`）。
- `default_stages_allowing_mass_delete`（`sync.rs:295-297`）は**新しい `DriveShared` を作る**ので、`serve` の保留の記憶とは
  別物になる。`serve` 内では使えない。
- 鍵が無いとき `index_drive` は `Skipped(NO_KEY)` で通信しない（`index.rs:514-518`）。試験で安全に使える。

### AI ジョブの操作

- `ai::ops::retry(writer, ai, handlers, target, clock)`（`ai/ops.rs:356-389`）: 種類ごとに登録済みの handler を探し
  （無ければ `unsupported`）、`ai.gate` で使えない用途は `policy_disabled`、`handler.prepare_retry()` を writer の外で呼び、
  `jobs::retry_failed`（`jobs.rs:1061`、`BEGIN IMMEDIATE`、`state='failed'` の行だけ）で `Requeue`／`Obsolete`／`Satisfied` に
  振り分ける。`RetryReport { requeued, obsolete, satisfied, unsupported, policy_disabled }`（`ops.rs:334-343`）。
- `ai::ops::release_pause(writer, ai, target)`（`ops.rs:410-444`）: `All` は `DELETE FROM ai_provider_pauses`、`Task` は今の設定の
  識別に当たる鍵だけ。設定が無ければ `ReleaseError::ConfigUnavailable`。
- 背景ワーカーの取り出しは `state IN ('pending','deferred')`（`jobs.rs:692`・`:719`）なので、`retry` が触る `failed` と重ならない。
  writer は単一スレッドで依頼が直列（`writer.rs`）。
- `serve` の組み立て（`cli.rs:429-452`）: `ai_clock = Arc::new(jobs::unix_now)`、`worker::production_handlers(...)` は値のまま
  `start_ai` に渡され、**`AppState` には handlers も clock も入らない**。`LanClient` は `query::Runtime` に move される。
- `release_pause` は `ai_provider_pauses` だけを消す。`ai_provider_waits`（`ai/waits.rs`。期限つきの一時待機）と、ワーカーの
  メモリ上の `blocked`（`worker.rs:521`）は残る。
- `ai/ops.rs:9-13` のモジュールコメントと `cli.rs:877-880` が「サービス停止中だけ」と書いている。

### 書き込み API の経路

- `post.rs`: `Route` enum（`:56-65`）、`label`（`:68-79`）、`route_of`（`:85-97`、完全一致）、`handle`（`:259-309`。
  `pre_body_check` で CSRF（`csrf.rs:65-80`）・`Content-Length` 必須・64 KiB、本文は JSON オブジェクト、`state.writer()` が
  無ければ `Unavailable`（503）、`spawn_blocking` の `match route`）。`Ok(value)` は **200 固定**（`:300-302`）。
  `ApiError`（`:100-113`）: 400・404・409 `Busy`・503・500。
- `guard`（`server.rs:582-611`）: Host 検査、`route_of` に無い POST は 403。GET の登録は `site`（`server.rs:689-`）の完全一致
  （`/api/admin/config` が `:748`、`/api/admin/curation/groups` が `:787`）を前方一致（`:793-805`）より前に置き、
  `labeled`・`json_response`（`no-store`）を使う。
- 選別タブの `token`（`curation.rs:277-301`。SHA-256 の先頭 8 バイトの hex）と 409 `group_changed`（`admin_curation.rs:416-423`）。

### `/api/admin/status` と試験

- `sync` は `{ready, pending, total}`（`admin.rs:207-212`）＋取り込み有効なら `mass_delete_hold`。`tests/admin_api.rs:892-893` が
  **`sync` の鍵を厳密に固定**している。`python_shape`（`admin_api.rs:28-52`）で除く項目を増やす手もあるが、`sync` の中には足せない。
- 取り込み有効の `serve` を子プロセスで起動する結合試験: `tests/ingest_cli.rs:604-634`（`serve(data, port, Some(("1", &staging)))`、
  `IngestEnv` は `GEOFRAME_TEST_EUID=1000` と存在しない鍵のパス）。`spawn_state`（`tests/common/mod.rs:453`）は `AppState` を
  変える閉包を受ける（`with_mass_delete_board` を入れられる）。

### 画面

- タブ: `admin.html:25-37` の `button[data-tab]`・`section#v-*`、`admin.js:64-79` の `showTab`（許可する名前の列挙、`.on`・`.hidden`、
  `closePhotoOverlays()`、タブごとの読み込み）、`boot()`（`admin.js:349-360`。`init*()` を呼び、状況タブは 30 秒の `setInterval`）。
  新しい JS は `admin.html:211` 付近の script タグと `build.rs`／`embedded.rs` の埋め込みに足す。
- 書き込み: 共通の `postJSON(path, body)`（`admin.js:42-51`。`Content-Type: application/json`、`!ok` は `error` を例外に）、
  `toast(msg, isErr)`（`admin.js:34`）。選別タブは `ver`（表示の版）で古い応答を捨て、`busy` で多重送信を防ぐ（`curation.js:1-35`）。
- 状況タブ: `massDeleteNotice`（`admin.js:114-131`。「この画面からは解除できません」の固定文言）、`aiOpsCells`
  （`admin.js:162-`。「サービスを止めて `geoframe ai-jobs release-pause …` で解除」が `admin_ai_ops_render.mjs:25` の期待値）。
- DOM は `el(tag, cls, text)`・`textContent`。`innerHTML`・`outerHTML`・`insertAdjacentHTML`・`document.write` は
  `tests/static_files.rs:479-491` が禁止（`confirm()` は禁止されていないが、fakedom に無いので使わない）。
- 描画試験: `web/tests/fakedom.mjs`（タイマーは無い。`admin_ai_ops_render.mjs:12` は `setInterval` 等を空実装で渡す）、
  `*_render.mjs` は `fetch` を URL ごとの合成 JSON に差し替える。Rust 側は `tests/common/node.rs:18-44`（ヒープ上限・120 秒）経由。

## 方針

1. **運用の要求は同期ワーカーの 1 本のスレッドで直列に処理する**（R10）。段は「読んでから書く」（`index_drive` の計画 → 書き込み、
   `quality` の未解析の読み取り → 解析 → 書き込み）ので、別スレッドで並行に走らせると同じ行を二重に扱う。ワーカーの待ち
   `stop.wait(poll)` を、**短い刻み（100 ms）の `stop.wait(min(次の周期まで, 100 ms))` の前後で要求の有無を見る**ループに
   置き換える。停止は今までどおり `StopFlag` の `Condvar` で即座に起き、要求の検出は最大 100 ms 遅れる（十分）。新しい
   `Condvar` も停止のポーリングも要らない。
   - 選ばなかった案: 要求ごとに `spawn_blocking` で段を走らせる（上の二重処理）。要求用の `Condvar` を足して 1 秒刻みで
     停止を見る（停止が最大 1 秒遅れ、`Condvar` が 2 つになる）。`StopFlag` に要求の通知を足す（停止の意味が濁る）。
2. **結果はメモリ上の掲示板（`ops::Board`）に残し、`GET /api/admin/ops` で読む**（R14）。`MassDeleteBoard` と同じ作り
   （`Arc<Mutex<…>>`、DB に書かない、再起動で消える）。`/api/admin/status` には足さない（`sync` の鍵が固定。R16）。
3. **AI の再試行・停止の解除は POST の中（`spawn_blocking`）で writer を通して行う**（R13）。同期ワーカーの待ち行列には
   入れない（段ではなく、CLI の `ai-jobs` と同じ関数の呼び出し）。ただし次の 2 点を守る:
   - **再試行用の handler は背景ワーカーとは別のインスタンス**にする。`prepare_retry` は handler の中の作業一覧を書き換える
     （`scene_judge/runtime.rs:273-278` が `self.work` を置き換える。背景の `reconcile` も同じ領域を書く）ので、共有すると
     再試行の対象を誤って `Obsolete` にしうる（Sol の指摘 6）。`serve` は `worker::production_handlers(...)` を **2 回**呼び、
     1 つを `start_ai` に、もう 1 つを `AppState.ai_retry_handlers` に持たせる（`clock` も `AppState.ai_clock`）。
     `production_handlers` は `DataDir::open` と `Arc` の複製だけで、通信はしない（`worker.rs:144-178`）。
   - **同時に 1 つだけ**。`AppState.ai_ops_lock: Arc<Mutex<()>>` を `try_lock` し、取れなければ 409 `busy`。再試行は種類ごとに
     writer を呼ぶ複数のトランザクション（`ops.rs:369-383`）で、`prepare_retry` が DB の走査（読み取り専用の接続）を行うので、
     数秒かかりうる。`retry_failed` は `failed` の行に加え、画像 embedding の同じ系統（lineage）の別世代の行の予算
     （`attempts`・`defer_count`・`terminal`）もリセットする（`jobs.rs:1105-1123`）。これは CLI と同じで、背景ワーカーが
     `pending`・`deferred` を取り出す条件（`jobs.rs:692`）とは行が重ならないが、「`failed` だけ」ではない。
   結果は応答に入れ、`Board` の履歴にも 1 件として残す（`kind: "ai_retry"`／`"ai_release_pause"`、`report`）。
4. **大量削除の解除は「集合つきの承認」＋「集合に限った許可」＋「消す前の写し」の 3 重にする**（R12）。
   - `MassDeleteHold` に保留した行の ID の集合（`ids: Vec<i64>`、昇順。メモリの中だけで応答には出さない）を足す。
     `plan()` の `doomed` がそのまま使える（`index.rs:387-396`）。
   - 印（`token`）は `held`・`reason`・`drive_rows`・`since` と、並べた ID を改行でつないだものの SHA-256 の先頭 8 バイトの hex
     （選別タブの `group_token`、`curation.rs:277-301` と同じ作り）。POST で一致を確かめ、要求は印と ID の集合を持ち続ける。
   - ワーカーが取り出して**実行する直前にもう一度** `MassDeleteBoard::current()` の印と照らし、違えば実行せず履歴に
     `hold_changed`（待っているあいだの定期サイクルで保留が変わった・消えた場合。Sol の指摘 5）。
   - 実行する `index_drive` には `Allow::Only(ids)` を渡す。`plan()` は **`Allow` で最初に分岐**する: `Yes` は今までどおり全部消す、
     `Only(ids)` は取り直した一覧で消える行（`doomed`）が**すべて `ids` に含まれるときだけ** `doomed` を消し、含まれない行が
     1 件でもあれば保留に戻す（空の一覧なら `deletes_withheld`、それ以外は `mass_delete_withheld`。既存の 2 つの鍵を使う）。
     `doomed` が `ids` の一部だけ（戻ってきた行がある）なら、その一部だけを消す（結果の `counts` に `approved`・`deleted`）。
     `No` は今までどおり。承認したのとは別の行は、件数が同じでも消えない（Opus の指摘 1）。
   - **一覧が空のための保留（`EmptyListing`）は画面から解除しない**（POST は 409 `not_releasable`、画面はボタンを出さず案内）。
     設定ミスで同じ表示になり、通すと Drive の全行が消えるため。CLI の `--allow-mass-delete` だけに残す（Opus の指摘 3。
     PRD の「決まっていないこと」）。
   - 削除の書き込みの直前に、writer で `VACUUM INTO 'DATA_DIR/tmp/catalog-before-release.db'`（既存の写しは消してから。1 つだけ）
     を作り、失敗したら削除しない（`StageError`）。確認の文言に「解除前の写しが残る」と書く（Opus の指摘 4）。
   - 受付（承認した件数・理由）と終了（削除した件数・結果の種別）を `tracing::warn!` で固定の語と件数だけ 1 行ずつ出す。
   - `allow_mass_delete: bool` は `enum Allow { No, Yes, Only(Vec<i64>) }` に広げ、CLI の `--allow-mass-delete` は `Yes` のまま
     （引数・挙動は変えない。R18）。`IndexDrive::allowing_only(shared, ids)`。
   - 選ばなかった案: `default_stages_allowing_mass_delete` を使う（別の `DriveShared` で記憶が分かれる）。全段を回す
     （削除だけが目的なので `index_drive` 1 段でよい。続きは次の定期サイクル）。件数の手入力（ユーザーの選択で不採用）。
5. **品質解析の一括は `quality::scan`（CLI と同じ）を `Stage` に包み（`quality::Scan`、`name() = "quality"`）、他の要求と同じ
   `run_cycle` の経路で走らせる**（R11）。段ごとのパニックの隔離と、`one_cycle` の外側の隔離（DB を開けない・`run_cycle` 自身の
   パニック）を要求にも同じに効かせ、どの経路でも `Board` の記録を終えて `running` を外す（Sol の指摘 8）。長く走るあいだ
   定期サイクルと他の要求は待つ（PRD の「決まっていないこと」）。停止の合図で段が止まる（CLI より安全）。
6. **応答に環境の情報を出さない**。段の失敗は `result: "failed"` だけ（文面はログ）。`Skipped` の理由は `&'static str` の
   固定文なので出す。件数は `Counts`（`(&'static str, u64)`）をそのまま出す。
7. **画面は 1 つの `GET /api/admin/ops` を数秒ごとに読む**。応答の順序は要求番号（`seq`）で守る（最新の要求より古い応答は
   捨てる）。選別タブの `ver` ほど複雑な規則は要らない（段は 1 枚で、画面の状態はサーバーの写し＋送信中の印＋確認の段だけ）。
   POST を出した時点で `sending` に種類を入れてボタンを無効にし、応答で外して読み直す。確認の段（`confirming`）は開いた
   時点の `token`・`held` を固定して持ち、読み直しで `hold` の `token` が違う・無ければ閉じて `toast` で知らせる。

## 構成

```mermaid
flowchart LR
  subgraph browser[管理画面（素の JS）]
    admin[admin.js タブ・postJSON・toast]
    ops[ops.js 運用タブ（操作・進み具合・履歴・確認の段）]
  end
  subgraph server[geoframe serve]
    opsapi[admin_ops.rs GET /api/admin/ops・POST /api/admin/ops/*]
    board[ops.rs Board（実行中・待ち・履歴）/ Queue（要求の受け口）]
    worker[sync.rs spawn_worker（定期サイクル＋要求）]
    index[drive/index.rs IndexDrive Allow::Only]
    quality[quality.rs scan]
    aiops[ai/ops.rs retry・release_pause]
    writer[(writer)]
    mdb[MassDeleteBoard]
  end
  ops -->|GET 数秒ごと| opsapi
  ops -->|POST| opsapi
  opsapi -->|要求を入れる| board
  opsapi -->|retry / release_pause| aiops --> writer
  board -->|起こす| worker
  worker --> index & quality --> writer
  index --> mdb
  worker -->|結果を書く| board
  opsapi -->|読む| board & mdb
```

### 新しいモジュールと変更

| 場所 | 変更 |
|---|---|
| `src/ops.rs`（新） | `Request`・`RequestKind`・`Record`・`Board`（実行中・待ち・履歴・採番を 1 つの `Mutex` で）、`token_of(&MassDeleteHold)`、JSON 化 |
| `src/sync.rs` | `spawn_worker` に `Board` と `MassDeleteBoard` を渡す。ループを「要求があれば要求、無ければ周期でサイクル」に（100 ms 刻みの `stop.wait`）。`run_cycle` の結果を `Board` へ。`StageResult` を JSON にする補助 |
| `src/quality.rs` | `Scan`（`quality::scan` を包む `Stage`） |
| `src/drive/index.rs` | `allow_mass_delete: bool` → `Allow { No, Yes, Only(Vec<i64>) }`。`plan()` の分岐に `Only` を足す。`MassDeleteHold.ids`、`DrivePlan.counts` に `approved`・`deleted`。`IndexDrive::allowing_only(shared, ids)`。削除の直前の `VACUUM INTO` |
| `src/sync.rs` | `stages(drive, allow)` を `Allow` に。`index_drive_stage_only(drive, ids)`（1 段だけの `Vec<Box<dyn Stage>>`） |
| `src/admin_ops.rs`（新） | `GET /api/admin/ops` の組み立て、5 本の POST の入力検証と実行 |
| `src/post.rs` | `Route::OpsSync`・`OpsQualityScan`・`OpsDriveRelease`・`OpsAiRetry`・`OpsAiReleasePause` |
| `src/server.rs` | `AppState` に `ops: Option<Arc<ops::Board>>`、`ai_retry_handlers: Arc<Vec<Arc<dyn Handler>>>`、`ai_clock: Clock`、`ai_ops_lock: Arc<Mutex<()>>`。`site` に `/api/admin/ops` の完全一致 |
| `src/cli.rs` | `serve` で `ops::Board` を作り、`spawn_worker` と `AppState` に渡す。`production_handlers` を 2 回呼び、2 つ目を `AppState` へ（PR ②） |
| `web/static/js/ops.js`（新）、`admin.html`、`admin.css`、`admin.js` | 運用タブ。`showTab` に `ops` を足し、離れるとき `stopOpsPolling()` |
| `web/tests/ops_render.mjs`、`tests/admin_ui_ops.rs`、`tests/ops_api.rs` | 試験 |
| 文書 | `deploy/README.md`、`deploy/geoframe-admin.sh`、`docs/rust-migration/server.md`、`docs/architecture/photo-pipeline.md`、`docs/README.md`、`ai/ops.rs` のコメント |

### 同期ワーカーの流れ

```mermaid
sequenceDiagram
  participant UI as 運用タブ
  participant API as admin_ops
  participant Q as Queue/Board
  participant W as sync-worker
  UI->>API: POST /api/admin/ops/sync
  API->>Q: enqueue(Request{sync}) → queued(id) / Busy
  API-->>UI: 200 {request:{id, state:"queued"}} / 409 busy
  W->>Q: wait_request_or_tick(next_cycle_at)
  Q-->>W: Request
  W->>Q: start(id)
  W->>W: run_cycle(stages, ctx)
  W->>Q: finish(id, CycleReport)
  UI->>API: GET /api/admin/ops（数秒ごと）
  API->>Q: snapshot()
  API-->>UI: {worker, hold, history}
```

ワーカーのループ（擬似コード）:

```
next_cycle_at = now                                   // 起動直後に 1 回
loop {
  if stop.is_stopped() { break }
  if let Some(req) = board.take_queued() {            // 待ち → 実行中 を同じロックの中で（running に入れて返す）
    let outcome = run(&req);                          // 下の隔離つき
    board.finish(req.id, outcome);
    if req.kind == Sync { next_cycle_at = now + poll } // 手動の同期は定期の 1 回分（続けて 2 回回さない）。品質解析・保留の解除は周期を変えない
    continue;
  }
  if now >= next_cycle_at {
    board.start_cycle(); board.finish_cycle(one_cycle(...)); next_cycle_at = now + poll;
    continue;
  }
  if stop.wait(min(next_cycle_at - now, 100 ms)) { break }   // 停止は即座、要求の検出は最大 100 ms 遅れ
}
```

`run(req)` は `one_cycle` と同じ形（読み取り接続を開く → `StageCtx` → `catch_unwind(run_cycle)`）で、段の一覧だけを要求ごとに変える:

- `Sync` → 定期と同じ `stages`。
- `QualityScan` → `[quality::Scan]`（1 段 `quality`）。
- `DriveRelease { token, ids }` → 実行の直前に `mass_delete.current()` の印と `token` を照らし、違えば実行せず `outcome: hold_changed`。
  一致すれば `[IndexDrive::allowing_only(drive, ids)]`。段の結果が `Skipped`（鍵なし・認証失敗・停止）なら `outcome: skipped`
  （理由つき。`done` にしない）。`Done` で `counts` の `deletes_withheld` か `mass_delete_withheld` が **0 より大きければ**
  `outcome: exceeded`（`counts()` は 0 でも鍵を入れるので、有無では判定しない。Opus の指摘 6）。それ以外は `done`
  （`counts` の `approved`・`deleted` を画面に必ず出す）。

DB を開けない・`run_cycle` 自身がパニックしたときも `board.finish(id, failed)` を必ず呼ぶ（`finish` は `Drop` で保証する
`Running` のガードにする）。スレッドは終わらせない。

`Board` は `Mutex<Inner { running, queued, next_id, history, last_cycle, next_cycle_at }>` 1 つ。`enqueue(kind, …)` は同じロックの
中で「`queued` が空」「`running` が同じ種類でない」「停止の合図が無い」を確かめて入れ、どれかに当たれば `Err(Busy)`／
`Err(Stopping)`。`take_queued` は `queued` を `running` に移して返す（取り出した直後に同じ種類を受け付けない）。
定期サイクルの実行中に来た要求は入り、サイクルが終わった直後のループの冒頭で取り出される。

### `Board` の中身

```
Board {
  running: Option<Running { id, kind, started_at }>,          // 要求か定期サイクル（kind: "cycle"）
  queued: Option<Queued { id, kind, requested_at }>,
  next_cycle_at: Option<String>,                              // 周期の予定（実行中は None）
  last_cycle: Option<Record>,                                 // 定期サイクルの直近
  history: VecDeque<Record>,                                  // 要求の履歴。最新 10 件
  next_id: u64,
}
Record { id, kind, requested_at, started_at, finished_at, outcome, stages: Vec<StageJson>, report: Option<Value> }
StageJson { name, result: "done"|"skipped"|"failed"|"panicked"|"not_run", reason: Option<&'static str>, counts: Map<&str,u64> }
outcome: "done"（失敗 0）| "partial"（失敗あり）| "failed"（DB を開けない・外側のパニック）| "skipped"（drive-release の段が省略）
       | "exceeded"（drive-release で承認した集合に無い行が消えようとした）| "hold_changed"（実行直前に保留が違った）
       | "retried"・"released"（AI 系。PR ②）
report: Option<Report>  // enum Report { Retry(RetryReport), Release { released: usize } }。自由な Value にしない（Opus の指摘 10）
```

時刻は `sysinfo::local_timestamp()` と同じ `YYYY-MM-DD HH:MM:SS`（`MassDeleteHold` と揃える）。

## インターフェース

### `GET /api/admin/ops`

完全一致。`Cache-Control: no-store`。HEAD・POST は 403（`guard`）。`INGEST=0` でも 200。

```json
{
  "ok": true,
  "ingest": true,
  "poll_minutes": 10,
  "worker": {
    "running": {"id": 12, "kind": "sync", "started_at": "2026-11-01 10:00:00"},
    "queued": null,
    "next_cycle_at": "2026-11-01 10:10:00",
    "last_cycle": {"id": 0, "kind": "cycle", "started_at": "…", "finished_at": "…", "outcome": "done",
                   "stages": [{"name": "index_local", "result": "done", "counts": {"indexed": 3}},
                              {"name": "index_drive", "result": "skipped", "reason": "Drive の鍵がありません"}]}
  },
  "hold": {"held": 120, "reason": "ratio", "drive_rows": 500, "since": "…", "checked_at": "…", "token": "9f3a…"},
  "ai": {"retry_kinds": ["photo_judge"], "failed": {"photo_judge": 4}, "pauses": 1},
  "history": [ {"id": 12, "kind": "sync", "requested_at": "…", "started_at": "…", "finished_at": null, "outcome": null, "stages": []}, … ]
}
```

- `ingest: false` のときは `worker`・`hold`・`ai` を `null`、`history` を `[]`。
- `hold` は `MassDeleteBoard::current()` が `Some` のときだけ。`token = hex(sha256("{held}\t{reason}\t{drive_rows}\t{since}\n{id1}\n{id2}…")[..8])`。
  ID は応答に出さない。`releasable: reason == "ratio"` を添える。
- `ai.retry_kinds` は再試行用の `handlers` に登録のある種類（いまの配布バイナリで `retry` が実際に再検査できる種類。起動時に
  固定）。**この GET は DB を読まない**（`Board`・`MassDeleteBoard` のスナップショットだけ。3 秒ごとの読み直しがタブの数だけ
  DB を引かないため。Opus の指摘 9）。AI の件数・理由コード・停止・待ちは `/api/admin/status` を画面が併読する（運用タブを
  開いたときと AI の操作のあとに 1 回ずつ）。

### POST（すべて JSON、`post.rs` の経路。`INGEST=0`／`ops` が無いときは 503）

| 経路 | 本文 | 成功 | 失敗 |
|---|---|---|---|
| `/api/admin/ops/sync` | `{}` | 200 `{ok, request:{id, kind:"sync", state:"queued"}}` | 409 `busy`・`too_soon`（前回の手動の同期の終了から 60 秒以内） |
| `/api/admin/ops/quality-scan` | `{}` | 同上 `kind:"quality_scan"` | 409 `busy` |
| `/api/admin/ops/drive-release` | `{"token": "<16 hex>"}` | 同上 `kind:"drive_release"`、`approved: N` | 400 `bad_params`、409 `no_hold`・`hold_changed`・`not_releasable`・`busy` |
| `/api/admin/ops/ai-retry` | `{"target": "all"}` か `{"target": {"kind": "<JobKind>"}}` | 200 `{ok, report:{requeued, obsolete, satisfied, unsupported, policy_disabled}}` | 400 `bad_params`、500 |
| `/api/admin/ops/ai-release-pause` | `{"target": "all"}` か `{"target": {"task": "<TaskKind>"}}` | 200 `{ok, released: n}` | 400 `bad_params`、409 `config_unavailable`、500 |

- `busy` は `ApiError::Busy("busy")`。`hold_changed`・`no_hold`・`config_unavailable` も `Busy` の符号（409）。停止の合図のあとは
  `Unavailable`（503）。
- `drive-release` は `token` を検証してから `enqueue`（要求に `token` と `ids` を入れる）。検証と実行のあいだに保留が
  変わる窓は、実行直前の再照合（違えば `hold_changed`）と `Only(ids)`（集合に無い行は消さない）の 2 つで塞ぐ。
- `ai-retry`・`ai-release-pause`（PR ②）は `ai_ops_lock` を `try_lock`（取れなければ 409 `busy`）し、`spawn_blocking` の中で
  `ai::ops::retry`／`release_pause` を呼ぶ（CLI と同じ関数、別インスタンスの handlers）。`JobKind`・`TaskKind` の文字列は CLI の
  `--kind`・`--task` と同じ名前。未知なら 400。途中で失敗したとき（種類ごとのトランザクションの一部が済んだあとの失敗）は 500 の
  定型文で、済んだ種類の件数は履歴の `report` に残す（CLI も同じく途中まで進む）。

### 画面（`ops.js`）

- 状態 `os = { seq, timer, snap, sending: Set<kind>, confirming: {token, held} | null }`。`openOpsTab()` で読み直しと 3 秒の
  `setInterval` を始め、`stopOpsPolling()`（`showTab` から）で止める。`fetch` は `seq` を控え、応答時に `seq` が違えば捨てる。
  ボタンの `disabled` は `sending`・`running`・`queued` のどれかに同じ種類があるとき。
- 描画は 4 つの区画: 「操作」（ボタン＋説明。`running`・`queued` と同じ種類は `disabled`、`ingest:false` なら区画ごと案内に置換）、
  「大量削除の保留」（`hold` があるときだけ。2 段階の確認。R6）、「定期サイクル」（`last_cycle`・`next_cycle_at`・実行中）、「履歴」
  （`history` を新しい順、段の表）。
- POST は `postJSON`。成功は `toast`、失敗は `toast(msg, true)`。`hold_changed`・`no_hold` は確認の段を閉じて読み直す。
  `busy` は「すでに実行中か待ちがあります」。読み直しで `hold` の `token` が `confirming.token` と違う・`hold` が無いときも
  確認の段を閉じる（R6）。
- AI の区画（PR ②）: `/api/admin/status` の `ai_jobs.reasons`（失敗の理由）と `ai_pauses`・`ai_task_blocks` を併読し、
  `retry_kinds` に無い種類は「この版では再検査できない（failed のまま保持）」と添える。解除の横に R7 の案内。
- 状況タブ: PR ① で `massDeleteNotice` の末尾の行を「意図した削除なら運用タブから解除（確かめた件数を超える削除はしない）」に。
  PR ② で `aiOpsCells` の案内を「運用タブから解除（`serve` が動いていないときは `geoframe ai-jobs release-pause --task <用途>`）」に
  し、`admin_ai_ops_render.mjs:25` の期待値を同じ文言に合わせる（検証の構造は変えない）。

## データ

- 保存先は無い（メモリ上の `Board` と既存の `MassDeleteBoard`）。DB のスキーマは変えない。移行なし。
- `ai-retry`・`ai-release-pause` が書く表は既存の `ai_jobs`・`ai_provider_pauses`（CLI と同じ）。

## 安全面

- **入力**: POST の本文は JSON オブジェクトのみ（`post.rs`）。`token` は 16 桁の小文字 hex、`target` は固定の語か既知の種類名。
  それ以外は 400 `bad_params`（入力の値は応答に入れない）。
- **権限**: 認証は無い前提（LAN）。Host 検査・CSRF（JSON の Content-Type・同一 Origin）・64 KiB・`INGEST=0` の 503 は既存の経路を
  そのまま通す。GET は `guard` で HEAD・POST を 403 に。操作はどれも**外部へ送らない**（同期は Drive と地理院へ既存の段が
  アクセスする。`ai-retry` は再検査のみで、送るのは背景ワーカー）。外部公開時に miharashi 用の `/api/v1/photos`・`/derivatives/` を
  Cloudflare Access から外す（bypass）設定をするなら、その範囲を `/api/admin/` に広げない（Opus の指摘 11）。
- **破壊的操作の三重の守り**: 大量削除は印の一致（画面が見た保留と、消える行の集合）＋集合に限った許可（実行時に取り直した
  一覧で集合の外の行があれば保留に戻す）＋消す前の DB の写し。一覧が空の保留（`EmptyListing`）は画面から解除しない。
  確認の段に「消える行は承認した N 件のうち、まだ一覧に無いものだけ」と消えるものの範囲を明記する。
  **印は認可ではない。** 認証の無い LAN では、`GET /api/admin/ops` を読める相手は誰でも `token` を得て POST できる。印が守るのは
  「古い画面（保留が変わる前に見た件数）で解除してしまう」ことだけで、相手の正当性は ADR 0008 の前提（家庭 LAN・外部公開時は
  Cloudflare Access）に任せる。`setflag`・`rotate`・選別の `confirm` と同じ境界だが、`drive-release` は管理 API で最も破壊的な
  書き込みなので、この境界を文書と `admin_ops.rs` のコメントに書いておく。
- **削除で消えるもの**: `write_plan`（`drive/index.rs:421-447`）は `DELETE FROM photos WHERE id=?`（行ごと。手で直した位置
  `loc_source='manual'`・★・除外・回転・タイトル・AI の説明・地名も行と一緒に消える）と、`remove_derivatives`（`index.rs:580-592`）で
  表示用画像を消す。原本（Drive 側）には触れない。**旅行ブログの下書き**（`blog_write.rs`。一次データで作り直せない）は
  `photo_id` で写真を参照しているので、行が消えると書き出しで画像が抜ける。孤児として残る表: `photo_quality`（次の `quality` の段が
  消す。`quality.rs:167`）、`photo_curation`（読む側が世代で突き合わせるので効かない。B の設計）、AI と位置推定の派生の行
  （`photo_ai_features`・`photo_ai_group_members`・`photo_loc_*`・`ai_*`。読む側が `photos` と結合するので効かない）。
  サムネイル（`thumbs/`）は要求時に作る物なので残っても害は無い。確認の段にこの範囲を書く（R6）。
- **情報の漏れ**: 応答に段の失敗の文面・パス・モデル名・接続先を出さない（方針 6）。アクセスログは既存の `labeled` でテンプレートだけ。
- **資源**: 要求は 1 つだけ待てる（溢れは 409）。手動の同期は前回の手動の同期の終了から 60 秒は 409 `too_soon`（Drive と地理院を
  叩き続けない）。AI の操作は同時に 1 つ（`ai_ops_lock`）。GET は DB を読まず `Board` の `Mutex` を短く持つ（スナップショットの
  複製）。3 秒の読み直しはタブ表示中だけ。`quality::scan` は長く走りうるが、既存の `quality` の段と同じ解析で、停止の合図で止まる。
- **停止**: 停止は `stop.wait` の `Condvar` で即座に起きる（今までと同じ）。実行中の段は既存どおり「今の段が終わるまで」。
  停止の合図のあとの要求は 503。

## テスト計画

| AC | 確かめ方 |
|---|---|
| AC1 | `ops_render.mjs`: `ingest:true` の合成応答でボタン 5 つ、`ingest:false` で案内だけ |
| AC2 | (a) `tests/ops_api.rs`: `ingest_cli.rs` と同じ子プロセスの `serve`（`INGEST=1`、鍵なし）。POST sync → 200 と `id`、`GET /api/admin/ops` を最長 20 秒待って履歴のその `id` が終了、`index_drive` が `skipped`、`index_local` が `done`。(b) `sync.rs` の単体試験（`ingest::start` で一時ディレクトリにロックと writer を作る既存の試験と同じ土台）: チャネルで止まる偽の `Stage` を段の一覧にして `spawn_worker`。実行中に同種の `enqueue` が `Busy`、別種は 1 つ待てて 2 つ目は `Busy`、定期サイクル中の要求がその直後に走る、DB のパスを壊して `failed` の記録と `running` の解除、パニックする偽の段でも同じ、待ち・実行中それぞれで `stop.stop()` から `join_within` が短時間で戻る、停止後の `enqueue` が `Stopping`。(c) 描画試験: 送信中・`running` の `disabled`、終了で段の表 |
| AC3 | 子プロセスの `serve` に `GEOFRAME_QUALITY_PER_CYCLE=1` と有効な合成 JPEG 3 枚（`imaging` の試験が使う生成の道具）。起動直後の定期サイクルの `last_cycle` が終わったあと、`photo_quality_features` の世代の合う行が 1 枚ぶんだけであることを確かめてから POST quality-scan。記録の段が `quality` 1 つ、`counts` に `analyzed >= 2`・`remaining = 0`・再構築あり、特徴量の行が 3 枚ぶん |
| AC4 | `drive/index.rs` の単体試験: `plan(existing, files, Allow::Only(ids))` で、`doomed ⊆ ids` なら消える、`doomed ⊂ ids`（一部が戻った）なら残りだけ消え `approved`・`deleted` が出る、集合外が 1 件あれば保留（空の一覧なら `deletes_withheld`、それ以外は `mass_delete_withheld`）。`Yes`・`No` の既存の試験は `Allow` に置き換えて同じ結果。`ops.rs` の単体試験: `token_of` が同じ入力で同じ、4 項目か ID の集合が 1 つ違えば違う。`tests/ops_api.rs`: `spawn_state` で `MassDeleteBoard` に記録を入れ、`token` 取得 → 正しい POST 200（`queued`、`approved`）、違う `token` 409 `hold_changed`、記録を消して 409 `no_hold`、`empty_listing` の記録で 409 `not_releasable`。`sync.rs` の単体試験: 待ちのあいだに `MassDeleteBoard` を書き換えると実行されず `hold_changed`。`tmp/` を書けなくして写しに失敗すると削除せず `failed`。偽の `IndexDrive` 相当の段で `Skipped` → `skipped`。描画試験: 確認の段の件数と範囲、409 で閉じて読み直し、確認中に `token` が変わると閉じる、`empty_listing` で案内だけ |
| AC5 | `tests/ops_api.rs`: `curation_api.rs` と同じ writer 付きの `spawn_state` に `ai_jobs`（`failed`、handler 未登録の種類）と `ai_provider_pauses` の行。`ai-retry all` → `unsupported` の件数、行は `failed` のまま。`ai-release-pause all` → `released`、行が消える。`task` 指定で設定なし → 409 `config_unavailable`。`ai_ops_lock` を試験から握って 2 つ目の POST が 409 `busy`。`ai/scene_judge` の単体試験: 2 つのインスタンスで `prepare_retry` と `reconcile` を交互に呼んでも `recheck` の結果が変わらない |
| AC6 | GET の見出し・`no-store`・HEAD/POST 403・`INGEST=0` で `ingest:false`。`Board` に `Failed("…/data/catalog.db …")` を含む合成の記録と ID つきの保留を入れて、本文に文面も ID も無いことを確かめる。`tests/admin_api.rs` は触らない |
| AC7 | 5 本の POST に対し、`csrf_python.rs`・`curation_api.rs` と同じ形で 403・503・413・400 |
| AC8 | `ops_render.mjs`: `last_cycle`・`next_cycle_at`・`history` 10 件の順序 |
| AC9 | `static_files`・`admin_ai_ops_render` が通る。文書は grep で手で確かめる |
| AC10 | `cargo test`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --all --check` |

## リスクと対策

- **品質解析の一括が長い**: 定期サイクルと他の要求が待つ。画面に「実行中（開始時刻）」を出し、PRD の「決まっていないこと」に残す。
  必要なら「1 回 n 枚まで」を後から足せる（`Request::QualityScan { limit }`）。
- **`admin_ai_ops_render.mjs` の期待値**: 文言を変えると落ちる。期待値だけを合わせ、検証の構造は変えない（R8）。
- **`cli.rs` の `serve` の組み立ての変更**: `handlers` を `clone` して `AppState` に持たせる。`start_ai` が `None`（AI 無効）でも
  `retry` は handlers で再検査できる（CLI と同じ）。`production_handlers` は `!config.ingest` で空（`INGEST=0` は 503 なので届かない）。
- **`release_pause` のあとも送られない**: `ai_provider_waits` と `blocked` が残る。R7 の案内で説明し、`/api/admin/status` の
  `ai_waits` を併読させる。
- **要求の検出の遅れ**: 100 ms 刻みなので最大 100 ms。画面の読み直しは 3 秒なので見えない。`tests/shutdown.rs` は同期ワーカーを
  起動していないので、停止の回帰は AC2(b) の単体試験で確かめる。
- **再試行用の handler を 2 組持つ**: `production_handlers` を 2 回呼ぶ。`DataDir::open` と `Arc` の複製だけなので起動の負担は
  小さい。背景ワーカーが無効（`start_ai` が `None`）でも再試行用の組は作る（CLI と同じく再検査できる）。
- **印の衝突**: `token` は保留の 4 項目と ID の集合から作るので、消える行の集合が 1 件でも違えば別の印になる。開始時刻 `since` は
  保留が続くあいだ変わらない。
- **`Allow` への変更**: `IndexDrive` の既存の試験（`index.rs` の `plan` の試験）を `Allow::Yes`／`No` に置き換える。CLI の `--allow-mass-delete` の
  挙動は `Yes` で同じ。
- **写しの容量と時間**: `VACUUM INTO` は `catalog.db` と同じ程度の空きを `tmp/` に要る。数百 MB 級なら数秒〜数十秒。1 つだけ置き、
  次の解除で消してから作る。失敗（容量不足）なら削除しないので安全側。
- **写真 ID の再利用**（既存の問題）: `photos.id` に `AUTOINCREMENT` が無く、ID の大きい行がまとめて消えると次の取り込みが同じ ID を
  使い、ブログの下書きや世代で照合しない表が別の写真を指しうる。CLI の `--allow-mass-delete` でも同じ。この設計では持ち込まず、
  別の Issue にする（Opus の指摘 8）。
- **`EmptyListing` を画面から解除しない**のは設計の判断（ユーザーの承認待ち）。画面から解除したくなったら、理由ごとの別の警告と
  件数の手入力を足す。

## 実装の順番

1. `ops.rs`（`Request`・`Record`・`Board`・`token_of`、単体試験）。
2. `drive/index.rs` の `Allow`・`MassDeleteHold.ids`・写し、`sync.rs` の `stages(drive, Allow)`・`index_drive_stage_only`、`quality::Scan`、`plan` の試験。
3. `sync.rs` の `spawn_worker` の変更（`Board`・`MassDeleteBoard`、100 ms 刻みの待ち、`run(req)` と隔離、結果の記録）、偽の `Stage` の
   単体試験（AC2(b)、AC4 の `hold_changed`）。`cli.rs` の `serve` で配線。
4. `admin_ops.rs`（GET の JSON、`sync`・`quality-scan`・`drive-release` の POST）、`post.rs`・`server.rs` の登録、`tests/ops_api.rs`（AC2〜AC4、AC6、AC7）。
5. `ops.js`・`admin.html`・`admin.css`・`admin.js`（タブ、`showTab`、`boot`）、状況タブの文言、`ops_render.mjs`・`tests/admin_ui_ops.rs`（AC1、AC4、AC8、AC9）。
6. 文書（同期系）。── ここまで PR ①。
7. `cli.rs` で再試行用の handlers（2 組目）・clock・`ai_ops_lock` を `AppState` へ、`admin_ops.rs` に `ai-retry`・`ai-release-pause`、
   `tests/ops_api.rs`（AC5、AC7 の AI 分）、`scene_judge` の交差の試験。
8. `ops.js` の AI の区画と R7 の案内、状況タブの AI の文言と `admin_ai_ops_render.mjs` の期待値、R9、描画試験、文書（AI 系）。── PR ②。
