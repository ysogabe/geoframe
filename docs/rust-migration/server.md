# Rustサーバーの基盤: 設定・DB読み取り・ログ・ヘルス（P1 / T1.1）

- 状態: T1.1 実装済み（2026-10-24）。レビュー指摘（パニック・トランザクション・ヘルス）を反映済み。T1.2（`GET /api/v1/photos`、§8）を追記
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 計画: [plan.md](plan.md) / タスク: [tasks.md](tasks.md) / 棚卸し: [inventory.md](inventory.md)（§2 DB、§4 設定、§6.3 ログ）
- 実装: `crates/geoframe/src/config.rs`・`db.rs`・`logging.rs`・`server.rs`・`cli.rs`
- ビルドと配布は[build.md](build.md)。ここは**起動したあとの振る舞い**の決めごと。

## 1. 設定

現行Pythonの`GEOFRAME_*`と**同じ名前・同じ既定値**で読む。読み込みは`Config::load`で、環境変数は引数のクロージャ経由で引く（テストが並行しても干渉しない）。

### 1.1 優先順位

高い順に **起動引数 > 環境変数 > 既定値**。

| 項目 | 起動引数 | 環境変数 | 既定値 |
| --- | --- | --- | --- |
| 待ち受け | `--listen <addr:port>`（hostとportの**両方**を置き換える） | `GEOFRAME_HOST` / `GEOFRAME_PORT` | `0.0.0.0` / `8080` |
| データ置き場 | `--data-dir <dir>` | `GEOFRAME_DATA_DIR` | `./var`（実行時のカレントディレクトリ基準。絶対パスにして使う） |
| 許可するHost名 | なし | `GEOFRAME_ALLOWED_HOSTS`（カンマ区切り。`localhost`は常に入る） | `localhost`のみ |
| ログの詳しさ | なし | `GEOFRAME_LOG`（`error`/`warn`/`info`/`debug`/`trace`。**Rustだけの項目**） | `info` |
| `TZ` | なし | `TZ` | （後述。解釈しない） |

- **空文字の環境変数は未設定と同じ**に扱う。現行の`GEOFRAME_DATA_DIR`が`or`で空文字を未設定扱いにするのと同じで、`EnvironmentFile`に`KEY=`と書いた事故を避ける。
- **不正な値は起動を断る**（終了コード2）。メッセージには環境変数の名前と理由だけを書き、値は書かない。`GEOFRAME_HOST`はIPアドレスだけ（現行Pythonはホスト名も通すが、本番は`0.0.0.0`か`127.0.0.1`）。
- `--listen`を渡したときは`GEOFRAME_HOST`/`GEOFRAME_PORT`を**読まない**（不正でも問わない）。
- `GEOFRAME_ALLOWED_HOSTS`の解釈は現行の`_parse_allowed_hosts`と同じ（小文字、末尾の`.`を除く、`name:port`・`[v6]:port`のポートを無視）。この集合を使うHost検査そのものはT1.3。
- **`GEOFRAME_DATA_DIR`の既定が現行と違う**: Pythonは`app/var`（ソースの隣）だが、単一バイナリには`app/`が無いので`./var`にした。本番は常に`GEOFRAME_DATA_DIR=/var/lib/geoframe`をunitが与える。

### 1.2 `TZ`

**Rustは`TZ`を解釈しない。** P1は時刻を変換せず、`taken_at`などのnaiveな日時は文字列のまま扱う（[inventory.md §6.1](inventory.md)）。旅番号の48時間差などの計算（T1.2）も、環境のTZに依存しない`NaiveDateTime`で行う。ただしunitは`TZ=Asia/Tokyo`を与え続ける（並走するPythonのため、将来`Local`を使うときの保険）。起動ログに値を出すだけ。

### 1.3 `app/.env`は読まない

決定: **Rustは`app/.env`を読まない。** 理由は次のとおり。

