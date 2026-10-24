# Rustサーバーの基盤: 設定・DB読み取り・ログ・ヘルス（P1 / T1.1）と、書き込みの土台（P2 / T2.1、§11）

- 状態: T1.1 実装済み（2026-10-24）。レビュー指摘（パニック・トランザクション・ヘルス）を反映済み。T1.2（`GET /api/v1/photos`、§8）、T1.3（静的配信・Host検査・管理画面ページ、§9）、T1.4（管理画面の読み取りAPI、§10）を追記。T1.5 で systemd unit と配布スクリプトを入れ、検証環境で受入確認した（§6、[p1-acceptance.md](p1-acceptance.md)）。P2 の T2.1 で書き込みの土台（writer・スキーマの作成と移行・同期サイクルの骨組み・`--dry-run`・起動時の掃除・`.cache.json`）を入れた（§11）。T2.2 で画像処理（derivative の生成・EXIF・HEIC・パスの検査）を入れた（§12）。T2.3 でローカル写真の索引（`index_local`・`local_derivatives`・`organize`）と書きかけの`.part`の掃除を入れた（§13）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 計画: [plan.md](plan.md) / タスク: [tasks.md](tasks.md) / 棚卸し: [inventory.md](inventory.md)（§2 DB、§4 設定、§6.3 ログ）
- 実装: `crates/geoframe/src/config.rs`・`db.rs`・`logging.rs`・`server.rs`・`cli.rs`、T1.3 は`webpath.rs`・`hostcheck.rs`・`statics.rs`・`embedded.rs`と`build.rs`、T1.4 は`admin.rs`・`pyquery.rs`・`sysinfo.rs`、T2.2 は`imaging/`（`derivative.rs`・`exif.rs`・`heic.rs`・`probe.rs`）と`datadir.rs`、T2.3 は`local.rs`
- ビルドと配布は[build.md](build.md)。ここは**起動したあとの振る舞い**の決めごと。

## 0. Pythonとの差の索引

P1・P2（T2.1〜T2.3）でPythonと意図して違える点・既知の差の入口。本文は各節にあり、ここは1行ずつの索引だけ（内容／理由／適用条件／検証状況／解消予定）。

