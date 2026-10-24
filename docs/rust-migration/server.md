# Rustサーバーの基盤: 設定・DB読み取り・ログ・ヘルス（P1 / T1.1）

- 状態: T1.1 実装済み（2026-10-24）
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
- 接続は最大4本（`READ_CONNECTIONS`）。起動時に1本だけ開き、残りは必要になったときに開く。同時に走る読み取りは4が上限で、順番待ちは**非同期のセマフォ**で行う（待っている間もworkerは塞がらない）。
- 呼び出し側が先にFutureを捨てても（タイムアウトなど）、始まった読み取りは最後まで走って接続を戻す。読み取りがパニックしたときはその接続を捨て、次の読み取りが開き直す。
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
- 起動・停止、待ち受けアドレス、データ置き場のパス、`Config`の要約（待ち受け・データ置き場・許可Host名の件数・ログの詳しさ・`TZ`）。
- スキーマ不足の**列名**、件数、状態名、写真ID。
- SQLiteなどが返したエラーメッセージ（**ログにだけ**。HTTP応答には載せない）。

### 3.2 出してはいけないもの

[inventory.md §6.3](inventory.md)の方針を引き継ぐ。

- 写真の内容、EXIF・GPS、緯度経度、地名。
- ファイル名・パス（Drive IDを含む）。リクエストの実際のパスとクエリ（検索語やファイル名が入りうる）。
- 秘密情報: 鍵ファイルの中身、トークン、`Authorization`などのヘッダ値。
- 接続元アドレス、`Host`ヘッダの値、許可Host名の中身。

テスト`access_log.rs`が、緯度経度らしいクエリ・ファイル名・Drive IDらしい値・接続元アドレス・`Host`値がログに出ないことを検査する（tracingは呼び出し箇所ごとに購読者の有無を最初に覚えるため、単独のテストファイルにしてある）。新しいログを足すときは、3.1に当てはまるかを先に確かめる。

### 3.3 応答にも載せない

500などの応答は定型文にし、例外・SQLiteのエラー文字列・パスを本文に入れない（理由はログにだけ出す）。

## 4. ヘルス確認

`GET /healthz`（Pythonには専用のヘルスが無かったため、新設の仕様）。

| 状況 | 状態 | 本文 |
| --- | --- | --- |
| プロセスが動いていて、`photos`を1行読める（空のテーブルでも可） | 200 | `{"ok":true}` |
| DBを読めない、または3秒以内に答えが返らない | 503 | `{"ok":false}` |

- 本文は`ok`だけ。件数・パス・エラーの理由は載せない（理由はwarnログ）。`Cache-Control: no-store`を付ける。
- 起動時のスキーマ検査は通っているので、ここで見るのは「その後も読めるか」。
- P0の`{"ok":true}`との互換: 200のときの本文は同じ。

## 5. 起動引数の更新

```
geoframe serve [--listen <addr:port>] [--data-dir <dir>]
```

`--listen`は必須ではなくなった（環境変数と既定値で決まる）。`check-db`は従来どおり、スキーマの全項目を確かめず`photos`の行数だけを出す（古いDBの調査にも使えるように）。

## 6. 確認の記録

- `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（合成した一時DBだけ。実データ・実在の位置は使わない）。
- musl静的ビルド: `static-pie linked`、`ldd`は`statically linked`。追加した依存`tracing`・`tracing-subscriber`は純Rust（C依存なし）。
- 合成DBでの手動確認: `serve`が`/healthz`に200、未知のパスに404を返し、SIGTERMで正常終了（終了コード0）。DBが無いときは終了コード1、`GEOFRAME_PORT`が不正なときは2。
