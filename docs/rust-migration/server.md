# Rustサーバーの基盤: 設定・DB読み取り・ログ・ヘルス（P1 / T1.1）

- 状態: T1.1 実装済み（2026-10-24）。レビュー指摘（パニック・トランザクション・ヘルス）を反映済み。T1.2（`GET /api/v1/photos`、§8）、T1.3（静的配信・Host検査・管理画面ページ、§9）、T1.4（管理画面の読み取りAPI、§10）を追記
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 計画: [plan.md](plan.md) / タスク: [tasks.md](tasks.md) / 棚卸し: [inventory.md](inventory.md)（§2 DB、§4 設定、§6.3 ログ）
- 実装: `crates/geoframe/src/config.rs`・`db.rs`・`logging.rs`・`server.rs`・`cli.rs`、T1.3 は`webpath.rs`・`hostcheck.rs`・`statics.rs`・`embedded.rs`と`build.rs`、T1.4 は`admin.rs`・`pyquery.rs`・`sysinfo.rs`
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
| 位置推定の1段目の窓（分） | なし | `GEOFRAME_INFER_MIN` | `15` |
| embeddingの場所 | なし | `GEOFRAME_EMBED_BACKEND` | `gemini` |
| タイルキャッシュの上限（MB） | なし | `GEOFRAME_TILE_CACHE_MAX_MB` | `16384` |

- **空文字の環境変数は未設定と同じ**に扱う。現行の`GEOFRAME_DATA_DIR`が`or`で空文字を未設定扱いにするのと同じで、`EnvironmentFile`に`KEY=`と書いた事故を避ける。
- **不正な値は起動を断る**（終了コード2）。メッセージには環境変数の名前と理由だけを書き、値は書かない。`GEOFRAME_HOST`はIPアドレスだけ（現行Pythonはホスト名も通すが、本番は`0.0.0.0`か`127.0.0.1`）。
- `--listen`を渡したときは`GEOFRAME_HOST`/`GEOFRAME_PORT`を**読まない**（不正でも問わない）。
- `GEOFRAME_ALLOWED_HOSTS`の解釈は現行の`_parse_allowed_hosts`と同じ（小文字、末尾の`.`を除く、`name:port`・`[v6]:port`のポートを無視）。この集合を使うHost検査は§9.2。
- **最後の3つは T1.4 で読み始めた**。取り込み・AI・タイルは P2 以降なので、これらは**管理画面の表示にだけ**効く（`/api/admin/status`、§10）: `GEOFRAME_INFER_MIN`は「推定の内訳」の区切りと位置候補の「日」の区切り、`GEOFRAME_EMBED_BACKEND`はAIの件数が数える`embedding_model`（`gemini`→`gemini-embedding-2`、`local`→`embeddinggemma-2`）、`GEOFRAME_TILE_CACHE_MAX_MB`は`disk.tile_cache_max_mb`にそのまま出る。**読まないと Python と値がずれる**ため（`/etc/default/geoframe`に書いてあれば、Python と同じ値を Rust も見る）。解釈は現行に合わせ、`GEOFRAME_EMBED_BACKEND`は前後の空白を除いて小文字にし、`gemini`・`local`以外は**起動を断らず`gemini`に落とす**（現行も警告して`gemini`にする。Rustは起動ログにwarnを1行出す。値は書かない）。`GEOFRAME_INFER_MIN`は0以上の有限の数、`GEOFRAME_TILE_CACHE_MAX_MB`は整数でなければ**起動を断る**（Rustの方針。現行は`float()`/`int()`が例外で落ちるので、起動できない点は同じ）。
- **`GEOFRAME_DATA_DIR`の既定が現行と違う**: Pythonは`app/var`（ソースの隣）だが、単一バイナリには`app/`が無いので`./var`にした。本番は常に`GEOFRAME_DATA_DIR=/var/lib/geoframe`をunitが与える。

### 1.2 `TZ`

**Rustは`TZ`を解釈しない。** P1は時刻を変換せず、`taken_at`などのnaiveな日時は文字列のまま扱う（[inventory.md §6.1](inventory.md)）。旅番号の48時間差などの計算（T1.2）も、環境のTZに依存しない`NaiveDateTime`で行う。ただしunitは`TZ=Asia/Tokyo`を与え続ける（並走するPythonのため、将来`Local`を使うときの保険）。起動ログに値を出すだけ。**例外**: `/api/admin/status`の`server_time`と`disk.measured_at`だけは現地時刻を出すので、Cライブラリの`localtime_r`に任せる（`sysinfo.rs`。§10.4）。muslは実行時に`TZ`と`/usr/share/zoneinfo`を読むので、unitが`TZ=Asia/Tokyo`を与えていれば Python と同じ時刻になる。tzdataが無い機体ではPythonもRustもUTCになる。

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
- **スキーマの作成・移行をしない。** それは当面Python（`store.py:db`）の仕事。Rustは`REQUIRED_SCHEMA`（`photos`の25列、`photo_quality`、`photo_ai_groups`、`photo_ai_group_members`のうちP1の読み取りが使う列。T1.4で`photo_quality_features`・`photo_ai_features`・`photo_loc_clues`・`photo_loc_runs`・`photo_loc_suggestions`・`settings`を足した）がそろっているかを起動時に確かめる。使う列が増えたら、ここに足す。**T1.4 で足した表は Python が起動時に作る**ので、古い版のPythonで止まったままのDBには無く、Rustは起動を断る。**T1.5 の本番コピーでは、これらの表があること（比較用Pythonを一度でも動かしたコピーならある）を先に確かめる。**
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
- **ファイル記述子の上限は`LimitNOFILE=1024`以上を前提にする**（systemdの既定）。同時の接続256＋配信中のファイル32＋SQLite（読み取り5接続と`-wal`/`-shm`）＋標準入出力で300前後を使う（§9.3）。上限を下げるなら、`limits.rs`の`MAX_CONNECTIONS`・`STATIC_FILE_SLOTS`もあわせて見直す。
- **DBファイルの入れ替え（バックアップからの復元など）にはRustの再起動が要る**（§2.4の注意）。