- 単一バイナリの配布には`app/`が無く、`.env`の置き場が定まらない。
- 本番は既にsystemdの`EnvironmentFile=-/etc/default/geoframe`が同じ役目を持つ（unitの最後に読まれ、後勝ち）。
- 現行でも`GEOFRAME_DATA_DIR`は`.env`と鶏卵の関係にあり（`config.py`のコメント）、手書きのパーサーはsystemdの引用符の扱いとずれやすい。

手元で試すときは、環境ファイルを読み込んでから起動する（`set -a; . ./環境ファイル; set +a`）。現行の`.env`は`setdefault`（環境変数が優先）なので、優先順位の向きは同じ。

### 1.4 未対応の項目（P2以降）

`config.rs`の`UNSUPPORTED_ENV`に、現行の`config.py`にあるP2以降の項目（取り込み・タイル・AI・位置候補など30個）を**列挙だけ**してある。P1は読まず、警告も出さない（`/etc/default/geoframe`に書いてあっても無視される）。`docs/reference/configuration.md`にある`GEOFRAME_*`が、読む項目（`SUPPORTED_ENV`）か未対応のどちらかに分類されていることを、テスト`every_documented_variable_is_classified`が検査する。Pythonに項目が増えたら、このテストが落ちて気づける。

秘密になりうる項目（P2の`GEOFRAME_DRIVE_KEY`ほか）が入るときも、設定が持つのは**鍵ファイルのパスだけ**で、中身は設定の型に載せない。`Config`は`Debug`を手書きにしてあり、ログへ出す要約（`log_summary`）に載せた項目だけが出る。許可Host名も件数だけで、名前は出さない。

## 2. SQLite読み取り層

`crates/geoframe/src/db.rs`の`Catalog`。

### 2.1 読み取り専用

- `SQLITE_OPEN_READ_ONLY`（`CREATE`なし）＋`PRAGMA query_only`。書き込みはSQLiteが拒否し、無いDBを作ることもない。
- **スキーマの作成・移行をしない。** それは当面Python（`store.py:db`）の仕事。Rustは`REQUIRED_SCHEMA`（`photos`の25列、`photo_quality`、`photo_ai_groups`、`photo_ai_group_members`のうちP1の読み取りが使う列）がそろっているかを起動時に確かめる。T1.2/T1.4で使う列が増えたら、ここに足す。
- WALのDBは、読み取り専用で開いても`-shm`が要る（無ければ作る）。そのため**DBのあるディレクトリには書ける**必要がある（systemdの`StateDirectory=`なら満たす）。DB本体と`-wal`は変更しない（テストで確認。`-shm`だけは読み取りでも触れる）。

### 2.2 接続プール

- tokioのworkerでSQLiteを直接実行しない。`Catalog::read(|conn| …)`が、`spawn_blocking`の中で接続を借りて実行する。
- API用の接続は最大4本（`READ_CONNECTIONS`）。起動時に1本だけ開き、残りは必要になったときに開く。同時に走る読み取りは4が上限で、順番待ちは**非同期のセマフォ**で行う（待っている間もworkerは塞がらない）。順番待ちには**上限10秒**（`QUEUE_TIMEOUT`）があり、超えると`CatalogError::QueueTimeout`を返す。API（T1.2以降）はこれを**503**にする。
- **`/healthz`専用の接続を別に1本**持つ（`busy_timeout`は2秒）。API側の4本が長い読み取りで埋まっていても、DBが正常なら`/healthz`は待たされず200を返せる（§4）。
- 呼び出し側が先にFutureを捨てても（タイムアウトなど）、始まった読み取りは最後まで走って接続を戻す（テスト`the_connection_returns_when_the_caller_gives_up`）。
- **接続を戻す前の後始末**: 読み取りのあと、トランザクションが開いたままなら`ROLLBACK`する（できなければ接続を捨てる）。開いたまま戻すと、その接続は古いスナップショットを見続け、Python側の`-wal`のチェックポイントも進められず際限なく大きくなる。また、`SQLITE_IOERR`・`SQLITE_CORRUPT`・`SQLITE_NOTADB`など接続の状態が壊れたと見られるエラーが出たときも接続を捨てる（SQLの誤りのような普通のエラーでは戻す）。
- 読み取りがパニックしたときはその接続を捨て、次の読み取りが開き直す。エラーの文字列は**定型文**（`読み取りがパニックしました`）で、パニックのメッセージ（切り出しに失敗した文字列など値が入りうる）はログに出さない（§3.1）。
- 新しい依存（プールのcrate）は入れていない。
- **P3で書き込みを足す構造**: 書き込みは単一の接続を専用スレッドに置き、チャネルで依頼を受ける（現行の「1接続＋グローバルロック」と同じ直列化）。`Catalog`に`writer`を足す形で、`read`のAPIは変えずに済む。今は作らない。

