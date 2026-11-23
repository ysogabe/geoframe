# geoframe-export: Drive API 直接方式

geoframe-export の Drive 操作を rclone から Drive API の直接呼び出しへ置き換える設計。3 段の PR の 2 段目（PR②）で、前半（この文書・偽の Drive HTTP サーバー・ID で指す Drive 操作の型。バイナリの振る舞いは変えない）と、後半（フローへのつなぎ込み・CLI 引数・unit・包み・`import-credentials`・受け入れ試験の移し替え）に分けて実装する。実装で設計と違えた判断は、この文書を直して残す（末尾の「実装で決めたこと」）。design.md 4.2 からここへリンクする。

## 0. 前提と方針
- 3 段の PR の 2 段目。1 段目（PR #87）で rclone 由来の型を `src/remote.rs` に中立化し、flow・publish・recover・drive は具象の `remote::Remote` を持ち回るようになった。
- この段で `Remote` の中身を Drive API（HTTPS、`ureq`）に差し替え、バイナリの Drive 操作は API だけにする。**rclone 版との実行時の差し替え（trait・設定での切り替え）は作らない。** 戻す手段は前のバイナリ（.prev）と、関門を通るまで残す rclone 本体・rclone.conf。
- `src/rclone.rs`・`src/rclone_conf.rs`・`fake-rclone/main.rs`・rclone 固有の試験（`tests/rclone_runner.rs`・`tests/rclone_config.rs`、`// rclone 固有` の行）は、**この段では消さない**（lib の pub モジュールとして残り、試験も通り続ける）。消すのは 3 段目。ただし、バイナリ（cli・serve・run）は rclone を一切呼ばない。`rclone_conf.rs` の読み取りは 6 章の取り込みで使い続ける。
- **ジョブと公開の記録の形式は変えない**（読めない記録は blocked）。`tests/record_format.rs` の実例は、この段でも 1 バイトも変えずに通ること。新しい Stage・フィールドは足さない。エラーコードは既存のものだけ（`drive_auth_invalid` は 1 段目で読めるようにしてある）。
- エラーコード `rclone_failed`・`rclone_timeout` の**綴りは変えない**（記録・HTTP 応答・CLI の標準出力で呼び出し側との契約）。意味は「Drive の変更系の操作の失敗」「Drive の操作が期限内に終わらない」に広げ、メッセージの文言だけ中立にしてよい（ログ・応答の message。記録には code だけが入るので記録の形式は変わらない。変えたら試験・文書もそろえる）。
- 依存: `ureq` の feature に `rustls` を足す（ワークスペースの geoframe crate が既に `["rustls", "gzip"]` で使っている。Cargo.lock に新しいパッケージが増えないことを確かめる）。それ以外の crate は足さない（JSON は serde_json、MD5 は md-5、乱数は既存の手段）。musl の静的ビルド（CI の `cargo build --release --target x86_64-unknown-linux-musl -p geoframe-export --bin geoframe-export`）が通ること。

## 1. 設定（起動引数）の契約
呼び出し側（unit・包み `deploy/geoframe-export-admin.sh`・整合試験 `deploy/geoframe-export-admin-test.sh`）と一貫させる。

消す引数: `--rclone`・`--rclone-config`・`--remote-yamareco`・`--remote-travel-journal`・`--work-root`。
改名: `--rclone-copy-timeout` → `--drive-upload-timeout`、`--rclone-timeout` → `--drive-timeout`（既定値は今と同じ。45 分と 5 分）。
足す引数:
- `--drive-credentials <path>`: 資格情報の JSON（2 章）。必須。
- `--folder-yamareco <ID>`・`--folder-travel-journal <ID>`: 宛先の親フォルダの Drive ID。必須（パスの既定値は持てないため）。
- `--work-folder <ID>`: 作業用のフォルダ（`incoming/`・`replaced/` を中に作る）の Drive ID。必須。
- ID は `is_valid_drive_id` で検査。3 つが同じなら起動時に断る（終了コード 2）。
- 試験専用: `--drive-endpoint <http://127.0.0.1:PORT>`（偽の Drive HTTP サーバーへ向ける。API・アップロード・トークンの 3 つの URL をこの下に置く）。**`http://127.0.0.1:<port>` と `http://[::1]:<port>` だけ受け付け**、それ以外は終了コード 2。unit・包みは渡さない（整合試験で、どこにも出ないことを確かめる）。help には出さない。未指定のときは本物の固定値（`https://www.googleapis.com/drive/v3`、`https://www.googleapis.com/upload/drive/v3`、`https://oauth2.googleapis.com/token`）。