## 7. 確認の記録

- `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（合成した一時DBだけ。実データ・実在の位置は使わない）。
- musl静的ビルド: `static-pie linked`、`ldd`は`statically linked`。追加した依存`tracing`・`tracing-subscriber`は純Rust（C依存なし）。
- 合成DBでの手動確認: `serve`が`/healthz`に200、未知のパスに404（T1.3以降は403。§9）を返し、SIGTERMで正常終了（終了コード0）。DBが無いときは終了コード1、`GEOFRAME_PORT`が不正なときは2。
- T1.3: musl静的ビルドの`file`は`static-pie linked`、`ldd`は`statically linked`のまま。管理画面の静的ファイルの埋め込み後のサイズは[build.md §3](build.md)。

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

### 8.5 横断の仕組み（T1.3 で入れた）

パスの正規化（末尾スラッシュの除去、URLデコード、`..`の解決など）、`HEAD`の扱い（Pythonは`/api/`の`HEAD`を403にする。axumの`get`は`HEAD`にも答えてしまう）、Host検査は、T1.3で入れた（§9）。`/api/v1/photos/`は静的のホワイトリストに当たらず403になる（現行と同じ）。

### 8.6 テスト

`crates/geoframe/tests/photo_feed_endpoint.rs`（合成データのみ）:

- フィクスチャと同じ合成DBをRustで作り（`tools/gen_photo_feed_fixture.py`が書き出す`photo-feed-catalog.sql`を流す）、エンドポイントの出力がPythonの出力（`photo-feed-python.json`）と`serde_json::Value`として完全一致すること。
- 除外条件、並び（時刻不明は最後）、旅番号の境界（ちょうど48時間、時刻不明を挟む場合）、タイトルの規則、`taken_at`のNULL、`lon`のNULL、ヘッダ、503（DB破損・順番待ちの超過）を個別に。
- `photo_feed_miharashi.rs`の読み方をエンドポイントの実出力にも適用。

## 9. 静的配信・Host検査・管理画面ページ（T1.3）

実装は`webpath.rs`（URLパスの正規化とホワイトリスト。純関数）、`hostcheck.rs`（Host検査。純関数）、`statics.rs`（実体の解決と応答、Pythonと同じ形のエラーページ）、`embedded.rs`＋`build.rs`（埋め込み）、`server.rs`（ルートと検査）。現行Pythonの`handlers.py`（`_host_ok`・`_norm_path`・`_static_allowed`・`end_headers`・`do_GET`/`do_HEAD`/`do_POST`）を、同じリクエストを流して挙動を確かめながら移した。

### 9.1 リクエストが通る順序

外側から **ログ → ヘッダ付与 → 検査（`guard`）→ ルート** の順。検査はPythonの`do_*`の入口と同じ順序。

1. `GET`/`HEAD`/`POST`以外は**501**。`OPTIONS`もここで、**CORSヘッダは一切返さない**（`Allow`も付けない）。クロスオリジンの`application/json`のプリフライトが必ず失敗する＝CSRF対策の一部（[inventory.md §1.2](inventory.md)）。Host検査より前に判定する（Pythonも同じ順）。
2. **`Host`ヘッダが2つ以上あれば400**（§9.2）。無い場合は従来どおり許可。
3. **Host検査**（§9.2）。不許可は403。
4. **絶対形式のrequest-target**（`GET http://host/…`）は、`Host`が許可でも**全ルートで403**（`/healthz`・`/api/v1/photos`も。Pythonはホワイトリストに当たらず403）。Host検査の後に判定するので、`Host`が不許可なら先にHost用の403になる（Pythonの`_host_ok`が先なのと同じ）。
5. **`POST`は全部403**。更新系はP3で実装するまで、現行の「未知のPOST」と同じ403にする。
6. ルート: `/healthz`、`GET /api/v1/photos`（§8）、それ以外は`site`（下記）。

`site`（`GET`/`HEAD`）は、現行の`do_GET`/`do_HEAD`と同じ順に判定する。

1. `GET`の`/api/admin/status`・`/api/admin/photos`は管理画面の読み取りAPI（T1.4、§10）。**生のパスの前方一致**で、現行と同じ。そのあと、`/tiles/`（地図タイル、P3）と、残りの`GET`の`/api/admin/`（地名検索`geocode`はP3）・`/api/ai/`・`/api/blog/`（P3・P4）は、**503**と定型のJSON `{"ok":false,"error":"この機能は Rust 版ではまだ使えません"}`（`Cache-Control: no-store`）。現行の「未知のAPI」の403とは区別して、「禁止」ではなく「まだ無い」と分かるようにした。実装するたびに該当のルートをここから外す。
2. 管理画面のページ（§9.5）。
3. 静的ファイル（§9.3）。許可は`/favicon.ico`と5つの接頭辞だけで、**それ以外はすべて403**（404ではない。`/nope`・`/api/nope`・`/api/v1/photos/`も403）。

`HEAD`も`GET`と同じ判定を通る（ホワイトリストを通し、本文だけ省く。`Content-Length`は`GET`と同じ）。`HEAD /api/v1/photos`は現行どおり403（axumの`get`は`HEAD`を自動で受けてしまうので、明示して`site`へ回している）。`/healthz`は新設のルートなので`HEAD`にも200を返す。

### 9.2 Host検査（DNSリバインディング対策）

全ルート（`/healthz`・`/api/v1/photos`を含む）の前に適用する。規則は現行の`_host_ok`と同じ。

