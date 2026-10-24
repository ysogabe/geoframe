# P1の受入確認（検証環境での記録）

- 日時: 2026-10-24 16:40〜17:10（JST）に実施し、レビュー指摘（Sol）を反映して17:10〜17:25（JST）にやり直し、2回目の指摘を反映して**17:50（JST）に配布と全件比較をやり直した**。以下は、最新の結果
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）/ 計画: [plan.md](plan.md) §4 P1 / タスク: [tasks.md](tasks.md) T1.5
- 対象のソース: コミット`d362f47`の実装（`crates/`の実装部分・`Cargo.*`・`app/static`・`app/vendor`は`d641505`から無変更。2回目の修正で`crates/`に足したのは、`limits.rs`の単体試験と`slow_clients`の統合試験だけ）。配布スクリプト・比較スクリプト・ハーネスは、この記録を含む修正コミット
- バイナリ: `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`（`static-pie linked`、6,170,752 B、sha256 `d5c19e4de35a467b…`）
- 検証環境（lxc-geoframe。Ubuntu 24.04、2コア、メモリ4GiB）。本番機ではない。接続先の値はGit管理外のメモにあり、ここには書かない
- **写真の値・ファイル名・位置・タイトルは書かない。件数と一致の数だけ**

## 1. 構成

| 項目 | 内容 |
| --- | --- |
| Rust | `geoframe-rs.service`（[`deploy/geoframe-rs.service`](../../deploy/geoframe-rs.service)）を**3回目の修正後の**`deploy/rust-deploy.sh`で配り直し（検査・転送・入れ替え・確認OK、終了コード0。バイナリの sha256 は前回と同じ）、`:8080`に常駐（enable済み）。`User=geoframe`、データは`/var/lib/geoframe`（本番2026-10-24 06:17のバックアップのコピー、`catalog.db`は6,535行） |
| 比較用Python | `geoframe-pyref.service`（`:8090`、データ`/var/lib/geoframe-pyref`）。AI・Drive無効。今回、本番と同じ`GEOFRAME_EMBED_BACKEND=local`を足した |
| 環境変数 | 両方とも`GEOFRAME_EMBED_BACKEND=local`（本番の`/etc/default/geoframe`にある、出力に影響する設定はこれだけ。ほかの2項目はタイルの`User-Agent`とHost名で、出力に影響せず、前者には連絡先が入っているので写していない） |
| 比較 | 手元（開発機）から、LAN越しに両方へ同じ要求を送る。[`tools/compare_with_python.py`](../../tools/compare_with_python.py)（標準ライブラリだけ） |

## 2. 全件比較（Rust と 比較用Python）

`tools/compare_with_python.py --rust … --python …`の集計（2回目の修正後の再配布で再実施。所要22秒、想定外の差 **0** 項目。集計は下の表のとおりで前回と変わらない）。レビュー指摘で次を厳密にした: JSON は**型を区別**して比べる（`true`と`1`、`false`と`0`、`35`と`35.0`は別。`--self-test`で、手元の小さなサーバー2つを使って確認）、画像は**両方が200**を返したうえでハッシュ・長さを比べる（同じ404/403/500同士は一致に数えない）、`GET /api/admin/photos`の最終ページは**filterとperの組ごとに**`pages`を取得して作る。