| 差 | 理由 | 適用条件 | 検証状況 | 解消予定 | 詳細 |
| --- | --- | --- | --- | --- | --- |
| `GEOFRAME_DATA_DIR`の既定が`./var`（Pythonは`app/var`）。`app/.env`を読まない | 単一バイナリに`app/`が無い。本番はunitの`EnvironmentFile`が同じ役目 | 環境変数・`.env`で与えず手で起動したとき | 設定のテスト。本番はunitが常に与える | なし（決定） | [§1.1](#11-優先順位)・[§1.3](#13-appenvは読まない) |
| 不正な設定値は起動を断る（終了コード2）。`GEOFRAME_EMBED_BACKEND`だけ`gemini`に落とす。`GEOFRAME_HOST`のホスト名はPythonは通すがRustは断る（IPアドレスだけ） | 事故を早く見つける。Pythonは例外で落ちる | 不正な値が設定にあるとき（本番は`0.0.0.0`か`127.0.0.1`） | 単体テスト、検証環境で再起動ループにならないこと | なし（決定） | [§1.1](#11-優先順位)・[§10.6](#106-pythonとの差まとめ) |
| 全角数字など ASCII 以外の数字の撮影時刻をRustのフィード（§8）は読めない（旅番号がずれうる） | 時刻の解釈を`strptime`に合わせきれていない | EXIFに全角数字が入った写真 | 本番コピーでは差0。単体テスト | **T2.2で`imaging::norm_time`が数字をASCIIに直す**（Pythonは元の文字のまま保存する）。呼ぶのはT2.3・T2.4のインデクサ。Pythonが書いて既にDBに入っている全角の値は直さない | [§8.2](#82-値)・[§12.3](#123-exif撮影時刻gps向き) |
| 夏時間を持つ`TZ`のときの時刻の差 | RustはP1で`TZ`を解釈せず、naiveな日時を文字列・暦で扱う | `TZ`が夏時間を持つとき（本番は`Asia/Tokyo`） | 本番設定では差なし | P2以降で時刻を変換するときに再検討 | [§1.2](#12-tz)・[§10.6](#106-pythonとの差まとめ) |
| `taken_at`がNULLの行: Pythonは`time: null`で先頭、Rustは空文字で最後 | 契約（`time`は必ずある）に合わせる | `taken_at`がNULLの行がDBにあるとき | 本番コピーに該当行なし（管理者確認済み） | なし（決定） | [§8.3](#83-pythonとの差) |
| `lat`ありで`lon`がNULLの行: Pythonは一覧全体が500になりうる、Rustは一覧から外す | 契約（`lon`は必ずある）に合わせる | 該当行があるとき（通常は起きない） | 本番コピー（2026-10-24のバックアップ）での全件比較で差0（表示対象に該当行なし） | なし（決定） | [§8.3](#83-pythonとの差) |
| 静的配信・HTTPの差（領域外のリンクは403、`ETag`での再検証、`/healthz`の新設、未実装ルートの503、`POST`は全部403、接続・配信の上限など） | 安全性、固定の挙動、まだ無いことの明示 | 該当する要求を送ったとき | 検証環境で現行Pythonとの比較（状態コード・本文の一致）、単体テスト | 503・`POST`はP3・P4で実装して外す。他は決定 | [§9.7](#97-pythonとの差まとめ) |
| 列の型が想定外の行（`lat`が文字列など）があると読み取り全体が失敗して503（Pythonは素通し） | スキーマ上は起きない | `GET /api/v1/photos` | 本番コピーの全件比較で差0（該当行なし） | なし（決定） | [§8.3](#83-pythonとの差) |
| DBが無いとRustは起動を断る（終了コード1、systemdで5秒おきに再起動）。PythonはDBを作る。**ただし`GEOFRAME_INGEST=1`のときは、Rustも空のDBを作る**（T2.1。§11.1） | DBを作るのはPython。配備の誤りを失敗として見せる | `GEOFRAME_DATA_DIR`の誤りなど、DBが無いまま起動したとき | 検証環境で確認（[p1-acceptance.md](p1-acceptance.md)） | なし（決定。P5の切替時にはDBがある） | [§2.4](#24-dbが無い壊れている古いときの扱い) |
| 管理画面の読み取りAPIの差（件数を1トランザクションで数える、想定外の型は503、キーの並びなど） | 一貫性、読み取り専用の表示で落とさない | `GET /api/admin/status`・`photos` | 検証環境の比較で差0、単体テスト | なし（決定） | [§10.6](#106-pythonとの差まとめ) |
| `GEOFRAME_INGEST`（Rustだけの項目。既定`0`）。`0`のRustは**DBに一切書かない**（writerを作らない・移行しない・同期サイクルを動かさない） | P5の切替まで本番の書き手はPython。`/etc/default/geoframe`のままRustを起動しても二重に書かない | 取り込みを使うとき（検証環境）だけ`1` | 単体テスト、`tests/ingest_cli.rs`（既定で`serve`が`catalog.db`もフォルダも変えない） | P5の切替で本番の設定に`1`を足す。既定の見直しもその時 | [§11.1](#111-取り込みを有効にする-geoframe_ingest) |
| **取り込み（`GEOFRAME_INGEST=1`の`serve`・`sync`の実行）は、検証環境の印（`/etc/geoframe/staging`）が無い、または`geoframe-server`が非稼働と確認できない（`ActiveState`が`inactive`／`failed`でない。unitファイルが無くても動いているサービスはありうるので、`LoadState`は見ない）と、書き込みの前に終了コード2で断る**（ガード）。Pythonには無い | 本番機で誤って`GEOFRAME_INGEST=1`を立てて起動しても、Pythonの稼働中に移行・`.cache.json`の更新・`.dl_*`の掃除をしない。配布スクリプトの拒否は直接起動や設定の変更には効かないため、バイナリ自身が守る | 取り込みを有効にして起動するとき。`--dry-run`と読み取り専用の`serve`は対象外 | 単体テスト（`guard.rs`・`ingest.rs`）、`tests/ingest_cli.rs`（印なし・Python稼働中・取得不能で書き込み前に止まり置き場が変わらない）、検証環境（§11.11） | **P5の切替で外す**（手順は[plan.md P5](plan.md)） | [§11.1.1](#1111-本番への誤指定の防止ガード) |
| `GEOFRAME_DRIVE_POLL_MIN`: 0以下・非数・上限（525600分）超は起動を断る（終了コード2）。Pythonは0以下で空回りか例外 | 事故を早く見つける | 不正な値が設定にあるとき（本番は既定の10） | 単体テスト | なし（決定） | [§11.2](#112-設定) |
| 同期サイクルの段の失敗は、段の名前と失敗の種類だけをログに出す（Pythonは例外文字列をそのまま出す）。段のパニックも隔離する（Pythonは`except Exception`） | ログに位置・ファイル名を出さない方針（inventory.md §6.3） | 常時 | 単体テスト（`sync.rs`） | なし（決定） | [§11.3](#113-同期サイクルの骨組み) |
| `.cache.json`のキーをRustは辞書順に書く（Pythonは挿入順）。停止時にも最後の保存をする（同期サイクルのスレッドが止まったあとに1回。保存スレッドが止まりきらず保存の最中なら、期限付きで待ち、順番が来なければ書かずに省略して警告する）。`.dl_*`は通常ファイルだけを消す | `serde_json`の`preserve_order`を入れると他のJSON応答の並びが変わる | `.cache.json`をRustが保存したとき（読むときの並びは問わない） | 単体テスト（Pythonの`json.dumps(indent=1, ensure_ascii=False)`と同じ形） | なし（決定） | [§11.5](#115-cachejson) |
| **画像処理（T2.2）**: ICCをsRGBへ変換して付けない（Pythonは捨てるだけ）。デコーダ・縮小フィルタ・エンコーダが違うので、画素はPythonと一致しない（寸法・向き・回転・EXIFを残さないことは一致）。16ビットのグレーPNGは8ビットへ正しく縮める（Pillowは`I;16`のRGB変換で飽和する）。HEICは常に取り込む（Pythonは`pillow_heif`があるときだけ。本番は入っている）。向きはEXIFのOrientationのタグが無いときだけXMPの`tiff:Orientation`を使う（Pillowの`getexif`と同じ。**圧縮したXMP（`zTXt`・圧縮した`iTXt`）とHEICのXMPは読まない**。HEICはpillow-heifが開くときにXMPの向きを取り除くので、Pillowも当てない）。**BigTIFFは非対応**（`ImageError::BigTiff`。Pythonは開ける。本番のTIFFは0件） | 広色域の写真が薄く見える問題を直す（plan.md P0で決定）。純Rustの部品の選択（image-poc.md） | ICC付きの写真、画素の比較 | 合成画像のテスト、向きは`tests/orientation_python.rs`が基準（`tools/gen_orientation_fixture.py`。四隅に色を付けた合成画像のPythonの出力と寸法・色を比べる）、検証環境でPythonとの寸法・画質の比較（[§12.9](#129-検証環境での確認2026-10-24)） | なし（決定） | [§12.2](#122-derivativeの手順とpythonとの差) |
| **EXIFの読み取り（T2.2）**: 壊れたGPS（成分が3つ未満・型が違う・分母0）の写真は**位置なしで索引する**（Pythonは例外で写真ごと索引しない、または分母0でNaNを保存する）。時刻の数字はASCIIに直す。時刻のタグが数値のときの偽値（0・個数0）は、Pythonの`or`と同じに次の候補へ落とす | 位置が読めないだけで写真を落とさない。`taken_at`の形式を守る | 壊れたGPSのEXIF | `tests/exif_python.rs`が基準（`tools/gen_exif_fixture.py`）と突き合わせる | なし（決定） | [§12.3](#123-exif撮影時刻gps向き) |
| **入力の限界（T2.2）**: 1ファイル512 MiBまで（まるごとメモリに読む。Pythonは上限なし。`img-probe`の`--out`なしも同じ）。**HEICは、デコーダに渡す前に、ファイルが宣言する長さ・寸法（`mdat`のサイズ・EXIFのextent・`ispe`・SPS）がファイルの実際の大きさと画素数の上限に収まるか検査する**（`zune-heic`は宣言値で先に確保するので、小さな偽のファイルで確保に失敗してプロセスごと落ちるため。§12.4）。**EOI（画像データの終わりのマーカー）の無いJPEGは失敗にする**（切れたファイルと見分けられないため。§12.8）。Pillow 12.3.0も、EOIだけが欠けてファイルがそこで終わるJPEGと、SOS以降の全ての切断点で切ったJPEGは失敗にする。**Pythonが読んでRustが失敗にするのは、EOIが無くその位置にゴミやゼロ埋めが続くファイルだけ**（Pillowはそれも画像データとして読む。`tests/jpeg_eoi_python.rs`）。手動回転は0・90・180・270だけ（管理画面もその値しか作らない）。`.part`は`<出力名>.<pid>.<乱数>.part`を**排他的に新規作成**して（`O_EXCL`＋`O_NOFOLLOW`。既存のシンボリックリンクを辿らない）書き、書き込みに失敗・パニックしても**自分の**`.part`は残さない（Pythonは失敗すると残しうる）。出力の`.part`は`fsync`してから`rename`する | メモリの上限、電源断で空のderivativeを残さない、`DATA_DIR`の外を書き換えさせない | 巨大なファイル、偽の長さのHEIC、書き込みの失敗、`derivatives/`に置かれたリンク | 単体テスト、`tests/jpeg_eoi_python.rs`（`tools/gen_jpeg_eoi_fixture.py`） | なし（決定） | [§12.2](#122-derivativeの手順とpythonとの差) |
| **ローカル写真の索引（T2.3）**: ①`mtime`が変わった行の更新は、手動で直した位置（`loc_source='manual'`）も`exif`か`NULL`に**上書きする**（Pythonと同じ。`place elev display`もNULLに戻る。お気に入り・除外・回転・`fetched_at`は残る）。②**シンボリックリンクのディレクトリには降りない**（Pythonの`rglob`も降りない）。**ファイルのリンクは、実体が`DATA_DIR`の中にあるときだけ**取り込む（Pythonは外のファイルも読む）。名前がUTF-8でないエントリは飛ばす（Pythonは行を作る段で例外）。③書き込みは**1つのトランザクション**（追加・更新・削除をまとめる。Pythonは1文ごとのコミット）。EXIFの読み取りは書き込みの外で先に済ませる。④`GEOFRAME_PHOTOS_DIR`が`DATA_DIR`の外・`DATA_DIR`そのものを指すとその段は失敗する（Pythonは外で例外。`DATA_DIR`そのものだと`derivatives/`と`tmp/`の原本まで索引する）。⑤`mtime`がNULL・文字列の行は「変わった」とみなす（Pythonは例外）。⑥消した行の`display`が`derivatives/<安全な名前>`でなければ、行は消すがファイルは消さない（Pythonは`DATA_DIR / display`を無検査で消す）。⑦`GEOFRAME_FETCH_PER_CYCLE`は0以上の整数だけ（負の値は起動を断る。Pythonは負だとSQLiteの`LIMIT`が無制限） | ①は現行の挙動（直すかはP3で決める。[inventory.md §2.3](inventory.md)）。②〜⑦は安全側 | ローカルに写真を置いたとき | `tests/local_index_python.rs`（合成の木を4段階の操作でPythonの`index_local`と`photos`の全列・`id`・書き換えの有無まで突き合わせる。基準は`tools/gen_local_index_fixture.py`、CIで`--check`）、単体テスト、検証環境（§13.9。比較用Pythonと14行・全列が同じ） | ①は**P3で直すか決める**。他は決定 | [§13.2](#132-index_local)・[§13.7](#137-pythonとの差まとめ) |
| **`local_derivatives`（T2.3）**: 取得待ちを`ORDER BY id`で選ぶ（Pythonは`ORDER BY`なしの`LIMIT`）。`photos.path`を`DataDir::resolve_existing`で検査してから読み、`DATA_DIR`の外へ出るものは失敗として数える（Pythonは原本のとき存在も見ずに`display`にする）。**変換が恒久的に失敗する写真が先頭の枠を埋め続けうる**のはPythonと同じ（`GEOFRAME_FETCH_PER_CYCLE`件より多く失敗すると後続が取得されない） | 順序の固定、DBの値を信用しない | 位置のあるローカルのHEIC・TIFFが壊れたとき | 単体テスト、検証環境 | 枠の占有はT2.4のDrive側と一緒に判断する（[§12.10](#1210-t23t24へ回したもの)） | [§13.3](#133-local_derivatives) |
| **`organize`（T2.3。`geoframe organize [--dry-run]`）**: ①移動先に同名のファイルがあっても**上書きせず**`名前 (2).拡張子`にする（Pythonの`rename`は上書きして失った）。②通常のファイルだけを移す（リンク・ディレクトリは動かさず、行も残す）。③**T2.5までは、推定で位置が付きうる写真は移さない**（撮影時刻があって、錨になる位置つき写真が1枚でもあるとき）。Pythonは`index_local`→**`infer_locations`**→移動の順で、推定された写真は残す。Rustの`infer`段はT2.5まで何もしないので、そのまま移すとPythonなら残る写真まで移してしまう。そこで移動対象をPythonの部分集合に絞る。④ガード・ロックはsyncと同じ。ログに名前を出さない（件数だけ）。`unit`の`ReadOnlyPaths`が`photos`を読み取り専用にするので、`organize`はunitの外（`sudo -u geoframe`）で動かす | データを失わない、Pythonの結果を超えて動かさない | 位置の無いローカル写真を整理するとき | 単体テスト、`tests/ingest_cli.rs`、検証環境 | ③はT2.5で`infer`段のあとに移動する形にして外す | [§13.4](#134-geoframe-organize) |
| **書きかけの`derivatives/*.part`を起動時に消す（T2.3）**: `GEOFRAME_INGEST=1`のガードとロックのあと、`.dl_*`の掃除と同じ時点で、`derivatives/`**直下の通常ファイルで名前が`.part`で終わり、安全な名前**のものを消す。リンクは消さず辿らない。`derivatives`自体がリンクなら何もしない。件数だけをログに出す（`--dry-run`にも出る）。Pythonには掃除が無い | SIGKILL・電源断で残る`<名前>.<pid>.<乱数>.part`は名前に乱数があるので上書きされず容量を食い続ける | 取り込みの起動時 | 単体テスト、検証環境 | なし（決定） | [§13.5](#135-書きかけの-part-の掃除) |

P1の検証の記録は[p1-acceptance.md](p1-acceptance.md)。

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
| 取り込み（DBへの書き込みと同期サイクル）を有効にするか（**Rustだけの項目**。`0`／`1`） | なし | `GEOFRAME_INGEST` | `0`（無効。§11.1） |
| 同期サイクルの間隔（分。小数も可） | なし | `GEOFRAME_DRIVE_POLL_MIN` | `10`（§11.2） |
| ローカル写真の置き場（`DATA_DIR`からの相対。T2.3） | なし | `GEOFRAME_PHOTOS_DIR` | `photos`（§13.1） |
| 同期1サイクルで`local_derivatives`が処理する枚数（0以上の整数。T2.3） | なし | `GEOFRAME_FETCH_PER_CYCLE` | `25`（§13.1） |

- **空文字の環境変数は未設定と同じ**に扱う。現行の`GEOFRAME_DATA_DIR`が`or`で空文字を未設定扱いにするのと同じで、`EnvironmentFile`に`KEY=`と書いた事故を避ける。
- **不正な値は起動を断る**（終了コード2）。メッセージには環境変数の名前と理由だけを書き、値は書かない。`GEOFRAME_HOST`はIPアドレスだけ（現行Pythonはホスト名も通すが、本番は`0.0.0.0`か`127.0.0.1`）。
- `--listen`を渡したときは`GEOFRAME_HOST`/`GEOFRAME_PORT`を**読まない**（不正でも問わない）。
- `GEOFRAME_ALLOWED_HOSTS`の解釈は現行の`_parse_allowed_hosts`と同じ（小文字、末尾の`.`を除く、`name:port`・`[v6]:port`のポートを無視）。この集合を使うHost検査は§9.2。
- **最後の3つは T1.4 で読み始めた**。取り込み・AI・タイルは P2 以降なので、これらは**管理画面の表示にだけ**効く（`/api/admin/status`、§10）: `GEOFRAME_INFER_MIN`は「推定の内訳」の区切りと位置候補の「日」の区切り、`GEOFRAME_EMBED_BACKEND`はAIの件数が数える`embedding_model`（`gemini`→`gemini-embedding-2`、`local`→`embeddinggemma-2`）、`GEOFRAME_TILE_CACHE_MAX_MB`は`disk.tile_cache_max_mb`にそのまま出る。**読まないと Python と値がずれる**ため（`/etc/default/geoframe`に書いてあれば、Python と同じ値を Rust も見る）。解釈は現行に合わせ、`GEOFRAME_EMBED_BACKEND`は前後の空白を除いて小文字にし、`gemini`・`local`以外は**起動を断らず`gemini`に落とす**（現行も警告して`gemini`にする。Rustは起動ログにwarnを1行出す。値は書かない）。`GEOFRAME_INFER_MIN`は0以上の有限の数、`GEOFRAME_TILE_CACHE_MAX_MB`は整数でなければ**起動を断る**（Rustの方針。現行は`float()`/`int()`が例外で落ちるので、起動できない点は同じ）。
- **`GEOFRAME_DATA_DIR`の既定が現行と違う**: Pythonは`app/var`（ソースの隣）だが、単一バイナリには`app/`が無いので`./var`にした。本番は常に`GEOFRAME_DATA_DIR=/var/lib/geoframe`をunitが与える。

### 1.2 `TZ`

**Rustは`TZ`を解釈しない。** P1は時刻を変換せず、`taken_at`などのnaiveな日時は文字列のまま扱う（[inventory.md §6.1](inventory.md)）。旅番号の48時間差などの計算（T1.2）も、環境のTZに依存しない`NaiveDateTime`で行う。ただしunitは`TZ=Asia/Tokyo`を与え続ける（並走するPythonのため、将来`Local`を使うときの保険）。起動ログに値を出すだけ。**例外**: `/api/admin/status`の`server_time`と`disk.measured_at`だけは現地時刻を出すので、Cライブラリの`localtime_r`に任せる（`sysinfo.rs`。§10.4）。muslは実行時に`TZ`と`/usr/share/zoneinfo`を読むので、unitが`TZ=Asia/Tokyo`を与えていれば Python と同じ時刻になる。tzdataが無い機体ではPythonもRustもUTCになる。そのため`TZ`が夏時間を持つときは、Pythonとの時刻の差が出うる（本番は`Asia/Tokyo`で、本番設定では差なし。[§0](#0-pythonとの差の索引)）。P2以降で時刻を変換するときに再検討する。

### 1.3 `app/.env`は読まない

決定: **Rustは`app/.env`を読まない。** 理由は次のとおり。

- 単一バイナリの配布には`app/`が無く、`.env`の置き場が定まらない。
- 本番は既にsystemdの`EnvironmentFile=-/etc/default/geoframe`が同じ役目を持つ（unitの最後に読まれ、後勝ち）。
- 現行でも`GEOFRAME_DATA_DIR`は`.env`と鶏卵の関係にあり（`config.py`のコメント）、手書きのパーサーはsystemdの引用符の扱いとずれやすい。

手元で試すときは、環境ファイルを読み込んでから起動する（`set -a; . ./環境ファイル; set +a`）。現行の`.env`は`setdefault`（環境変数が優先）なので、優先順位の向きは同じ。

### 1.4 未対応の項目（P2以降）

`config.rs`の`UNSUPPORTED_ENV`に、現行の`config.py`にあるP2以降の項目（取り込み・タイル・AI・位置候補など）を**列挙だけ**してある（T2.1で`GEOFRAME_DRIVE_POLL_MIN`、T2.3で`GEOFRAME_PHOTOS_DIR`・`GEOFRAME_FETCH_PER_CYCLE`を読み始めたので、27個）。読まず、警告も出さない（`/etc/default/geoframe`に書いてあっても無視される）。`docs/reference/configuration.md`にある`GEOFRAME_*`が、読む項目（`SUPPORTED_ENV`）か未対応のどちらかに分類されていることを、テスト`every_documented_variable_is_classified`が検査する。Pythonに項目が増えたら、このテストが落ちて気づける。

秘密になりうる項目（P2の`GEOFRAME_DRIVE_KEY`ほか）が入るときも、設定が持つのは**鍵ファイルのパスだけ**で、中身は設定の型に載せない。`Config`は`Debug`を手書きにしてあり、ログへ出す要約（`log_summary`）に載せた項目だけが出る。許可Host名も件数だけで、名前は出さない。

## 2. SQLite読み取り層

`crates/geoframe/src/db.rs`の`Catalog`。

### 2.1 読み取り専用

（読み取り側の話。取り込みが有効なときの書き込みとスキーマの作成は§11。ここに書いた「スキーマの作成・移行をしない」は、`GEOFRAME_INGEST`が`0`の場合と、読み取りの層そのものについて。）

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
- **書き込みを足す構造（P2で取り込み用のwriter、P3で更新API）**: **T2.1で入れた**（[§11.4](#114-writer)）。書き込みは単一の接続を専用スレッドに置き、チャネルで依頼を受ける（現行の「1接続＋グローバルロック」と同じ直列化）。`Catalog`に`writer`を足す形で、`read`のAPIは変えずに済む。P2の索引・取り込みから書き込みが要るので、取り込み用のwriterはP2で追加し、unitの`ReadOnlyPaths`を必要な分だけ解除する。手動修正などの更新APIはP3で足す。P1では作らない。

### 2.3 Pythonが書き込み中でも読める

DBはWAL（Pythonが`journal_mode=WAL`にし、その状態がDBに残る）なので、読み取りは書き込みを待たない。コミット前の行は見えず、チェックポイント前のコミットはWALから読める。`busy_timeout`は5秒で、WALの復旧中などで一瞬`SQLITE_BUSY`になる場合のため。テスト:

- `uncheckpointed_commits_are_readable_and_files_are_untouched`: 書き込み側が自動チェックポイントを止めてコミットした行（WALにだけある）が読めること、DB本体と`-wal`がバイト単位で変わらないこと（T0.3レビューからの持ち越し）。
- `reads_do_not_wait_for_an_open_write_transaction`: `BEGIN IMMEDIATE`で書き込み中でも読み取りが待たず、コミット後に見えること。

### 2.4 DBが無い・壊れている・古いときの扱い

| 状況 | 扱い |
| --- | --- |
| 起動時: DBが無い・開けない・SQLiteのDBでない | **起動を断る**（終了コード1。ログに`DB を開けません`と、`GEOFRAME_DATA_DIR`を確認する案内）。**ただし`GEOFRAME_INGEST=1`で「DBが無い」だけは、空のDBを作って起動する**（warnで知らせる。§11.1）。開けない・SQLiteのDBでないは取り込みが有効でも断る |
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

unit は [`deploy/geoframe-rs.service`](../../deploy/geoframe-rs.service)（T1.5）。配布は[`deploy/rust-deploy.sh`](../../deploy/rust-deploy.sh)、手順は[deploy/README.md §7](../../deploy/README.md)。**P1〜P4 は検証環境専用で、本番へ入れるのは P5。** 決めたことと、その理由は次のとおり。

- **設定不正の終了コード2で再起動ループしない**。unit に`Restart=on-failure`と`RestartPreventExitStatus=2`を書いた。設定の誤りは再起動しても直らないので、`failed`のまま止まる。DBが開けない・スキーマ不足の終了コード1は、Pythonより先に起動した場合などで再試行に意味があるので、`RestartSec=5`で再起動し続ける（検証環境で確認。[p1-acceptance.md](p1-acceptance.md)）。
- **`ExecStart`に`--listen`を書かない**。待ち受けは`GEOFRAME_HOST`/`GEOFRAME_PORT`（既定`0.0.0.0:8080`）で決まり、設定は`/etc/default/geoframe`に集まる（Pythonと同じ運用）。`--listen`を書くと`GEOFRAME_PORT`が黙って無視されるので、普段は書かない。
  - **Pythonと同じ機体で並走するとき**（P1〜P4の検証）は、drop-inで`ExecStart=`を空にして`--listen <addr:port>`を書く（[deploy/README.md §7.2](../../deploy/README.md)）。`--listen`は`GEOFRAME_HOST`/`GEOFRAME_PORT`より優先され、それらを読まない（§1.1）ので、`/etc/default/geoframe`の`GEOFRAME_PORT`がPythonと同じ値でも衝突しない。検証環境で、`GEOFRAME_PORT=abc`を置いても`--listen`のdrop-inなら起動することを確かめた。
  - 検証環境の比較用Pythonは別のポート（8090）と別のデータ（`/var/lib/geoframe-pyref`）で動いているので、Rustは drop-in なしで`:8080`を使う。
- Rustも**`User=geoframe`で動かす**。WALのDBは読み取り専用でも`-shm`・`-wal`を触るので、ファイルの持ち主がPythonの実行ユーザーと違うと、片方が開けなくなる。`StateDirectory=geoframe`は初回起動で持ち主を揃えるが、大きなデータでは時間がかかるので、**配備の前に`chown -R geoframe:geoframe /var/lib/geoframe`を済ませておく**。
  - 検証環境の注意: 比較用Pythonの`derivatives/`は、本番のコピーの`derivatives/`への**ハードリンク**（同じ inode）。コピー側を`chown`すると、モード`0660`のファイルは比較用Pythonの実行ユーザーから読めなくなり、`/derivatives/`が404になる。`chmod o+r`で読めるようにした（出力を比べる意味を保つため）。
- **`ReadOnlyPaths`はT2.1で縮めた**: `photos`・`blog_exports`・`travel_blog`だけを読み取り専用に残し、writerが書く`catalog.db`と`derivatives/`は外した（`-wal`/`-shm`・`tmp/`・`.cache.json`・`.ingest.lock`は元から対象外）。**`GEOFRAME_INGEST`が`0`（既定）の間はwriterが作られないので、外しても何も書かれない**。P1の検証では`catalog.db`・`derivatives/`への書き込みが`Read-only file system`になることを確かめていたが、その防御はT2.1で外した（二重書き込みの防止は`GEOFRAME_INGEST`の既定が担う）。P3で更新APIを足すとき`blog_exports`・`travel_blog`も外す。ファイルのbind mountの注意（`catalog.db`を別のinodeに置き換えたら再起動）は、`catalog.db`を`ReadOnlyPaths`から外したので、unitとしては不要になったが、開いている接続が古いファイルを指し続けることは変わらない（下の「DBファイルの入れ替え」。復元後は必ず再起動する）。
- **取り込みのガード（P5まで）**: `GEOFRAME_INGEST=1`のとき、バイナリが起動時に`/etc/geoframe/staging`の有無と`systemctl show geoframe-server.service`を確かめる（§11.1.1。判定は`ActiveState`が`inactive`／`failed`かだけで、`LoadState`は見ない）。unitの制限下（`NoNewPrivileges`・`ProtectSystem=strict`・`SystemCallFilter=@system-service`）でも`systemctl show`は動く必要がある（`/usr/bin/systemctl`を子プロセスで起こし、systemdのD-Bus（読み取りだけ）で答えを得る）。**動かない・答えない・出力が読めないときは断る**（終了コード2。unitの`RestartPreventExitStatus=2`により再起動ループにならず`failed`で止まる）。`PrivateTmp`・`ProtectHome`は関係しない。検証環境での確認は§11.11。
- **`MemoryDenyWriteExecute=true`を付けた**（Pythonは Pillow のC拡張が実行可能メモリへ書くので付けられない）。musl静的ビルドのRust（tokio・SQLite同梱）は、これを付けても起動・全エンドポイントの応答・スレッド生成が動いた（検証環境）。P2で画像処理（`zune-heic`・`image`・`fast_image_resize`）を入れたときに、実際のデコードで再確認する。
- **`LimitNOFILE=4096`**。同時の接続256＋配信中のファイル32＋SQLite（読み取り5接続と`-wal`/`-shm`）＋標準入出力で300前後を使う（§9.3）ので、systemdの既定（1024）でも足りるが、余裕を持たせた。1024未満にするなら`limits.rs`の`MAX_CONNECTIONS`・`STATIC_FILE_SLOTS`もあわせて見直す。
- **P5で、`app/.env`にだけある`GEOFRAME_*`を`/etc/default/geoframe`と突き合わせる**。Rustは`app/.env`を読まない（§1.3）ので、`.env`にしか書かれていない値は、移すか、捨てる判断をする。
- **DBファイルの入れ替え（バックアップからの復元など）にはRustの再起動が要る**（§2.4の注意）。
- 配布スクリプトは、再起動の後に`/healthz`が200で`/api/v1/photos`がJSON配列を返さなければ、直前のバイナリ（`.prev`）へ戻す。戻す先が無い（初回の配布）ときはサービスを止める。

## 7. 確認の記録

- `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`（合成した一時DBだけ。実データ・実在の位置は使わない）。
- musl静的ビルド: `static-pie linked`、`ldd`は`statically linked`。追加した依存`tracing`・`tracing-subscriber`は純Rust（C依存なし）。
- 合成DBでの手動確認: `serve`が`/healthz`に200、未知のパスに404（T1.3以降は403。§9）を返し、SIGTERMで正常終了（終了コード0）。DBが無いときは終了コード1、`GEOFRAME_PORT`が不正なときは2。
- T1.3: musl静的ビルドの`file`は`static-pie linked`、`ldd`は`statically linked`のまま。管理画面の静的ファイルの埋め込み後のサイズは[build.md §3](build.md)。
- T1.5: 検証環境（lxc-geoframe）で systemd 配下に常駐させ、比較用Pythonとの全件比較・再起動・`kill -9`・設定不正・巻き戻しを確かめた。結果は[p1-acceptance.md](p1-acceptance.md)。

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
- `lat`はあって`lon`がNULLの行: Pythonは、同じ旅に他の写真があれば`assign_trips`の`min(lon)`が`TypeError`になって**一覧全体が500**、その行だけの旅なら`lon: null`。Rustは一覧から外す（写真IDだけをwarnログに出す）。通常は`setloc`が`lat`/`lon`を一緒に書くので起きない。本番のコピー（2026-10-24のバックアップ）での全件比較（`/api/v1/photos`の3,065件、型まで一致。[p1-acceptance.md §1・§2](p1-acceptance.md)）で差は0で、表示対象に該当する行は無かった。

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

## 11. 書き込みの土台（P2 / T2.1）

取り込み（P2）が`catalog.db`へ書くための土台。P1までは読み取り専用だった。実装は`writer.rs`（書き込み接続）、`schema.rs`（スキーマの作成・移行）、`sync.rs`（同期サイクルの骨組み）、`ingest.rs`（起動処理・`sync`の本体）、`cache.rs`（`.cache.json`）、`net.rs`・`jwt.rs`（外向きHTTPSとJWT。[build.md §7](build.md)）。段の中身（索引・位置推定・地名・derivative・品質）はT2.2〜T2.6で入れる。

### 11.1 取り込みを有効にする（`GEOFRAME_INGEST`）

**既定は無効（`0`）。`1`のときだけ、書き込み接続・スキーマの作成と移行・同期サイクル・起動時の掃除が動く。**

| `GEOFRAME_INGEST` | `serve`の動き |
| --- | --- |
| `0`（未設定・空も同じ） | P1と同じ。**DBに一切書かない**（writerを作らない・移行しない・サイクルを回さない・`tmp/`も作らない）。DBが無ければ起動を断る。起動ログに`取り込みは無効です`と`ingest=0`を出す |
| `1` | 起動時に**ガード（§11.1.1）**→ロック→writerを開く→スキーマの作成・移行→`.cache.json`→`.dl_*`の掃除（§11.6）をしてから、読み取りの層を開く。DBが無ければ**空のDBを新しく作る**（warnで知らせる）。起動直後に同期サイクルを1回、以降`GEOFRAME_DRIVE_POLL_MIN`ごと |
| 上記以外（`true`・`2`など） | 起動を断る（終了コード2。値はメッセージに書かない） |

判断の理由:

- **既定を`0`にした**: P5の切替まで本番の書き手はPython（`geoframe-server`）で、本番の`/etc/default/geoframe`をそのままRustの起動に使っても二重に書かない設計にするため。PythonがDBを開いたまま、Rustが移行や`UPDATE`を行うと、一次データ（手動GPS・お気に入り）を壊しうる。書くには明示（`GEOFRAME_INGEST=1`）が要る。既定の見直しはP5の切替のとき（本番の設定に`1`を足す）。
- **空のDBを作る**（P1の「DBが無いと起動を断る」の例外）: 取り込みが有効ならRustが書き手なので、Pythonの`db()`と同じくDBを作る。無効のときは今までどおり断る（配備の誤りを失敗として見せる）。誤って空のDBを静かに作らないよう、作ったときはwarnを出す。データ置き場のディレクトリも無ければ作る（Pythonの`mkdir(parents=True)`と同じ）。
- **二重起動のロック**: `DATA_DIR/.ingest.lock`を排他で取る（`File::try_lock`）。`serve`が取り込み中に`sync`（実行）を起動する、といった**Rust同士の二重書き込み**は断る（終了コード1）。ロックの所有権は`Ingest`だけでなく、書き込みうる2つのスレッド（`sync-worker`・`cache-saver`）も複製（`ingest::LockHold`）を持つ。停止の期限（§11.5）を超えてスレッドが残っても、そのスレッドが終わる（プロセスが終わる）まで`.ingest.lock`は外れないので、残った旧スレッドの書き込みと、新しく起動した`sync`／`serve`が重ならない（`Ingest`を捨てた直後に外れていた、レビュー3回目の指摘の修正。試験は、止まったままの保存スレッドを模した`cli::tests::the_ingest_lock_stays_while_a_stuck_saver_remains`）。**Pythonとの二重書き込みはロックでは防げない**（Pythonはこのロックを見ない）。代わりにP5まではガード（§11.1.1）が、起動時に1回だけ、検証環境の印とPythonの非稼働を確かめる。起動したあとにPythonが起動された場合は検知しない。ドットファイルなので静的配信は403を返す。`--dry-run`はロックを取らず、動いている最中でも見られる。

#### 11.1.1 本番への誤指定の防止（ガード）

**P5の切替までの暫定。** `GEOFRAME_INGEST=1`は、設定の誤りや直接の起動だけで、本番で動いているPython（`geoframe-server`）の足元に書き込みを始められる。レビューでは、Pythonが接続を持ったままRustを起動すると成功し、開いている`.dl_*`まで消された（Pythonのダウンロードを壊す・`.cache.json`の更新が競合する）。配布スクリプトの拒否は、直接の起動や`/etc/default/geoframe`の書き換えには効かない。そこで**バイナリ自身が**、最初の書き込み（データ置き場の作成・ロック・移行・掃除・キャッシュの保存）の前に次の両方を確かめる（`guard.rs`。`ingest::start`の先頭。`serve`も`sync`の実行もここを通る）。

1. **検証環境の印**`/etc/geoframe/staging`が通常ファイルとしてある（リンクは不可。中身は見ない）。`deploy/rust-deploy.sh`が使う印と同じで、扱いも同じ。本番機には印が無いので、ここで必ず止まる。
2. **`geoframe-server.service`が非稼働と確認できる**。`systemctl show --property=LoadState,ActiveState`の結果の**`ActiveState`が`inactive`／`failed`のときだけ**通す（Pythonが入っていない機体は`LoadState=not-found`と`ActiveState=inactive`になるので、そのまま通る）。**`LoadState`では判断しない**: unitファイルを消して`daemon-reload`しても、動いているサービスは動き続けうる（systemdでは`LoadState`と`ActiveState`は独立で、`not-found`と`active`は両立する）ので、`not-found/active`も断る。稼働中（`active`・`activating`・`reloading`・`deactivating`）・未知の状態・`systemctl`が無い／起動できない／5秒以内に答えない／異常終了／出力が空・欠けている、は**全て断る**。`LoadState=`・`ActiveState=`は**それぞれちょうど1行で値が空でないときだけ**受け入れ、重複（同じ値の重複も）は断る（先頭の行だけを使うと、`ActiveState=inactive`のあとに`ActiveState=active`が続く矛盾した出力を「止まっている」と取り違えるため。配布スクリプトの`unit_state`も同じ）（判定できないから素通し、にはしない。unitの制限下で`systemctl`が使えない機体では、取り込みは動かない）。

断ったときは何も書かずに**終了コード2**で終わる（`serve`は`failed`で止まり、`RestartPreventExitStatus=2`により再起動ループにならない）。メッセージは理由（印の有無・Pythonの状態名）だけで、値は含まない。通ったときは`[guard]`でinfoを1行出す。

- 検査は**起動時の1回だけ**。そのあとにPythonが起動された場合は検知しない（P5までは、検証環境にPythonの本番unitは無い）。
- `sync --dry-run`は**ガードを通さない**。行・写真・キャッシュ・ロックを書かない（SQLiteのWAL関連ファイルだけはできうる）ので、本番機でも件数を見られるようにする。読み取り専用の`serve`（`GEOFRAME_INGEST`が`0`）も同じ。
- **試験用の差し替えは、デバッグビルドだけ**: `GEOFRAME_TEST_STAGING_MARKER`（印のパス）と`GEOFRAME_TEST_PYTHON_STATE`（`<LoadState>/<ActiveState>`。`unavailable`は取得失敗）。`#[cfg(debug_assertions)]`の内側にあるので、配布するリリースビルド（musl）にはコードが無く、環境変数は無視される。差し替えても「印が要る」ことは外れない（指した先に通常ファイルが無ければ断る）。
- **P5で外す**手順は[plan.md P5](plan.md#p5-切替pr-6)。外すときに消すもの: `guard.rs`と`lib.rs`の`pub mod guard`、`ingest::start`の`start_with`／`guard.check()`、`IngestError::Refused`と`cli.rs`の`exit_for_ingest_error`、`tests/ingest_cli.rs`の`Staging`と`GEOFRAME_TEST_*`、`USAGE`・本節・§0の行・§6の項・`deploy/README.md §7.1`・`deploy/geoframe-rs.service`の記述。

### 11.2 設定

| 変数 | 既定 | 解釈 | 棚卸しとの対応 |
| --- | --- | --- | --- |
| `GEOFRAME_INGEST`（Rustだけ） | `0` | `0`／`1`だけ（他は起動を断る）。空文字は未設定 | [inventory.md §3.1](inventory.md) B0・B1（起動時の準備と同期サイクル）を動かすか。Pythonには無い |
| `GEOFRAME_DRIVE_POLL_MIN` | `10` | 分。小数も可（`0.5`は30秒）。Pythonは`float()`で秒に換算。**Rustは0以下・非有限・上限（525600分＝1年）超を起動の断りにする**（Pythonは0以下で空回りか例外） | [inventory.md §4.1](inventory.md)の`GEOFRAME_DRIVE_POLL_MIN`（P2・引き継ぐ） |

`GEOFRAME_DRIVE_POLL_MIN`は`UNSUPPORTED_ENV`から`SUPPORTED_ENV`へ移した。`GEOFRAME_INGEST`は`GEOFRAME_LOG`と同じRustだけの項目なので`docs/reference/configuration.md`（Pythonの`config.py`から生成）には無く、どちらの一覧にも載せない。`GEOFRAME_PHOTOS_DIR`・`GEOFRAME_FETCH_PER_CYCLE`はT2.3で読み始めた（§13.1）。`GEOFRAME_DRIVE_KEY`・`GEOFRAME_DRIVE_FOLDER`・`GEOFRAME_QUALITY_PER_CYCLE`・`GEOFRAME_INFER_OFFSET_MIN`は、段の中身を入れるT2.4〜T2.6で読み始める。

### 11.3 同期サイクルの骨組み

現行の`catalog.py:cycle()`の順に、**段（`Stage`）を並べた**もの。段は`index_local`→`index_drive`→`infer`→`enrich`→`local_derivatives`→`fetch_derivatives`→`quality`。T2.1の段は何もしない（`Placeholder`）。

- **起動直後に1回、以降`GEOFRAME_DRIVE_POLL_MIN`ごと**。専用のstdスレッド`sync-worker`で動き、HTTPサーバーの起動を待たせない。SIGTERMで`StopFlag`を立て、待ちの最中ならすぐ、サイクルの最中なら今の段が終わってから止まる（最大10秒待ち、超えたらそのまま終了する。途中で止まってもDBはトランザクション単位なので壊れず、`.dl_*`の残骸は次回の起動で消す）。
- **段ごとに失敗を隔離する**: 段が`Err`を返しても、パニックしても、残りの段と次の周期へ進む。ログに出すのは段の名前と失敗の種類だけ（`StageError`はDB・I/Oの種類と定型文しか持たない。パニックのメッセージは出さず、場所だけをpanic hookが出す）。Pythonの`index_drive`段の「Drive認証に失敗したらDBの段だけ飛ばす」は、段が`Skipped(理由)`を返す形で表す（T2.4）。
- **ログは件数だけ**: 段ごとの件数（`added=3`のような固定の名前と数）と、サイクル末尾の`表示可能／取得待ち／全`（Pythonの`[cycle]`行と同じ集計）。
- **サイクルの前に、そのサイクル用の読み取り専用の接続を開く**（開けなければ次の周期へ持ち越す）。段は読むときは`ctx.read`、書くときは`ctx.writer()`（writer）を使う。

#### `--dry-run`

`geoframe sync --dry-run [--data-dir <dir>]`は、**書かずに「何をするか」の件数だけを出す**。書き込み接続を作らない（`ctx.writer`が`None`）ので、DBへの書き込みは構造的に起きない。キャッシュは凍結して読むだけ。**行・写真・キャッシュ・ロック・`tmp/`・`derivatives/`は変えないが、SQLiteが読み取りでも作る`catalog.db-wal`・`catalog.db-shm`は新しくできうる**（WALのDBを開くとできる。中身は書かれない。ヘルプ（`USAGE`）と起動の1行目にも書いてある）。**ガード（§11.1.1）は通さない**: 書かないので、本番機でも件数を見られるようにしてある。出すもの:

```
dry-run: 何も書きません（行・ファイル・キャッシュ・ロックは変えません。SQLite の WAL 関連ファイルだけはできうります）
スキーマ: 作る表・索引 N / 足す列 N / place を NULL に戻す あり|なし / embedding の写し N
掃除: 消す .dl_* N 件 / 捨てる .cache.json の旧キー N 件
段 <名前>: 完了 <件数>|省略（理由）|失敗: …
写真: 表示可能 N / 取得待ち N / 全N
```

P2の受入（取り込み済みの本番コピーに対して、行の変更・derivativeの再取得がほぼ0）は、段の中身が入ったあと、この`--dry-run`で確かめる。DBが無ければ作らずに終了コード1で終わる。`tests/ingest_cli.rs`が、DB・`.cache.json`・`tmp/`・`derivatives/`のバイト列が変わらず、`.ingest.lock`も作られず、増えるファイルは`-wal`・`-shm`だけであることを確かめる。

#### `geoframe sync`（実行）

同期サイクルを1回だけ動かして終わる（棚卸し §5 の「手動トリガー」）。`GEOFRAME_INGEST=1`が要り、無ければ終了コード2で断る。**ガード（§11.1.1）も`serve`と同じに通し、断られたら書かずに終了コード2**。ロック・writer・移行・掃除は`serve`と同じ（§11.1）。終了コードは、失敗した段があれば1、なければ0。

### 11.4 writer

`writer.rs`の`Writer`。**単一の読み書き接続を専用スレッド`db-writer`に置き、チャネルで依頼を受ける**（Pythonの「1接続＋グローバルロック」と同じ直列化を、ロックではなく所有で表す）。

- `Catalog::with_writer`・`Catalog::writer()`で`Catalog`が持つ（取り込みが無効なら`None`）。読み取り側（`Catalog::read`・プール）は変えていない。
- `writer.call(|conn| …)`は呼び出しスレッドをブロックして、依頼を書き込みスレッドで実行し、結果を返す。tokioのworkerから呼ぶ（P3の更新API）ときは`spawn_blocking`に包む。
- 依頼が終わったとき、トランザクションが開いたままなら`ROLLBACK`し、**自動コミットの状態に戻ったことを確かめてから**、結果を返信する（返信より後始末が先）。依頼がパニックしても接続は生きたまま（`ROLLBACK`して`WriteError::Task`を返す）。
- **`ROLLBACK`できない・自動コミットに戻らないときは、その接続を捨ててwriterを止める**。その依頼の結果は（成功していても）`WriteError::Task`に差し替え、以後の依頼（キューに残っていたものも）は実行せず`WriteError::Task`を返す。再接続はしない（プロセスの再起動で直す。ログに`書き込み接続のトランザクションを戻せません`が出る）。判断の理由: ログだけで同じ接続を使い続けると、失敗した依頼のトランザクションを次の依頼が引き継ぎ、後続の`COMMIT`で失敗した依頼の行まで確定してしまう（レビューで故障注入により再現）。安全な再接続は、開いている読み取り側との整合・`busy_timeout`の扱いが増えるので、writerが壊れること自体が通常は起きない（`ROLLBACK`は実質失敗しない）ことから、止めて見せる方を選んだ。
- `busy_timeout`は10秒、WAL。`WriteError`の文言はSQLiteのメッセージ（表名・列名まで）と定型文だけで、SQL・値・パスは入れない（テストで確認）。

### 11.5 `.cache.json`

`cache.rs`。仕様は[inventory.md §3.2](inventory.md)・B0・B7のとおり。

- 起動時に読み（無い・壊れている・オブジェクトでないときは空から）、`geo:`・`geocode_osm:`・`muni_table`以外のキーを捨てる。捨てたものがあれば**即保存**する。
- 変更があり、前回の保存から60秒以上たっているときだけ書く（10秒ごとに起きる`cache-saver`スレッド）。一時ファイル`.cache.json.tmp`に書いて`fsync`し、`rename`で差し替える（Pythonは`fsync`しない）。失敗したら次のtickで再試行する。停止時にも最後の保存をする（Pythonはしない）。**最後の保存は、同期サイクルのスレッドの終了を待ってから1回だけ**行う（`cli::stop_ingest_threads`。`cache-saver`は停止の合図で終わるだけ）。同じ合図で両方を起こして保存側が先に最後の保存を終えると、あとから書く側の更新が失われるため。サイクルのスレッドが10秒の上限内に止まらないときは警告を出して、その時点のキャッシュを保存して終了する。
- **保存の全体（スナップショット〜一時ファイルへの書き込み〜`fsync`〜`rename`）は、保存用の排他で1つずつ**にする。保存中は状態のMutexを持たず、一時ファイル名も共通なので、`cache-saver`がディスクI/Oの最中に停止の期限を超えて残ったまま最後の保存に進むと、truncate・write・renameが競合して更新の欠落やJSONの破損が起こりうる。最後の保存（`Cache::save_final`）は排他を**期限（10秒）付き**で取り、取れなければ**最後の保存を省略して警告を出す**。無期限に待つと停止が終わらなくなる点と、重ねて書いて壊すより直近の更新（地名の逆引き結果で、次の同期で作り直せる）を失うほうが軽い点から、省略を選んだ。saverが期限内に終わる遅さなら、終わるのを待ってから保存する。
- **停止にかかる時間（上限は保証しない）**: 期限があるのは次の待ちだけで、合計は最大約35秒（サイクルのスレッド10秒＋保存スレッド10秒＋最後の保存の排他10秒＋tokioの終了待ち5秒）。これとは別に、**排他を取ったあとの書き込み・`fsync`・`rename`には期限がない**（ディスクが固まれば、その分だけ延びる）。したがって「何秒以内に最後の保存まで届く」とは言えない。unitは`TimeoutStopSec`を書いていない（systemdの既定90秒）ので、通常は期限付きの待ちと書き込みが収まるが、収まらなければ`SIGKILL`で終わり、最後の保存は失われる（一時ファイルへの書き込みなので`.cache.json`自体は壊れない）。
- **残ったスレッドと`.ingest.lock`**: 停止の期限を超えて`sync-worker`・`cache-saver`が残っても、スレッドが`ingest::LockHold`を持つ間は`.ingest.lock`が外れない（§11.1のロック）ので、別の`sync`／`serve`は起動を断られ、`Cache`が別々の排他を持つ2つのプロセスが同じ一時ファイルを書くことはない。**一時ファイル名をプロセスごとに一意（pidなど）にすることは見送った**: ロックで同時に動くのは1プロセスだけになり、名前を変えても保護が増えない一方、途中で落ちたプロセスの`.cache.json.tmp.<pid>`が残り続けて掃除の仕組みが要る。単一名なら次の保存が上書きする。
- 試験は、書き込み処理に待ちを差し込める内部の入口（`save_with`。本番の呼び出しは常に実際の書き込みを渡す）で、saverの固まりと期限切れの省略・期限内に終わる遅れを再現する。
- 値は`serde_json::Value`のまま持つので、`geo:`の中身は往復で欠けない。書き出しはPythonの`json.dumps(ensure_ascii=False, indent=1)`と同じ形（インデント1、日本語はそのまま）。**違いはキーの並びだけ**: Rustは辞書順、Pythonは挿入順（§0の索引）。どちらが書いたファイルも、もう一方が読める。
- 座標を3桁に丸めた値がキーに入るので、ログには件数しか出さない。テストは合成の値だけ。

### 11.6 起動時の掃除

`ingest::prepare_tmp_dir`（`drive.py:prepare_tmp_dir`と同じ）。`DATA_DIR/tmp/`を用意し、`tmp/`と`derivatives/`の`.dl_*`を消す。フル解像度・EXIF完全保持のDrive原本が残らないようにするため。消すのは**通常のファイルだけ**（ディレクトリ・シンボリックリンクは触らない。消せないものは無視）。名前はログに出さず、件数だけ（`[fetch] 一時ダウンロードの残骸を…`）。`tile_cache_prune`（B4）はP3。T2.3で、続けて`derivatives/`の書きかけの`*.part`も消すようにした（[§13.5](#135-書きかけの-part-の掃除)）。

### 11.7 スキーマ（`schema.rs`）

[inventory.md §2.4](inventory.md)のM0〜M7を、Pythonの`store.py:db()`と同じ結果になるように行う。要点:

- **空のDBから作る**: `photos`の定義に`muni_cd pref city favorite hidden rotation`を最初から含める。`sqlite_master.sql`の文字列まで、Pythonが空のデータ置き場から作ったDB（`ALTER`で列が足された形）と一致する。テスト`fresh_database_matches_the_python_one`が、`tools/gen_schema_fixture.py`の出力（`tests/fixtures/schema-python.json`）と、`sqlite_master`の全行・各表の`table_xinfo`・索引を比べる。Pythonの`SCHEMA`を変えたらこのテストが落ちる（再生成は`python3 tools/gen_schema_fixture.py`、`--check`で差を検査）。
- **既存のDB（本番相当）では何もしない**: 列は`PRAGMA table_info`で有無を見て、無い列だけを`ALTER`する（Pythonは`ALTER`の失敗をすべて握りつぶすが、Rustはロックなどの本当の失敗を返す）。**M1（`UPDATE photos SET place=NULL`）は`muni_cd pref city`のどれかを足したときだけ**起きる。列が既にあるDBでは決して起きない。テスト`migration_is_a_no_op_on_a_production_shaped_database`が、Pythonの形のDBで、構造・`place IS NULL`の件数・`schema_version`・`total_changes()`・ファイルのバイト列が変わらないこと、`pending`（`--dry-run`）も何もないと答えることを示す。旧形のDBでは列を足して`place`を戻すこと（`migration_from_the_old_shape_adds_columns_and_resets_places_once`）、M2の列だけが無いDBでは戻さないこと（`adding_the_flag_columns_alone_does_not_reset_places`）も試験する。
- **M7（`photo_embeddings`への`INSERT OR IGNORE`）はPythonと同じく毎起動**行う。行が無ければ書き込みは起きない（`total_changes()`が増えない）。`--dry-run`は挿入されるはずの行数を数える。**`photo_embeddings`の表がまだ無い旧DBでも**、実際の移行は表を作ってから実行するので、作成後に入る件数（`photo_ai_features`の`error=''`の`(content_sha256, embedding_model)`の重複を除いた数）を数える（既にある表なら、そこに無いものだけ）。
- 全体を`BEGIN IMMEDIATE`の1トランザクションに入れる（Pythonは途中で落ちると中途半端に移行されうる）。`journal_mode=WAL`はトランザクションの外。
- 列の**並び**は移行で変えない（本番のDBは`infer_dt_s`が末尾など、空のDBと並びが違いうる）。読み取りの層は列名で読むので影響しない。

### 11.8 unit

[`deploy/geoframe-rs.service`](../../deploy/geoframe-rs.service)の`ReadOnlyPaths`から`catalog.db`と`derivatives`を外した（§6）。`MemoryDenyWriteExecute=true`は、HTTPS（rustls・ring）を入れたあとも付けたまま動くかを検証環境のsystemd配下で確かめる（§11.9）。

### 11.9 テスト

合成のDB・`.cache.json`・ループバックのスタブのHTTPサーバーだけを使う（実際の通信は検証環境の確認のみ）。

| 対象 | テスト |
| --- | --- |
| スキーマ（`schema.rs`） | Pythonの空DBとRustの空DBの`sqlite_master`・`table_xinfo`・索引の一致、本番相当のスキーマで何もしない（構造・`place`・`schema_version`・`total_changes()`・ファイルのバイト列）、旧形の移行、M2の列だけが無いときの`place`、M7、`pending`（読み取り専用の接続で動く） |
| writer（`writer.rs`） | 同じスレッド・同じ接続、並行する呼び出しの直列化、開いたトランザクションの`ROLLBACK`、**`ROLLBACK`の失敗の故障注入（後始末を差し替える。失敗した依頼の行が確定せず、後続の依頼が実行されず、writerが止まる。成功した依頼も後始末に失敗したら成功を返さない）**、失敗とパニックのあとも使えること、エラー文言に値・SQLが入らないこと、DBを作る・作らない、全ての`Writer`を落としたときの接続の終了 |
| 停止の順序（`cli.rs`） | サイクルのスレッドが最後に書いた更新が、最後のキャッシュの保存に入ること |
| 同期サイクル（`sync.rs`） | 段の順、失敗・パニックの隔離、dry-runで`writer`が無いこと、停止の合図、起動直後に1回＋周期ごとの繰り返し、段がパニックしてもスレッドが続くこと |
| `.cache.json`（`cache.rs`） | Pythonの`json.dumps(indent=1, ensure_ascii=False)`と同じ出力、旧キーの破棄と即保存、60秒の間引き、失敗時の再試行、凍結、保存スレッド（最後の保存は呼び出し側）、保存の排他と期限付きの最後の保存 |
| ガード（`guard.rs`） | 印が無い・ディレクトリ・リンクは断る、`systemctl show`の出力の厳密な解釈（欠け・空・異常終了・出力なしは断る）、稼働中・不明な状態は断る、時間内に答えない`systemctl`は止めて断る、デバッグビルドの差し替えが「印が要る」ことを外さないこと |
| 起動処理（`ingest.rs`） | 断られたら置き場・DB・ロックが作られず中身も変わらないこと、 `.dl_*`の掃除（通常ファイルだけ。リンク・ディレクトリは触らない）、DBの作成、ロック、再起動で何も変わらないこと、`plan`が書かないこと |
| バイナリ（`tests/ingest_cli.rs`） | `sync --dry-run`が本番相当のDB・`.cache.json`・`tmp/`・`derivatives/`のバイト列を変えず`.ingest.lock`も作らないこと、DBが無ければ作らないこと、`GEOFRAME_INGEST`が無いと`sync`も`serve`も書かないこと、`serve`が取り込みつきで起動してSIGTERMで正常終了すること、二重起動の拒否、**印が無い・Pythonが稼働中・状態が取れないとき、`sync`も`serve`も書き込みの前に終了コード2で止まり、置き場（DB・`.cache.json`・`tmp/`・`derivatives/`・`.ingest.lock`）が一切変わらないこと**、`--dry-run`と読み取り専用の`serve`はガードの対象外であること。取り込みを動かすテストは、デバッグビルドだけの差し替え（`GEOFRAME_TEST_STAGING_MARKER`・`GEOFRAME_TEST_PYTHON_STATE`）で印とPythonの状態を与える |
| JWT・HTTPS（`jwt.rs`・`net.rs`） | [build.md §7](build.md) |

### 11.10 検証環境での確認（2026-10-24）

n100でビルドしたmusl静的バイナリ（`static-pie linked`、`ldd`は`statically linked`）を`deploy/rust-deploy.sh`で検証環境へ配布して確かめた（検証環境の印とPython版の非稼働の確認を通った）。検証環境の`catalog.db`は本番の2026-10-24 06:17のバックアップのコピー（6,535行）。**書き換える前に、検証環境の中で`sqlite3 .backup`のコピーを取った**。

| 確認 | 結果 |
| --- | --- |
| (a) 外向きHTTPS | `geoframe selfcheck-net`が地理院へ**成功**（名前解決・TLS・HTTP 200・JSON）。unitと同じ`NoNewPrivileges`・`ProtectSystem=strict`・`ProtectHome`・`PrivateTmp`・`SystemCallFilter=@system-service`・`MemoryDenyWriteExecute=true`を`systemd-run`で与えて`geoframe`ユーザーで実行しても成功（rustls・ringを入れても`MemoryDenyWriteExecute`のまま動く） |
| (b) 移行が何もしない | `GEOFRAME_INGEST=1`（drop-in）で起動し直したところ、ログは`[db] スキーマは最新です`。**`sqlite3 .dump`のSHA-256が配布前のコピーと一致**、`.schema`も一致、`schema_version`も同じ、`photos`は6,535行のまま、`place IS NULL`は**1,173行のまま**。再起動をもう一度しても、`geoframe sync`（実行）を止めた状態で1回動かしても同じ。取り込み中の`serve`に対して`sync`を起動すると断られる（終了コード1） |
| (c) `--dry-run` | 本番コピーに対して、`スキーマ: 作る表・索引 0 / 足す列 0 / place を NULL に戻す なし / embedding の写し 0`、`掃除: 消す .dl_* 0 件 / 捨てる .cache.json の旧キー 0 件`、全7段が`完了（件数なし）`、`写真: 表示可能 5288 / 取得待ち 0 / 全6535`。`geoframe`ユーザーで実行した（取り込み中の`serve`と同時に動かせることは`tests/ingest_cli.rs`で確かめた） |
| (d) 応答がP1と変わらない | `tools/compare_with_python.py`（比較用Python :8090と全件比較）が、取り込み無効の配布直後と`GEOFRAME_INGEST=1`のときの**両方で、想定外の差0項目**（写真一覧3,065件のJSON完全一致・並びも一致、管理画面の読み取りAPI 222通り、`/derivatives/`のバイト一致3,065件ほか） |
| 後片付け | drop-inを外して取り込み無効（既定）に戻した。`.ingest.lock`・`tmp/`が検証環境のデータ置き場に残る。配布前のコピーは`/var/lib/geoframe-backup-t21/`に残してある |

本番コピーでは、取り込み済みの6,535行すべてが`source='drive'`で、段の中身がまだ無いので、サイクルは件数を数えるだけ。行の変更・derivativeの再取得が0であること（P2の受入）は、T2.3〜T2.6で段の中身を入れたあと、同じ`--dry-run`で確かめる（T2.3の分は§13.9(a)）。

### 11.11 検証環境での確認: ガード（レビュー指摘後。2026-10-24）

指摘修正のバイナリ（musl静的。リリースビルドに`GEOFRAME_TEST_*`の文字列が無いことも確認）を`deploy/rust-deploy.sh`で配布し、検証環境のsystemd配下（unitのハードニングのまま）で確かめた。いずれも`sqlite3 -readonly … .dump`のSHA-256（配布前に取ったもの）と、データ置き場のファイル一覧（サイズ・更新時刻）の比較で、書き込みの有無を見た。

| 確認 | 結果 |
| --- | --- |
| (a) 印あり・`geoframe-server`は未導入（`LoadState=not-found`）で`GEOFRAME_INGEST=1`（drop-in） | 今までどおり起動。ログは`[guard] 検証環境の印を確認しました。geoframe-server.service は 未導入です`→`[db] スキーマは最新です`→サイクル完了（表示可能5288／全6535）。`.dump`のSHA-256は配布前と一致。**unitの制限下でも`systemctl show`が動いた** |
| (b) 印を`staging.off`へ改名して起動 | `ERROR 検証環境の印（…）が無いので…`で終了コード2、`failed`、`NRestarts`は0（再起動ループにならない）。**置き場のファイル一覧・`.dump`は変わらず、開いたままの`.dl_*`（`derivatives/`に置いた）も消えない**。印を戻すと取り込みが通り、`.dl_*`は掃除された（対照） |
| (c) 印あり・`geoframe-server.service`（確認用のダミーunit）を`active`にして起動 | `ERROR Python 版（geoframe-server.service）が非稼働と確認できない（状態: active）…`で終了コード2、置き場は変わらず、開いたままの`.dl_*`も消えない（`sudo test -e`で確認）。確認後、ダミーunitは止めて撤去（`LoadState=not-found`に戻した） |
| 比較用Python | `geoframe-pyref.service`（別データ・別ポート）が`active`でも、判定は`geoframe-server.service`だけを見るので通る（(a)） |
| 後片付け | drop-inを外して`ingest=0`（`取り込みは無効です`）に戻し、`/healthz`は200、`.dump`のSHA-256は配布前と一致。印は戻してある |

**レビュー2回目の修正後（`LoadState`を見ず`ActiveState`で判定）。** 同じ手順（`rust-deploy.sh`で配布→`GEOFRAME_INGEST=1`のdrop-in）で、`.dump`のSHA-256（配布前に取ったもの）と比べた。

| 確認 | 結果 |
| --- | --- |
| (a) 印あり・`geoframe-server`は未導入で`GEOFRAME_INGEST=1` | 実際の`systemctl show geoframe-server.service`は**`LoadState=not-found`・`ActiveState=inactive`**。`[guard] 検証環境の印を確認しました。geoframe-server.service は 未導入です`→`[db] スキーマは最新です`→サイクル完了。`.dump`のSHA-256は配布前と一致 |
| (b) ダミーunit（`/run/systemd/system`に置いた`sleep`）を`active`にして起動 | `LoadState=loaded`・`ActiveState=active`。`ERROR Python 版（geoframe-server.service）が非稼働と確認できない（状態: active）…`で終了コード2、`NRestarts`は0 |
| (b') (b)の続きで、unitファイルを消して`daemon-reload`（動いているサービスは残る） | **`LoadState=not-found`・`ActiveState=active`**になる（両立することを実機で確認）。この状態でも同じ拒否（終了コード2）。`.dump`のSHA-256は変わらない |
| 後片付け | ダミーを停止し`/run`の unit を撤去（`not-found/inactive`に戻した）。drop-inを外して`ingest=0`（`取り込みは無効です`）に戻し、`/healthz`は200、`.dump`のSHA-256は配布前と一致。印は戻してある |

**レビュー3回目の修正後（`.ingest.lock`をスレッドが持つ・状態行の重複を拒否）。** 同じ手順（`rust-deploy.sh`で配布→`GEOFRAME_INGEST=1`のdrop-in）で確かめた。`sqlite3 -readonly … .dump`のSHA-256は配布前に取ったものと比べた。

| 確認 | 結果 |
| --- | --- |
| (a) 印あり・`geoframe-server`は未導入で`GEOFRAME_INGEST=1` | `[guard] 検証環境の印を確認しました。geoframe-server.service は 未導入です`→`[db] スキーマは最新です`→サイクル完了（表示可能5288／全6535）。`.dump`のSHA-256は配布前と一致 |
| (b) 取り込み中の`serve`に`sync`（実行）を起動 | `別の geoframe が取り込み中です`で終了コード1（ロックは`serve`が持ち続けている） |
| (c) 停止・再起動 | `systemctl stop`は約0.1秒で`inactive`、ログは`停止しました`。`start`・`restart`とも`active`で、`ERROR`／`WARN`は0件。`.dump`のSHA-256は変わらない |
| 後片付け | drop-inを外して`ingest=0`（`取り込みは無効です`）に戻し、`/healthz`は200、`.dump`のSHA-256は配布前と一致 |

## 12. 画像処理（P2 / T2.2）

derivative（表示用の長辺2560pxのJPEG）の生成、EXIF（撮影時刻・GPS）の読み取り、HEIC、`DATA_DIR`の外へ出ないパスの検査。実装は`crates/geoframe/src/imaging/`（`derivative.rs`・`exif.rs`・`heic.rs`・`probe.rs`）と`datadir.rs`。部品の選択と計測は[image-poc.md](image-poc.md)、依存・サイズは[build.md §8](build.md)。**PoC（`crates/geoframe-imgpoc`）をここへ移して削除した**。

### 12.1 取り込みの段から呼ぶ関数

**同期API**で、CPUを食うので、同期サイクルのstdスレッドから1枚ずつ呼ぶ（tokioのworkerでは呼ばない）。エラー型（`ImageError`）の`Display`は**パス・ファイル名・画像の内容を含まない**（定型文、固定の語、`io::ErrorKind`、寸法、HEICの構造の分類だけ）ので、そのままログに出してよい（[inventory.md §6.3](inventory.md)）。

| 関数 | 対応するPython | 呼ぶ段 | 内容 |
| --- | --- | --- | --- |
| `imaging::derivative_name(drive_id, rotation)` | `util.derivative_name` | T2.4・管理画面（P3） | `{id}.jpg`、回したものは`{id}.r{rotation}.jpg`。IDは英数字・`_`・`-`だけ（区切りを含められない） |
| `imaging::store_derivative(&DataDir, src, name, rotation)` | `util.make_derivative` ＋ `dest.relative_to(DATA_DIR)` | T2.4（`fetch_derivatives`）・T2.3（`local_derivatives`） | `DATA_DIR/derivatives/<name>`に作り、DBの`display`に入れる`derivatives/<name>`と生成の内容（`DerivativeInfo`）を返す |
| `imaging::make_derivative(src, dest, rotation)` | `util.make_derivative` | 上の下請け・テスト | 保存先を検査しない低レベル版。`dest`は`DataDir`で検査したものを渡す |
| `imaging::extract_exif(path)` | `util.extract_exif` | T2.3（`index_local`） | `PhotoMeta { lat, lon, taken_at }`。ヘッダだけ読む（JPEGは先頭4 MiBだけ） |
| `imaging::extract_exif_info(path)` | （向きも） | 同上 | `ExifInfo { meta, orientation }` |
| `imaging::norm_time(s)` | `util.norm_time` | T2.3・**T2.4（Driveの`imageMediaMetadata.time`）** | `YYYY-MM-DD HH:MM` |
| `imaging::is_supported_ext(path)` | `p.suffix.lower() in EXTS` | T2.3 | `.jpg .jpeg .png .tif .tiff .heic .heif` |
| `imaging::is_browser_displayable_ext(path)` | `suffix in (".jpg", ".jpeg", ".png")` | T2.3（`local_derivatives`） | 原本をそのまま`display`にしてよい拡張子 |
| `DataDir::open(dir)` | — | 各段の入口 | `canonicalize`して持つ |
| `DataDir::resolve_existing(rel)` | `path.resolve().relative_to(DATA_DIR)` | T2.3（`photos.path`）・読み出し全般 | DBの相対パス文字列を、`DATA_DIR`の中の実在ファイルの絶対パスにする |
| `DataDir::download_tmp(drive_id)` | `TMP_DIR / f".dl_{id}"` | T2.4 | `tmp/.dl_<id>`（`tmp/`を作る） |
| `DataDir::relative(abs)` | `relative_to(DATA_DIR).as_posix()` | 各段 | `/`区切りの相対パス |
| `imaging::probe`（`geoframe img-probe`） | — | 確認・計測 | §12.6 |

呼び出しの例（T2.4の`fetch_derivatives`）:

```rust
let data = DataDir::open(&config.data_dir)?;
let tmp = data.download_tmp(&drive_id)?;           // ID が不正ならここで断る
let result = download_to(&tmp)                      // 原本を tmp/.dl_<id> へ
    .and_then(|_| store_derivative(&data, &tmp, &derivative_name(&drive_id, rotation)?, rotation));
let _ = std::fs::remove_file(&tmp);                 // 成功でも失敗でも EXIF 付き原本を残さない
```

### 12.2 derivativeの手順とPythonとの差

手順（Pythonの`make_derivative`と同じ）: 読む → EXIFの向きを当てる → 手動回転（`rotation`、時計回り）→ 長辺を2560pxに収める（拡大しない）→ RGBかL以外はRGBへ → JPEG品質85 → `.part`に書いて`rename`。

| 項目 | Python | Rust |
| --- | --- | --- |
| 読む形式 | Pillowが内容で判別（EXTSは拡張子の集合） | JPEG・PNG・TIFF・HEICを**内容で**判別（拡張子は見ない）。それ以外は`NotAnImage` |
| JPEGのデコード | libjpeg(-turbo)。途中で切れていればエラー | `zune-jpeg`の厳格モード。**途中で切れていれば失敗**（`image`の`JpegDecoder`は部分的な画像で成功するので使わない）。EOIが無ければ失敗。CMYK・YCCKはRGBへ、グレーは1成分のまま（§12.8） |
| 向き | `exif_transpose`（PillowのEXIF。タグが無ければXMPの`tiff:Orientation`）。HEICはlibheifがirot/imirを適用済み | 自前のEXIF読み取り（§12.3）の1〜8。範囲外・無しは当てない。EXIFにタグが無いときだけXMPの向きを使う（§12.3）。**HEICはデコーダが適用済みなのでEXIFの向きもXMPの向きも使わない**（二重に回らない） |
| 順序 | 向き→縮小 | **縮小→向き**（同じ結果で速く、メモリも減る。寸法は向きを当てた後の長辺で決める） |
| 縮小の寸法 | `thumbnail((2560, 2560))` | `target_dims`。Pillowと同じ丸め（縮める側と反対の辺は、縦横比の誤差が小さい方へ切り捨て／切り上げ）。**Pillowの`thumbnail`と170件で一致**（`tools/gen_thumbnail_fixture.py`が基準、CIで`--check`） |
| 縮小のフィルタ | BICUBIC（`reducing_gap=2.0`、JPEGは`draft`でDCT縮小もする） | CatmullRom（`fast_image_resize`）。画素は一致しない（§12.9のPSNR） |
| モード | `L`はそのまま、RGB以外はRGBへ（アルファは落とすだけ） | 同じ（グレースケールのJPEG・PNG・TIFFは1成分のJPEGで出す）。アルファは落とす。16ビットは8ビットへ |
| ICC | **捨てる**（画素は変換しない） | **sRGBへ変換して付けない**（意図した変更）。読めない・壊れている・RGB用でないICCは、**警告なしで捨てて続ける**（`DerivativeInfo.icc`が`Dropped`。件数を数えてログに出すのは呼ぶ段） |
| EXIF・GPS | 出力に残らない（`save`にexifを渡さない） | 同じ。エンコーダに渡さないので、JPEG・PNG・TIFF・HEICのどの入力でも残らない（テストが機種名・GPSの文字列とTIFFヘッダの有無を見る） |
| 出力 | JFIFあり、4:2:0、ハフマン最適化なし | 同じ（`jpeg-encoder`。サイズはPythonと同じ水準） |
| 書き込み | `dest.with_suffix(".part")`に保存して`rename`。保存に失敗すると`.part`が残りうる。既存の`.part`がリンクなら先を書き換える | `<出力名>.<pid>.<乱数>.part`（`x.r90.jpg`→`x.r90.jpg.1234.9f3a….part`）を**`O_CREAT\|O_EXCL`＋`O_NOFOLLOW`で新規作成**する（既存のファイル・シンボリックリンクがあれば開けず、名前を変えて8回まで作り直し、それでも駄目なら失敗）。`rename`は出力先がリンクならそのリンク自体を置き換える（先は書き換えない）。**成功・失敗・パニックのどれでも自分の`.part`を残さない**（`Drop`で消す。自分が作ったものだけ）。`rename`の前に`fsync`する（ファイルの中身は保証する。電源断のあとに、DBの`display`が指す先が「中身が空のファイル」になるのを避ける。ディレクトリの`fsync`はしないので、`rename`のエントリ自体は失われうるが、その場合は「指す先が無い」で済む。検証環境で約26 ms／枚。生成は約230 ms／枚なので許容した）。親ディレクトリは作る |
| 入力の上限 | 約1.79億画素で`DecompressionBombError`。ファイルの大きさは無制限 | 同じ画素数（178,956,970。デコードの前に宣言の寸法で断る）。**ファイルは512 MiBまで**（まるごとメモリに読む。`render`・`img-probe`も検査する）。HEICは宣言の長さ・寸法の検査つき（§12.4） |
| 回転 | 任意の角度 | 0・90・180・270（負・360超は剰余）。それ以外は`BadRotation`（管理画面もその値しか作らない） |
| ファイル名 | `{id}.jpg`／`{id}.r{rotation}.jpg` | 同じ。ただし`drive_id`が`[A-Za-z0-9_-]+`でなければ断る |

HEICの`zune-heic`はスレッドを自前で持ち、グリッド画像のタイルや1枚の中の処理を並列に復号する。呼び出し側は**1枚ずつ**呼ぶのを前提にした（検証環境の2コアで、2枚同時にすると、JPEGは処理量が約1.8倍（3.7→6.8枚/秒）になるが最大RSSが144→274 MiBに増え、HEICは約1.08倍（1.05→1.13枚/秒）で321 MiBになる。§12.9）。`GEOFRAME_FETCH_PER_CYCLE`のように1サイクルの件数が小さいので、1枚ずつで足りる見込み。並列にするかはT2.4で決める。

書き込みの最中にプロセスが落ちる（SIGKILL・電源断）と、`derivatives/<name>.<pid>.<乱数>.part`が残りうる。名前に乱数が入るので、次に同じderivativeを作っても上書きされず残り続ける（EXIFを持たない部分的なJPEGなので、個人情報の問題はないが、容量は使う）。**T2.3で、取り込みの起動時の掃除に`derivatives/`直下の`*.part`を足した**（通常ファイルだけ。リンクは消さず辿らない。[§13.5](#135-書きかけの-part-の掃除)）。なお`tmp/.dl_<id>`（T2.4。固定の名前）にも同じリンクの問題があるので、`create_new`＋`O_NOFOLLOW`で作ること（§12.10）。

### 12.3 EXIF（撮影時刻・GPS・向き）

PillowのEXIFの読み方を**境界まで合わせるため**、外部のEXIF crateは使わず、必要なタグだけを自前で読む（`imaging/exif.rs`）。基準は`tools/gen_exif_fixture.py`が作る`tests/fixtures/exif-python.json`（合成のJPEG 43ケースと、Pythonの`extract_exif`の結果。`tests/exif_python.rs`が突き合わせる）。

- **どこから読むか**: JPEGは最初の`Exif\0`で始まるAPP1（SOSの前まで）、PNGは`eXIf`チャンク、TIFFはファイル自身（**BigTIFFは非対応**。`II+\0`・`MM\0+`は`ImageError::BigTiff`で断る。Pythonは開けるが、本番のTIFFは0件で、対応には依存かコードが要るので見送った）、HEICは`zune-heic`の`exif_data()`（TIFF構造から始まる）。見つからなければ「EXIFなし」（位置なし・時刻なし）。
- **撮影時刻**: Exif IFDの`36867`（DateTimeOriginal）、なければ`36868`、なければIFD0の`306`。Pythonの`or`の連鎖と同じで、**Pythonで偽になる値だけが次へ落ちる**。ASCIIの空文字（NULだけも）のほか、数値のタグでは、値0のSHORT・LONGなど、個数0、分子0・分母1の有理数が偽（BYTE・UNDEFINEDは`bytes`なので空でなければ、1バイトの0も真。2個以上の組、分母0の有理数（NaN）も真）。真なら、そこで止まり、`norm_time`が空にする。空白だけ・`0000:00:00 00:00:00`のような文字列も真なので、そこで止まり、`norm_time`が空か`0000-00-00 00:00`にする（`gen_exif_fixture.py`がこの境界を型ごとにPythonの結果で記録している）。ASCIIは**末尾のNULを1つだけ**取り、latin-1として読む（Pillowと同じ。UTF-8の全角数字は文字化けして合わず空になる）。IFD0に誤って置かれた`36867`は無視する。
- **`norm_time`**: 先頭から`\d{4}[:-]\d{2}[:-]\d{2}[ T]\d{2}:\d{2}`に合うときだけ`YYYY-MM-DD HH:MM`にする（暦は見ない。`2024:13:45 25:61:00`→`2024-13-45 25:61`）。秒以降は捨てる。**数字はUnicodeの十進数字（Nd。68の範囲）を通し、ASCIIに直す**（Pythonの`\d`と同じ範囲を読み、元の文字のまま返すPythonと違い、`taken_at`の形式を守る。§0）。
- **GPS**: GPS IFDに`2`（緯度）と`4`（経度）の**両方**があるときだけ。各3つ以上の数値（有理数・整数。先頭の3つを使う）の度分秒を`度 + 分/60 + 秒/3600`で十進にする（Pythonと同じ式・同じ順序）。参照（`1`・`3`）は**緯度も経度も`S`か`W`のとき負**（緯度の参照が`W`でも負になるのはPythonと同じ）。小文字・その他の文字・型が違うものは正。参照が無いときは正（`N`/`E`）。`round(x, 6)`は、Rustの固定小数点の書式（2進の厳密な値を偶数丸め）で同じ値になる。**範囲は検証しない**（`95.0, 200.0`もそのまま。`0, 0`もそのまま位置ありとして返す）。
- **Pythonと違う点**（§0）: 緯度か経度が壊れている（成分が3つ未満、数値でない型、分母0）ときは**位置なし（`lat`・`lon`とも`None`）で、時刻は残す**。Pythonは成分不足・型違いで例外（`index_local`は「読み込み失敗」でその写真を索引しない）、分母0はNaNを保存する（SQLiteではNULLで、`loc_source='exif'`だけが残る）。
- **向き**（Pillowの`getexif()`＋`exif_transpose`と同じ）: IFD0の`0x0112`（SHORT/LONG/BYTEの先頭）。1〜8以外は1。**タグが（値が範囲外でも）あれば、それを使いXMPは見ない**。無いときだけ、XMPの`tiff:Orientation(="|>)([0-9])`の**最初の一致の1桁**を使う（`"16"`は1、属性形式`tiff:Orientation="6"`と要素形式`<tiff:Orientation>6</tiff:Orientation>`は左にあるものが勝つ）。2〜8だけが変換になる。XMPの在りか: JPEGは`http://ns.adobe.com/xap/1.0/\0`で始まるAPP1（複数あれば**最後**。拡張XMPは読まない）、PNGは`XML:com.adobe.xmp`の`tEXt`・圧縮しない`iTXt`（複数あれば最後）、TIFFはタグ700（BYTE・UNDEFINED型）。**読まないもの**: 圧縮した`iTXt`・`zTXt`のXMP（Pillowは展開して読み、Pythonは回す。展開には依存が要るので見送った。意図した差）、HEICのXMP（pillow-heifは開くときにXMPの向きを取り除くので、PillowもHEICのXMPだけの向きは回さない）。基準は`tools/gen_orientation_fixture.py`の`orientation-python.json`（JPEG・PNG・TIFF・HEICの60ケース。四隅に色を付けた140x90の合成画像に向きを付け、Pythonの`make_derivative`の出力の寸法と四隅の色を記録。CIで`--check`）で、`tests/orientation_python.rs`が突き合わせる。
- **壊れたEXIF**: 範囲外のオフセット・巨大な個数・切れたデータは**黙って捨てる**（Pillowも警告して飛ばす）。EXIFが読めないことは画像が読めないことではない。EXIFの区間を1バイトずつ壊す・どこで切ってもパニックしないことをテストしている。
- **画像として読めないファイル**は`ImageError::NotAnImage`（Pythonの`Image.open`の失敗。`index_local`は飛ばす）。Pillowは`.jpg`の名前のGIF・WebPなども開くが、Rustは対象外。

### 12.4 HEICと失敗の分類

`zune-heic` 0.5.0（純Rust）。`irot`・`imir`・`clap`を適用して返す。`mdat`の上限は128 MiB（既定の16 MiBは大きいHEICを拒否する）。主画像だけを取り出す（深度・ゲインマップ・アルファの補助画像は使わない。アルファは捨てる）。ICCは`icc_data()`（`prof`形式）。**nclxだけの色の記述は変換に使わない**（デコーダがnclxでYCbCrからRGBへ変換する分だけ。sRGB以外の原色のnclxは未確認。T2.4）。

**実機のHEICが読めないとき、原因を切り分けられるよう、失敗の文面に*構造の種類*だけを付ける**（`ImageError::Heic`の`Display`。ファイル名・EXIFの値・寸法・座標は出さない。4文字の種別コードは英数字だけに限る）:

```
heicをデコードできません（hevc-bitstream）構造: brand=heic primary=grid codec=hevc profile=2 bit_depth=10 chroma=1 colr=prof aux=gainmap items=52
```

| 項目 | 値 |
| --- | --- |
| 先頭の語（括弧） | `HeicErrors`の種類を畳んだもの: `io`・`truncated`・`unsupported-version`・`box-too-short`・`inconsistent`・`hevc-bitstream`・`box-header`・`buffer`、`ParseError`はボックスの種別から`grid`・`hevc-config`・`item-structure`・`image-property`、`Generic`はメッセージの*語*から`grid`・`unsupported`・`limit`・`missing-box`・`dimensions`・`decode-failure`・`other`（メッセージ自体は出さない） |
| `brand` | `ftyp`のメジャーブランド（`heic`・`mif1`・`msf1`など） |
| `primary` | 主画像（`pitm`）の項目の種類: `single`（`hvc1`など）・`grid`・`overlay`・`identity`・`other`・`unknown` |
| `codec` | `hevc`・`av1`・`other`・`unknown` |
| `profile`・`bit_depth`・`chroma` | `hvcC`から。プロファイル（1=Main、2=Main 10、3=Main Still Picture、4=RExt…）、ビット深度、クロマ形式（0=モノクロ、1=4:2:0、2=4:2:2、3=4:4:4） |
| `colr` | `none`・`prof`（ICC）・`nclx`・`prof+nclx` |
| `aux` | 補助画像の種類（`auxC`のURNの語から）: `alpha`・`depth`・`gainmap`・`other` |
| `items` | 項目の数 |

**ファイルが宣言する長さ・寸法の検査**（`imaging/heic_guard.rs`）。`zune-heic`は、ファイルの中の宣言値で**先にメモリを確保してから**読む。小さなファイルがそれを偽ると、確保に失敗して**プロセスごと`SIGABRT`で落ちる**（Rustの確保失敗は`panic`ではなくabortで、`catch_unwind`で捕まえられない）。レビューで、750バイトの合成HEICのEXIFのextent長だけを1 GiBにすると再現した（`prlimit --as`・musl静的ビルドでも）。そこで`zune-heic` 0.5.0の確保の経路を読んで洗い、デコーダに渡す前に自前で検査する。

| 確保（`zune-heic`） | 宣言値 | 検査 | いつ |
| --- | --- | --- | --- |
| `mdat`本体（`vec![0; size]`。上限128 MiB） | トップレベルのボックスのサイズ | ボックスの宣言サイズが残りのバイト数に収まる | 常に |
| EXIF（`vec![0; length]`） | `iloc`のextentの位置と長さ | 全項目のextentがファイル（構築方法1は`idat`）に収まる | 常に |
| alphaなどの補助面（`vec![0; w*h]`） | `ispe`の寸法 | 画素数が上限（約1.79億）以下 | デコード時 |
| フレーム・タイルの面（`RawFrame`・`BlockMap`。SPSの寸法は最大65,536×65,536で、`ispe`とは独立） | `hvcC`のSPSと、画像データ（`hvc1`の項目）の中のSPS | 画素数が上限以下（SPSをビット単位で読んで寸法だけ見る） | デコード時 |

`ipco`の子ボックスの中身（`colr`・`hvcC`など）は`zune-heic`が自分で10 MiBに、`iinf`・`iloc`・`iref`・`ipma`の個数は137に制限するので、確保が小さく収まり、ここでは見ない。失敗は固定の語（`truncated`・`extent`・`dimensions`・`item-structure`・`box-header`）で返し、値は出さない。**残る限界**: この検査は`zune-heic`のパーサとは別の実装なので、`zune-heic`が自分では拒否しない形でボックスの並びがずれるような壊れ方は、ここでは見えない。また、検査を通った宣言値でも、画素数の上限いっぱい（約1.79億画素）のHEICは1枚で0.5〜1 GiBのメモリを使う（§12.8）。回帰テストは、`iloc`のextent長・`mdat`のサイズ・`ispe`・SPSを偽った合成入力で、検査が断ること（確保に進まないこと）を確かめ、検査を外すと1 GiBの確保でプロセスが落ちることも確認した。

`geoframe img-probe`（§12.6）を実機のHEICに向けると、読めたものは寸法・向き・ICCを、読めなかったものはこの分類を、ファイル名なしで出す。**実機iPhoneのHEIC（グリッド分割・HDRゲインマップ・深度・10ビット）の確認はT2.4**（Driveの鍵が検証環境にまだ無い。§12.10）。

### 12.5 パスの検査（`datadir.rs`）

DBの`display`・`path`列は**信用しない**。Pythonは画像を読む箇所ごとに`resolve().relative_to(DATA_DIR)`で検査している（inventory.md §6.2）のを、Rustは`DataDir`の1か所にまとめた。

1. 文字列として: 空・NUL・絶対パス・`..`の要素を拒否（`.`は読み飛ばす）。判定は文字列のままで、`%2e%2e`のようなデコードは扱わない（Web層`webpath`の仕事）。
2. ファイルシステムとして: `canonicalize`（シンボリックリンクを辿った実体）が`DATA_DIR`（これも`canonicalize`済み）の中であること。外を指すリンクは拒否。
3. 書き込み先（`derivative_dest`）: 名前は`[A-Za-z0-9_.-]`だけ・先頭が`.`でない・`..`を含まない・200文字まで。`derivatives/`がシンボリックリンクなら使わない（`display`の`derivatives/...`と実体が食い違うため）。
4. Driveのファイル名に使うID（`check_drive_id`）: `[A-Za-z0-9_-]`の1〜128文字。DBの行やDriveの応答に区切りが入っていても、`derivatives/`・`tmp/`の外へ出られない。

検査と使用のあいだの差し替え（TOCTOU）は防がない（置き場は取り込み自身だけが書く）。`webpath`のドット始まりの拒否はWeb配信の規則なので持ち込まない（原本の一時ファイルは`tmp/.dl_*`にある）。

### 12.6 `geoframe img-probe`

画像処理の確認・計測用。DB・データ置き場に触れない。

```
geoframe img-probe [--out <dir>] [--rotation <度>] [--jobs <n>] <画像>...
#0003 ok format=Jpeg in=4032x3024 out=2560x1920 exif_orientation=6 gray=0 icc=srgb bytes=764042 ms=273
#0004 error heicをデコードできません（hevc-bitstream）構造: brand=heic primary=grid codec=hevc ... ms=12
summary files=2 ok=1 failed=1 jobs=1 wall_ms=285 median_ms=273 p95_ms=273 max_rss_kib=147572
```

ファイル名・パスは出さず、連番（引数の順）で呼ぶ。`--out`があれば`<連番>.jpg`で書き出し（`make_derivative`＝`.part`・`fsync`・`rename`まで含めた時間）、なければ読んで変換するだけ（書かない）。終了コードは、失敗が1枚でもあれば1。musl静的ビルドをCIが合成HEICで動かす。

### 12.7 テスト

- 単体（`imaging/tests.rs`）: 向き1〜8、手動回転の順序、縮小の寸法（Pillowの`thumbnail`と170件で一致）、グレー・アルファ・パレット・16ビットのPNG、PNGの`eXIf`、TIFF（向き）、EXIF・機種名・ICCを出力に残さない（JPEG・PNG・HEICの入力）、P3のICCのsRGB変換、変換できないICC（グレー用・壊れたもの）は捨てて`ICCなし`と同じ出力、JPEG・PNG・TIFF・HEICを**全ての切断点**で切って失敗になること、EOIの無いJPEGの拒否と末尾のゴミ・マーカーの間のゴミの受け入れ、宣言の寸法が上限を超えるJPEGの拒否、エラーの文面にパス・ファイル名・座標が入らないこと、`.part`を残さない（成功・失敗・`rename`の失敗）、`.part`の名前に置かれたシンボリックリンク（生きている・切れている）を辿らず外を書き換えないこと、出力先がリンクのときはリンク自体を置き換えること、既存の出力を壊さない、512 MiB超の入力を読む前に断ること（`render`・`img-probe`）、BigTIFFの拒否、`store_derivative`の保存先の検査、EXIFの区間を壊してもパニックしない、拡張子の集合。
- Pythonとの突き合わせ（`tests/exif_python.rs`）: 合成JPEG 43ケース（境界は§12.3）と`norm_time`の16入力。意図して違える点だけを`rust_differs`に列挙し、それ以外は完全一致を求める。基準のJSONはCIの`app`ジョブが`--check`で再検査する（Pythonの読み取りが変わったら落ちる）。
- HEIC（`imaging/heic.rs`・`imaging/tests.rs`）: 合成HEIC3枚（`tests/fixtures/synthetic_*.heic`。pillow-heifで作った64x48、`tools/image-poc/gen_fixtures.py`）で、irotを一度だけ適用、ICC、構造の分類、失敗の分類、EXIFの読み取り。
- 宣言値の検査（`imaging/heic_guard.rs`）: `iloc`のextent長・位置、`mdat`のサイズ、`ispe`、SPS（ビット列の読み方、hvcCと画像データの中、長さ付き・Annex B）を偽った合成入力が、`read_header`・`decode`の両方で検査に断られること、実際の合成HEICは通ること、壊れた入力（全ての切断・全バイトの破壊）で検査が落ちないこと。
- 向き（`tests/orientation_python.rs`）: 合成画像60ケース（EXIFだけ・XMPだけ・両方・範囲外・複数のXMP・JPEG/PNG/TIFF/HEIC・手動回転との組み合わせ）を、Pythonの`make_derivative`の出力の寸法と四隅の色に突き合わせる。意図した差（圧縮したXMP）だけを`rust_differs`に列挙。
- JPEGのEOI（`tests/jpeg_eoi_python.rs`）: EOIの欠けや後ろのゴミなど10ケースと、全ての切断点を、Pythonの結果に突き合わせる（差は表に列挙）。
- パス（`datadir.rs`）: `..`・絶対パス・NUL・外を指すリンク・`derivatives`のリンク・IDの区切り。

### 12.8 実装上の判断

- **JPEGのデコードは`zune-jpeg`を直接使う**（`image`の`JpegDecoder`を使わない）。`image`は寛容なモードで読み、**途中で切れたJPEGを部分的な画像のまま成功として返す**（グレーで埋まった下半分のderivativeが「取得済み」になり、Pythonのようにエラーで再試行されない）。一方、`zune-jpeg`の厳格モードは、ヘッダの間の余分なバイトのようなマーカーの並びにも厳しく、libjpegが警告だけで読む写真を落とす。そこで次のようにした。
  1. **まず厳格モードで読む。**
  2. 失敗が**マーカーの並びだけが理由**のとき（`[strict-mode]: Extra bytes between headers`、`Marker .. found where not expected`、`Unexpected marker .. between scans`・`.. at restart boundary`。メッセージに`arker`を含む、または`[strict-mode]`で始まるものだけの**許可リスト**）は、寛容なモードで読み直す。データの不足（`Code length N  more than bits left M`＝ハフマン復号中にビットが尽きた、`Premature end of buffer`、`ExhaustedData`、入出力）は読み直さない。寛容なモードはそれらを部分的な画像で成功させてしまう。
  3. **読めても、画像データのあとにEOI（`FFD9`）が無ければ失敗にする**（`Truncated`）。厳格モードのデコーダも、最後のMCU行の途中で切れたファイルを成功させてしまう（検証: 64x48の合成JPEGで、切断点の約2%＝データ部の末尾約120バイト）ためで、切れたファイルを確実に弾く。**Pillow 12.3.0は、EOIだけが欠けてファイルがそこで終わるJPEGも、SOS以降のどの切断点で切ったJPEGも失敗にする**（合成JPEG 1枚で全ての切断点、別の4種で計6,539通りも、読めたものは0。「PillowはEOIの無いJPEGを読む」とは言えない）。**差が出るのは、EOIが無くその位置にゼロ埋めやゴミが続くファイル**（Pillowはそれも画像データとして読み、足りない分を補う。`tests/jpeg_eoi_python.rs`の`no_eoi_then_zero_padding`・`no_eoi_then_garbage`）で、Pythonは取り込めてRustは失敗する（§0）。逆に、SOIの直後のゴミは、PillowはJPEGと識別できず失敗するがRustは読み直して読む。切れたファイルと見分けられないため、見えないまま下がグレーのderivativeになるより、失敗してログに残る方を選んだ。EOIはエントロピー符号化されたデータの中には現れないので、最初のSOSのあとを探す（EXIFのサムネイルの`FFD9`は数えない）。
  テストは、JPEGを**全ての切断点**（SOSの少し後から末尾の手前まで、サブサンプリング2種）で切り、1つでも成功すれば落ちる。PNG・TIFF・HEICも全ての切断点で失敗になることを見る。EOIなしは失敗、末尾のゴミ・マーカーの間のゴミは成功。Pythonとの突き合わせは`tests/jpeg_eoi_python.rs`。**実機のJPEG原本（Drive）での確認はT2.4**（EOIの無い原本が実際にあるか、Pythonの結果と合わせて件数だけ確かめる）。
- **PNG・TIFFは`image`**（`png`・`tiff`のfeature）。向きはJPEGと同じ自前のEXIFで読む。
- **EXIFは自前**（`kamadak-exif`を使わない）。理由はPillowの読み方を境界まで合わせるため（§12.3）。ASCIIの末尾のNULを1つだけ取る、後勝ち、`or`の連鎖は、外部のcrateでは再現できない（NULで分割する、先勝ちなど）。
- **ICCの変換失敗は黙って捨てる**（画像ごと失敗にしない）。件数は`DerivativeInfo.icc == Dropped`で数えられるので、呼ぶ段がサイクルの末尾に件数だけログに出す（PoCの予告。T2.3・T2.4）。
- **入力はまるごとメモリに読む**（512 MiBまで）。画素数の上限は約1.79億画素（Pillowと同じ）。約48MPで最大RSSが232 MiB（RGBのラスタ約146 MBの約1.6倍）だったので、上限の画素数（約1.79億）なら約0.85 GiB。Python（Pillow）も同じ画素数まで読む。
- **MD5・`*.part`の掃除**: T2.3で入れた（`md-5`（純Rust）で`local_<md5(path)[:16]>.jpg`、起動時の`*.part`の掃除。[§13.3](#133-local_derivatives)・[§13.5](#135-書きかけの-part-の掃除)）。

### 12.9 検証環境での確認（2026-10-24）

検証環境（lxc-geoframe。2コア、メモリ4 GiB）で、**本番コピーの`derivatives/`（長辺2560のJPEG）から無作為に選んだ入力**を使い、[image-poc.md §2](image-poc.md)と同じ作り方（`tools/image-poc/prepare_data.py`: 長辺4032へ拡大して輝度ノイズを足したJPEG 50枚、品質92、平均2.7 MB。3枚に1枚はDisplay P3相当のICC付き。同じ入力から作ったHEICは、作成に時間がかかるので8枚だけ）で、現行Python（Pillow 13.0.0＋pillow-heif 1.8.1）の`make_derivative`と、musl静的ビルドの`geoframe img-probe`を比べた。**写真は検証環境の外へ出していない。計測後に入力・出力・バイナリ・使い捨てのvenvを消した**（`/var/tmp/gf-t22`、mode 0700）。数値と件数だけを書く。

**時間と最大RSS**（1枚あたり。Rustの「書き出しあり」は`.part`・`fsync`・`rename`まで含む）:

| 入力 | 現行Python | Rust（musl静的＋mimalloc） |
| --- | --- | --- |
| JPEG 約12MP 50枚、中央値（p95） | 310.2 ms（350.9）。読み・変換・書き出し | **222〜227 ms**（読んで変換だけ）／**281〜282 ms**（書き出しあり。p95 332） |
| 50枚の所要時間（1ジョブ） | 14.7 s | 10.9〜11.1 s（変換だけ）／13.4〜13.7 s（書き出しあり） |
| 50枚の処理量（2ジョブ） | 5.64枚/秒 | 6.75枚/秒（書き出しあり） |
| 最大RSS（1ジョブ／2ジョブ） | 178 MiB／172 MiB | 144 MiB／274 MiB |
| JPEG 約48MP 1枚 | 1,108 ms、475 MiB | 686 ms、232 MiB |
| HEIC 約12MP 8枚、中央値（p95） | 1,276 ms（1,386） | 958 ms（1,081） |
| HEICの処理量（2ジョブ） | 1.25枚/秒 | 1.13枚/秒（`zune-heic`が1枚で2コアを使うため） |
| HEICの最大RSS（1ジョブ／2ジョブ） | 190 MiB／176 MiB | 217 MiB／321 MiB |

PoC（image-poc.md §1）とほぼ同じ値（JPEG 12MPで222 ms、HEIC 871〜939 ms）。書き出しの`fsync`が1枚あたり約26 ms（同じ検証環境のディスクで、760 KBのファイルを書いて`fsync`した別の測定）。

**寸法・向き・メタデータ**（Python対Rust）:

| 確認 | 結果 |
| --- | --- |
| 寸法の不一致 | JPEG 50枚・HEIC 8枚とも**0**。失敗も**0** |
| 出力のEXIF・GPS・ICC | Python・Rustとも**0**（JPEG 50＋HEIC 8のどちらの出力も）。サブサンプリングは全て4:2:0 |
| 出力の大きさ（平均） | JPEG 763,834 B（Python）→ 766,674 B（Rust。+0.4%、ICCの変換分）。HEIC 610,087 B → 610,825 B |
| 向き・回転 | 合成画像（Orientation 1〜8、手動回転90・180・270、HEICのirot）で、Pillowの出力と寸法・モード（L／RGB）・目印の位置が一致（JPEG・PNG・TIFF・HEIC・CMYK・グレー・パレット・アルファ付き計30入力） |

**画質**（Python対Rust、輝度のPSNR・SSIM。画素の一致は求めない）:

| 群 | 枚数 | Rust対Python PSNR | Rust対正解 | Python対正解 |
| --- | --- | --- | --- | --- |
| JPEG、ICCなし | 33 | 中央値**48.3 dB**（最小42.0） | 42.30 dB | 42.50 dB |
| HEIC、ICCなし | 5 | 中央値**47.9 dB**（最小47.3） | 42.54 dB | 43.03 dB |
| JPEG、P3のICC付き | 17 | 中央値40.3 dB（最小28.8。**RustがsRGBへ変換するため**差が出る） | **41.58 dB**（最小37.3） | 38.75 dB（最小28.7） |
| HEIC、P3のICC付き | 3 | 中央値40.6 dB | **42.45 dB** | 38.76 dB |

SSIMは全群で中央値0.97〜1.00（Rust対正解・Python対正解とも0.96〜0.98）。ICCなしではRustがPythonの出力に約48 dBまで近く、正解との差は−0.2 dB（PoCの47.7 dB・−0.2 dBと同じ）。ICC付きではsRGBへ変換する分、正解に近づく（+2.8 dB、最悪値は28.7→37.3 dB）。

**配布と起動**: `deploy/rust-deploy.sh`で検証環境へ配布（`/healthz`200・`/api/v1/photos`が配列を確認して完了）。`GEOFRAME_INGEST`は未設定（`ingest=0`、`取り込みは無効です`）のまま。`/healthz`は200、`geoframe-rs`の最大RSSは約50 MiB（写真一覧を1回返した後）。取り込みは動かしていない（T2.2は画像処理の部品で、DBにも`DATA_DIR`にも書かない）。検証環境のDBは書き換えていない。

### 12.10 T2.3・T2.4へ回したもの

- **T2.4（Drive取り込みの受入）で、実機iPhoneのHEICを確認する**: グリッド分割（512x512のタイル）、HDRゲインマップ、深度・補助画像、10ビット（Main 10）、nclxだけの色の記述、Live Photo・連写、`clap`、EXIFのOrientationとirotの両方。**検証環境にDriveの鍵がまだ無い**ので、T2.2では合成のHEIC（pillow-heif）だけを確認した。切り分けは`geoframe img-probe`（連番と構造の分類だけを出す。§12.4）か、取り込みのログの`ImageError`（同じ文面）でできる。`zune-heic`が読めない形式が出たら、PoCの`libheif-rs`の経路を足す（[build.md §6](build.md)）。
- **T2.4の受入条件（T2.2のレビューから）**:
  1. **Driveの原本の互換性は、原本で確かめる。** 検証に使ったJPEG 50枚・HEIC 8枚（§12.9）は本番コピーのderivativeからの再生成で、Driveの原本（カメラ・iPhoneが書いたもの）の互換性を裏付けない。T2.4では、**原本の形式別（JPEG・HEIC・PNG・TIFF）の成功件数と、失敗の分類（`ImageError`の固定の語。HEICは構造の分類つき）を、同じ原本に対するPythonの結果と比べ**、Rustだけが失敗する写真が無いこと（あれば原因ごとに判断すること）を確かめる。ファイル名・座標は出さず、件数と分類だけを記録する。
  2. **変換の失敗が取得待ちを塞がない。** Pythonの`fetch_derivatives`は、取得待ちを`LIMIT`つきのSQLで選ぶ。恒久的に変換に失敗する写真（壊れた原本、`NotAnImage`、BigTIFF、読めないHEIC）があると、毎回それが先頭の枠を埋め、後続の写真がいつまでも取得されないおそれがある。**失敗が続いても、後続の写真を取得できること**を、スタブのDrive APIで確かめる（`GEOFRAME_FETCH_PER_CYCLE`件より多い、恒久的に失敗する写真を先頭に並べ、後続の写真の`display`が埋まること）。失敗した写真を飛ばす・後回しにする仕組み（失敗の記録を除く、試行回数の上限など）を入れるかは、Pythonの挙動を確かめてから決める。
  3. `tmp/.dl_<id>`（固定の名前）は、`create_new`＋`O_NOFOLLOW`で作る（`derivative`の`.part`と同じリンクの問題。§12.2）。
- **T2.4**: JPEG原本（Drive）で、厳格モード＋寛容な読み直しが落とす写真が無いか（失敗の分類は`ImageError`の文面で件数だけ出す）。本番の約6,500枚を一括で作り直すときの時間とメモリ（概算。今回の測定の1枚あたりの時間から、JPEG 4,795枚が約0.28 s、HEIC 1,735枚が約1 s、PNG 5枚で合計約50分。Pythonは同じ見積りで約1時間。実機のHEICの時間は別）。`fetch_derivatives`が`ImageError`を握ってログに出すとき、パス・ファイル名を付けないこと。ICCを捨てた件数のログ。`norm_time`をDriveの`imageMediaMetadata.time`に通すこと。
- **T2.3（実施済み。§13）**: `index_local`は`extract_exif`の`NotAnImage`を「読み込み失敗」として飛ばす（Pythonと同じ）。`local_<md5(path)[:16]>.jpg`の名前（`md-5`を足した）。`local_derivatives`は`resolve_existing`で`photos.path`を検査してから読む。`*.part`の掃除は入れた（起動時）。
- **P3（更新API）**: 管理画面の回転は`derivative_name(drive_id, rotation)`で別名にして`store_derivative`を呼ぶ（元のderivativeからの回転か、Driveから取り直すか。admin.py:350）。`rotation`の値は`0/90/180/270`のまま。

## 13. ローカル写真の索引（P2 / T2.3）

`PHOTOS_DIR`（既定`DATA_DIR/photos`）に置かれた写真を`photos`の行にし（`index_local`）、位置のある写真に表示用のファイルを用意し（`local_derivatives`）、位置の無い写真を退避する（`organize`）。現行の`drive.py`の`index_local`・`local_derivatives`・`organize_photos`に対応する。棚卸しS1（[inventory.md §5](inventory.md)）。実装は`crates/geoframe/src/local.rs`。画像の読み取りと変換は§12の部品を使う。

### 13.1 段と設定

| 項目 | 内容 |
| --- | --- |
| 段 | `index_local`（サイクルの先頭）・`local_derivatives`（`enrich`の後、`fetch_derivatives`の前）。順序は現行の`cycle()`のまま。どちらも失敗・パニックは隔離される（§11.3） |
| `GEOFRAME_PHOTOS_DIR`（既定`photos`） | `DATA_DIR`からの相対（絶対パスなら、そのパス）。現行の`(DATA_DIR / 値).resolve()`と同じ。`canonicalize`した先が`DATA_DIR`の**中のサブディレクトリ**でなければ`index_local`は失敗する（§0）。ディレクトリ名`photos`は変えない（`display`・`path`列の相対パスが配信URLを兼ねる） |
| `GEOFRAME_FETCH_PER_CYCLE`（既定`25`） | `local_derivatives`が1サイクルで処理する枚数。T2.4で`fetch_derivatives`も同じ値を使う。0以上の整数だけ（それ以外は起動を断る。終了コード2） |
| ログ | 段ごとの件数だけ（`files added updated removed unreadable skipped …`、`original made failed icc_dropped`）。ファイル名・座標・撮影日時・例外の文字列は出さない |

### 13.2 `index_local`

**規則はPythonと同じ**で、違いは§0の行と下の表に挙げたものだけ。

1. **走査**: `PHOTOS_DIR`の下を再帰して、拡張子が`imaging::EXTS`（大文字小文字を区別しない）のファイルを集める。パスの要素のどれかが**ちょうど`gdrive`**なら除外する（旧ミラー先。部分一致ではなく、`gdrive2`・`mygdrive`は対象）。ドット始まりのファイル・ディレクトリも対象（Pythonと同じ）。`PHOTOS_DIR`が無ければ作る（`mkdir`。`--dry-run`では作らない）。
2. **並び**: パスの要素の列の昇順（Pythonの`sorted(rglob("*"))`）。`a/b.jpg`は`a-b.jpg`より前。新しい行の`id`はこの順に振られる（`INTEGER PRIMARY KEY`なので`max+1`）。
3. **追加**: `photos(source='local', path, name, mtime, taken_at, lat, lon, loc_source, indexed_at)`。`path`は`DATA_DIR`からの相対（`/`区切り）、`name`はファイル名、`mtime`は`st_mtime`（`sec + nsec * 1e-9`の`double`。CPythonと同じ式で、比較テストでビット一致を確かめた）、`taken_at`は`imaging::extract_exif`の値（読めなければ空文字。NULLにしない）、`lat`・`lon`はGPSがあるときだけ、`loc_source`はGPSがあれば`exif`、無ければNULL、`indexed_at`は`YYYY-MM-DDTHH:MM:SS`（現地時刻・オフセット無し）で、段の先頭で1回だけ作って全行に使う。
4. **更新**: 既存の行（`path`が一致）で`mtime`の差が**1秒以上**（または`mtime`がNULL）のとき、`mtime taken_at lat lon loc_source`を上書きし、`place elev display`をNULLに戻し、`indexed_at`を更新する。**`name favorite hidden rotation title inferred_from infer_dt_s muni_cd pref city fetched_at`は触らない**（手動の位置は上書きされる。§0）。1秒未満の差は何もしない。
5. **削除**: 走査で見つからなかった`source='local'`の行（`path`がNULLの行も）を削除する。`display`が`derivatives/`で始まる行は、そのファイルも消す（`photos/`の原本は消さない）。消すのは行の削除を確定したあとで、名前は`DataDir::derivative_file`の検査（`derivatives/`直下の1ファイル、安全な名前、`derivatives`がリンクでない）を通ったものだけ。
6. **読めない写真**: 画像として読めない（`NotAnImage`・読み取りの失敗・512 MiB超・BigTIFF）写真は、行を作らず・更新せず、件数（`unreadable`）に数える。既に索引した写真が壊れても、走査では見つけているので行は消えない（Pythonと同じ）。EXIFが無い・壊れているのは失敗ではない（位置なし・時刻なし）。
7. **中断**: 停止の合図が来たら、何も書かずに段を終える（走査が途中の「見つからなかった」で行を消さない）。

**書き込みの形**: 走査・EXIFの読み取りはwriterの外で先に済ませ、計画（追加・更新・削除）を作ってから、`writer.call`1回の中で`BEGIN`〜`COMMIT`にまとめて書く。途中で落ちても中途半端な状態を残さない。`--dry-run`は計画の件数だけを出し、置き場の作成・書き込み・ファイルの削除をしない。

| Pythonの挙動 | Rustの挙動 |
| --- | --- |
| `rglob`はシンボリックリンクのディレクトリに降りない。`is_file()`はリンクを辿るので、外のファイルへのリンクも読む | ディレクトリのリンクには降りない（同じ）。ファイルのリンクは`canonicalize`した実体が`DATA_DIR`の中のときだけ取り込む。外を指すものは`skipped`に数えて飛ばす。切れたリンクは無視 |
| 名前がUTF-8でないファイルは、`sqlite3`への挿入で例外 | 飛ばして`skipped`に数える |
| `PHOTOS_DIR`が`DATA_DIR`の外だと`relative_to`が例外 | 段が失敗（`GEOFRAME_PHOTOS_DIR が DATA_DIR の中のサブディレクトリではありません`）。`DATA_DIR`そのものも断る |
| 壊れたGPS（成分不足など）は例外で、その写真を索引しない | 位置なしで索引する（§12.3。T2.2で決定） |

### 13.3 `local_derivatives`

位置があり（`lat IS NOT NULL`）`display IS NULL`の`source='local'`の行を、`id`の昇順で`GEOFRAME_FETCH_PER_CYCLE`件まで選ぶ。位置の無いローカル写真には作らない（Driveの写真は位置の有無で絞らない。[inventory.md §6.4](inventory.md)）。

- 拡張子が`.jpg`・`.jpeg`・`.png`（大文字小文字を区別しない。`imaging::is_browser_displayable_ext`）なら、**原本をそのまま**`display`にする（`display = path`。DBの文字列そのまま。配信は`/photos/...`）。
- それ以外（`.heic`・`.heif`・`.tif`・`.tiff`）は`derivatives/local_<md5(path)[:16]>.jpg`を作る（`imaging::store_derivative`。回転0）。名前は`local_derivative_name`で、`path`列の文字列のUTF-8バイト列のMD5の先頭8バイトの16進（Pythonの`hashlib.md5(path.encode()).hexdigest()[:16]`と同じ。検証環境でPythonと同じ名前になることを確かめた）。`md-5`（純Rust）を足した（[build.md §9](build.md)）。
- 行ごとに`UPDATE photos SET display=?, fetched_at=?`（`fetched_at`は`YYYY-MM-DDTHH:MM:SS`）。1枚ずつ書くので、途中で止まっても進んだ分は残る。
- `photos.path`は`DataDir::resolve_existing`を通してから読む（`DATA_DIR`の外へ出るもの・存在しないものは`failed`）。変換に失敗した写真は`failed`に数えて次へ進み、`display`はNULLのまま（毎サイクル再試行される）。`ImageError`の文面はパスを含まないので`debug`ログに写真のIDと一緒に出せる。ICCを捨てた件数は`icc_dropped`。
- **残る注意**: 恒久的に失敗する写真が`FETCH_PER_CYCLE`件を超えると、後続が取得されない（Pythonと同じ。ローカルの位置あり写真が対象なので、実際には起きにくい）。T2.4のDrive側（受入条件②）と一緒に判断する。
- `--dry-run`は`display=path`にする枚数（`original`）と、derivativeを作る枚数（`made`）を数えるだけ。

### 13.4 `geoframe organize`

```
geoframe organize [--dry-run] [--data-dir <dir>]
```

位置の無いローカル写真を`DATA_DIR/_excluded/no_gps/`へ移し、行を削除する（Pythonの`server.py --organize`）。`index_local`を先に1回動かし、その結果を使う（失敗したら移さない）。書く実行は`GEOFRAME_INGEST=1`が要り、**`sync`と同じガード（§11.1.1）・ロック・移行・掃除を通す**。無いと終了コード2で断る。

- **対象**: `source='local' AND lat IS NULL`の行のうち、次のいずれか: 撮影時刻が空（NULLか空文字）、または錨になりうる写真（`loc_source IN ('exif','drive_meta','manual')`で時刻つき）が1枚も無い。それ以外（時刻つきで、錨がある）は**T2.5まで移さない**（`held_back_until_infer`として数える）。Pythonは`index_local`→`infer_locations`→移動で、推定で位置が付いた写真は移さない。Rustの`infer`段は何もしないので、絞らないとPythonなら残る写真まで移して行を失う。T2.5で`infer`段のあとに移す形にして、この絞り込みを外す。
- **ファイル**: `entry_path`（親を`canonicalize`して`DATA_DIR`の中か検査し、最後の要素のリンクは辿らない）で絶対パスにし、**通常のファイルだけ**を移す。リンク・ディレクトリは動かさず、行も残す（`skipped`）。ファイルが無い行は、Pythonと同じく行だけ消す（`row_only_removed`）。
- **移動先**: `_excluded`・`_excluded/no_gps`を1階層ずつ`symlink_metadata`で見て、リンクやファイルならディレクトリを作らずに断る（`DATA_DIR`の外へ出さない）。ファイル名はそのまま。**同名があっても上書きしない**: ハードリンクを作ってから元を消す（リンクは既にあれば失敗するので、同名のファイルを壊さない）。あれば`名前 (2).拡張子`、`(3)`…と探す。ハードリンクを作れないファイルシステムでは、無いことを確かめてから`rename`する。1つの移動に失敗しても次へ進む（`failed`）。
- 行の削除は、ファイルを移したあとに1行ずつ。
- `--dry-run`は`targets`と`held_back_until_infer`だけを出し、何も動かさない。対象は今のDBの行だけで、`index_local`が追加・更新する分は含まない（出力にその旨が出る）。
- unitの`ReadOnlyPaths`は`photos`を読み取り専用にするので、`organize`はunitの外（`sudo -u geoframe geoframe organize`）で動かす。出力はファイル名を含まない。

### 13.5 書きかけの`.part`の掃除

T2.2の注意（§12.2）の対応。`ingest::start`の最後（ガード・ロック・移行・`.dl_*`の掃除のあと）で、`derivatives/`**直下**の次のファイルを消す。

- 通常のファイル（`DirEntry::file_type`はリンクを辿らない。シンボリックリンク・ディレクトリは消さない・辿らない）
- 名前が`.part`で終わり、`datadir::check_file_name`を通る（英数字・`_`・`-`・`.`、先頭が`.`でない、`..`を含まない）。Rustの`<出力名>.<pid>.<乱数>.part`もPythonの`<stem>.part`も対象
- `derivatives`自体がシンボリックリンクなら何もしない

消した数だけをログに出す（`[derivative] 書きかけの .part を N件削除しました`）。`--dry-run`は「消す書きかけの .part N 件」として数える。**判断**: 取り込みのロックを持つ間は、他に`.part`を作るプロセスがいない（Rustの別プロセスはロックで、Pythonはガードが止まっていることを確かめる）ので、残っているものはすべて書きかけの残骸と言える。中身はEXIFを持たない部分的なJPEGで、個人情報の問題はなく、容量を食うだけなので、起動時に1回の掃除で十分（サイクルの最中は掃除しない）。P3で管理画面の回転がderivativeを作るようになっても、`serve`の起動時の掃除は他のスレッドが動く前なので重ならない。

### 13.6 `--dry-run`

`geoframe sync --dry-run`の`index_local`・`local_derivatives`の段が、書かずに件数を出す（§11.3）: `index_local`は`files added updated removed unreadable skipped …`（置き場が無ければ全部0）、`local_derivatives`は`original made`。掃除の行には`.part`の件数が増えた（`掃除: 消す .dl_* N 件 / 消す書きかけの .part N 件 / 捨てる .cache.json の旧キー N 件`）。`geoframe organize --dry-run`は`targets`・`held_back_until_infer`。

### 13.7 Pythonとの差（まとめ）

§0の4行に載せたものがすべて。要点:

- **現行のまま残した危ない挙動（P3で決める）**: `mtime`が変わると**手動で直した位置も上書きされる**。お気に入り・除外・回転・タイトルは残る。
- **安全側に変えたもの**: ファイルのリンクは実体が`DATA_DIR`の中のものだけ、`PHOTOS_DIR`が外・`DATA_DIR`そのものなら断る、消すderivativeの名前の検査、`organize`は上書きせず通常のファイルだけ動かす。
- **T2.5で解消するもの**: `organize`が推定で位置が付きうる写真を移さない制限。
- 意図して同じにしたもの: 走査順（`id`の採番順）、`gdrive`の完全一致、1秒の許容、読めない写真は飛ばす、削除時の`photos/`の原本は残す、`indexed_at`/`fetched_at`の形式、位置のあるローカル写真だけにderivativeを作る。

### 13.8 テスト

| 対象 | テスト |
| --- | --- |
| Pythonとの突き合わせ（`tests/local_index_python.rs`） | `tools/gen_local_index_fixture.py`が作る`local-index-python.json`（合成のJPEG・PNG・TIFF・HEICの木。ナノ秒つきの`mtime`、`gdrive`・隠しディレクトリ・日本語名・並びの境目・画像でないファイル。4段階の操作: 初回の索引・変化なし・更新と削除と追加と手動の書き込み・変化なし）を、Rustが同じ木と操作で`IndexLocal`に通し、`photos`の**全列と`id`**、書き換えられたか（`indexed_at`に入れた目印が残るか）、`derivatives/`のファイルの有無が一致することを確かめる。意図した差（壊れたGPSの写真）は別のテストで確かめる。CIの`app`ジョブが`--check`でPythonの結果を検査する（`pillow-heif`が必要） |
| `local.rs`の単体テスト | 追加の列、2回目に何も書かない、1秒の許容、更新で上書きする列と残す列、削除（`derivatives/`だけファイルも消す、危険な`display`は消さない）、`gdrive`・拡張子・隠しファイル、走査順、読めない写真、リンク（ディレクトリは降りない・外を指すファイルは読まない）、置き場が外・`DATA_DIR`そのもの、`dry-run`、停止、ログに名前が出ないこと、`local_derivatives`（JPEG・HEIC、上限、失敗、`path`の逸脱、位置なし、`dry-run`）、`organize`（移動・同名・保留・`dry-run`・欠けた行・リンク・移動先がリンク）、MD5の名前（Pythonの値） |
| `ingest.rs` | `.part`の掃除（対象と対象外、リンク、`derivatives`がリンク、ガードが断れば消さない） |
| `datadir.rs` | `derivative_file`・`entry_path` |
| `tests/ingest_cli.rs` | `sync --dry-run`の出力（`.part`の件数・新しい段の件数）、`organize`（INGESTなし・印なしで断る、`--dry-run`が何も変えない、ガードを通すと移して行を消す。Driveの行は触らない。出力に名前が出ない） |

### 13.9 検証環境での確認（2026-10-24）

musl静的バイナリを`deploy/rust-deploy.sh`で配布して確かめた。検証環境の`catalog.db`は本番のバックアップのコピー（6,535行、すべて`source='drive'`）。**書き換える前に、検証環境の中で`sqlite3 .backup`のコピーを取った**（`.dump`のSHA-256を記録）。合成の写真は、`tests/fixtures/local-index-python.json`の操作を検証環境の中で再生して置いた（実写真は使わず、検証環境の外へも出していない）。

| 確認 | 結果 |
| --- | --- |
| (a) 写真を置く前の`--dry-run` | 本番コピーに対して、`index_local`は`files=0 added=0 updated=0 removed=0`、`local_derivatives`は`original=0 made=0`、掃除は`.dl_* 0 / .part 0 / 旧キー 0`。他の段は件数なし、`写真: 表示可能 5288 / 取得待ち 0 / 全6535`（本番コピーに対して変更ほぼ0。P2の受入の関門） |
| (b) 合成の写真19ファイル（`gdrive`配下・対象外の拡張子・壊れたファイル含む）を置いた`--dry-run` | `files=16 added=15 unreadable=1`（壊れたGPSの1枚はPythonと違い索引する。§0）。置き場は変わらない |
| (c) `GEOFRAME_INGEST=1`の`sync`1回目・2回目 | 1回目は`added=15`、`local_derivatives`は`original=8`（位置のあるJPEG）。**2回目は`added=0 updated=0 removed=0`、`made=0`**。`.dump`の差分は**15行（すべて`source='local'`の`INSERT`）で、ほかの表・行の差は0**。Driveの6,535行の中身（`SELECT *`のSHA-256）は変わらない |
| (d) 比較用Python（`geoframe-pyref`、別データ）との比較 | pyrefを止めて（DBは検証環境の中でバックアップ）、同じ木をpyrefの置き場に置き、pyrefのvenv（Python 3.12.3・Pillow 13.0.0・pillow-heif 1.8.1）の`index_local`・`local_derivatives`を通し、`photos`の`source='local'`の行をRustの行（壊れたGPSの1枚を除く）と**`id`の並び順・`path name mtime taken_at lat lon loc_source display …`の全列まで比べた: 14行すべて一致**（`indexed_at`・`fetched_at`は形式まで一致）。続けて変化の操作（更新・削除・追加）を流して、もう一度比べて**14行すべて一致**。HEICに位置を与えたときのderivativeの名前も同じ（`local_<md5>.jpg`、64x48のJPEG） |
| (e) `organize` | `INGEST`なしは終了コード2。`--dry-run`は`targets=5 held_back_until_infer=2`。実行で5枚を`_excluded/no_gps/`へ移して行を削除（`moved=5 failed=0`）。もう一度動かすと`targets=0`。時刻つきで錨がある2枚は残った |
| (f) `.part`の掃除 | `derivatives/`に書きかけの`.part`1つと、`.part`という名前のシンボリックリンク1つ（リンク先は別の場所の一時ファイル）を置いた。`--dry-run`は`消す書きかけの .part 1 件`。`sync`が1件削除し、リンクは残り、リンク先の中身は無傷 |
| (g) unitの制限下（`NoNewPrivileges`・`ProtectSystem=strict`・`ReadOnlyPaths`に`photos`・`MemoryDenyWriteExecute`）で`GEOFRAME_INGEST=1`のdrop-inで起動 | 起動時のサイクルが通り（`index_local`が`photos`が読み取り専用でも走査できる。置き場の`mkdir`は`AlreadyExists`で済む）、写真を1枚足して再起動すると`added=1`・`original=1`。`/api/v1/photos`は3,065＋合成のぶん増え、`/photos/<名前>`は200 |
| 後片付け | 合成の写真・`_excluded`・`derivatives/local_*.jpg`・Rustが足した`source='local'`の行を消し、drop-inを外して`ingest=0`（`取り込みは無効です`）に戻した。**`.dump`のSHA-256は配布前のコピーと一致**。`/healthz`は200。比較用Pythonは、DBを変更前のバックアップと同じ内容に戻して（`.dump`が一致）起動し直した。検証環境の中に作ったバックアップは確認後に削除した |

### 13.10 T2.5・T2.4へ回したもの

- **T2.5**: `organize`の「推定で位置が付きうる写真は移さない」制限（§13.4）を外す。`infer`段のあとに移す。
- **T2.4**: `fetch_derivatives`の取得待ちの占有（§12.10の受入条件②）は、`local_derivatives`の枠の占有（§13.3）と同じ形で判断する。
- **P3**: 手動の位置がmtimeの変更で上書きされる挙動（§0、[inventory.md §2.3](inventory.md)）を直すか決める。直すなら、`loc_source='manual'`の行は`lat lon loc_source`を更新しない・`display`だけをNULLにする、などの選択肢がある。