- **ホスト部がIPリテラルなら無条件に許可**（IPv4、`[::1]`、ブラケットなしのIPv6、ゾーンID付き`fe80::1%en0`・`[fe80::1%25en0]`、`::ffff:192.0.2.7`）。先頭にゼロのある`010.0.0.1`や`999.1.1.1`、`1.2.3`はIPではなく「名前」として扱われ、許可リストに無いので403（Pythonの`ipaddress`も拒否する）。
- **ホスト名**は`localhost`と`GEOFRAME_ALLOWED_HOSTS`だけ（小文字、末尾の`.`を除く）。
- **ポート**は待ち受けポートの**文字列と一致**するか、省略（`localhost:`のような空も省略扱い）。`localhost:0{port}`・`:{port}`（ホスト名が空）は403。`--listen`で待ち受けを変えたときは、そのポートが基準になる。
- **`Host`ヘッダが2つ以上あれば400**（本文はPythonの400と同じ形。Hostの値は載せない）。どれで判定するかが実装ごとに違う形で、先頭だけを見ると`Host: localhost`を先に置いてすり抜けられる。**差**: Pythonは先頭の値だけで判定して通す。同じ値の繰り返しも400。
- **Hostヘッダが無い・空は許可**（HTTP/1.0の素の要求・curl・監視を壊さない。ブラウザは必ず送る）。ヘルスの確認を`curl http://127.0.0.1:{port}/healthz`で行えるのはこのため（IPリテラル宛も許可）。
- 不許可は**403**。本文はPythonと同じHTMLのエラーページ（`Message: Forbidden host.`と、`GEOFRAME_ALLOWED_HOSTS`への案内。475バイトでPythonと一致）。ステータス行の理由句はhyperの標準（`Forbidden`）で、Pythonの`Forbidden host`にはならない。
- 起動時に1行ログを出す: `Host 検査: 名前は localhost を含む N 件のみ許可、IP リテラルは全て許可、ポートは {port} か省略のみ …`。**差**: Pythonは許可する名前そのものと、この機体のIPを出すが、Rustは**件数だけ**（§3.2の方針。名前は運用者が環境ファイルで確かめられる）。機体のIPの検出もしない（IPリテラルは無条件に許可なので、判定に使わない）。
- 拒否したときは`Host ヘッダが許可されていないため拒否しました…`の1行をwarnで出す。**Hostの値は出さない**（Pythonは出す）。理由の追跡は、起動ログの案内と状態コード403で足りる。

### 9.3 静的配信

#### 判定（`webpath.rs`）

判定は必ず**デコードしたあとの正規化済みパス**で行う。手順はPythonの`_norm_path`と同じ: クエリ・フラグメントを落とす → 先頭の`//`を1つに（`http.server`の癖。`//admin`・`//derivatives/…`は200）→ `%XX`を戻す（壊れた`%`はそのまま、不正なUTF-8はU+FFFD）→ `posixpath.normpath`（`.`と`..`を解決、連続・末尾のスラッシュを落とす。ルートより上へは出られない）。

- **ホワイトリスト**: `/favicon.ico`と接頭辞`/static/`・`/vendor/`・`/derivatives/`・`/photos/`・`/blog_exports/`の内側だけ。大文字小文字は区別する。ディレクトリ自体（`/derivatives`）は接頭辞に当たらず403。
- **ドット始まり**: パスの**どの要素**がドットで始まっても403（`/derivatives/.dl_<drive_id>`＝Drive原本の一時ファイル、`/blog_exports/.<trip>-<temp>/`＝書き出しの作業領域）。**実在するかを見る前に**判定するので、無いファイルも同じ403で、存在の有無が応答から漏れない。`%2e`でエンコードしても同じ。名前の途中にドットを含むだけ（`a.b.jpg`）は通る。
- **トラバーサル**: `..`・`%2e%2e`・`%2f`・二重スラッシュ・絶対パス・二重エンコード（`%252e`）は、正規化したあとにホワイトリストの内側へ残るか、外へ出て403になるかのどちらか。`/derivatives/sub/../a.jpg`のように内側に残るものは現行どおり200。デコード後に**ちょうど`//`で始まる**もの（`/%2fderivatives/…`）はホワイトリスト外で403（`normpath`が`//`だけは残す癖を移してある）。
- **NULとバックスラッシュ**: デコード後に`\0`または`\`を含む要求は**403**。Pythonも`\0`は403。バックスラッシュは、Pythonは`/`に読み替えて判定したうえで実ファイルを探し404になる（リテラルな名前のファイルがあれば200）が、Rustは区切りの解釈を持たず最初から拒否する（`..\..\etc\passwd`のような形を一律に塞ぐ）。
- 先頭が`/`でない要求（`GET derivatives/a.jpg`）: hyperが先に400にする。Pythonは200で配った。実クライアントは送らない形。
- 絶対形式（`GET http://host/…`）: 静的配信の中ではなく、**全ルート共通の検査（`guard`、§9.1の4）で403**。`Host: localhost`を付けた`GET http://evil.example/healthz`・`/api/v1/photos`も403で、`HEAD`・`POST`も同じ。

#### 実体の解決（`statics.rs`）

`/static/`・`/vendor/`は埋め込み（§9.4）。`/derivatives/`・`/photos/`・`/blog_exports/`は`GEOFRAME_DATA_DIR`配下（`derivatives`・`photos`・`blog_exports`）から配る。

1. **データ置き場は起動時に`canonicalize`**して`AppState.data_root`に持つ（T1.1レビューからの申し送り。Pythonは`resolve()`でデータ置き場のシンボリックリンクを辿る）。解決した先が元と違えば起動ログに出す。
2. 要求ごとに、領域のディレクトリ（`<data_root>/<領域>`）と**要求されたファイルの両方をリンクを辿って解決**し、**ファイルの解決結果が領域のディレクトリの解決結果の内側**でなければ**403**（警告ログに領域のテンプレートだけ。ファイル名は出さない）。比べる先をデータ置き場全体ではなく**領域のディレクトリ**にしたのは、`derivatives/x.jpg -> ../catalog.db`のような「データ置き場の中だが領域の外」を指すリンクも塞ぐため。
   - **差**: Pythonは外を指すリンクも辿って配信する（`derivatives/x -> /etc/passwd`が200）。本番に外を指すリンクは無い前提なので、互換への影響はない。`derivatives/`を`photos/`へのリンクで作るような構成はできない（P2で`photos/`と`derivatives/`の関係を決めるときに見直す）。
   - 領域のディレクトリ自体がリンクなのは許す（別のディスクへ向ける運用の設定。Pythonも辿る）。その中のリンクは解決後の領域の内側でなければ403。
   - 解決できない（無い・循環・権限）は404（Pythonは`open`の失敗を一律404にする）。