| 比較 | 結果 |
| --- | --- |
| `GET /api/v1/photos` | 状態コードともに200。**3,065件／3,065件でJSON完全一致（型も区別、並びも一致）**。`Content-Type`・`Cache-Control`も同じ |
| `GET /api/admin/status`（`server_time`・`disk`を除く） | 完全一致 |
| `GET /api/admin/photos` | 7つのfilter（`all`・`noloc`・`fav`・`hidden`・`inferred`・`manual`・`suggest`）×`per`（1・10・50・200）の28組それぞれについて、`pages`を取得して`page`（1・2・3・最終・最終+1・0・-1・99999）を作り（最終ページは組ごとに1〜6,535。4ページ以上の組が17）、既定値・不正な値・`parse_qs`/`int()`の癖の20通りを足した重複なしの**222通りで、状態コード・JSONともに222/222一致**（200が219、400が3） |
| `/derivatives/`（一覧の`file`の全件） | **3,065/3,065件で、両方が200を返し、バイト列（sha256・長さ）が一致**（計2,187 MiB）。`Content-Type`も3,065/3,065で同じ |
| 管理画面のページ（`/`・`/admin`・`/admin/`・`//admin`・`/admin?x=1`・`/blog-admin`・`/ai-search`） | 状態コード・本文とも7/7一致 |
| 埋め込みファイル（`app/static`・`app/vendor`の14ファイル） | Rustの本文がリポジトリの実ファイルと**14/14バイト一致**。Pythonとも状態コード・本文が14/14一致 |
| 拒否・404になるべきパス（ドット始まり、`..`・`%2e%2e`・`%2f`・二重エンコード、NUL、ディレクトリ、未知のパスなど33通り） | 状態コード33/33一致（本文も33/33同じ） |
| メソッド（`HEAD`・`POST`・`OPTIONS`・`PUT`・`DELETE`） | 状態コード10/10一致（`HEAD`は`Content-Length`も） |
| Host検査（IPリテラル・名前・ポートの10通り） | 状態コード10/10一致 |
| 既知の差 `/healthz`（Python 403、Rust 200） | 想定どおり1/1（[server.md §9.7](server.md)） |

- **差は無かった。** したがってRust側の修正は無い。厳密にした比較（型の区別、画像の200必須、最終ページ）でも、1回目と同じく差は0（件数は上のとおり）。
- `Last-Modified`も3,065/3,065で同じだが、これは比較の対象外（参考）。比較用Pythonの`derivatives/`が本番のコピーの`derivatives/`へのハードリンクで、同じファイルを見ているため。
- 一覧の3,065件は、`photos`6,535行のうち、表示条件（位置あり・表示ファイルあり・非表示でない・品質／AIの推奨外を除く）を満たすもの。
- 手順の途中で、比較用Pythonが`/derivatives/`を404にする問題が出た（比較の不具合）。原因は、`/var/lib/geoframe`の`chown`が、ハードリンクで共有している`derivatives/`の持ち主・モードを変えたため（`0660`のファイルを比較用Pythonの実行ユーザーが読めなくなった）。`chmod o+r`で直した。Rustの不具合ではない（[server.md §6](server.md)）。

### 比較で扱っていないもの

- 地図タイル・地名検索・Driveなど外部へ出る要求（`/tiles/`・`/api/admin/geocode`）は、比較用Pythonが外へ取りに行くので送っていない。
- 更新系（`POST`）はP3まで無い（全部403）。
- `taken_at`がNULLの行など、本番のコピーに無いデータの差は[server.md §8.3](server.md)・[§10.6](server.md)の既知の差のとおり。Pythonとの差の全体は[server.md §0の索引](server.md)から辿れる。

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
| 手動の巻き戻し `rust-deploy.sh --rollback`（修正後のスクリプトで再実施） | `.prev`のバイナリへ戻して再起動、確認OK、終了コード0。続けてもう一度実行すると`.prev`が無く終了コード1 |
| ハードニングの確認 | `MemoryDenyWriteExecute=true`を付けたまま起動し、全エンドポイントが動いた。`ReadOnlyPaths`は、`geoframe`ユーザーの一時unitで`catalog.db`・`derivatives/`への書き込みが`Read-only file system`になり、ディレクトリには書けること（WALの`-shm`用）を確認。`/healthz`・`/api/v1/photos`は読み取り専用のままで通った |
| 資源 | 常駐のRSS 6.5 MiB、ピークRSS 9.7 MiB（全件比較の後）、スレッド4 |

### 3.1 配布スクリプトの失敗経路（レビュー指摘の反映後）

修正の要点: 入れ替え以降の全工程（unit の導入・`daemon-reload`・`enable`・`restart`・確認）と中断（SIGTERM）を一つの復元処理で覆う。復元の各操作に明示的な成否の判定を付け、失敗したら即座に終了コード3で、手作業の復旧手順を出す（「戻した」とは言わない）。配布前の`enable`・稼働・unit の有無を覚えて、初回の失敗では`disable`と unit の撤去まで行う。変更の前に、検証環境の印`/etc/geoframe/staging`と、Python 版の非稼働を確かめる。