unit（`deploy/geoframe-export.service`）:
- `LoadCredential=drive-credentials:/etc/geoframe-export/drive-credentials.json`、`--drive-credentials %d/drive-credentials`（`%d` は資格情報のディレクトリ）。
- フォルダ ID は `/etc/default/geoframe-export`（0600 root。値は文書に書かない）の `GEOFRAME_EXPORT_FOLDER_YAMARECO`・`GEOFRAME_EXPORT_FOLDER_TRAVEL_JOURNAL`・`GEOFRAME_EXPORT_WORK_FOLDER` を `${VAR}` で渡す（空なら引数の検査で終了コード 2）。
- rclone の HOME・キャッシュの説明、`--rclone*` は消す。ハードニングは今のまま（`RestrictAddressFamilies=AF_INET AF_INET6`・`MemoryDenyWriteExecute=true` は rustls・ring でも足りるはず。関門で確かめる）。MemoryMax・TasksMax は関門で測り直すので、この段では変えない。
包み（`deploy/geoframe-export-admin.sh`）:
- `PROPS` に同じ `LoadCredential=` を足し、`--drive-credentials` を同じ形で渡す。資格情報のファイルは本番と検証で同じものを読む（API 版はファイルを書き換えないので、検証用の写しは要らない。rclone.conf の写しの手順は無くす）。
- フォルダ ID は `/etc/default/geoframe-export` から読む。検証用（`--verify`）は同じファイルの `GEOFRAME_EXPORT_VERIFY_FOLDER_YAMARECO`・`GEOFRAME_EXPORT_VERIFY_FOLDER_TRAVEL_JOURNAL`・`GEOFRAME_EXPORT_VERIFY_WORK_FOLDER`。読み方は、シェルで source せず、決まったキーの行だけを厳しく取り出す（値は Drive ID の文字だけ）。`--folder-*`・`--work-folder`・`--drive-endpoint` は利用者が指定できない（今の `--remote-*` と同じく断る）。
- 整合試験は、unit と包みで同じ引数・同じ `LoadCredential` を検査する形にそろえる。

## 2. 資格情報
- ファイルは `{"client_id": "...", "client_secret": "...", "refresh_token": "..."}` の 3 つのキーだけの JSON。ほかのキー・重複キー（既存の重複キー検査の仕組みを使う）・空の値は断る。大きさの上限 16KiB。読み方は O_NOFOLLOW、通常のファイル、グループ・他人の権限ビットが無いこと（LoadCredential の写しは 0400 で所有者はサービスのユーザー。root 所有の元ファイルを直接渡す使い方も許すため、所有者は euid か root）。
- 値はログ・エラー・Debug 表示に出さない（`Debug` を手で実装して伏せる）。
- アクセストークンはメモリだけに持つ。**資格情報のファイルは決して書かない**（関門: トークン更新後もファイルの更新時刻が変わらない）。
- トークンの取得: `POST <token_url>` に `grant_type=refresh_token&client_id=…&client_secret=…&refresh_token=…`（application/x-www-form-urlencoded、値は URL エンコード）。応答の `access_token`・`expires_in`（上限 3600、期限の 60 秒前に取り直す）。
- `400` で本文の `error` が `invalid_grant` → `drive_auth_invalid`（専用のエラーコード）。ほかの 4xx（`invalid_client` など）も再試行せず認可の失敗として扱う（`drive_auth_invalid` に寄せるか、`drive_unavailable` にするかは実装で決めて文書に書く。invalid_grant は必ず `drive_auth_invalid`）。429・5xx・通信の失敗は再試行。
- **起動時に 1 回確かめる**（serve・run・recover。`--dry-run` も）: トークンを 1 回取り、3 章の宛先の検査も行う。invalid_grant なら標準エラーに「再認可が要る（手順は deploy/README.md）」と出して終了コード 2。通信の失敗・5xx は終了コード 1（serve は systemd が再起動する）。
- 取り込み: サブコマンド `geoframe-export import-credentials --rclone-config <path> --remote <name> --out <path>`。今の rclone.conf（`rclone_conf.rs` で読む）から `client_id`・`client_secret`・`token` の `refresh_token` を取り出し、`--out` に 0600・O_CREAT|O_EXCL|O_NOFOLLOW で書く（既にあれば断る）。値は標準出力・標準エラーに出さない（出すのは書いたパスと「書いた」ことだけ）。root が sudo で 1 回だけ実行する前提（同意のやり直しはしない）。再認可のとき（別の機体で `rclone authorize` して得た設定を取り込む）にも使う。手順の文書は 3 段目で deploy/README.md に書く。