3. 解決後のパスがディレクトリなら**403**（一覧は出さない。Pythonも403）。通常のファイル以外（FIFO・デバイス）は404（FIFOを開いて、読み手が来るまで固まらないように）。ファイルに末尾の`/`が付いた要求（`/derivatives/a.jpg/`・`…%2F`）は404（Pythonと同じ）。
4. 解決後のパスを開き、**開いたハンドル**のサイズと更新時刻で`Content-Length`・`Last-Modified`を決め、64KiBずつ流す（大きな原本を丸ごとメモリに載せない）。
5. `canonicalize`から`open`までの間にリンクを差し替えられる競合は塞いでいない。差し替えるにはデータ置き場への書き込み権限が要り、その権限があれば直接読める。`User=geoframe`（§6）で権限を分けていることが前提。

#### 上限（遅い・読まないクライアント対策、`limits.rs`）

認証の無いLAN内サーバーなので、読み取らないクライアントがソケットとファイルを握り続けて、`/healthz`・`/api/v1/photos`まで止めないようにする。次の4つを入れた（値は定数で、設定では変えない）。

| 上限 | 値 | 働き |
| --- | --- | --- |
| 同時に配るデータ置き場のファイル数 | 32 | ファイルを**開く前**に枠（セマフォ）を取り、応答の本文が捨てられる（最後まで流れた・切断・停滞で切られた）まで持つ。空きが無ければ**待たずに503**（定型の本文`Service Unavailable`、`Retry-After: 5`。`HEAD`も同じ）。埋め込みの`/static/`・`/vendor/`・ページ、304、404、403は記述子を握らないので枠を使わない |
| 送信の停滞 | 30秒 | 書き込み・flushが進まないまま30秒で接続を切る（進めば測り直す）。読まない相手を待つとき、hyperは本文を読みに来ない（ソケットの書き込み待ち）ので、本文側では測れず、hyperの下（`LimitedIo`）で測る。切られると本文が捨てられ、枠が戻る |
| 無通信 | 60秒 | 読み取りが60秒来ず、その間に書き込みの進捗も無い接続を切る。keep-aliveの待機と、ヘッダを少しずつしか送らない相手を切る。大きなファイルを遅い相手へ送っている最中（書き込みは進んでいる）は切らない。DBの順番待ち（最大10秒）より長い |
| 同時の接続数 | 256 | 超えたぶんは`accept`せず、OSの待ち行列で待たせる。枠は接続が閉じるまで持つ |

- 記述子の予算: 256（接続）＋32（ファイル）＋SQLite（読み取り5接続と`-wal`/`-shm`）＋標準入出力で300前後。`LimitNOFILE=1024`に収まる（§6）。
- 限界: ヘッダを60秒に1バイトずつ送るような相手は無通信では切れず、接続の枠（256）を使い切れば他の接続が待たされる。LAN内の無認証サーバーという前提で、リバースプロキシなしの範囲では接続数の上限で被害を限る（hyperのヘッダ読み取りのタイムアウトは`axum::serve`から設定できず、自前のacceptループは持ち込まなかった）。

#### 応答

- `Content-Type`は拡張子の**固定の表**（`.jpg`→`image/jpeg`、`.png`、`.heic`、`.html`→`text/html`、`.css`、`.js`→`text/javascript`、`.woff2`→`font/woff2`、`.md`→`text/markdown`ほか。表に無いものは`application/octet-stream`）。Pythonと同じく、テキストにも`charset`は付けない。Pythonは機体の`/etc/mime.types`に左右されるが、Rustは固定。
- `Last-Modified`（`httpdate`、秒未満は切り捨て）と`If-Modified-Since`→**304**。Pythonと同じ規則: 更新が日付以前なら304、読めない日付は無視、`If-None-Match`があれば日付は見ない（Rustは`ETag`を出さないので毎回200）。304は`Last-Modified`も付けず、本文なし。`HEAD`でも304。
- `Range`は扱わない（Pythonも同じ。常に200で全体を返す）。`Accept-Ranges`も付けない。

### 9.4 静的ファイルの埋め込み

`app/static`・`app/vendor`（14ファイル、約936 KiB）を、`crates/geoframe/build.rs`が`include_bytes!`の表（`$OUT_DIR/embedded_files.rs`）にしてバイナリへ入れる。実行時に`app/`は読まない（plan.md §1）。`rust-embed`・`include_dir`などの外部crateは足さず、依存は増やしていない（`httpdate`・`futures-util`は`hyper`/`axum`が既に引いている純Rustのcrate）。

- ディレクトリ配下の追加・削除・変更で再生成される（`cargo:rerun-if-changed`）。ドット始まりの名前は含めない（配信側でも403）。
- パスは`app/`からの相対（`static/js/admin.js`）。ディレクトリ（`static/js`）は403、無ければ404。
- **`Last-Modified`は出さず、内容のハッシュ（FNV-1a 64bit＋長さ）の`ETag`と`If-None-Match`→304で再検証させる**（`*`・カンマ区切り・`W/`に対応）。**差**: Pythonは`Last-Modified`。ファイルの時刻はチェックアウトやビルドの時刻で決まり、内容が変わったのに古い時刻で304になる（`handlers.py`のコメントにある、常時起動のブラウザが旧JSを使い続けた事故）おそれがあるため、時刻に依存しない方式にした。`If-Modified-Since`だけの再検証は常に200で全文を返す。
- `Cache-Control: no-cache`と組み合わせるので、ブラウザは毎回再検証し、変わっていなければ304（本文なし）になる。
- テストが、`app/static`・`app/vendor`の全ファイルを、リポジトリの実ファイルとバイト単位で比べる（`every_embedded_file_is_served_byte_for_byte`）。