2回目の修正（レビュー指摘）: ①復元の最中は SIGHUP・SIGINT・SIGTERM・SIGPIPE を無視し（子プロセスにも引き継がれる）、復元が終わってから終了する（終了コードは復元の結果。通常の復元・EXIT trap 内の復元・手動`--rollback`のどれでも）。復元中に想定外の終了があれば、入れ子の復元はせず手作業の手順と終了コード3を出す。②復元で止める操作は`stop`の成功を必須にし、unit が存在しない（`LoadState=not-found`）ときだけ失敗を許す。そのうえで`systemctl show`の`ActiveState`が`inactive`/`failed`と確認できなければ（`activating`・`deactivating`・状態の取得失敗・空出力）終了コード3。③Python 版は`systemctl show`の値で、`inactive`/`failed`か unit の不在が確認できたときだけ通し（**のちに変更**: T2.1 のレビューで、`LoadState`は見ず`ActiveState`が`inactive`/`failed`のときだけ通す形にした。`not-found/active`は拒否。[server.md §11.1.1](server.md#1111-本番への誤指定の防止ガード)）、稼働中・取得失敗・空出力・未知の状態は終了コード4で拒否する。④事前検査の終了コード4を、ラッパーが1に変えずにそのまま返す。

**モックでの試験**（[`deploy/rust-deploy-test.sh`](../../deploy/rust-deploy-test.sh)、ssh なし。一時ディレクトリに作った環境と偽の`systemctl`/`curl`で、検証環境側の本体をそのまま走らせる）: **88/88項目が成功**（前回は79件）。印なし・Python 版稼働中・印がシンボリックリンクでの拒否（終了コード4、何も変更しない）、Python 版の状態取得の失敗・空出力・未知の状態・`activating`での拒否と、`failed`・unit 不在での許可、成功、`restart`・`daemon-reload`・`enable`・unit 導入・起動できないバイナリ・`/healthz`不通の各失敗からの復元（もとが`disabled`・停止中のときもそのまま）、復元の`mv`・`daemon-reload`の失敗（終了コード3、手順が出る、「戻した」と出ない）、`rollback`の`mv`の失敗、復元は済んだが戻したあとの確認が通らない場合（使い済みの`.prev`への`mv`は案内せず、先に止める手順を出す）、初回の配布の失敗と成功（古い`.prev`を戻す先にしない）、`restart`中の SIGTERM、復元の最中の SIGTERM（通常の復元・EXIT trap 内の復元・手動の`rollback`のそれぞれで、復元をやり切って終了コード1／0）、復元の失敗と SIGTERM が重なる場合（復元時の`restart`の失敗と、その`restart`が送る SIGTERM を重ねる。SIGTERM を実際に送ったことも確かめ、終了コード3・手順が出る）、復元の中で致命的なシェルエラー（一時コピーの`restore()`へ未定義変数の参照を注入）が起きる場合（EXIT trap 内・通常の復元・手動の`rollback`のいずれも、終了コード3・手順が出る。EXIT trap 内の復元は専用のサブシェルで行い、異常終了しても親が手順と3を保証する）、`stop`の失敗＋`activating`・`stop`は成功でも`activating`・`stop`の失敗（状態は`inactive`）・停止後の状態取得の失敗と空出力（いずれも終了コード3）、unit 未導入の初回で`stop`が「not loaded」になる場合（戻せる）、手動の`rollback`。手元のラッパー（ssh・scp をモックにして`rust-deploy.sh`本体を通す）では、事前検査の拒否が終了コード4のまま返ること（何も転送しない）、成功0・戻せた失敗1・戻せなかった失敗3、`--rollback`の拒否4と成功0。

**検証環境（実機）での試験**（`systemd`の drop-in `ExecStartPre=+`で、起動前の失敗を1回だけ起こした。試験後に drop-in は消した。上の変更の一覧にも書いた）:

| 試験 | 結果 |
| --- | --- |
| 印が無い状態で配布 | 「検証環境の印が無い」で**拒否、何も変更せず**終了（転送もしない） |
| `restart`の失敗（旧版と1バイト違う版を配布。`restart`が失敗する） | `daemon-reload`・`enable`の後の`restart`の失敗を検知し、**バイナリ・unit が配布前に戻り（sha256が元と一致）、`enable`・稼働のまま、確認OK、終了コード1**。失敗した版は`geoframe.failed`に残った |
| 復元の`mv`の失敗（`restart`の失敗と同時に、`geoframe.prev`をディレクトリにすり替えた） | `mv: cannot overwrite non-directory`を検知し、「失敗した操作: mv …」と手作業の復旧手順を出し、「戻した」とは出さずに**終了コード3** |
| 初回の配布の失敗（バイナリ・unit を退避し、`disable`して「未配布」にしてから、`restart`が失敗する配布） | **サービスは停止、`enable`の印（`multi-user.target.wants`）なし、unit は撤去、バイナリは`geoframe.failed`へ退避、配置先は空、終了コード1** |
| 修正後のスクリプトでの通常の配布 | 印の作成のあと、初回の配布・更新とも成功（確認OK、終了コード0） |

## 4. miharashiの実プロセスでの確認

miharashi（`~/work/miharashi`、コミット`1bc73a6`）の**本番プロセス（`miharashi.service`）と`/etc/miharashi`には触っていない**。miharashiのリポジトリも変更していない（ビルドの出力先は`/tmp`、設定は一時ファイルの`--config`）。

miharashiには画面なしで動かせる経路が2つある。取得先（`geoframe.url`）を検証環境のRust（`:8080`）へ向けて確かめた。再現できるように、秘密を含まないハーネスと手順を[`tools/miharashi-feed-check/`](../../tools/miharashi-feed-check/README.md)に保存した（ワークスペースの外。miharashi本体は読むだけ）。以下は、そのハーネスでのやり直し（miharashiコミット`1bc73a6`）。

### 4.1 実バイナリの`png`サブコマンド

`miharashi png --config <一時の設定> --renderer cpu --size 640x360`を[`repeat-png.sh`](../../tools/miharashi-feed-check/repeat-png.sh)で60回。実クライアントの`Catalog::open`（`GET /api/v1/photos`）と`current_photo`（写真1枚の取得・デコード・合成）を、GPU・DRMを開かずに動かし、PNGを書く。`--config`を明示するので`/etc/miharashi`を拾わず、`STATE_DIRECTORY`も無いので表示履歴も書かない。

- **60回実行して60回成功**（失敗0、27秒）。1回ごとに一覧を取り直し、別の写真を1枚読み込む。
- 一覧から表示対象に選ばれた枚数は**3,029枚**（miharashi自身の品質フィルタで、一覧3,065件のうち36件を除く。1回目の実施と同じ）。旅は309、暮らしは1（Home Assistantの設定が無いので自宅は不明で、GEOFRAMEの旅番号でまとめる。キオスクの本番はHAの自宅位置を使う）。

### 4.2 `Feed`（一覧の取得と写真の読み込みの別スレッド）

miharashiは`photos.rs`・`album.rs`を持つ`miharashi-app`が実行ファイルだけでライブラリではないため、ハーネス（`miharashi-feed-check`）がこの2つのソースを`#[path]`でそのまま取り込む（画面側の型だけ最小の代役。本体の取得・デコード・並べ方・再接続のコードは同じ）。画面を出さずに`Feed::spawn`を動かし、`next()`で写真を送り続けて数えた。

| 試験 | 結果 |
| --- | --- |
| 定常（Rustに向けて200枚） | **200枚を読み込み（写真184枚＋旅の区切りのカード16枚。デコード・合成まで成功）**。再接続の発生なし（Retrying 0）。24秒。3回続けて失敗して`Retrying`になる取得・デコードの失敗は0 |
| 途中でRustを止めて戻す（10秒後に`systemctl stop`、その8秒後に`start`。300枚） | 停止中は`Retrying`を4回通知（指数バックオフ）、起動後に`Online`を1回通知して**自動で回復**し、**300枚を読み込み（写真278枚＋カード22枚）**。最初の`Retrying`から次の写真まで15.1秒（再開コマンドのsshを含む）。キー操作は不要。区間の外の`Retrying`は0。34秒 |

- **取得できた件数: 3,029枚（一覧3,065件）。読み込めた写真: 4.1で60枚、4.2で462枚（定常184＋停止・再開278）とカード38枚。エラー: 停止・再開の区間の外の`Retrying`（3回続けて失敗した取得・デコード）は0。**
- `Feed::spawn`の経路では、miharashiが別の写真へ飛ばして先へ進む1〜2回の取得・デコードの失敗はイベントにも標準エラーにも出ないので、ハーネスは数えられない（標準エラーの`photo:`を出すのは`PhotoItem::ensure_frame`とinlineモードだけで、ハーネスはどちらも通らない）。検出できるのは3回続けて失敗して`Retrying`になったときだけ。
- 画面（DRM/KMS）への描画・フェードは確かめていない（このサブコマンド・ハーネスは描画しない）。キオスクの実機で、取得先を切り替えて表示を見るのは、P5の切替のときに行う。
- miharashi本体の契約テスト（[`crates/geoframe/tests/photo_feed_miharashi.rs`](../../crates/geoframe/tests/photo_feed_miharashi.rs)）はT1.2で済み。今回は、それを本物の`Feed`と本物のRustサーバーで確かめたことになる。

## 5. 検証環境に加えた変更（戻せるように）

| 変更 | 戻し方 |
| --- | --- |
| `/var/lib/geoframe`を`geoframe:geoframe`へ`chown -R`（もとの持ち主は検証環境の作業ユーザー）。`derivatives/`の`0660`のファイルに`o+r` | `sudo chown -R <もとの作業ユーザー>: /var/lib/geoframe`。比較用Pythonがハードリンクで共有するので、`o+r`は残してよい |
| `/etc/default/geoframe`を新規作成（`GEOFRAME_EMBED_BACKEND=local`だけ） | 削除 |
| 比較用Pythonへdrop-in`/etc/systemd/system/geoframe-pyref.service.d/override.conf`（`GEOFRAME_EMBED_BACKEND=local`）を追加し、再起動（torchを読み込もうとして落ちることはなかった） | drop-inを削除して`daemon-reload`・`restart` |
| `/usr/local/bin/geoframe`（`.prev`は直前の世代）、`/etc/systemd/system/geoframe-rs.service`（`.prev`）を配置し、`geoframe-rs.service`をenable・起動 | `systemctl disable --now geoframe-rs`、上のファイルを削除 |
| **やり直しで追加: 検証環境の印`/etc/geoframe/staging`**（`/etc/geoframe/`も新規。中身は「検証環境の印」という1行だけで、接続先などは書いていない） | 削除（削除すると`rust-deploy.sh`は拒否する） |
| やり直しの失敗経路の試験で、drop-in`/etc/systemd/system/geoframe-rs.service.d/zz-failtest.conf`（`ExecStartPre=+`で1回だけ失敗）を追加し、バイナリ・unit の退避（`/root/t15-aside`）と`geoframe.failed`、`/run`の目印を作った | **すべて削除済み**（drop-inのディレクトリも）。そのあと通常の配布を初回・更新と2回行い、`.prev`のある通常の状態に戻した |
| `/tmp`の試験用ファイル（`default-geoframe.orig`など） | 削除 |

比較用Pythonのデータ（`/var/lib/geoframe-pyref`）と旧版の退避（`*.old-202608`）は触っていない（比較用Pythonは自身のデータを起動時に更新する）。

## 6. 結論

P1の受入（[plan.md §4](plan.md)）の、検証環境で確かめられる部分は満たした。

- 比較用Pythonと`/api/v1/photos`が**全件（3,065件）完全一致**、`/derivatives/`が**同じバイト列（3,065/3,065）**。
- 管理画面の読み取りAPI・ページ・埋め込みファイル・拒否の動きも一致。
- systemdで常駐し、再起動・`kill -9`で自動復帰、設定不正で再起動ループにならない。配布は、誤配布を拒み、入れ替え以降のどこで失敗しても配布前の状態へ戻り、戻せないときは手順を出して非ゼロで終わる（初回の配布の失敗も含む）。
- miharashiの実コードが、Rustから一覧を取得して写真を読み込み続け、Rustの停止・再開からも回復する（再現用のハーネスを保存）。

残り（画面への表示、本番の切替、`catalog.db`を置き換えたときの運用）はP5。