## 3. 起動時の宛先の検査
`files.get`（`fields=id,name,mimeType,trashed,parents`）で 3 つの ID を読み、次を確かめる。どれかが違えば終了コード 2（設定の誤り）。
- フォルダである（`mimeType == application/vnd.google-apps.folder`）。ショートカットは断る。
- ゴミ箱に入っていない（`trashed == false`）。
- 3 つが別物で、どれも他のどれかの祖先でない（それぞれ `parents` を根までたどる。深さの上限 64、たどる途中の get の失敗は `drive_unavailable`＝終了コード 1）。
- ジョブを始めるたびにも、その宛先の親フォルダと作業用のフォルダについて「フォルダ・ゴミ箱でない」を get で確かめる（安い。起動後に誰かがゴミ箱へ移した場合に備える）。失敗は `drive_unavailable`。

## 4. ID で指す Drive 操作の型（`src/drive/api.rs` など。drive.rs をディレクトリにしてよい）
Drive v3。HTTP クライアントは ureq（rustls）。**リダイレクトはたどらない**（max_redirects 0）。要求ごとに期限（その操作の期限とジョブの残り時間の短いほう）を掛ける。`fields` で必要な項目だけ取る。

| 操作 | API | 備考 |
|---|---|---|
| 子の一覧 `list_children(parent)` | `GET files?q='<parent>' in parents and trashed=false&fields=nextPageToken,files(id,name,mimeType,size,md5Checksum)&pageSize=1000&spaces=drive` | **名前で q を絞らない**。全ページを取り、手元で名前の完全一致で照合する。`parent` は検査済みの ID なので q に入れてよいが、念のため `'` と `\` を拒む。件数の上限（今の `LSJSON_MAX_ENTRIES`=20000 と同じ）・ページ数の上限。`nextPageToken` が同じ値で返り続けたら失敗 |
| 再帰の一覧 `list_tree(root)` | 子の一覧を幅優先で | 相対パスを組み立てる（今の `Entry.path` と同じ形。区切りは `/`、`rel_path_ok` で検査）。深さ・件数の上限 |
| ID での取得 `get(id)` | `GET files/<id>?fields=id,name,mimeType,size,md5Checksum,trashed,parents` | |
| フォルダ作成 `create_folder(parent, name)` | 先に `GET files/generateIds?count=N&space=drive&type=files` で ID を取り、`POST files`（本文 `{"id":<ID>,"name":…,"mimeType":folder,"parents":[parent]}`） | 409 は「その ID で既に作られた」。`get(id)` で親・名前・種類が期待どおりなら成功、違えば失敗 |
| アップロード `upload(parent, name, local, expected_md5, size)` | 8MiB 以下は `POST upload/files?uploadType=multipart`、超えるものは `uploadType=resumable`。どちらも事前に取った ID を本文に入れる | 409 は「済み」→ `get(id)` で親・名前・大きさ・`md5Checksum` を照合し、一致すれば成功、違えば失敗。**既存ファイルの中身を更新する呼び出し（PATCH の media）は作らない** |
| ID での移動 `move_to(id, from_parent, to_parent, new_name)` | `PATCH files/<id>?addParents=<to>&removeParents=<from>`（本文 `{"name": new_name}`） | 1 回の PATCH で親と名前を変える。ID は保たれる |
| ID でのゴミ箱行き `trash(id)` | `PATCH files/<id>`（本文 `{"trashed": true}`） | 完全な削除（`DELETE files`・`emptyTrash`）は作らない |
| 中身の読み出し `read(id, max)` | `GET files/<id>?alt=media` | 上限を超えたら失敗（今の `cat` の上限と同じ扱い） |

Entry への写し方（今の `remote::Entry` を使う。形は 1 段目のまま）:
- `is_dir`: `mimeType == application/vnd.google-apps.folder`。
- ショートカット: `mimeType == application/vnd.google-apps.shortcut`。API ではショートカットは解決されずそのまま 1 件で出るので、rclone 版の「2 回一覧して差を見る」（`list_checked`・`classify` の 2 つ目の一覧）は要らなくなる。`has_shortcut` は一覧にショートカットが 1 つでもあるか。
- 大きさ: `size` が無いもの（Google 文書・フォームなど）は今の `Size: -1` と同じ扱い（大きさ不明）。`md5Checksum` が無いものは MD5 無し。どちらも「一式に無いファイル」として断られる（今と同じ）。
- `id` は Drive の ID（複合 ID は出ない）。
- 子の一覧は `trashed=false` で絞る（ゴミ箱の中のものは見えない＝無いものとして扱う）。

drive.rs（`Drive`）の書き換え:
- `Layout` は宛先の親フォルダ ID と作業用フォルダ ID を持つ。宛先・`incoming/<job_id>`・`replaced/<…>` は**名前で親の子を照合して ID を得る**（今の「親の一覧が成功したうえで名前に一致するものが無いときだけ無い」の約束はそのまま）。
- `incoming/`・`replaced/` が無ければ作業用フォルダの直下に作る（今の `rclone mkdir` が親ごと作っていたのと同じ結果。作るのは generateIds つきの create_folder）。
- 公開の移動（`publish_move`）: `incoming/<job_id>` の ID を、宛先の親へ、名前をフォルダ名にして移す。**移す直前に宛先に同名が無いことを確かめる**（今と同じ）。API の移動はファイル単位の移動に化けないので、rclone 版のコメントにある fallback の注意は消す。移動のあとの照合（`check_published`）は今のまま。
- 退避（`retire_old`）: 宛先のフォルダの ID を、作業用の `replaced/` の ID の下へ、名前を `<種類>-<フォルダ名>-<job_id>` にして移す。
- ゴミ箱行き（`remove_incoming`・`trash_replaced`）: 今の事前の照合（ID が記録と一致する 1 つだけ、中身が旧 manifest と一致、直前にもう一度確かめる）をそのまま行い、**照合した ID** をゴミ箱へ移す。パスで指す制約（「乱数の名前だけを purge する」）は無くなるが、照合は弱めない。
- アップロード（`upload`）: 作業領域の写真を、ファイルごとに `upload` する（並列は 4。`std::thread::scope` で、**全スレッドを join してから戻る**）。全部の写真が成功してから最後に manifest を送る（今の順序の約束）。1 つでも失敗したら、残りを始めずに、走っているものを待ってから失敗を返す。
- `dry_run` は読むだけ（list・get・read だけ）。

## 5. 再試行・期限・キャンセル
- 写真サービスの `crates/geoframe/src/drive.rs` の `send_with_retry`・`authorized` を写して、書き込み向けに直す（共通 crate は作らない）。
  - 再試行するもの: 通信の失敗（接続・TLS・読み取りの途中の切断）・429・5xx・403 で理由が `rateLimitExceeded`/`userRateLimitExceeded`。`Retry-After`（秒）を守る。指数の待ち＋揺らぎ。回数の上限。待ちは期限とキャンセルで打ち切る。
  - 401 はトークンを取り直して 1 回だけやり直す。
  - **書き込みの再試行は generateIds の前提を守る**: create_folder・upload の再送は、必ず最初に取った同じ ID で送る（応答を失った再送は 409 になり、重複しない）。ID を取り直して送り直さない。move_to・trash の再送の前（または 4xx で断られたとき）は `get(id)` で今の状態を見て、既に目的の状態（親・名前・trashed）なら成功とする。
  - resumable のアップロードは、セッションの `Location` を保存し、**`Location` のスキーム・ホスト・ポートが設定のアップロードの URL と同じであること**を確かめる（違えば送らずに失敗）。途中で切れたら、`Content-Range: bytes */<size>` で受け取り済みの位置を問い合わせて続きから送る（308 と `Range` 見出し）。セッションが 404/410 で失われたら、**同じ ID** で新しいセッションを始める（作られていれば 409 → 済みの照合）。チャンクは 8MiB の倍数で、チャンクの間にキャンセルと期限を見る。
- 期限: 一覧などの照会と各書き込みには `--drive-timeout`、写真の送信全体には `--drive-upload-timeout`、そしてジョブ全体の残り時間。要求ごとの ureq の timeout に、その短いほうを掛ける。期限切れは `rclone_timeout`（照会なら今の対応どおり）。
- キャンセル（`Cancel`）: 要求の前・再試行の待ち・チャンクの間で見る。キャンセルされたら、アップロードのスレッドを全部 join してから `Cancelled` を返し、それから実行枠を手放す（今の「子を止めて wait してから」の約束の置き換え）。変更系の操作の途中で切れた場合は、今と同じく「結果不明」として復旧の表（recovery.rs）に渡す。
- エラーの対応: 照会の失敗 → `drive_unavailable`、変更系の失敗 → `rclone_failed`、期限 → `rclone_timeout`、invalid_grant → `drive_auth_invalid`（`RemoteError` に変種を足し、`into_query_error`・`into_op_error` で対応づける）。

## 6. ログと秘密
- ログに出してよいのは、操作名・状態コード・再試行の回数・Drive のエラーの `reason`（短い固定の語）だけ。URL のクエリ（`pageToken`）、アクセストークン、資格情報、応答の本文、ファイル名（実データ）は出さない。
- `Authorization` 見出しは TLS の上だけ（試験の `--drive-endpoint` の loopback を除く）。

## 7. 偽の Drive HTTP サーバー（試験）
- `fake-rclone/model.rs` の模型（ノードの表）を使い回す。模型のファイルは置き場所を `tests/common/` か `fake-drive/` に移してよい（fake-rclone からも引き続き使えるように）。
- 試験のプロセスの中で起動する HTTP サーバー（std の TcpListener かワークスペースの既存の crate。新しい crate は足さない）。CLI を子プロセスで動かす試験には `--drive-endpoint` でその URL を渡す。トークンの入口も同じサーバーが受ける。
- calls.log には 1 段目と同じ中立の操作名（list・read・create_folder・upload・move・trash。get・generate_ids・token も足してよい）を書き、`Env::ops`・`mutating_ops`・`assert_no_mutations` がそのまま使えるようにする。`add_rule` の当て方（操作名＋何回目か＋効果）も同じ形にする。`contains`（今は rclone の引数の部分一致）は、対象のノードのパス（模型から引ける）で当てる。
- 故障注入（受け入れの試験に入れる）:
  - 応答を失った create（サーバーは作ってから接続を切る／500 を返す）→ 再送が 409 → 済みとして MD5 を照合して成功。重複したファイルができないこと。
  - 流量制限の 403（`rateLimitExceeded`）、429 と `Retry-After` → 待って成功。回数の上限を超えたら失敗。
  - 401 → トークンを取り直して成功。
  - invalid_grant → ジョブは `drive_auth_invalid`。起動時なら終了コード 2。
  - ページ送り（pageSize を小さくして nextPageToken を何ページも返す）→ 全部取れる。同じトークンが続くと失敗。
  - 途中で切れた resumable → 続きから送って成功。セッションが失われたら同じ ID で始め直す。
  - `Location` が別のホストを指す → 送らずに失敗。リダイレクト（3xx）をたどらない。
  - ゴミ箱の中のフォルダ・ファイルは一覧に出ない。
- 1 段目で中立にした受け入れの試験（drive_publish.rs・drive_recovery.rs・job_flow.rs・serve_*・run_cli.rs・signal_cli.rs など）を、偽の Drive HTTP サーバーで通し直す。rclone の呼び出しの形そのものを見る行（`// rclone 固有`）は、API の呼び出しの形（例: 一覧が名前で q を絞っていない、変更系に完全な削除が無い、中身の更新が無い、Authorization が付いている）を見る行に置き換える。
- rclone 版の試験（rclone_runner.rs・rclone_config.rs など）は rclone のモジュールを直接試すものとして残し、通し続ける（3 段目で消す）。