### 9.5 管理画面のページとヘッダ

- ページ: 正規化後の**完全一致**だけを埋め込みのHTMLに読み替える（接頭辞一致にしない。`/admin/../.env`のようなパスを通さないため）。`/`・`/admin`（`/admin/`・`//admin`・`/admin?x=1`・`/%61dmin`も同じ）→`static/admin.html`、`/blog-admin`→`static/blog-admin.html`、`/ai-search`→`static/ai-search.html`。**`/blog`というルートは現行に無い**（403。旅行ブログの画面は`/blog-admin`）ので、そのまま移した。
- `Cache-Control: no-cache`: HTML（`/`と`.html`）と、`/static/`・`/vendor/`配下（ディレクトリ自体への要求の403も含む）。キャッシュはするが毎回再検証させる。`/derivatives/`・`/photos/`・`/blog_exports/`の画像などには付けない。APIは`no-store`（`/api/v1/photos`・`/healthz`）。
- HTMLには`X-Frame-Options: DENY`と`Content-Security-Policy: frame-ancestors 'none'`（透明なiframeに重ねて誘導クリックさせる攻撃への対策）。
- ヘッダは403・404・304にも付く（Pythonの`end_headers`と同じ）。
- **差**: 判定は**デコード・正規化したあとのパス**（ページの読み替え後）で行い、`.htm`もHTMLとして扱う。Pythonは生のパスの`endswith(".html")`で見るため、`/static/admin%2Ehtml`のようにエンコードすると、同じHTMLに`X-Frame-Options`も`no-cache`も付かなかった（クリックジャッキング対策の抜け）。

### 9.6 ログ

§3の方針のまま。アクセスログの**ルートのテンプレート**に、静的ファイルは`/derivatives/{*path}`・`/photos/{*path}`・`/blog_exports/{*path}`・`/static/{*path}`・`/vendor/{*path}`・`/favicon.ico`、ページは`/`・`/admin`・`/blog-admin`・`/ai-search`、未実装は`/tiles/{*path}`・`/api/admin/{*path}`など、を出す。**実際のパスは出さない**（derivativesのファイル名にはDrive IDが入る）。ホワイトリスト外・トラバーサルで領域の外へ出た要求は`(unmatched)`。ドット始まりなどホワイトリストの内側で拒否したものは、領域のテンプレートで出す。テストは`access_log.rs`。

### 9.7 Pythonとの差（まとめ）

意図した差:

| 項目 | Python | Rust | 理由 |
| --- | --- | --- | --- |
| 領域の外を指すシンボリックリンク | 辿って200 | 解決後の領域の外なら403 | データ置き場の外・`catalog.db`が配信されないように（§9.3） |
| 埋め込みファイルの再検証 | `Last-Modified`/`If-Modified-Since` | `ETag`/`If-None-Match` | 時刻に依存して古いJSが居座る事故を避ける（§9.4） |
| バックスラッシュを含む要求 | 区切りに読み替えて判定、実ファイルは404 | 403 | 区切りの解釈を持たない |
| HTMLヘッダ・`no-cache`の判定 | 生のパス | デコード・正規化後のパス、`.htm`も | `%2E`で付かなくなる抜けを塞ぐ（§9.5） |
| 未実装のルート（`/tiles/`、GETの`/api/admin/geocode`など`status`・`photos`以外の`/api/admin/`、`/api/ai/`、`/api/blog/`） | 応答する | 503と定型のJSON | P3・P4で実装する（`status`・`photos`はT1.4で実装、§10） |
| `POST` | 既知のルートはContent-Type不一致でJSONの403、それ以外はHTMLの403 | 全部HTMLの403 | P3まで更新系が無い |
| `/healthz` | 無い（403） | 200/503（Host検査あり） | 新設（§4） |
| 起動ログ・拒否ログ | 許可する名前、機体のIP、拒否したHostの値を出す | 件数だけ。Hostの値は出さない | §3.2の方針 |
| 通常のファイル以外（FIFOなど） | 開こうとする（読み手が来るまで固まりうる） | 404 | 固まらないように |
| 領域をまたぐリンク（`derivatives/x -> ../photos/y`） | 辿って200 | 403 | 領域ごとに内側を確かめる |
| Content-Typeの表 | 機体の`/etc/mime.types`に左右される | 固定の表 | 配備先で変わらないように |
| 理由句 | `Forbidden host`、`File not found` | `Forbidden`、`Not Found`（hyper標準。本文の`Message:`行はPythonと同じ） | |
| `Server`ヘッダ、HTTPのバージョン | `SimpleHTTP/0.6 Python/…`、HTTP/1.0 | 付けない、HTTP/1.1 | |
| 304の`Content-Length` | 付かない | `0` | |
| 先頭が`/`でないtarget | 200 | 400 | hyperが先に判定する。実クライアントは送らない |
| 絶対形式のtarget（`GET http://host/…`） | 403（ホワイトリストに当たらない） | 全ルートで403（`/healthz`・`/api/v1/photos`も） | 同じ。ルートの種類によらず`guard`で拒否する |
| `Host`ヘッダが複数 | 先頭の値で判定（すり抜けうる） | 400 | 順序でHost検査をすり抜けさせない |
| 読まないクライアント・同時の配信 | 1接続1スレッド（`ThreadingHTTPServer`）で、数・時間の上限なし | 配信32件・接続256件の上限、送信30秒の停滞と無通信60秒で切る。枠が無ければ503 | §9.3 |

