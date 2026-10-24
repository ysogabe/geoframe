# P1の受入確認（検証環境での記録）

- 日時: 2026-10-24 16:40〜17:10（JST）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）/ 計画: [plan.md](plan.md) §4 P1 / タスク: [tasks.md](tasks.md) T1.5
- 対象のソース: コミット`d641505`（T1.5で`crates/`は変更していないので、T1.5のコミットでビルドしても同じ）
- バイナリ: `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`（`static-pie linked`、6,170,752 B）
- 検証環境（lxc-geoframe。Ubuntu 24.04、2コア、メモリ4GiB）。本番機ではない。接続先の値はGit管理外のメモにあり、ここには書かない
- **写真の値・ファイル名・位置・タイトルは書かない。件数と一致の数だけ**

## 1. 構成

| 項目 | 内容 |
| --- | --- |
| Rust | `geoframe-rs.service`（[`deploy/geoframe-rs.service`](../../deploy/geoframe-rs.service)）を`deploy/rust-deploy.sh`で配布し、`:8080`に常駐（enable済み）。`User=geoframe`、データは`/var/lib/geoframe`（本番2026-10-24 06:17のバックアップのコピー、`catalog.db`は6,535行） |
| 比較用Python | `geoframe-pyref.service`（`:8090`、データ`/var/lib/geoframe-pyref`）。AI・Drive無効。今回、本番と同じ`GEOFRAME_EMBED_BACKEND=local`を足した |
| 環境変数 | 両方とも`GEOFRAME_EMBED_BACKEND=local`（本番の`/etc/default/geoframe`にある、出力に影響する設定はこれだけ。ほかの2項目はタイルの`User-Agent`とHost名で、出力に影響せず、前者には連絡先が入っているので写していない） |
| 比較 | 手元（開発機）から、LAN越しに両方へ同じ要求を送る。[`tools/compare_with_python.py`](../../tools/compare_with_python.py)（標準ライブラリだけ） |

## 2. 全件比較（Rust と 比較用Python）

`tools/compare_with_python.py --rust … --python …`の集計（所要22秒、想定外の差 **0** 項目）。

| 比較 | 結果 |
| --- | --- |
| `GET /api/v1/photos` | 状態コードともに200。**3,065件／3,065件でJSON完全一致（並びも一致）**。`Content-Type`・`Cache-Control`も同じ |
| `GET /api/admin/status`（`server_time`・`disk`を除く） | 完全一致 |
| `GET /api/admin/photos` | 7つのfilter（`all`・`noloc`・`fav`・`hidden`・`inferred`・`manual`・`suggest`）×`per`（1・10・50・200）×`page`（1・2・3・最終・最終+1・0・-1・99999）と、既定値・不正な値・`parse_qs`/`int()`の癖の20通りを足した**244通りで、状態コード・JSONともに244/244一致**（200が241、400が3） |
| `/derivatives/`（一覧の`file`の全件） | **3,065/3,065件でバイト列（sha256・長さ）が一致**（計2,187 MiB）。`Content-Type`も3,065/3,065で同じ |
| 管理画面のページ（`/`・`/admin`・`/admin/`・`//admin`・`/admin?x=1`・`/blog-admin`・`/ai-search`） | 状態コード・本文とも7/7一致 |
| 埋め込みファイル（`app/static`・`app/vendor`の14ファイル） | Rustの本文がリポジトリの実ファイルと**14/14バイト一致**。Pythonとも状態コード・本文が14/14一致 |
| 拒否・404になるべきパス（ドット始まり、`..`・`%2e%2e`・`%2f`・二重エンコード、NUL、ディレクトリ、未知のパスなど33通り） | 状態コード33/33一致（本文も33/33同じ） |
| メソッド（`HEAD`・`POST`・`OPTIONS`・`PUT`・`DELETE`） | 状態コード10/10一致（`HEAD`は`Content-Length`も） |
| Host検査（IPリテラル・名前・ポートの10通り） | 状態コード10/10一致 |
| 既知の差 `/healthz`（Python 403、Rust 200） | 想定どおり1/1（[server.md §9.7](server.md)） |