## 8. 本番の関門（PR③ のあと。参考）
§8.8 の --verify の入口で: 移動で ID が保たれる、同名のフォーム・ショートカットが ambiguous_destination、md5Checksum が作業領域の MD5 と一致、Google 文書入り・同じ大きさで中身を変えたフォルダが modified で断られる、rclone 版が公開したフォルダを API 版で replace できる、ゴミ箱行きがフォルダと中身ごと戻せる、トークン更新後も資格情報のファイルの更新時刻が変わらない、所要時間、unit の上限の測り直し、.prev へ戻す練習。
**8MiB を超えるファイル（resumable）を 1 つ、本物の Drive へ送る**（本番の写真は 1 枚 1MB 前後で、通常の関門では resumable の経路が通らない。チャンクの `PUT` に `Authorization` を付けない 9.5 の判断も、ここで本物で確かめる。`Content-Range`・308・`Range` の形、同じ ID での始め直しも）。

## 9. 実装で決めたこと（前半: 型と偽の Drive の HTTP サーバー）

設計メモの書きぶりを具体にしたもの、設計と違えたもの、設計に無かった判断。コードは `crates/geoframe-export/src/drive/`（`mod.rs`＝今までの `drive.rs`、`http.rs`・`auth.rs`・`api.rs`）、偽のサーバーは `tests/common/fake_drive.rs`。