そのまま同じもの: ホワイトリストの5つの接頭辞と`/favicon.ico`（実ファイルが無く404）、ドット始まりの403、ディレクトリの403（一覧なし）、ファイルに`/`を付けると404、`%2f`は区切り、`//`の扱い、Host検査の全規則、`HEAD`の判定、`OPTIONS`の501とCORSヘッダなし、`HEAD /api/...`の403、エラーページの本文（403は349バイト、404は335バイト）、`Cache-Control`/`X-Frame-Options`/CSPの付く条件、`Last-Modified`と`If-Modified-Since`の規則（データ置き場のファイル）。

### 9.8 テスト

合成データだけ（`TempDir`に作る）。実データ・実在の位置は使わない。

- `webpath.rs`・`hostcheck.rs`・`statics.rs`の単体テスト: 正規化（Pythonの`normpath`の癖を含む）、ホワイトリストとドット、ページの読み替え、Hostの全規則（現行Pythonで確かめた表）、`Content-Type`、エラーページの本文、`If-Modified-Since`/`If-None-Match`。
- `tests/static_files.rs`（結合）: 配信とヘッダ、`HEAD`、304、埋め込み全ファイルのバイト一致、ページ、ホワイトリスト、ドット、ディレクトリと404、トラバーサル（`..`・`%2e%2e`・`%2f`・バックスラッシュ・NUL・二重スラッシュ・絶対パス・二重エンコード・不正なUTF-8）、シンボリックリンク（外を指すファイル・外を指すディレクトリ・`catalog.db`を指す・領域の内側を指す・壊れた・循環・データ置き場自体がリンク・領域のディレクトリ自体がリンク）、FIFO、Host検査（許可と拒否の表を全ルートで）、`POST`/`OPTIONS`/未実装のメソッドとCORSヘッダなし、未実装ルートの503、大きなファイルの流し、`Host`の重複（両方の順序）と絶対形式のtargetを全ルート・全メソッドで。
- `tests/slow_clients.rs`（遅いクライアント。128MiBの穴あきファイルを読まずに握る）: 枠が埋まると静的は定型の503（`GET`/`HEAD`）で、`/healthz`・`/api/v1/photos`・埋め込み・404は止まらない／握りを閉じると枠が戻る／読まない相手を停滞の上限で切って枠が戻る／遅くても読み続ける相手は切らない／無通信の接続を切り、接続数の上限の間は`accept`されない。
- `tests/access_log.rs`: 引数付きのルートでも、ログに出るのは実際のパスではなくテンプレートであること（`secret-name`・Drive IDらしい値・Hostの値などが出ない）。
- 手元で、現行Python（合成データ）と同じリクエスト131件を流して差分を確かめた（意図した差は§9.7の表のとおり）。

## 10. 管理画面の読み取りAPI `GET /api/admin/status`・`GET /api/admin/photos`（T1.4）

実装は`admin.rs`（DBの読み取り、ディスク）、`pyquery.rs`（クエリの解釈）、`sysinfo.rs`（`statvfs`と現地時刻）、`server.rs`（ルートとキャッシュ）。現行Pythonの`admin.py:api_admin_status`・`api_admin_photos`、`loc_suggest.py:loc_status`、`handlers.py`のクエリの扱いを、同じ入力で出力を比べながら移した。棚卸しの[E2・E3](inventory.md)。管理画面（`admin.js`）が読むフィールドは、`status`の`sync`・`db.*`・`disk.*`・`server_time`と、一覧の`items[]`・`page`・`pages`・`total`で、**すべてPythonと同じ値で出す**。

### 10.1 ルートとクエリ

- **生のパスの前方一致**（現行の`self.path.startswith(...)`）。`GET`で、生のパスが`/api/admin/status`または`/api/admin/photos`で始まれば当たる（`/api/admin/statusX`・`/api/admin/photos/`・クエリ付きも）。先頭の`//`は現行の`http.server`と同じく`/`に畳んでから判定する（`//api/admin/status`も当たる。T1.4で`/tiles/`・`/api/ai/`・`/api/blog/`・`/api/v1/photos`の判定にも同じ畳み込みを入れた。axumのルートは完全一致なので`//api/v1/photos`は`site`で受ける）。`%73tatus`のようなエンコードと大文字（`/API/...`）は当たらない（`/api/admin/`で始まるものは未実装の503、そうでないものはホワイトリスト外の403）。`HEAD`は現行どおり403、`POST`は全部403。
- `status`はクエリを見ない。`photos`は`page`（既定1）・`per`（既定50）・`filter`（既定`all`）。
  - **`parse_qs`と同じ**: `&`で区切り（`;`は区切りでない）、`+`は空白、`%XX`を戻す（壊れた`%`はそのまま、不正なUTF-8はU+FFFD）、**値が空の項目と`=`の無い項目は無かったことになる**（`page=`は既定の1。400にならない）、同じ名前は**最初の値**、`#`以降は落とす。
  - **`int()`と同じ**: 前後の空白（ASCIIは` \t\n\v\f\r`、ASCII以外はUnicodeの空白。`\x1c`〜`\x1f`は落とさない）、`+`/`-`の符号、数字の間に1つだけの`_`（`1_0`は10、`1__0`・`_1`・`1_`は不可）、**全角など任意のUnicodeの10進数字**（`pyquery.rs`にNd表を持つ）、4300桁を超えるとエラー。桁あふれは`i64`に飽和させる（使い道は丸めだけで、結果は同じ: `per=99999999999999999999`は200、`page=99999999999999999999`は最後のページ）。`page`と`per`のどちらかが通らなければ**400** `{"ok":false,"error":"bad_params"}`（値は応答にもログにも出さない。`filter`は検証しない）。
  - 丸め: `per`を1〜200に → 総ページ数`max(1, ceil(total/per))` → `page`を1〜総ページ数に。応答の`page`・`per`は**丸めたあとの値**、`filter`は**解決後の名前**（未知の値・大文字小文字違い・空は`all`）。受け取った文字列は返さない。