### 2.3 Pythonが書き込み中でも読める

DBはWAL（Pythonが`journal_mode=WAL`にし、その状態がDBに残る）なので、読み取りは書き込みを待たない。コミット前の行は見えず、チェックポイント前のコミットはWALから読める。`busy_timeout`は5秒で、WALの復旧中などで一瞬`SQLITE_BUSY`になる場合のため。テスト:

- `uncheckpointed_commits_are_readable_and_files_are_untouched`: 書き込み側が自動チェックポイントを止めてコミットした行（WALにだけある）が読めること、DB本体と`-wal`がバイト単位で変わらないこと（T0.3レビューからの持ち越し）。
- `reads_do_not_wait_for_an_open_write_transaction`: `BEGIN IMMEDIATE`で書き込み中でも読み取りが待たず、コミット後に見えること。

### 2.4 DBが無い・壊れている・古いときの扱い

| 状況 | 扱い |
| --- | --- |
| 起動時: DBが無い・開けない・SQLiteのDBでない | **起動を断る**（終了コード1。ログに`DB を開けません`と、`GEOFRAME_DATA_DIR`を確認する案内） |
| 起動時: 必要なテーブル・列が無い（古い版・別物） | **起動を断る**。ログに足りない`テーブル.列`の名前だけを出す |
| 起動後: DBが読めなくなった（入れ替え・破損・ディスクエラー） | **プロセスは動かし続け**、`/healthz`が503を返す。ログにwarn |

起動時に断るのは、`GEOFRAME_DATA_DIR`の誤りや配備の誤りが最も多く、503を返し続けるプロセスに隠すよりsystemdの失敗として見えるほうがよいため。DBを作るのはPythonなので、Rustを先に起動した場合は失敗する（P5でPythonを止めてRustに切り替えるときは、既にDBがある）。一方、動いている間の一時的な障害は、再起動のループにせず`/healthz`で見せる。

注意: 起動後にDBファイルを別のファイルへ入れ替える（バックアップからの復元など）と、開いている接続は古いファイルを指し続ける。復元したらRustを**再起動**する（P5の手順に書く）。

## 3. ログ

`tracing`で標準出力へ、1行1イベント。時刻と色は付けない（journaldが時刻を付ける）。`GEOFRAME_LOG`で詳しさを選ぶ（既定`info`）。**GEOFRAME自身のログだけ**を出し、依存クレート（axum・hyperなど）のログは全部捨てる（`trace`でも出ない）。依存のログには接続元アドレスなどが入りうるため（axumはTRACEで`connection <addr> accepted`を出す）。

### 3.1 出してよいもの

- リクエストログ: **メソッド・ルートのテンプレート・状態コード・所要時間（ミリ秒）**だけ。例: `GET /healthz 200 0ms`、`GET (unmatched) 404 0ms`。
  - ルートは`/derivatives/{*path}`のような**テンプレート**で出す。どのルートにも当たらないリクエストは`(unmatched)`。実際のパスとクエリは出さない。
  - 5xxはwarn、`/healthz`の成功はdebug、それ以外はinfo。