### 9.1 型の置き場所と公開している入口（後半が使う）
- `drive::http`: `Endpoints`（`production()`・`loopback(&str)`。`--drive-endpoint` の検査はこの `loopback`。`http://127.0.0.1:<port>` と `http://[::1]:<port>` の完全一致だけ）、`Deadline`、`ApiError`、`Retry`。
- `drive::auth`: `Credentials::load(path)`（検査して読む。書かない）、`CredentialsError`。トークンは `DriveApi` の中でメモリだけに持つ。
- `drive::api`: `DriveApi`（`Clone`・`Send + Sync`）。操作は `list_children`・`list_tree`・`get`・`generate_ids`・`create_folder`・`upload`・`move_to`・`trash`・`read`・`check_credentials`。
  `Limit::{Query, Upload}`・`FileMeta`・`FOLDER_MIME`・上限の定数（`MAX_ENTRIES` など）も公開。
- `ApiError` → `RemoteError` は `From`。`RemoteError` に `AuthInvalid`（照会でも書き込みでも `drive_auth_invalid`）と `Failed`（API の失敗。照会は `drive_unavailable`、書き込みは `rclone_failed`）を足した。ほかの対応は 5 章のまま。

### 9.2 期限とキャンセル
- どの操作も、最後の引数に `Deadline` を取る。`DriveApi::deadline(Limit)` が「今から数えた操作の期限」と「ジョブ全体の期限」（`with_job_deadline`）の早いほうで作る。操作の期限が切れると `ApiError::Timeout`（→ `rclone_timeout`）、ジョブの期限なら `ApiError::JobDeadline`（→ `timeout`）。呼び出し側が自分で `Deadline::operation(Instant)`・`Deadline::job(Instant)` を作って渡してもよい。
- 1 つの操作（フォルダの作成・移動などで複数の要求になるもの）は、1 つの `Deadline` を共有する。写真の送信全体の期限（`--drive-upload-timeout`）は、呼び出し側が写真ごとに同じ `Deadline`（`Limit::Upload`）を渡して表す。
- 要求 1 回の時間切れは、締め切りの残りと上限（JSON は 60 秒、チャンク・multipart は 10 分）の短いほう。**キャンセルは、要求の前・再試行の待ち・チャンクの間で見る**（要求の最中には見ない。最長でその上限まで待つ）。キャンセルされたら `ApiError::Cancelled`（→ `RemoteError::Cancelled`）。並列のアップロードでは、各スレッドが自分の `DriveApi` の複製（同じ `Cancel` を共有）で止まるので、呼び出し側が全スレッドを join してから戻る。