- 差: **Pythonの`int()`と違うのは、Pythonのバージョン差（Unicode表）だけ**。Nd表はPython 3.13（Unicode 15.1）から作った。他のバージョンの`unicodedata`と差が出るのは、新しい文字体系の数字が追加された場合に限る。

### 10.2 `GET /api/admin/photos`

`SELECT`・並び（`taken_at`がNULLか空文字なら最後、そのあと`taken_at`、`id`。`id`まで固定なので同順位の揺れは無い。NULLは空文字より先）・件数・値は現行と同じ。

- 項目: `id name thumb taken_at lat lon loc_source infer_dt_s infer_bucket place pref city fav hidden rotation source`。`taken_at`・`place`・`pref`・`city`・`loc_source`はNULLを空文字に（`or ""`）、`fav`・`hidden`は`bool()`の真偽（0・NULL・空文字は偽）、`rotation`は偽なら`0`、`lat`・`lon`・`infer_dt_s`・`name`・`source`は列の値のまま（型も同じ。REAL列は`35.0`のように小数で出る）。
- `thumb`: `display`があれば`"/" + display`。無ければ、`drive_id`があって`<データ置き場>/derivatives/<drive_id>.jpg`が**実在**すれば`/derivatives/<drive_id>.jpg`（過去に作ったderivativeを拾う。Driveへ取りに行かない）。それ以外は空文字。
- `infer_bucket`: `loc_source='inferred'`の行だけ。`infer_dt_s`が段階窓（既定15分・2時間・12時間。`GEOFRAME_INFER_MIN`で1つ目が変わる）のどれに入るか（`<=`）で`±15分`・`±2時間`・`±12時間`、超えれば`範囲外`、NULLは空文字（`status`の内訳では`未計測`）。1時間未満は分、以上は時間で、端数は切り捨て。
- `hint`（`filter`が`noloc`・`suggest`のとき）: 位置あり（`loc_source`が`exif`/`drive_meta`）で撮影時刻が読める写真を「錨」とし、各行の撮影時刻に**最も近い錨**（同じ差なら時刻の早いほう。同じ時刻の錨が複数あるときはSQLiteの走査順に依存するのはPythonと同じ）の`{label, lat, lon, taken_at, dt_s}`。差が7日を超える・自分の時刻が読めない・時刻なしは`null`。**錨が1枚も無ければ`hint`のキー自体を付けない**（`null`と区別がある）。`label`は都道府県・市区町村・町名の空でないものを空白でつなぐ。
- `suggest`: 位置なしの行で、`photo_loc_suggestions`に行があれば付く（`place_name day_place_name scope precision confidence evidence lat lon geocode_title model`）。`evidence`は`evidence_json`をJSONとして読み、**配列でなければ（壊れている・オブジェクトなど）`[]`**。
- 時刻の読み方は`photos.rs:parse_epoch`（§8.2）。**既知の差**（§8.2と同じ）: 全角数字の時刻は読めない扱い。

### 10.3 `GET /api/admin/status`

`ok`・`sync{ready,pending,total}`・`db{...}`・`disk{...}`・`server_time`。**Pythonがバックグラウンドで計算して持つ値は無く、すべてDBとファイルシステムから読み取りだけで出せる**（Rustが持たない値はない）。決めごと:

| 項目 | Rust |
| --- | --- |
| `sync`・`db`の件数、`by_loc_source`・`by_source` | 同じSQL。`by_loc_source`のキーはNULLを`""`に寄せる。NULLと空文字の両方があると、後から数えた空文字の件数が残る（Pythonの辞書と同じ） |
| `db.infer_buckets` | `loc_source='inferred'`の行を窓のラベルで数える（NULLは`未計測`） |
| `db.quality`・`db.ai` | 同じSQL。AIの`embedded`・`detailed`は`embedding_model`が**今のモデル**（`GEOFRAME_EMBED_BACKEND`。§1.1）の行だけ。`pending=max(0, eligible-embedded)` |
| `db.loc` | `loc_status()`の移植。位置なし・撮影時刻ありの写真（`targets`）のうち、`photo_loc_clues`の判定が**今の画像**（表示ファイル、`md5`、`mtime`が一致）のものを判定済みとして`clue_pending`/`clue_passed`を数える。通る条件は、エラーが空、`clue`がNULLでなく、`clue>=0.2`または`kind='landmark'`。`days`は、撮影時刻順に「最大の窓以上の空き」か「日付の変わり目」で区切ったまとまりの数、`days_with_clue`は通る写真を含むまとまりの数。`runs_done`・`runs_error`は`photo_loc_runs`、`suggested`は位置なしの写真にある候補の数。`decision`・`last_at`は`settings.photo_loc_status`（Pythonのワーカーが前回の処理時に書くJSON）の`decision`・`at`で、**値の型のまま**出す。**Rustは Decision に問い合わせず、この値を書き換えない**。無い・壊れている・オブジェクトでないときは空文字（Pythonはオブジェクトでないと500） |
| `disk` | §10.4 |
| `server_time` | 現地時刻の`YYYY-MM-DD HH:MM:SS` |

- 表示ファイルが無い写真の`rel`（判定との照合に使う）は、`thumb`と同じく`derivatives/<drive_id>.jpg`が実在すればそれ。
- 件数は**1つの読み取りトランザクション**の中で数える（Pythonは件数ごとにロックを取り直すので、書き込みが挟まると合計がずれうる）。`photos`も件数と行を同じトランザクションで読む。
- **空のテーブルでも動く**（件数は0、内訳は`{}`、`decision`・`last_at`は空文字）。管理画面は`d.db.ai`・`d.db.loc`・`d.db.quality`が無ければ項目を飛ばすが、Rustは常に出す（Pythonも常に出す）。

### 10.4 ディスク使用量と現地時刻