- パニック: **場所（ソースのファイルと行）だけ**。メッセージは出さない（標準のパニック表示はstderrに値を書きうるので、`cli::serve`が`logging::init`の直後に`std::panic::set_hook`で置き換える。テスト`panic_hook.rs`）。
- 起動・停止、待ち受けアドレス、データ置き場のパス、`Config`の要約（待ち受け・データ置き場・許可Host名の件数・ログの詳しさ・`TZ`）。
- スキーマ不足の**列名**、件数、状態名、写真ID。
- SQLiteなどが返したエラーメッセージ（**ログにだけ**。HTTP応答には載せない）。

### 3.2 出してはいけないもの

[inventory.md §6.3](inventory.md)の方針を引き継ぐ。

- 写真の内容、EXIF・GPS、緯度経度、地名。
- ファイル名・パス（Drive IDを含む）。リクエストの実際のパスとクエリ（検索語やファイル名が入りうる）。
- 秘密情報: 鍵ファイルの中身、トークン、`Authorization`などのヘッダ値。
- 接続元アドレス、`Host`ヘッダの値、許可Host名の中身。

テスト`access_log.rs`が、緯度経度らしいクエリ・ファイル名・Drive IDらしい値・接続元アドレス・`Host`値がログに出ないことを検査する（購読者の切り替え──`set_default`はスレッドごと、`set_global_default`は1プロセス1回──が並行して走る他のテストと干渉しないよう、プロセスを分けた単独のテストファイルにしてある）。新しいログを足すときは、3.1に当てはまるかを先に確かめる。

### 3.3 応答にも載せない

500などの応答は定型文にし、例外・SQLiteのエラー文字列・パスを本文に入れない（理由はログにだけ出す）。

## 4. ヘルス確認

`GET /healthz`（Pythonには専用のヘルスが無かったため、新設の仕様）。

| 状況 | 状態 | 本文 |
| --- | --- | --- |
| プロセスが動いていて、`photos`を1行読める（空のテーブルでも可） | 200 | `{"ok":true}` |
| DBを読めない、または3秒以内に答えが返らない | 503 | `{"ok":false}` |

- `/healthz`は**API用の4本とは別の専用の接続**で確かめる。API側の読み取りが4本を埋めていても、DBが正常なら待たされない（以前は同じプールの順番待ちを含めて3秒で打ち切っていたため、DBが正常でも混雑で503になりえた）。
- 専用の接続の`busy_timeout`は2秒で、`/healthz`自身の上限3秒より短い。DBが詰まっていても、タイムアウトの前にSQLiteが`SQLITE_BUSY`で諦めて、503をこちらから返せる。

- 本文は`ok`だけ。件数・パス・エラーの理由は載せない（理由はwarnログ）。`Cache-Control: no-store`を付ける。
- 起動時のスキーマ検査は通っているので、ここで見るのは「その後も読めるか」。
- P0の`{"ok":true}`との互換: 200のときの本文は同じ。

## 5. 起動引数の更新

```
geoframe serve [--listen <addr:port>] [--data-dir <dir>]
```

`--listen`は必須ではなくなった（環境変数と既定値で決まる）。`check-db`は従来どおり、スキーマの全項目を確かめず`photos`の行数だけを出す（古いDBの調査にも使えるように）。

## 6. systemdで動かすときの注意

unitの本体はP5で整える（[build.md](build.md)）。T1.1の時点で分かっている注意を先に書いておく。