### 9.3 トークン
- 取得: `POST` の本文は設計どおり。`400`・`401` で `error` が `invalid_grant`・`invalid_client`・`unauthorized_client` なら `AuthInvalid`（再試行しない）。**ほかの 4xx は認可の失効とは言えないので `ApiError::Status`（`drive_unavailable`）**にして、再試行もしない。429・5xx・通信の失敗は再試行する。
- 取り直しは 1 つずつ（ロックを持ったまま）。401 を受けたら、**その要求に使ったトークンだけ**を捨てて取り直し、1 回だけやり直す（別のスレッドが先に取り直していれば、その新しいトークンは捨てない）。取り直したトークンでも 401 なら `Status(401)`。
- `expires_in` が無い・読めないときは 3000 秒とみなす（写真サービス側と同じ）。

### 9.4 再試行
- 最初の 1 回のあとに 5 回まで（待ちは 1・2・4・8・16 秒に 0〜25% の揺らぎ。上限 30 秒）。`Retry-After` は**秒の形だけ**読む（日時の形は無視して、指数の待ちにする）。上限（30 秒）を超える値は上限で切る。待ちが締め切りの残りを超えるときは、眠らずに期限のエラーを返す。
- 再試行するもの: 通信の失敗（接続・TLS・時間切れ・読み取りの途中の切断。本文を読む失敗も含む）、429、5xx、403 で、本文のどこかに `rateLimitExceeded`・`userRateLimitExceeded` がある（`errors[]` の何番目でも、形が少し違っても拾う。写真サービスの判定と同じ。権限の不足の 403 は含まない）。使い切ったあと、流量制限は `RateLimited`、5xx は `Status`、通信は `Net`。
- 再試行しない: ほかの 4xx・3xx（`Status`。404 は `NotFound`）、応答の形の誤り。3xx は `max_redirects(0)` で応答として受け取り、たどらない。**ureq は 308 も応答として返す**（resumable の続きの合図に使う）。環境変数の proxy も使わない。
- 再送の約束: `create_folder`・`upload` は、最初に `generateIds` で取った ID を本文に入れ、再送も同じ ID（ID を取り直さない）。応答を失った再送は 409 になり、`get` で親・名前・種類（アップロードは大きさと `md5Checksum` も）が期待どおりなら成功、違えば `Mismatch`。`move_to`・`trash` は、再送の前と 4xx で断られたときに `get` で今の状態を見る（`move_to` は「親が移動先だけ・名前が新しい名前・ゴミ箱でない」、`trash` は「ゴミ箱の中」）。済んでいれば再送しない。`move_to` は、移動元と移動先が同じなら送らずに断る。