- `disk`は`statvfs`（Pythonの`shutil.disk_usage`と同じ式: `total=f_blocks*f_frsize`、`used=(f_blocks-f_bfree)*f_frsize`、`free=f_bavail*f_frsize`）と、`derivatives/`・`tile_cache/`の再帰的な走査（`os.walk`と同じ: ディレクトリへのシンボリックリンクは辿らず数えない、ファイルへのリンクは先の大きさ、壊れたリンクと走査中に消えたものは無視、無いディレクトリは`{files:0,bytes:0,exists:false}`）。`used_pct`は小数1桁（`round(x,1)`と同じ偶数丸め。総量が0なら整数の`0`）。`tile_cache_max_mb`は`GEOFRAME_TILE_CACHE_MAX_MB`。
- **60秒キャッシュ**（Pythonと同じ）。全走査になるので、`spawn_blocking`で行い（DBの接続は握らない）、キャッシュを持つロックを走査の間も持つので、同時に来ても走査は1回。`admin.js`は30秒ごとにこのAPIを引く。測定に失敗したとき（データ置き場が無いなど）はキャッシュせず**500**と定型の本文（Pythonも`OSError`で500）。
- `statvfs`と`localtime_r`のために`libc`を直接の依存にした（`tokio`が既に引いているcrateで、Cのビルドは無く、musl静的ビルドを壊さない）。**`unsafe`は`sysinfo.rs`だけ**で、`lib.rs`が`#![deny(unsafe_code)]`にしている。

### 10.5 応答とエラー

- §8.4と同じ: `application/json; charset=utf-8`、`Cache-Control: no-store`、日本語はエスケープしないUTF-8。**キーの並びはPythonと違う**（Rustは`status`をアルファベット順で出す。`photos`の`items[]`は`api_admin_photos`と同じ順）。比較は構文解析後。
- DBを読めない・順番待ち（`QUEUE_TIMEOUT`）が上限を超えたら**503**と定型の本文（`status`は`状態を読み取れません。…`、`photos`は`一覧を読み取れません。…`）。SQLiteのエラー文字列・パスは本文に入れずログ（warn）にだけ出す。不正なクエリは**DBを読む前に400**。JSON化の失敗とディスクの測定失敗は500。
- `photos`は、位置なしの行が一覧にあるとき`photo_loc_suggestions`を引く。この表を読めなければ、その一覧だけ503になる（`fav`など位置ありの行だけの一覧は影響しない）。
- ログは§3のまま。ルートのテンプレートは`/api/admin/status`・`/api/admin/photos`（`photosX`などの前方一致も同じ）。クエリの値・写真の値は出さない（テスト`access_log.rs`）。

### 10.6 Pythonとの差（まとめ）

意図した差:

| 項目 | Python | Rust | 理由 |
| --- | --- | --- | --- |
| 件数の一貫性 | 件数ごとにロックを取り直す | 1つの読み取りトランザクション | 書き込みが挟まっても合計がずれない |
| `settings.photo_loc_status`がオブジェクトでない | 500 | 空として扱う（200） | 読み取り専用の表示で落とさない |
| 型が想定外の列（`name`がBLOBなど） | 素通し／例外 | 読み取り全体が503 | スキーマ上は起きない |
| 表示ファイルが絶対パス（`drive_id`が`/`で始まる）の`rel` | 500（`relative_to`の`ValueError`） | 空文字 | 起きない |
| 設定 | `GEOFRAME_INFER_MIN`などが不正だと起動できない（例外） | `INFER_MIN`・`TILE_CACHE_MAX_MB`は起動を断る（終了コード2）、`EMBED_BACKEND`は`gemini`に落とす | §1.1 |
| キーの並び・区切りの空白 | `", "`・`": "`、挿入順 | 空白なし、`status`はアルファベット順 | 構文解析後に比べる |
| 空・壊れたJSONの扱い（`evidence_json`、`photo_loc_status`） | `json.loads`（`NaN`などを通す） | `serde_json`（通さない） | Pythonの書き手は`json.dumps`で`NaN`を書かない |

残る既知の差（§8.2と同じ）: 全角数字の撮影時刻、`TZ`が夏時間を持つときの時刻の差、時刻の解釈。

### 10.7 テスト

合成データだけ（実在の地名・座標・ファイル名を使わない）。

- `app/tests/admin_api_fixture.py`が合成DB（本物のスキーマ＋30枚の写真・品質・AI・位置候補・Decisionの判定・実行・設定）を作り、**現行のPythonハンドラを実際に起動**して、`/api/admin/photos`の80通り超のクエリ（各filter、ページ境界、`per`の上限、`parse_qs`と`int()`の癖、400、前方一致）と、`status`（既定・`local`・設定なし・壊れた設定・`GEOFRAME_INFER_MIN`違い・空のテーブル）の出力を`crates/geoframe/tests/fixtures/admin-api-python.json`に書く。DBは`admin-api-catalog.sql`。再生成と検査は`python3 tools/gen_admin_fixture.py`（`--check`）。Pythonのテスト（`test_admin_api_fixture.py`）が、ファイルが最新であることと、フィクスチャが押さえている癖を確かめる（CIで古いフィクスチャが落ちる）。
- `crates/geoframe/tests/admin_api.rs`: 同じDBを作り直してRustのサーバーに同じリクエストを流し、`serde_json::Value`として完全一致を確かめる。比べない値は`disk`と`server_time`だけで、形・数え方・キャッシュを別に検査する。そのほか、`per`の上限（205枚）、並び（NULL・空文字・同時刻はid）、ヘッダ、`HEAD`/`POST`の403、未実装の503、DBが壊れたとき・順番待ちが超えたときの503、`status`の`disk`の数え方（リンク）。
- `pyquery.rs`の単体テストの期待値は、Pythonの`parse_qs`・`int()`で確かめたもの（開発時に、ランダムな入力5万件で突き合わせて差が無いことを確認した。コミットしていない）。
- 手元での確認: musl静的ビルドの`file`は`static-pie linked`、`ldd`は`statically linked`のまま。