- **差は無かった。** したがってRust側の修正は無い。
- `Last-Modified`も3,065/3,065で同じだが、これは比較の対象外（参考）。比較用Pythonの`derivatives/`が本番のコピーの`derivatives/`へのハードリンクで、同じファイルを見ているため。
- 一覧の3,065件は、`photos`6,535行のうち、表示条件（位置あり・表示ファイルあり・非表示でない・品質／AIの推奨外を除く）を満たすもの。
- 手順の途中で、比較用Pythonが`/derivatives/`を404にする問題が出た（比較の不具合）。原因は、`/var/lib/geoframe`の`chown`が、ハードリンクで共有している`derivatives/`の持ち主・モードを変えたため（`0660`のファイルを比較用Pythonの実行ユーザーが読めなくなった）。`chmod o+r`で直した。Rustの不具合ではない（[server.md §6](server.md)）。

### 比較で扱っていないもの

- 地図タイル・地名検索・Driveなど外部へ出る要求（`/tiles/`・`/api/admin/geocode`）は、比較用Pythonが外へ取りに行くので送っていない。
- 更新系（`POST`）はP3まで無い（全部403）。
- `taken_at`がNULLの行など、本番のコピーに無いデータの差は[server.md §8.3](server.md)・[§10.6](server.md)の既知の差のとおり。

## 3. 常駐・障害・巻き戻し

すべて検証環境（Rustの`geoframe-rs.service`）で。

| 試験 | 結果 |
| --- | --- |
| `systemctl restart` | 約0.3秒で`/healthz`が200。`NRestarts=0`、`Result=success` |
| `kill -9` | `Result=signal`、`auto-restart`になり、**約5.2秒後（`RestartSec=5`）に`/healthz`が200**、PIDが変わり、`NRestarts=1` |
| 設定不正 `GEOFRAME_PORT=abc` | 終了コード2（`設定が不正です: GEOFRAME_PORT は 0〜65535 の整数で指定してください`。値は出さない）で`failed`。**18秒見て、起動は1回だけ、`NRestarts=0`。再起動ループにならない**（`RestartPreventExitStatus=2`） |
| 設定不正 `GEOFRAME_INFER_MIN=abc` | 同じく終了コード2で`failed`、起動は1回だけ |
| DBが開けない `GEOFRAME_DATA_DIR=/nonexistent` | 終了コード1。**5秒おきに再起動し続ける**（17秒で4回）。これは意図どおり（Pythonより先に起動した場合などに備え、再試行する）。連続起動の制限（既定10秒に5回）には当たらない |
| 復旧 | `/etc/default/geoframe`を元に戻し、`reset-failed`→`restart`で`active`、`/healthz`200 |
| drop-inで`--listen 0.0.0.0:8081`＋`GEOFRAME_PORT=abc` | `--listen`が`GEOFRAME_PORT`より優先されて起動し、`:8081`で200（`:8080`は閉じている）。[deploy/README.md §7.2](../../deploy/README.md)の手順の確認。試験後にdrop-inを消した |
| 起動できないバイナリを配布 | `rust-deploy.sh`が`/healthz`を10秒待って失敗と判定し、**自動で直前のバイナリへ戻して再起動、確認OKで終了コード1**。失敗した方は`geoframe.failed`に残った |
| 手動の巻き戻し `rust-deploy.sh --rollback` | `.prev`のバイナリへ戻して再起動、確認OK、終了コード0。`.prev`が無いときは終了コード1 |
| ハードニングの確認 | `MemoryDenyWriteExecute=true`を付けたまま起動し、全エンドポイントが動いた。`ReadOnlyPaths`は、`geoframe`ユーザーの一時unitで`catalog.db`・`derivatives/`への書き込みが`Read-only file system`になり、ディレクトリには書けること（WALの`-shm`用）を確認。`/healthz`・`/api/v1/photos`は読み取り専用のままで通った |
| 資源 | 常駐のRSS 6.5 MiB、ピークRSS 9.7 MiB（全件比較の後）、スレッド4 |

## 4. miharashiの実プロセスでの確認

miharashi（`~/work/miharashi`、コミット`1bc73a6`）の**本番プロセス（`miharashi.service`）と`/etc/miharashi`には触っていない**。miharashiのリポジトリも変更していない（ビルドの出力先は`/tmp`、設定は一時ファイルの`--config`）。

miharashiには画面なしで動かせる経路が2つある。取得先（`geoframe.url`）を検証環境のRust（`:8080`）へ向けて確かめた。

### 4.1 実バイナリの`png`サブコマンド

`miharashi png --config <一時の設定> --renderer cpu --size 640x360`。実クライアントの`Catalog::open`（`GET /api/v1/photos`）と`current_photo`（写真1枚の取得・デコード・合成）を、GPU・DRMを開かずに動かし、PNGを書く。`--config`を明示するので`/etc/miharashi`を拾わず、`STATE_DIRECTORY`も無いので表示履歴も書かない。