### 9.5 resumable のアップロード
- チャンクは 8MiB（256KiB の倍数）。8MiB 以下は multipart、超えたら resumable（試験は `with_sizes` で小さくする）。
- セッションの `Location` は、スキーム・ホスト・ポートが設定のアップロードの URL と同じものだけ（ホスト名は小文字にして比べる。ユーザー情報つき・相対 URL は断る）。違えば送らずに `Rejected`。
- **チャンクの `PUT` には `Authorization` を付けない**（`upload_id` を含むセッションの URL が認可する。アクセストークンを別のホストへ出さない二重の備えにもなる）。セッションを始める `POST` は付ける。
- チャンクは 1 回だけ送り、通信の失敗・429・5xx のときは、少し待ってから `Content-Range: bytes */<size>` で位置を問い合わせる（308 と `Range`。見出しが無ければ 0。200・201 ならその応答が完成したファイル）。位置が進まないまま失敗が `max_retries` を超えたら失敗（チャンクを送ったのに 308 の位置が進まないときも失敗として数える。壊れた応答で、期限まで同じチャンクを送り続けない）。404・410 はセッションの喪失で、**同じ ID** で始め直す（3 回まで。始めた時点で 409 なら、できているものを照合する）。
- 手元のファイルは、multipart では送る前に大きさと MD5 を確かめて（違えば送らずに `Rejected`）、resumable では大きさだけ確かめる（MD5 は、完成の応答の `md5Checksum` と照らす。違えば `Mismatch`）。

### 9.6 一覧・木
- 件数の上限 20000（`MAX_ENTRIES`）、ページ数の上限 1000、木の深さの上限 32。同じ `nextPageToken` が前に使ったものと重なったら（直前でなくても）`TooManyPages`。名前に `/` を含む項目が木にあれば `BadResponse`（相対パスが曖昧になる。`list_children` は名前をそのまま返す）。木で同じフォルダに 2 度入ったら `TooManyPages`。
- `Entry.mime` は Drive の `mimeType` のまま（フォルダは `application/vnd.google-apps.folder`。rclone の `inode/directory` ではない）。`is_dir`・`is_shortcut()` で判定するので、後半の呼び出し側は `mime` の値に頼らない。ショートカットは `size` が無いので -1。
- `read` が上限を超えたら `TooLarge`（`BadOutput`）。エラー応答の本文の上限は 64KiB（成功の本文の上限とは別）。