- **設定不正の終了コード2で再起動ループしない**ようにする。設定の誤りは`Restart=on-failure`で直らないので、unitに`RestartPreventExitStatus=2`を書く（DBが開けない・スキーマ不足の終了コード1は、Pythonより先に起動した場合などで再試行に意味があるので、そのまま再起動してよい）。
- **Pythonと並走するとき**（P1〜P4の検証）は、`ExecStart`に`--listen <addr:port>`を**書く**。`GEOFRAME_PORT`が`/etc/default/geoframe`にあってもPythonと同じ値になり、ポートが衝突するため。`--listen`は`GEOFRAME_HOST`/`GEOFRAME_PORT`より優先され、それらを読まない（§1.1）。
- Rustも**`User=geoframe`で動かす**。WALのDBは読み取り専用でも`-shm`・`-wal`を触るので、ファイルの持ち主がPythonの実行ユーザーと違うと、片方が開けなくなる。
- **P5で、`app/.env`にだけある`GEOFRAME_*`を`/etc/default/geoframe`と突き合わせる**。Rustは`app/.env`を読まない（§1.3）ので、`.env`にしか書かれていない値は、移すか、捨てる判断をする。
- **DBファイルの入れ替え（バックアップからの復元など）にはRustの再起動が要る**（§2.4の注意）。

## 7. 確認の記録

