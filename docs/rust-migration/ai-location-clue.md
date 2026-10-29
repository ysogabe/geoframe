# 写真別 location_clue（T4.6b）

AC2/4/5/8/10a の写真別 LAN 判定までを実装した。実機・実 LAN・実 Decision・実写真は未確認で、P4 完了や本番切替を意味しない。このclue handlerはLAN専用。日候補・クラウド・座標化の本番登録は[T4.6c2](ai-location-suggest.md)で追加した。Embedding (#93)・グループは対象外。試験は合成画像／DBと loopback Discovery / Decision のみ。

## 本番入口と後続 API

`worker::production_handlers(config, shared_lan)` は INGEST と AI の有効な `location_clue` 設定がある場合だけ `LocationClue` を1つ登録する。呼び出し側で `LanClient::new` を一度作り、同じ Provider の利用者と共有する。serve の `start_ai` と `ai-jobs retry` の双方が同じ factory を使う。fake の配布設定や任意の通信先 override はない。従来の writer / StopFlag / LockHold / 共通停止期限を維持する。

起動と毎周期に現在の写真を時刻/id順で reconcile する。取り込みイベントに依存せず、後から追加された写真・Python 運用中の追加・後から有効にした用途も拾う。対象は `lat IS NULL` かつ `photos::parse_epoch`（既存 t_epoch 互換入口）で解釈可能な taken_at。hidden は現在値を読むが、Python と同じく LAN の列挙・結果世代を変えない。favorite / hidden / rotation / GPS その他の photos 一次列には書かない。

T4.6c は `location_clue::load_targets(read, data, model)` で現在の対象と `Readiness` を取得する。`Target` は photo_id / taken_at / epoch / JobKey / readiness を持ち、写真情報の Debug は実装しない。

- `Pending`: 未判定・品質待ち・一時利用不可・pause・途中失敗・非適合結果。その日を待たせる。
- `Ready(Clue)`: 有限 0..1 の score と既存6種の kind。`Clue::{score,kind,passes}` を使う。
- `Failed`: 現世代の terminal failed と適合する互換 error 行。判定済み・手がかりなしとして進み、画像を送らない。

`passes` は Python と同じく `score >= 0.2 || kind == "landmark"`。正常型の構築は `Clue::new` で検証する。質問 state / 文面 / 選択肢は Python `loc_suggest.py` と同じで、AST から抽出した fixture と照合する（再生成: `python3 tools/gen_location_clue_fixture.py`、アプリの import・通信なし）。Unanswerable、違う model、形・範囲・kind の不正は `bad_response` の試行。日候補側は `Ready` だけを候補にし、クラウドの `hidden` / `include_hidden` / `allow_cloud` を**独立して**再検査する。この API は日候補送信・位置候補保存・座標化をしない。

## 世代・結果版

JobKey は `location_clue` / 写真ID / 入力ハッシュ / 処理版。入力ハッシュは型タグ・長さ付きで、SQLite NULL / 空文字 / 数値型を区別する。材料は photo_id、DB display、読む相対パス、md5、mtime、rotation、taken_at、現世代に結合した `photo_quality_features.content_sha256` と品質解析失敗の有無。品質 worker の対象外となる display 空の fallback では、実際に観測した元 bytes の SHA も含める。品質の結合条件は既存と同じ display / source_md5 / source_mtime の NULL を含む一致、成功の SHA だけを使う。

処理版は model + `loc-clue-python-v1` + `display-exif-once-srgb-1536-1600000-rgb-jpeg-q88-v1` のハッシュ。質問／前処理を変更するときは対応する版を上げる。URL・timeout・設定全体のハッシュ・別用途の許可・LAN の結果に関係しない hidden は含めない。設定・Provider pause・停止は既存の送信 gate で別に検査する。

新世代を enqueue すると旧非成功キーを obsolete にする。削除・SAVE・無効時刻など対象外になったキーも、読み取りで計画し、短い writer 操作内で再検査して失効する。succeeded と terminal 履歴は消さず、A→B→A でも上限を解除しない。成功済みの A へ戻った場合は保持した A の結果を再利用できる。

display が空なら、検証した drive_id の `derivatives/<id>.jpg` を既存 fallback として選ぶ（Drive 取得なし）。存在しなくてもこの候補相対パスを世代に含め、存在すれば実 SHA も含める。Python `_image_rel` との互換のため、fallback が無い場合の error 行の display だけは空文字にする。fallback が後からできた／実 bytes が変わった場合は SHA の変わった新世代になる。同じ SHA への往復は terminal 履歴を保持する。display が非空の場合は DATA_DIR 内の直接表示原本も扱う。

## 追加表と互換投影

`ai_location_clues`（WITHOUT ROWID、主キー photo_id / input_hash / processor_version）を追加する。列は次のとおり。

| 列 | 用途 |
| --- | --- |
| photo_id, input_hash, processor_version | Rust が検証した入力・結果版。世代ごとの履歴を残す |
| source_sha256 | 実際にデコードした元 bytes の SHA（成功時）。fallback の失敗では世代を検査するため観測済み SHA があれば保持 |
| clue, kind | 検証した正常結果。失敗は NULL / 空文字 |
| model, analyzed_at, error | 現 model、UTC ISO 8601 TEXT（秒、Z）、固定エラーコード（成功は空文字） |

`photo_loc_clues` の既存列・意味は変更せず、display / source_md5 / source_mtime / clue / kind / model / analyzed_at / error へ投影する。生の model 応答・例外・質問・パスを error に保存しない。旧行には rotation / 質問版 / SHA の由来がないため、model が同じでも Rust へ自動 trusted 移行しない。LAN で再判定する。

正常結果は `jobs::complete` の token 検査と同じトランザクションで、現在の DB 入力・処理版・latNULL・対象時刻を照合し、追加表・互換行・succeeded を同時確定する。実 bytes の SHA は準備時、Discovery / health 待ちの後の POST 直前、応答後に照合する。品質 SHA がある場合は読んだ bytes と一致しなければ送らず obsolete。画像の準備や HTTP 中に writer transaction は持たない。

再起動後の再利用は追加表の正常型・model・世代・処理版・source SHA と互換行の**結果全体**が適合するときだけ。Python が互換行を書き換えた場合、その値を Rust 結果とはみなさず Pending にする。次周期で現在の由来に適合する保存済み Rust 結果を再投影し、追加表に残した検証済み値を再利用する（succeeded をリセットしたり再送したりしない）。品質 SHA の無い fallback の成功は実 bytes の SHA を毎回確認する。普通の display の品質未解析では、欠けた SHA から古い結果を trusted にしない。手動で追加表そのものを消す／壊す運用は想定せず、由来を検証できない結果は Pending のまま保持する。

新表は `ai::{pending,migrate}` 系だけに置き、REQUIRED_SCHEMA や Python 再現スキーマには加えない。本番のガード・ロック取得後の `ai::migrate_active` は有効な location_clue がある場合だけ新表を作る。未指定・不正・無効設定ではこの新表を作らない（既存 T4.1 の基盤移行は従来どおり）。INGEST=0 は移行・送信とも行わず、追加表の無い旧 DB を読み続けられる。dry-run は有効な location_clue の場合だけ `ai_location_clues` を不足一覧へ足す。dry-run 自体は書かない。

## 品質待ち・失敗・再試行

| 状態 | 扱い |
| --- | --- |
| 読める display、現世代の品質未解析／SHA 未取得 | `deferred/input_pending`、attempts 0、その写真だけ待つ。他写真は進める |
| display 空で既存 fallback あり | 品質 worker の列挙対象外なので、自分で読み・デコード・SHA 検証して判定する |
| 画像なし・壊れた画像・危険なパス・通常ファイル以外・現世代の品質解析失敗 | `Permanent(input_invalid)`。巨大入力は `input_too_large`。画像を送らずその写真だけ terminal failed |
| Discovery unavailable / lease / 429 / pause / STOP | 既存分類で後回し。互換 error 行を作らない |
| 不正応答 | ai_jobs.attempts を1回加算。5回目の terminal failed だけ error 行へ |
| 予定外の中断 | ai_jobs.interruption_count が唯一の正。起動回収の3回目で terminal failed / interrupted |

失敗を二重計数しない。`jobs::record_failure_with` が token の合う running だけを遷移し、terminal failed のときだけ `Handler::projection` を同じ tx / savepoint で呼ぶ。現世代が変わっていれば投影せず obsolete（terminal 履歴保持）。SQL 失敗は失敗遷移・投影を共に rollback する。固定 ErrorCode 以外の既存理由は `other` にする。起動回収で failed になったもの・未投影は、次周期の reconcile 後に最大200件ずつの短い writer 操作で冪等に修復する。回収時に用途固有の巨大処理はしない。

`ai-jobs retry` はサービス停止後の既存 guard / ingest lock / writer を維持し、通信せず現入力・処理版・適合結果を検査する。fallback の retry は `prepare_retry` で writer 外にて実 SHA を観測し、writer 内ではその値と読み直した DB 世代を照合する。未投影の中断上限にも対応し、次周期の reconcile と送信直前にも実ファイルを再検査する。Requeue なら同じ世代の failed メタデータと対応する互換 failed 投影を同じ tx で無効にし、未判定へ戻す。succeeded・別世代・手動 GPS は消さない。再成功は正常 clue へ置き換える。実際の Python `_load_targets` / `passes` でも正常／failed 行の再利用と retry 後の未判定を試験する。

## STATUSの`clue_pending`・`clue_runnable`・`clue_pause`

`GET /api/admin/status` の `db.loc` は、**Rustが位置の手がかりを判定するとき**、次の3つを使う。管理画面の「手がかり判定待ち」は `clue_pending`、毎朝のDecisionのtimer `deploy/loc_decision_morning.py` は `clue_runnable`（無ければ `clue_pending`）を読む。

- `clue_pending`（総待ち）: 位置なしで撮影時刻が読める写真のうち、Rustの保存結果（`ai_location_clues`）が**今の入力の世代**（上の「世代・結果版」の入力のハッシュとDecisionのモデルの処理版）に合っていないものの数。ジョブが未登録・待ち・後回し・再試行待ちの写真と、品質の解析待ちの写真を含む。`photo_loc_clues`に旧Pythonの判定済みの行があっても数える（Rustは旧Pythonの行を信用しない）。保存済みの写真と、恒久失敗で`error`の行を保存した写真（`Failed`。判定済みで、再送もしない）は数えない。保存が済むと`photo_loc_clues`にも投影されるので、全部保存されれば0。
- `clue_runnable`（timer用）: `clue_pending` のうち、**今Decisionが動けば進む**写真の数。次は数えない（Decisionを起動しても進まないため）: ①用途がProviderまたは用途の停止（`ai_provider_pauses`）にかかっているとき（全体を0にする。送信側・`ai-jobs status`と同じ `ops::task_pause`。停止の表を読めないときも0）、②品質の解析待ち・品質の解析に失敗した写真（画像を送らず後回し・恒久失敗になる）、③`ai_jobs`の `next_attempt_at` が先の後回し・再試行待ちだけの写真、④失敗の履歴（`failed`）だけで明示の再試行（`ai-jobs retry`）まで動かない写真。ジョブがまだ無い・期限が来ている・実行中の写真は数える。共有の一時待機（`ai_provider_waits`。Decisionが落ちているときの未到達を含む）と日・周期の上限は**見ない**（Decisionの未起動こそtimerが起こす理由で、待機は短いため）。
- `clue_pause`: `clue_runnable` を0にした停止の理由の固定コード（`unauthorized`・`forbidden`・`model_not_found`・`contract_mismatch`、読めないときは `unknown`）。停止がなければ空文字。URL・モデル名・hashは出さない。停止は値に入れてキャッシュせず、毎回読む（`release-pause` がすぐ見える）。
- 「Rustが判定するとき」: **設定上**、`GEOFRAME_INGEST=1`で`ai.gate(location_clue)`が通る＝AIが有効・Decisionの用途が設定済み・位置候補が有効（ワーカーのハンドラ登録の設定上の条件。LANクライアントの生成とデータ置き場を開けることまでは、STATUSは確かめない）。そうでないとき（AI未設定・設定の誤り・無効・用途なし・取り込み無効）は、Rustが減らすことのない値を出してtimerに待たせないよう、`clue_pending` は**従来の値**（旧Pythonの`photo_loc_clues`の世代一致で数えた未判定の数）のままにし、`clue_runnable`・`clue_pause` は**出さない**（timerは `clue_pending` を使う）。
- 他のキー（`targets`・`clue_passed`・`days`・`days_with_clue`など）は旧Pythonの行から数える従来の値のまま。そのため、Rustが判定するときは `targets − clue_pending` は「旧Pythonの行で判定済み」の数とは一致しない。
- 計算（`location_clue::wait_counts`）: **画像ファイルは開かない**（`lock_derivatives`も取らない）。1枚につき主キーでの数回の問い合わせで、画像の中身（BLOB）は読まない。`display`の無いfallbackの写真は、世代にファイルの実SHAが入っていて読まずには組み立てられないので、保存された最新の行（`analyzed_at`が最新）の `source_sha256`（無ければ観測なし）で今のDBの入力から世代を組み直し、その行の `input_hash` と一致して判定が合っていれば判定済みとする。ファイルだけが変わった場合は、ワーカーが次の周期で観測して新しい世代のジョブを積むまでSTATUSは気づかない（現世代と違うジョブが `pending`／`running`／`deferred` で生きていれば待ちに戻す）。結果は**30秒キャッシュ**（Decisionのモデルごと。モデルが変われば使わない）し、再計算は同時に1つだけ（計算中の別の要求は終わりを待って値を使う）。千件の位置なし写真で約20〜30ms（release）。

## 画像境界と停止

`imaging::lock_derivatives` の中で、DataDir の読み取り用検査を通して通常ファイルを上限付きで読み切る。DB の traversal・絶対パス・リンク（親／末尾、内部向きも拒否）・FIFO・ディレクトリを拒否する。open は O_NOFOLLOW / O_NONBLOCK、fd の実体も `/proc/self/fd` から DATA_DIR 内と確認してから読む。既存 DataDir や他 reader は変更しない。削除用 derivative_file を読み取りの根拠にしない。

ロックを解放した後、既存 imaging のデコード／寸法・破損検査／ICC→sRGB／縮小部品を `prepare_decision_rgb` で再利用する。derivative は現表示向き、photos.rotation は再適用しない。直接表示原本だけ EXIF 向きを一度適用する。灰色は RGB 化し、縮小のみ・比率維持・長辺1536以下・面積1,600,000以下・RGB長さ正確、`PreparedImage::encode_jpeg_rgb(...,88,Decision)` で新規 JPEG にする。元 bytes / EXIF / ICC / GPS / filename を要求に混ぜず、入力ファイルには書かない。既存 imaging と同じく壊れた／非RGB用 ICC は変換せず落とす。

`decision::run_checked` は既存 transport / probe / 分類を共有し、Discovery と health の後に callback で DB 世代・現在の hidden・実読取元 SHA を再確認してから POST する。STOP は既存 transport gate が初 HTTP write 前に再確認する。送信済みで token / 世代が有効な応答は停止中でも確定可能。LAN 不稼働時の自動 cloud fallback はなく、Vertexclient は呼ばない。

ログ・API・CLI の追加情報は固定理由と件数だけ。写真 ID / hash / 入力パス / model 応答 / 質問 / GPS / location はログに足さない。worker の SQL 失敗も SQLite の動的メッセージを出さない。

## この実装の検証

`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（1056 pass / 0 fail / 2 ignore）、`cargo build --workspace --release --target x86_64-unknown-linux-musl` を通過した。既存1029 passに対して27試験を追加。loopback bind は sandbox 内では拒否されたため、試験だけ権限付きで再実行した。実機受入・別Solによる簡易レビューは親タスクへ引き継ぐ。