- **60回実行して60回成功**（エラー出力なし）。1回ごとに一覧を取り直し、別の写真を1枚読み込む。
- 一覧から表示対象に選ばれた枚数は**3,029枚**（miharashi自身の品質フィルタで、一覧3,065件のうち36件を除く）。**比較用Python（`:8090`）に向けても同じ3,029枚**。旅は309、暮らしは1（Home Assistantの設定が無いので自宅は不明で、GEOFRAMEの旅番号でまとめる。キオスクの本番はHAの自宅位置を使う）。

### 4.2 `Feed`（一覧の取得と写真の読み込みの別スレッド）

miharashiは`photos.rs`・`album.rs`を持つ`miharashi-app`が実行ファイルだけでライブラリではないため、`/tmp`に使い捨てのプロジェクトを作り、この2つのソースを`#[path]`でそのまま取り込んだ（画面側の型だけ最小の代役。本体の取得・デコード・並べ方・再接続のコードは同じ）。画面を出さずに`Feed::spawn`を動かし、`next()`で写真を送り続けて数えた。

| 試験 | 結果 |
| --- | --- |
| 定常（Rustに向けて200枚） | **200枚を読み込み（デコード・合成まで成功）、旅の区切りのカード16枚**。再接続の発生なし。42秒 |
| 途中でRustを止めて戻す（10秒後に`systemctl stop`、8秒後に`start`。300枚） | 停止中は`Retrying`を4回通知（指数バックオフ）、起動後に`Online`を1回通知して**自動で回復**し、**300枚を読み込み、カード33枚**。最初の再試行から次の写真まで11.1秒。キー操作は不要 |

- **取得できた件数: 3,029枚（一覧3,065件）。読み込めた写真: 4.1で60枚、4.2で500枚。エラー: 取得・デコードの失敗は0。**
- 画面（DRM/KMS）への描画・フェードは確かめていない（このサブコマンド・使い捨てプロジェクトは描画しない）。キオスクの実機で、取得先を切り替えて表示を見るのは、P5の切替のときに行う。
- miharashi本体の契約テスト（[`crates/geoframe/tests/photo_feed_miharashi.rs`](../../crates/geoframe/tests/photo_feed_miharashi.rs)）はT1.2で済み。今回は、それを本物の`Feed`と本物のRustサーバーで確かめたことになる。

## 5. 検証環境に加えた変更（戻せるように）

| 変更 | 戻し方 |
| --- | --- |
| `/var/lib/geoframe`を`geoframe:geoframe`へ`chown -R`（もとの持ち主は検証環境の作業ユーザー）。`derivatives/`の`0660`のファイルに`o+r` | `sudo chown -R <もとの作業ユーザー>: /var/lib/geoframe`。比較用Pythonがハードリンクで共有するので、`o+r`は残してよい |
| `/etc/default/geoframe`を新規作成（`GEOFRAME_EMBED_BACKEND=local`だけ） | 削除 |
| 比較用Pythonへdrop-in`/etc/systemd/system/geoframe-pyref.service.d/override.conf`（`GEOFRAME_EMBED_BACKEND=local`）を追加し、再起動（torchを読み込もうとして落ちることはなかった） | drop-inを削除して`daemon-reload`・`restart` |
| `/usr/local/bin/geoframe`（`.prev`は巻き戻し試験の残り）、`/etc/systemd/system/geoframe-rs.service`（`.prev`）を配置し、`geoframe-rs.service`をenable・起動 | `systemctl disable --now geoframe-rs`、上のファイルを削除 |
| `/tmp`の試験用ファイル（`default-geoframe.orig`など） | 削除 |

比較用Pythonのデータ（`/var/lib/geoframe-pyref`）と旧版の退避（`*.old-202608`）は触っていない（比較用Pythonは自身のデータを起動時に更新する）。

## 6. 結論

P1の受入（[plan.md §4](plan.md)）の、検証環境で確かめられる部分は満たした。

- 比較用Pythonと`/api/v1/photos`が**全件（3,065件）完全一致**、`/derivatives/`が**同じバイト列（3,065/3,065）**。
- 管理画面の読み取りAPI・ページ・埋め込みファイル・拒否の動きも一致。
- systemdで常駐し、再起動・`kill -9`で自動復帰、設定不正で再起動ループにならず、巻き戻しができる。
- miharashiの実コードが、Rustから一覧を取得して写真を読み込み続け、Rustの停止・再開からも回復する。

残り（画面への表示、本番の切替、`catalog.db`を置き換えたときの運用）はP5。