- `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（合成した一時DBだけ。実データ・実在の位置は使わない）。
- musl静的ビルド: `static-pie linked`、`ldd`は`statically linked`。追加した依存`tracing`・`tracing-subscriber`は純Rust（C依存なし）。
- 合成DBでの手動確認: `serve`が`/healthz`に200、未知のパスに404を返し、SIGTERMで正常終了（終了コード0）。DBが無いときは終了コード1、`GEOFRAME_PORT`が不正なときは2。

## 8. 写真フィード `GET /api/v1/photos`（T1.2）

実装は`crates/geoframe/src/photos.rs`（選び方・並び・タイトル・旅番号）と`server.rs`（ルート）。応答の型は`feed.rs`、契約は[photo-feed-contract.md](../architecture/photo-feed-contract.md)。現行Pythonの`api_photos()`と**同じ行・同じ並び・同じ値**を返す。

### 8.1 SQLと並び

- 品質・AIの`JOIN`と表示条件（`lat`あり、`display`あり、`hidden`でない、品質`exact`の推奨外とAI同一場面の推奨外を除く。お気に入りはAIでは残る）の**SQLは文字列まで同じ**。違うのは`p.*`を使う列だけに絞ったことと、`ORDER BY`だけ:
  `CASE WHEN COALESCE(p.taken_at,'')='' THEN 1 ELSE 0 END, COALESCE(p.taken_at,'')`（Pythonは`taken_at`のみで、NULLが先頭になる）。
- **同順位（同じ`taken_at`）の並びは固定していない。** Pythonも同じで、SQLiteのソートと`LEFT JOIN`の走査順に依存する。同じSQLなので通常は同じ並びになるが、リンクするSQLiteのバージョン（Rustは同梱のSQLite）が違えば変わりうる。T1.5で本番コピーの全件比較をして、差が出たら`, p.id`を足す（`inventory.md §6`の方針）。テストはフィクスチャ（同順位が出ない）と、同順位を固定しない確認だけ。
- 複数のAIグループに入った写真は行が重なる（PythonのSQLが同じなので同じ。本番では起きない前提）。

### 8.2 値

- タイトル: 手動/AIの`title` → `city`があれば`"{city} {place}"`を`strip` → ファイル名の拡張子なし（`pathlib.Path(name).stem`と同じ。`a.b.c.jpg`→`a.b.c`、`noext`→`noext`、`trail.`→`trail.`、`.hidden`→`.hidden`、ディレクトリ部分は除く）。`std::path::Path::file_stem`とは末尾が`.`の名前などで違うので使っていない。
- `bool()`の真偽（NULL・0・空文字は偽）、`or ""`（NULLと空文字を同じ空文字に）、`str.strip()`（Unicode空白と`\x1c`〜`\x1f`）はPythonに合わせてある。
- 品質の内部値はグループ（`quality_kind`が空でない）に入ったときだけ入れ、AIの内部値は`ai_group`が空でないときだけ入れる（それ以外は`null`/`false`/空文字）。
- 時刻の解釈は`photos.rs:parse_epoch`。Pythonの`strptime("%Y-%m-%d %H:%M")`が通す形（月日時分が1〜2桁、日付と時刻のあいだが複数の空白、存在しない日付は不可、前後の余分な文字は不可）に合わせた。**既知の差**: 全角数字などASCII以外の数字は、Pythonは通すが、Rustは読めない扱い。`norm_time`は全角数字を正規化しないので、EXIFに全角数字が入った写真では旅番号がずれうる（T1.2レビューで確認。本番コピーでは差が出ていない）。P2でRustのインデクサを作るときに、保存時の正規化で解消する。読めない時刻は「不明」と同じ（旅番号では1枚ごとに別の旅。`time`の値はそのまま出す）。
- 旅番号は`assign_trips`と同じ: 直前に**読めた**時刻との差が**48時間を超えたら**次の旅（ちょうど48時間は同じ旅）。時刻が読めない写真は前後とつながず、それだけで1つの旅。次の読める写真は、不明な写真を飛ばして直前の読めた時刻と比べる。差は暦だけで数える（TZに依存しない。§1.2）。

### 8.3 Pythonとの差

意図した差は2つ。どちらも契約（`time`・`lon`は必ずある）に合わせたもの。

- `taken_at`がNULLの行: Pythonは`time: null`で先頭、Rustは空文字（時刻不明）で最後。本番のコピーには該当する行が無いことを管理者が確認済み。
- `lat`はあって`lon`がNULLの行: Pythonは、同じ旅に他の写真があれば`assign_trips`の`min(lon)`が`TypeError`になって**一覧全体が500**、その行だけの旅なら`lon: null`。Rustは一覧から外す（写真IDだけをwarnログに出す）。通常は`setloc`が`lat`/`lon`を一緒に書くので起きないが、**本番のコピーでは未確認**。T1.5の全件比較で、Pythonが500でRustが200の差が出たらこれが原因。

ほかに、列の型が想定と違う行（`lat`が文字列など）があるとRustは読み取り全体が失敗して503になる（Pythonは素通し）。スキーマ上は起きない。

### 8.4 応答とエラー

- `Content-Type: application/json; charset=utf-8`、`Cache-Control: no-store`（Pythonの`_send_json`と同じ）。JSONは日本語をエスケープしないUTF-8。区切りに空白は入れない（Pythonは`", "`・`": "`。比較は構文解析後に行う）。クエリは無視する。
- DBを読めない・順番待ち（`QUEUE_TIMEOUT`）が上限を超えたら**503**と`{"ok":false,"error":"写真一覧を読み取れません。しばらくしてから再試行してください"}`。SQLiteのエラー文字列・パスは本文に入れず、ログ（warn）にだけ出す。JSON化の失敗は500。
- ログに写真の値（位置・ファイル名・タイトル）は出さない（§3）。リクエストログは`GET /api/v1/photos 200 12ms`の形。

### 8.5 T1.3以降に残すもの

axumのルーティングは、Pythonの`_norm_path`（末尾スラッシュの除去、URLデコード、`..`の解決など）、`HEAD`の扱い（Pythonは`/api/`の`HEAD`を403にする。axumの`get`は`HEAD`にも答える）、Host検査をまだ再現していない。`/api/v1/photos/`は404になる。これらは横断の仕組みなのでT1.3で入れる。

### 8.6 テスト

`crates/geoframe/tests/photo_feed_endpoint.rs`（合成データのみ）:

- フィクスチャと同じ合成DBをRustで作り（`tools/gen_photo_feed_fixture.py`が書き出す`photo-feed-catalog.sql`を流す）、エンドポイントの出力がPythonの出力（`photo-feed-python.json`）と`serde_json::Value`として完全一致すること。
- 除外条件、並び（時刻不明は最後）、旅番号の境界（ちょうど48時間、時刻不明を挟む場合）、タイトルの規則、`taken_at`のNULL、`lon`のNULL、ヘッダ、503（DB破損・順番待ちの超過）を個別に。
- `photo_feed_miharashi.rs`の読み方をエンドポイントの実出力にも適用。