### 9.7 偽の Drive の HTTP サーバー（7 章の実装）
- 置き場所は `tests/common/fake_drive.rs`。模型（`fake-rclone/model.rs`）は動かさず、`Node` に `trashed` を足しただけ（偽の rclone の `purge` は今までどおり木から消して `Drive::trashed` に ID を残す）。保存は一時ファイル＋rename にした（HTTP サーバーが読んでいる最中に半端な内容を見せない）。
- 状態は偽の rclone と同じファイル（`drive.json`・`ctl.json`・`calls.log`）。要求ごとに読み書きするので、`Env::drive()`・`Env::save()`・`Env::add_rule()`・`Hook` がそのまま使える。CLI を子プロセスで動かす試験は `FakeDrive::url()` を `--drive-endpoint` に渡す。
- calls.log の行は `kind: "http"`、`op`（中立の操作名＋ `get`・`generate_ids`・`token`）、`method`・`route`・`subject`・`authorization`（`ok`・`invalid`・`missing`。値は書かない）・`injected` ほか。resumable は 1 回の送信が複数行（`phase` が `init`・`chunk`・`status`）。規則の「何回目」は、条件（`op`・`contains`）に合う要求の数。`contains` は `subject`（対象のノードのパス）と `phase:<名前>` の部分一致。
- **`Env::mutating_ops` は `get`・`generate_ids`・`token` も読むだけとして除く。`Env::ops_used`・`op_count` は `op` を持つ行を全部数える**（PR #87 のレビューの推奨）。
- 偽のサーバーは、実際の Drive に近い厳しさにしてある: 一覧の `q` が `'<ID>' in parents and trashed=false` 以外なら 400（名前で絞る退行を捕まえる）、`generateIds` で払い出していない ID での作成は 400、重複した ID は 409、`removeParents` が今の親でなければ 404、`PATCH` の本文は `name`・`trashed:true` だけ、`DELETE`・`PATCH /upload/…`・`emptyTrash` は 405（`op: unsupported`）。`size` は文字列。エラーは `error.errors[].reason`、トークンのエラーは `{"error":"invalid_grant"}`。308 は `Range: bytes=0-N`（何も受けていなければ見出しなし）。
- 故障注入の効果は `tests/common/fake_drive.rs` の関数（`status`・`rate_limit_403`・`too_many_requests`・`unauthorized`・`invalid_grant`・`drop_connection`・`do_then_drop`・`cut_response`・`redirect`・`repeat_page_token`・`bad_location`・`cut_after`・`lose_session`）と、`tests/common/drive.rs` の `fail`・`do_then_fail`・`sleep_ms`・`garbage`。rclone 版だけの効果（`late_then_fail`・`filewise`）は HTTP 版には無い（API の移動はファイル単位に化けないので要らない。移動の反映の遅れを試すなら、後半で `sleep` か状態の書き換えで作る）。

### 9.8 共通にしたもの
- 重複キーを断る JSON の読み方は、`serve.rs` の本文の検査から `jsonfile::parse_strict_object(bytes, 許すキー)` に移して、資格情報の読み込みと共有した（serve の振る舞いは変えない）。
- ureq は `rustls`・`gzip`（geoframe crate と同じ形）。Cargo.lock に新しいパッケージは増えない。

### 9.9 後半（つなぎ込み）へ

- `Env::ops()` には `token`・`get`・`generate_ids` が混ざる（rclone 版には無い）。`op_count("upload")` は**要求の数**で、ファイルの数ではない（resumable は 1 ファイルが複数行）。1 段目の受け入れ試験の「並び」の検査は、`token` を除く・`get`・`generate_ids` を含めた並びに直す。`mutating_ops`・`assert_no_mutations` は読むだけの操作を除くので、そのまま使える。
- 試験側の `Env::save()` は、サーバーのロックの外で `drive.json` を書く。要求が飛んでいる最中に書き換えると、サーバーの更新と食い違って更新が消えうる。書き換えは `Hook::mutate_after` のように、フローが止まっている間に行う。
- 要求 1 回の上限（JSON 60 秒、チャンク・multipart 10 分）は、unit の `TimeoutStopSec=180s` より長い。キャンセルは要求の最中には効かない（要求の前・再試行の待ち・チャンクの間で見る）。
- multipart は、中身と本文で最大 16MiB 弱をメモリに持つ。並列 4 なら 64MiB 前後で、`MemoryMax` の測り直しに含める。
- 名前に `/` は渡せない（`Rejected`）。サブフォルダつきの相対パスの写真は、後半がフォルダを作って、葉の名前で送る。
- `read` は上限を超えると `TooLarge`（→ `BadOutput`）を返す。`Remote::read` の「上限 + 1 バイトまで返す」とは違うので、呼び出し側の「超えたか」の判定を合わせる。
- フォルダの `Entry.mime` は Drive の値（`application/vnd.google-apps.folder`）。`is_dir` で判定する。
- `DriveApi::list_tree` の順は幅優先（rclone の `lsjson -R` の順とは違う）。呼び出し側は、集合として並べ替えて比べる（`drive/mod.rs` は並べ替えて比べている）。
