# Rustサーバーの基盤: 設定・DB読み取り・ログ・ヘルス（P1 / T1.1）と、書き込みの土台（P2 / T2.1、§11）

- 状態: T1.1 実装済み（2026-10-24）。レビュー指摘（パニック・トランザクション・ヘルス）を反映済み。T1.2（`GET /api/v1/photos`、§8）、T1.3（静的配信・Host検査・管理画面ページ、§9）、T1.4（管理画面の読み取りAPI、§10）を追記。T1.5 で systemd unit と配布スクリプトを入れ、検証環境で受入確認した（§6、[p1-acceptance.md](p1-acceptance.md)）。P2 の T2.1 で書き込みの土台（writer・スキーマの作成と移行・同期サイクルの骨組み・`--dry-run`・起動時の掃除・`.cache.json`）を入れた（§11）。T2.2 で画像処理（derivative の生成・EXIF・HEIC・パスの検査）を入れた（§12）。T2.3 でローカル写真の索引（`index_local`・`local_derivatives`・`organize`）と書きかけの`.part`の掃除を入れた（§13）。T2.4 で Drive の索引と取得（`index_drive`・`fetch_derivatives`）と、失敗が続く写真が枠を塞がない仕組みを入れた（§14）。T2.5 で位置の推定と地名（`infer`・`enrich`）を入れ、`organize`の絞り込みを外した（§15）。T2.6 で簡易品質（`quality`・`quality-scan`）を入れた（§16）。T2.7 で検証環境の受入を行った（[p2-acceptance.md](p2-acceptance.md)）。P3 の T3.1 で更新 API（E12 手動の位置・E13 お気に入り/除外）と POST の入口（ルーティング・CSRF 対策・エラー応答）を入れ、同期サイクルとの競合を調べた（§17）。T3.2 で E14 回転を入れ、回転と取得・品質・同期サイクルの競合を調べた（§17.8）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 計画: [plan.md](plan.md) / タスク: [tasks.md](tasks.md) / 棚卸し: [inventory.md](inventory.md)（§2 DB、§4 設定、§6.3 ログ）
- 実装: `crates/geoframe/src/config.rs`・`db.rs`・`logging.rs`・`server.rs`・`cli.rs`、T1.3 は`webpath.rs`・`hostcheck.rs`・`statics.rs`・`embedded.rs`と`build.rs`、T1.4 は`admin.rs`・`pyquery.rs`・`sysinfo.rs`、T2.2 は`imaging/`（`derivative.rs`・`exif.rs`・`heic.rs`・`probe.rs`）と`datadir.rs`、T2.3 は`local.rs`、T2.4 は`drive.rs`・`drive/`（`index.rs`・`fetch.rs`）と`backoff.rs`、T2.5 は`geo.rs`・`geo/`（`infer.rs`・`enrich.rs`。試験用に`stub.rs`・`tests.rs`）、T2.6 は`quality.rs`・`quality/`（`features.rs`・`group.rs`。試験は`tests.rs`）、T3.1 は`post.rs`・`update.rs`、T3.2 は`rotate.rs`（と`imaging`の`rotate_jpeg`・`write_atomic`・`lock_derivatives`）
- ビルドと配布は[build.md](build.md)。ここは**起動したあとの振る舞い**の決めごと。

## 0. Pythonとの差の索引

P1・P2（T2.1〜T2.6）・P3（T3.1〜T3.3）でPythonと意図して違える点・既知の差の入口。本文は各節にあり、ここは1行ずつの索引だけ（内容／理由／適用条件／検証状況／解消予定）。

| 差 | 理由 | 適用条件 | 検証状況 | 解消予定 | 詳細 |
| --- | --- | --- | --- | --- | --- |
| `GEOFRAME_DATA_DIR`の既定が`./var`（Pythonは`app/var`）。`app/.env`を読まない | 単一バイナリに`app/`が無い。本番はunitの`EnvironmentFile`が同じ役目 | 環境変数・`.env`で与えず手で起動したとき | 設定のテスト。本番はunitが常に与える | なし（決定） | [§1.1](#11-優先順位)・[§1.3](#13-appenvは読まない) |
| 不正な設定値は起動を断る（終了コード2）。`GEOFRAME_EMBED_BACKEND`だけ`gemini`に落とす。`GEOFRAME_HOST`のホスト名はPythonは通すがRustは断る（IPアドレスだけ） | 事故を早く見つける。Pythonは例外で落ちる | 不正な値が設定にあるとき（本番は`0.0.0.0`か`127.0.0.1`） | 単体テスト、検証環境で再起動ループにならないこと | なし（決定） | [§1.1](#11-優先順位)・[§10.6](#106-pythonとの差まとめ) |
| 全角数字など ASCII 以外の数字の撮影時刻をRustのフィード（§8）と**位置の推定（T2.5の`infer`）**は読めない（旅番号がずれうる。`infer`は、そのような時刻の写真を「時刻なし」として扱う） | 時刻の解釈を`strptime`に合わせきれていない | EXIFに全角数字が入った写真 | 本番コピーでは差0。単体テスト | **T2.2で`imaging::norm_time`が数字をASCIIに直す**（Pythonは元の文字のまま保存する）。呼ぶのはT2.3・T2.4のインデクサ。Pythonが書いて既にDBに入っている全角の値は直さない | [§8.2](#82-値)・[§12.3](#123-exif撮影時刻gps向き) |
| 夏時間を持つ`TZ`のときの時刻の差（T2.5の`infer`の時刻の差を含む） | RustはP1で`TZ`を解釈せず、naiveな日時を文字列・暦で扱う（`infer`は暦の日数×86400＋時分で差を取る。Pythonは`strptime(…).timestamp()`で`TZ`として解釈する） | `TZ`が夏時間を持つとき（本番は`Asia/Tokyo`） | 本番設定では差なし | P2以降で時刻を変換するときに再検討 | [§1.2](#12-tz)・[§10.6](#106-pythonとの差まとめ) |
| `taken_at`がNULLの行: Pythonは`time: null`で先頭、Rustは空文字で最後 | 契約（`time`は必ずある）に合わせる | `taken_at`がNULLの行がDBにあるとき | 本番コピーに該当行なし（管理者確認済み） | なし（決定） | [§8.3](#83-pythonとの差) |
| `lat`ありで`lon`がNULLの行: Pythonは一覧全体が500になりうる、Rustは一覧から外す | 契約（`lon`は必ずある）に合わせる | 該当行があるとき（通常は起きない） | 本番コピー（2026-10-24のバックアップ）での全件比較で差0（表示対象に該当行なし） | なし（決定） | [§8.3](#83-pythonとの差) |
| 静的配信・HTTPの差（領域外のリンクは403、`ETag`での再検証、`/healthz`の新設、未実装ルートの503、ルートの無い`POST`は403、接続・配信の上限など） | 安全性、固定の挙動、まだ無いことの明示 | 該当する要求を送ったとき | 検証環境で現行Pythonとの比較（状態コード・本文の一致）、単体テスト | 503はP3・P4で実装して外す（`POST`の更新ルートはT3.1、`GET /api/admin/geocode`はT3.3で実装。`/api/admin/`配下の残りの`GET`は、T3.3で503をやめて現行どおり403にした）。他は決定 | [§9.7](#97-pythonとの差まとめ) |
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
| **`local_derivatives`（T2.3）**: 取得待ちを`ORDER BY id`で選ぶ（Pythonは`ORDER BY`なしの`LIMIT`）。`photos.path`を`DataDir::resolve_existing`で検査してから読み、`DATA_DIR`の外へ出るものは失敗として数える（Pythonは原本のとき存在も見ずに`display`にする）。**変換が恒久的に失敗する写真は、失敗を覚えて飛ばし、後続を枠に入れる**（T2.4で解消。Pythonは`GEOFRAME_FETCH_PER_CYCLE`件より多く失敗すると後続が取得されない。[§14.6](#146-失敗が続く写真が枠を塞がない受入条件)） | 順序の固定、DBの値を信用しない | 位置のあるローカルのHEIC・TIFFが壊れたとき | 単体テスト、検証環境 | 枠の占有はT2.4で解消した（[§14.6](#146-失敗が続く写真が枠を塞がない受入条件)） | [§13.3](#133-local_derivatives) |
| **`organize`（T2.3。`geoframe organize [--dry-run]`）**: ①移動先に同名のファイルがあっても**上書きせず**`名前 (2).拡張子`にする（Pythonの`rename`は上書きして失った）。②通常のファイルだけを移す（リンク・ディレクトリは動かさず、行も残す）。③**`index_local`→`infer`→移動の順**（Pythonと同じ。推定で位置が付いた写真は移さない。T2.3〜T2.4はRustの`infer`が空だったので、推定で位置が付きうる写真を移さず`held_back_until_infer`として数えていたが、T2.5で外した）。④元の削除に失敗したら退避先に作ったリンクを取り消す（残ったリンクは次の実行が同じ`inode`で再利用し、`(2)`・`(3)`を増やさない）。⑤`--dry-run`は`index_local`のあとの状態で、実行と同じ分類の件数を出す。⑥ガード・ロックはsyncと同じ。ログに名前を出さない（件数だけ）。`unit`の`ReadOnlyPaths`が`photos`を読み取り専用にするので、`organize`はunitの外（`sudo -u geoframe`）で動かす | データを失わない、Pythonの結果を超えて動かさない | 位置の無いローカル写真を整理するとき | 単体テスト（推定で位置が付く写真が残る・窓の外と時刻なしは移る・錨が消えれば次に移る）、`tests/ingest_cli.rs`、検証環境 | なし（決定。③はT2.5で解消した） | [§13.4](#134-geoframe-organize) |
| **書きかけの`derivatives/*.part`を起動時に消す（T2.3）**: `GEOFRAME_INGEST=1`のガードとロックのあと、`.dl_*`の掃除と同じ時点で、`derivatives/`**直下の通常ファイルで名前が`.part`で終わり、安全な名前**のものを消す。リンクは消さず辿らない。`derivatives`自体がリンクなら何もしない。件数だけをログに出す（`--dry-run`にも出る）。Pythonには掃除が無い | SIGKILL・電源断で残る`<名前>.<pid>.<乱数>.part`は名前に乱数があるので上書きされず容量を食い続ける | 取り込みの起動時 | 単体テスト、検証環境 | なし（決定） | [§13.5](#135-書きかけの-part-の掃除) |
| **`.dl_*`の掃除は、`tmp/`・`derivatives/`がシンボリックリンク（など通常のディレクトリでないもの）なら飛ばす（T2.3）**: 列挙の前に`symlink_metadata`で見る。飛ばした数だけを警告ログに出し（パスは出さない）、`--dry-run`にも「掃除を飛ばす」として出る。無い場合（写真を置く前の`derivatives/`）は飛ばした数に入れない。`.part`の掃除の`derivatives/`の検査も同じ関数 | Pythonの`prepare_tmp_dir`は`is_dir()`でリンクを辿り、リンク先の`.dl_*`を消す。外へのリンクだと関係のない場所のファイルを消すので、Rustは辿らない | 取り込みの起動時 | 単体テスト（`tmp/`・`derivatives/`の両方）、検証環境 | なし（決定） | [§13.14](#1314-再々レビュー2026-10-24の指摘への対応) |
| **Driveの認証（T2.4）**: ①トークンの交換先は定数`https://oauth2.googleapis.com/token`で、鍵JSONの`token_uri`は読まない（google-authはファイルの値を使う）。②`GEOFRAME_DRIVE_KEY`の相対パスは**カレントディレクトリ基準**（Pythonは`app/`基準。Rustに`app/`は無い）。`deploy/geoframe-rs.service`は、Python版のunitと同じ`GEOFRAME_DRIVE_KEY=/var/lib/geoframe/drive_key.json`を与える。③鍵が他のユーザーに読める権限（`& 0o077`）なら警告する（読みはする）。ディレクトリ・FIFO・64 KiB超は鍵として読まない。④鍵が無ければDriveの2段は通信せずに飛ばす（Pythonと同じ）。**鍵が壊れている・トークンを断られた（4xx）も飛ばす**（Pythonは認証エラーをprintして`svc`無しで進む。同じ）が、**Google側の不調（429・5xx）や通信の失敗は段の失敗**（`sync`の終了コード1）として出す | 鍵ファイルで任意のURLへ署名付きのassertionを送らせない。壊れた鍵と一時的な不調を見分ける | `GEOFRAME_DRIVE_KEY`を置いたとき | 単体テスト（スタブがassertionの署名・`iss`・`scope`・`aud`・`exp`を検証する）。検証環境で合成の鍵を実際のGoogleに向けると400で断られ、飛ばす（[§14.10](#1410-検証環境での確認鍵が無い状態2026-10-25)） | なし（決定） | [§14.2](#142-認証) |
| **`index_drive`（T2.4）**: ①md5が変わった行の更新は、手動で直した位置（`loc_source='manual'`）も`drive_meta`かNULLに**上書きする**（Pythonと同じ。`place elev display fetched_at`はNULL、`rotation=0`。お気に入り・除外・タイトル・`inferred_from`・`infer_dt_s`・`muni_cd pref city`は残る。古いderivativeのファイルは消さない）。②**一覧が0件で、Driveの行が残っているときは、削除しない**（共有を外した・フォルダの取り違えでもDriveは空の一覧を成功として返し、Pythonは全行を消す。お気に入りや手動の位置まで失う）。**0件以外でも、消す件数が50件を超え、かつDriveの行の20%を超えるときは、削除だけを保留する**（`mass_delete_withheld`に数えて警告する。6,535件から1件だけ見える一覧のような取り違えを止める。追加・更新は通常どおり）。どちらの保留も、**`geoframe sync --allow-mass-delete`を付けた1回だけ**外れる（環境変数・設定・unitでは外せず、`serve`の定期実行は常に保留する。`--dry-run`にも付けられ、外した場合の件数が見える。Pythonは無条件に全部消す）。③一覧は**全ページが取れたときだけ**計画を作る（途中で失敗したら追加も削除もしない。Pythonも例外で同じ）。④書き込みは1つのトランザクション（Pythonは1文ごとのコミット）。⑤消すderivativeの名前は`derivatives/<安全な名前>`だけ（Pythonは`DATA_DIR / display`を無検査で消す）。⑥`name`の無いファイルは索引に入れず（Pythonは途中で例外）、行は残す。`id`の無いファイルは一覧の失敗（Pythonも例外）。⑦フォルダ名の`'`・`\`はエスケープして検索する（Pythonは検索式が壊れてHTTP 400）。⑧再試行（429・5xx・流量制限の403）と応答の大きさの上限を付ける | ①は現行の挙動（直すかは**P3で決める**。[inventory.md §2.3](inventory.md)）。②〜⑧は安全側 | Driveに写真があるとき | 単体テスト＋`drive-index-python.json`（偽のDriveに流したPythonの`index_drive`と、`photos`の全列・`id`・Driveへの問い合わせ・追加/削除の件数が一致。②は意図した差として別に確かめる）。保留の閾値の前後（50件ちょうど・51件、割合がちょうど20%・それを超える）・1件だけ残る一覧・フラグで外れる・`--dry-run`の件数は`a_listing_that_would_delete_most_rows_holds_the_deletes_back`など | ①は**P3で直すか決める**。他は決定 | [§14.4](#144-index_drive) |
| **`fetch_derivatives`（T2.4）**: ①原本の一時ファイル`tmp/.dl_<drive_id>`は`create_new`＋`O_NOFOLLOW`＋権限0600で作り、既にある名前（リンクを含む）は辿らず・消さず、その写真の失敗にする（Pythonは`"wb"`で上書き）。②ダウンロードは512 MiBまで・1枚15分まで（Pythonは上限なし）。③取得待ちは`ORDER BY taken_at, id`（Pythonは`taken_at`のみ。実際のSQLiteは同じ並びになる）。④**失敗が続く写真を飛ばす**（受入条件②。[§14.6](#146-失敗が続く写真が枠を塞がない受入条件)）。⑤通信や認証の不調（接続・名前解決・TLS・**応答のヘッダーを受け取る前の切断などの I/O の失敗**・応答の待ちの時間切れ・流量制限の403（**理由の本文が受け取れなかった403を含む**）・429・5xx・認証の失敗・原本の本文の途中の切断）は、写真のせいではないので段を中断して失敗にし、失敗の記録は付けない（**本文の受信が1枚15分を超えた**ことだけは、その写真の失敗`fail_slow_download`）。⑥ログ・件数にファイル名を出さない（Pythonは名前を出す）。失敗は写真の`id`と固定の語だけ。成功・失敗の件数は形式別・失敗の種類別（`ok_jpeg`・`fail_decode_heic`など）。⑦`--dry-run`は一覧を読む以外の通信をせず、取得するはずの件数だけ出す。⑧**原本は通常の終了経路（成功・写真の失敗）で明示的に消して結果を確かめ**、消せなかったら`original_remove_failed`の件数と固定の文言の警告で報告する（Rustの`Drop`は早期`return`・パニックの補助で、結果は捨てる。通常の経路では消えたことを確かめて、報告する） | 原本（GPS・機種情報つき）を残さない・リンクで外を書き換えさせない・枠を塞がせない | 取得待ちがあるとき | 単体テスト（スタブ。原本が成功・失敗・パニックのどれでも残らない、リンクを辿らない、先頭の枠より多い失敗があっても後続が取得される、障害中に試した写真が復旧した次のサイクルで待たずに取得される、原本を消せなかったことが件数に出る）、`drive-index-python.json`（偽のDriveに流したPythonの`fetch_derivatives`と、最初のサイクルが同じ写真を同じ順に試し、同じ行・同じ寸法になる） | なし（決定） | [§14.5](#145-fetch_derivatives) |
| **位置の推定 `infer`（T2.5）**: ①**値が変わらない行には書かない**（Pythonは毎サイクル、対象の全行に同じ値を`UPDATE`する。最終の行は同じ）。②`UPDATE`は`loc_source IS NULL OR 'inferred'`の行にだけ効く（計画を作ってから書くまでの間に手動で位置が付いた行を上書きしない。Pythonにはこの隙間がある）。③時刻の差は**`TZ`に依存しないnaiveな日時**で取る（Pythonは`strptime(…).timestamp()`）。ASCIIの数字だけ読む（全角の数字が混じった時刻をPythonは読む場合がある。CPython 3.13で確認: 年が全角でも、分が1桁の全角でも読める）。④`GEOFRAME_INFER_OFFSET_MIN`は有限の数だけ（負も小数も可。Pythonは検証なし）。⑤同順位（同じ距離の錨が複数）は**`id`の小さい方**（Pythonは`ORDER BY`なしの全件走査の最初で、実際は`rowid`順＝同じ）。⑥**Pythonと同じで直さないもの**: `place elev muni_cd pref city`には触らない（錨が変わって位置が動いても、推定を取り消して別の場所へ推定し直しても、`place`は古い地名のまま残る）。`display`は取り消す行だけをNULLにする（`fetch_derivatives`が取り直す。ファイルは消さない）。`manual`は再推定しない・錨にはなる。`inferred`は錨にしない | ①②は書き込みを減らす（本番コピーの`--dry-run`が「変更ほぼ0」と読める）。③はP1以来の方針。⑥は現行の挙動（位置が動いたときに地名を付け直すかはP3で決める） | 位置の無い写真があるとき | `geo-python.json`（合成の行にPythonの`infer_locations`を段階的に流した結果と、`photos`の全列・戻り値が一致: 窓の境界・同順位・オフセット・錨の削除・錨の移動・手動・時刻なし・暦の不正。`tools/gen_geo_fixture.py`、CIで`--check`）、単体テスト、検証環境（§15.8。本番コピーで推定済みの2,162行すべてが「変更なし」） | ⑥は**P3で直すか決める**。他は決定 | [§15.2](#152-infer) |
| **地名の enrich（T2.5）**: ①**処理は`id`順の1パス**（Pythonと同じ）。キャッシュのキーは丸めた座標なので、近い座標の行は同じキーを共有し、先に来た行がキャッシュを作る（国外の行は国内の行のキャッシュを空で上書きする）。国内外を分けて国内を後回しにする作りは、先の国内の行が後の国外の空のキャッシュに当たって空の地名で確定するので採らない（レビュー指摘。`(20.0001,130.0)`と`(19.9999,130.0)`は同じキー）。②**通信の障害は短い待ちで2回まで再試行し**（0.5秒・1秒。停止の合図で打ち切る）、**続くときは座標の種類で分ける**: 接続できない（`dns`・`connect`・`tls`）・429は座標によらない障害なのですぐ段を中断する（その座標は覚えない）。5xx・時間切れ・切断・本文が読めないは座標による場合もあるので、その座標を覚えて30分以上飛ばして**次へ進み**、**続けて3つの異なるキー（座標）**で起きたら中断する（同じ座標の写真が何枚あっても1つ。応答が返った座標で数は0に戻る）。**段全体の経過時間にも上限がある**（2分。超えたら通信が要る未確定の行を次のサイクルへ回す。失敗にはしない）。Pythonは再試行せず、障害の行を飛ばして続ける（1行8秒×最大40行）。中断しても確定した行は残る。③**片方のAPIだけ障害のときは確定しない**（Pythonはもう片方の応答だけで確定してキャッシュするので、一時的な標高の不調で`elev`が、逆ジオの不調で`place`が、空のまま永久に残る。海上の空と見分けがつかない）。標高だけの長期障害の間は、国内の未キャッシュの行が進まない（直れば欠けなく埋まる。Pythonは進むが、その間の行の`elev`は恒久的にNULL）。応答は返ったが使えない（4xx・JSONでない）ときはPythonと同じく、もう片方の応答で確定する。④**確定していない先の行は、サイクルをまたいでも追い越させない**: 座標による障害・両方の応答なし・バックオフ中・段の中断や時間の上限で止まった行があるキーは、同じキーの後の行（国外・キャッシュ・別の座標の国内）を見送る（サイクル内は`deferred`、前のサイクルから続くバックオフ中の行は枠に入れず`skipped_blocked`）。追い越すと、国外の行が空のキャッシュを作り、先の国内の行は直ったあともその空に当たって地名が空のまま永久に確定する。通信の要らない行（別のキーの国外・キャッシュ）は止まったあとも済ませる。⑤`muni.js`は、国内で通信が要る行があるときだけ取りに行き、**取れなければ（障害でも、4xxでも、表が空でも）通信が要る行を見送り段を失敗にする**。Pythonは空の表で続け、`pref`・`city`が空のまま確定する。⑥**両方のAPIの応答が使えない座標も、プロセスの中で覚えてしばらく飛ばす**（30分から倍々、12時間まで）。Pythonは毎サイクル、40件の枠を占める。⑦**通信を試した行は、確定してもしなくても0.2秒待つ**（Pythonは確定した行のあとだけ）。件数は`queried`（最初の要求を出した行。再試行の待ちで止まった行も含む）と`resolved`（うち確定した行）・`unresolved`（確定しなかった行）に分ける。⑧**`lon`がNULL・数値でない行は対象にしない**（`unusable`に数える）。Pythonは例外になり、`cycle()`は`enrich_rows`を`try`で包んでいないので、周期全体が落ちる。⑨キャッシュの値は`place`と`city`の両方を持つ辞書だけ有効、型が違う値は空文字（Pythonは`city`だけ見て、`place`が無いと例外）。⑩書き込みは`lat`・`lon`が計画のときと同じで`place`がNULLの行にだけ効く。⑪**国外は空で確定**（オフラインの逆ジオコーダは本番に入っていない。Pythonも入っていなければ同じ）。⑫応答は1 MiB、`muni.js`は8 MiBまで。時間は8秒・15秒（Pythonと同じ）。⑬`muni_table`は、国内の問い合わせが要るときだけ取る（Pythonは毎サイクルの先頭で、無ければ取りに行く）。⑭ログ・失敗の文面に座標・地名・URLを出さない。⑮**停止の合図**は、行の境界・行のあとの待ち・再試行の待ちで見る。通信が進んでいる行は、標高・キャッシュ・DBの更新まで済ませてから止まる（普通の行は1秒未満、再試行込みでは最大で約51秒）。**時間の上限**は次の要求（再試行を含む）を始める前に見る。**Pythonとの差**: Pythonは、確定しなかった国内の行と同じキーの国外の行が後に来ると、国外の行がそのキーを空で上書きするので、先の国内の行は次のサイクルで空に当たる。Rustは先の行が確定するまで後の行を見送るので、障害があっても、直ったあとは障害が無かった場合（Pythonの順）と同じ地名に収束する | 障害で誤った「確定」を作らない、枠を塞がせない、サイクルを落とさない、Pythonの処理順を守る | 位置があって`place`がNULLの行があるとき | `geo-python.json`（Pythonの`enrich_rows`に偽の地理院を流した結果と、`photos`の全列・地理院への要求の順序・`.cache.json`の`geo:`キーが一致: 国内・国外・空で確定・矩形の角・片方だけ応答・両方なし・偶数丸め・先頭の0の自治体コード・キャッシュ済み・40件・`{:.3}`の丸め・**国内外でキーが衝突する行**。`--dry-run`の`would_query`もPythonの逆ジオの要求の数と一致）、単体テスト（再試行・全体の障害と座標による障害・標高だけの長期障害・本文が読めない失敗・待ち・停止・`muni.js`の失敗・記憶・競合・`--dry-run`）、検証環境（§15.8。実際の地理院と`muni.js`で、Pythonが保存した8行の地名が一致） | なし（決定） | [§15.3](#153-enrich)・[§15.4](#154-通信の失敗の分類) |
| **簡易品質 `quality`（T2.6）**: ①**特徴量の行はPythonが作ったものも有効として使い、Rustは新しい（または`display`・`md5`・`mtime`が変わった）写真だけを自分の算法で解析する**（全写真を解析し直さない。特徴量の表に算法の印が無いので、混在する）。②Rustの鮮明度・輝度・露出はPythonと少し違う（Pillowの`thumbnail`の`reducing_gap`による箱型の縮小を再現しない。本番コピー6,535枚で鮮明度の比の中央値1.004）。pHashは同じ作り方で、同じ写真のPythonの値との距離は最大6（12以上は0枚）。③**同じSHA-256の写真は、指標（鮮明度・露出・コントラストなど）を`id`が最小の行にそろえてから順位をつける**（Pythonは行ごと。Pythonだけが作った行では同じ値なので差は出ない。本番コピーで確認）。**pHashはそろえない**: 類似の結びつき（3分以内・距離12以下）は各行の元のpHashで決めるので、グループの顔ぶれ・`kind`・配信の抑制の規則はPythonと同じ（同じSHAの2枚のpHashがPython／Rustの算法の差で違うとき、結びつきも各行のpHashで決まる）。④失敗の行の`error`は種類だけ（Pythonは`型名: 例外の文字列`を500文字まで）。⑤`photo_quality.algorithm`は`rust-phash-v1:t3:h12`（Pythonは`poc-phash-v1:t3:h12`。再構築のたびに全行がこの値になる）。**これは特徴量の由来ではなく、その`photo_quality`を作った実装の印**で、Pythonはこの値を読まない（書き換えても特徴量を無効にしない。切り戻しは§16.10）。⑥特徴量の書き込みと`dirty`、再構築の読み書き（未解析の数・dirty・特徴量の読み取り・全差し替え）がそれぞれ1つのトランザクション（Pythonは別々）。⑦`GEOFRAME_QUALITY_PER_CYCLE`が負・小数・非数なら起動を断る（Pythonは`int()`で通し、`max(1, …)`で負を1にする）。⑧撮影時刻は`strptime`と同じ規則でASCIIの数字だけ読む（全角の数字が混じった時刻はPythonだけが読める場合がある。同じ既知の差）。⑨**混在の限界**: 特徴量の行にはPython製とRust製が混ざり、同じ画像のpHashはPythonとRustで最大6離れる（計測）。類似の判定の閾値は12なので、別の写真との距離が12の境目にいる組は、算法の差で結びつきが変わりうる（境目の保証はできない。本番コピーで無作為の300枚をRustで解析した混在では、顔ぶれが変わらなかった回（eb54361。§16.2の表）と、7枚が入れ替わった回（de90a57。§16.9の(h)）があった。完全重複の非推奨＝配信から外れる数は、測った回ではどれも93。§16.2・§16.9）。⑩Rustで作った結果をPythonへ切り戻すと、変化なしのサイクルは結果を保つが、後で`dirty`になるとPythonがRust製の特徴量を含めて再構築する。顔ぶれ・`kind`は変わらず、同じSHAの写しの指標の差から出る順位・推奨・スコアだけが変わりうる（完全重複の写しのどちらを表に出すかが入れ替わりうる。件数は同じ。スコアの変化で信頼度の並びが変わると、グループ番号`group_id`は振り直されうる。§16.10） | 全写真を解析し直すと、一括で424秒、既定の同期サイクル（25枚／10分）では約44時間かかり、グループの顔ぶれが変わる（7枚）。Pythonの行を使えば`photo_quality`もE1の件数も変わらない。他は安全側・決定性 | 取り込み済みの本番コピー・新しい写真があるとき | `quality-python.json`（合成の`photos`と合成の特徴量にPythonの`quality_cycle`・`quality_backfill`を段階的に流した結果と、特徴量・`photo_quality`・`settings`の全列が一致: 完全重複・3分と距離12の境界・推奨・順位・並び・無効化・孤児・件数制限・dirty・同じSHAでpHashが違う写真の結びつき。さらに`switchbacks`: Rust→Pythonへの切り戻し（変化なしのサイクル・dirtyでの再構築）をPython本体に流した結果。`tools/gen_quality_fixture.py`、CIで`--check`）、単体テスト、検証環境（§16.9。Rustの再構築が本番コピーの`photo_quality`559行と全列で一致、実サイクルで`.dump`不変） | ②は、Pythonの行を全部Rustで作り直す必要が出たら`geoframe quality-scan`（特徴量を消してから）で解消できる。他は決定 | [§16](#16-簡易品質p2--t26) |
| **更新API（T3.1。E12 `setloc`・E13 `setflag`と`POST`の入口）**: ①`i64`を超える整数の`id`はRustが存在しない`id`として404（Pythonは`sqlite3`が受け取れず500）。入力の検査（400）とwriterの確認（503）のあとで、**SQLを実行せずに**404にする（`i64::MAX`に丸めると、その`rowid`の行が実在するとき、より大きい`id`の要求が当たってしまう）。②JSONの`NaN`・`Infinity`はRustの解析が受け付けず400（Pythonは受け付けて「範囲外」の400。状態コードは同じで文面だけ違う）。③パスの`\`を`/`に読み替えない（`/api\admin\setloc`はPythonは当たり、Rustは403）。④`setflag`は`UPDATE … RETURNING`で1回の書き込みにする（PythonはUPDATEとSELECTが別）。⑤**書き込み（writer）が無い（`GEOFRAME_INGEST=0`）ときは、入力の検査（400）のあとで書かずに503**（Pythonに無い）。⑥未知の`POST`は`/api/blog/`配下も403（Pythonも403。依頼文の「`/api/blog/`の未知は404」は`GET`の話）。 ⑦**巨大・不正な`Content-Length`はaxum/hyperのHTTP層が`post.rs`より先に拒否する**（`u64`の最大値は431、それを超える値・数字でない値・負・空・`+`付き・アンダースコア入り・`N, N`・値が食い違う重複は400。Pythonは巨大なら413、`Content-Type`が違えば403、`+N`・`2_7`・最初の値が正しい重複は読んで書く）。状態コードと本文が[§17.1](#171-順序と状態コード)の表と違い、本文は定型JSONでない。**これらは書き込みが起きず、Pythonが書く入力をRustが断る向きの差だけ**。厳密な互換は求めない（[§17.1.1](#1711-http層が先に拒否する場合)）。⑧**逆向き（Pythonが断るのにRustが書く）の差は2つ見つかり、どちらもRustが書く前に断る**。(a)hyperは先頭の`0`を足した長い`Content-Length`（`0…037`）を通すが、Pythonの`int()`は10進で4300桁を超えると`ValueError`で400（`post::content_length`が同じ上限で400。4300桁ちょうどは両方通す。値が上限超過でも413でなく400）。(b)Python（`http.server`）は1行65536バイト（改行を含む）を超えるヘッダを431、要求行を414、ヘッダ100個以上（空行も数えるので99個まで）を431にするが、hyperの上限（head全体約400KiB・ヘッダ100個）は緩く書いてしまう（`post::head_limits`が書く前に431・414）。**残る穴**: 値の前後の空白と、hyperが捨てる重複した`Content-Length`の行はhyper通過後に見えないので、その分の長さ・個数はPythonの方が厳しいことがある。 ⑨**CSRFの判定（`Content-Type`・`Origin`・`Host`の値）は、生バイトを Latin-1 として読み、Pythonの`str.strip()`・`urlparse().netloc`・`ipaddress`と同じ規則で判定する**（`csrf.rs`。UTF-8として読み直すと、`\xC2\xA0`（UTF-8のNBSP）が空白として取り除かれ、Pythonが断る値を通してしまった。非ASCIIを一律に断る方式にしなかったのは、UTF-8の名前をそのまま`Origin`・`Host`に載せる非ブラウザを、Pythonは通すため）。**Pythonが断るものをRustが通す差は、`Origin`の値の末尾の空白・タブの1種類だけ**（hyperがRFC 9110のOWSとして取り除く。Pythonは空白を残して`netloc`に入れ403にする。HTTPのフレーミングの差として許す。ブラウザは`Origin`に空白を付けられず、付けられるクライアントは`Origin`を省けばPythonでも通る（無ければ通す仕様）ので、守りは変わらない）。差分試験の9213件のうち73件と、実際のHTTPの84件のうち10件がこれで、条件を狭く（`Origin`の値の末尾が空白かタブ）固定し、それ以外の許す向きは0件（[§17.1.2](#1712-csrfの判定のヘッダの値の読み方)）。Rustだけが断るのは次の2種類で、どちらも拒否の向き。(a)`Origin`の角括弧の中がIPv6でない（IPv4・ゾーンIDだけ・IPvFutureの書式違反・閉じ忘れ・前後の余り）などで、Pythonは`_csrf_ok`の中で`ValueError`になり**応答を返さず接続を閉じる**（状態コードが付かない）。Rustは403の定型JSON。(b)値に制御文字（0x00〜0x08・0x0a〜0x1f・0x7f）や折り返し（obs-fold）があると、hyperが要求ごと400にする（Pythonは`strip()`が取り除くので200になることがある）。 | ①②③⑦⑧⑨は実装の違い（⑦⑨は安全側）。⑤はP5まで本番の書き手がPythonで、Rustは書かない設計 | 更新APIを送ったとき | `tests/update_api.rs`が基準（`tools/gen_update_fixture.py`。現行の`handlers.Handler`に167件のPOSTを順に流した状態コード・応答・`photos`の変化と一致。差は`known_differences`の19件だけ。CIで`--check`）。HTTP層の境界（4300桁・ヘッダの大きさを含む）は実際のTCP接続で状態コードと接続の切断・書き込みの有無を固定（`content_length_boundaries_at_the_http_layer`）。CSRFの判定は`tests/csrf_python.rs`（`tools/gen_csrf_fixture.py`。9213件の判定と84件の実HTTP。CIで`--check`） | なし（決定） | [§17](#17-更新apit31) |
| **更新APIと同期サイクルの競合（T3.1）**: Pythonは`infer`の`UPDATE`が`WHERE id=?`だけで、計画を読んだあとに管理画面で直された位置（`manual`）を推定で上書き・取り消す。`enrich`の`UPDATE`も`WHERE id=?`だけで、地名の問い合わせ中に位置が直されると、古い座標の地名が新しい位置に書かれて`place`が埋まったまま残る（再enrichされない）。Rustは`infer`が`loc_source IS NULL OR 'inferred'`、`enrich`が`lat=? AND lon=? AND place IS NULL`の条件で守る（P2）。T3.1では`infer`の位置付けに「錨が計画のときの位置・種類のまま」の条件を足した（錨が手動で動かされたあとに古い位置を写さない） | 利用者の変更を失わない | 管理画面の操作が同期サイクルの段の途中に入ったとき | 割り込みを再現する単体テスト（§17.3） | なし（決定） | [§17.3](#173-同期サイクルとの競合) |
| **回転API（T3.2。E14 `POST /api/admin/rotate`）**: ①**小数の`delta`（`90.0`・`-90.0`・`9e1`）はRustが400**（Pythonは`90.0 == 90`で通し、`<drive_id>.r90.0.jpg`という名前のファイルを作って応答の`rotation`を`90.0`にする。DBには整数の90が入る）。②**`display`がシンボリックリンク・`derivatives/<安全な名前>`でない・通常のファイルでない（ディレクトリ・FIFO）ときは400「表示用の画像がまだありません」**（Pythonは`is_file()`でリンクを辿り、`DATA_DIR`の下の任意のファイル（リンク先が外でも）を読んで回し、元のファイルを`unlink`する）。③`drive_id`が名前に使えない文字（`/`・`..`・非ASCIIなど）の行は500（Pythonは経路にそのまま使う）。④`delta`と`id`の検査のあと、**writerが無ければファイルを作る前に503**（Pythonに無い）。⑤**DBの更新は条件付き**（読んだ時点の`display`・`rotation`・`md5`・`fetched_at`のままなら）で、外れたら何も書かず新しいファイルを消して**409**「他の処理と重なったので回転しませんでした。もう一度お試しください」（行が消えていれば404）。Pythonは無条件に書く。⑥回転どうし・回転と取得（`fetch_derivatives`）は`imaging::lock_derivatives`で直列化する（Pythonは読んで書くまでを排他しない）。⑦画像: Pillowの`rotate(-delta, expand=True)`と同じ向き・寸法、JPEG品質85・4:2:0・ベースライン、EXIFを付けない、EXIFの向きを当てない（Pillowも当てない）。画素は再圧縮の差を許す（デコーダがlibjpegと違うので、平均の差は数段階） | Pythonの癖を再現しない・安全側・競合の守り | 回転の要求を送ったとき | `tests/rotate_api.rs`が基準（`tools/gen_rotate_fixture.py`。合成のderivativeを置いた現行`admin_rotate`に57ケースを順に流した状態コード・応答・`photos`の変化・ファイルの増減（画像は寸法・四隅の色・EXIFの有無）と一致。差は`known_differences`の5件だけ。CIで`--check`）、`rotate/tests.rs`、`drive::tests`・`quality::tests`の競合の試験 | なし（決定） | [§17.8](#178-e14-rotatet32) |
| **回転と同期サイクル・取得・品質の競合（T3.2）**: Pythonは回転で「読む→画像を作る→`display`・`rotation`を書く→元のファイルを消す」の間を排他せず、①同期サイクルが`display`をNULLにした（推定の錨が消えた取り消し・Driveの`md5`の変更）あとでも、古い原本から作った画像を`display`に書き戻し、`fetch_derivatives`は`display`がNULLでないので取り直さない（`md5`が新しいのに古い画像が出続ける）。②回転どうしが重なると、片方の回転が失われ、負けた側の新しいファイルが孤児になる。③読んだ`src`を無条件に消すので、その間に同じ名前で作り直されたファイルを消すことがある。Rustは条件付きの更新（409）・ロックで守り、DBが今指しているファイルは消さない。**Rustでも残る穴**: `index_drive`が一覧から消えた行と一緒に消すファイルは、計画を読んだ時点の`display`なので、その直後に回転が入ると新しいファイルが孤児で1つ残る（Driveから消えた写真の回転と、一覧を読んだ直後の数ミリ秒が重なる場合だけ。増え続けない）。`md5`の変更で`rotation`が0に戻るとき古い回転済みのファイルを消さない件（[inventory.md §2.3](inventory.md)③）はPythonと同じ | 利用者の変更と実在するファイルの一貫性 | 回転が同期サイクル・取得と重なったとき | `rotate/tests.rs`（`display`のNULL化・`md5`の変更・同じ名前での作り直し・行の消失・同時の回転・DBの失敗）、`drive::tests::fetch_waits_for_a_rotation_in_progress_and_leaves_no_orphan`、`quality::tests::解析の途中で回転が入っても…` | ③は**T3.7で決める**。他は決定 | [§17.8.4](#1784-競合) |
| **地名検索 `GET /api/admin/geocode`（T3.3。E4）**: ①応答の本文を**1 MiBまで**にする（Pythonは上限なし）。超えたら通信の失敗と同じ（地理院なら空→Nominatim、Nominatimなら空でキャッシュしない）。②**同時に処理する検索は8件まで**。超える要求は待たせずに503（定型の本文。Pythonは無制限に待たせる）。2文字未満の語は数えない。③Nominatimの待ちの間に同じ語が別の要求でキャッシュされたら、**通信せずにそれを返す**（Pythonは待ったあとに必ず通信する）。④JSONの`NaN`・`Infinity`・`1e400`のようにf64に収まらない数は、本文全体が不正なJSONになる（Pythonは読む）。⑤`display_name`が文字列でない（数・配列など）ときは検索語を`title`にする（Pythonは`str()`）。⑥失敗のログに検索語を出さない（Pythonは`q=`を出す）。種類と状態コードだけ。⑦User-AgentはPythonの`http_json`と同じ`GeoFrame/0.2`（P2の他の通信は`geoframe/<版>`）。⑧**取り込みが無効（`GEOFRAME_INGEST=0`）では、`.cache.json`を読んでメモリに入れるだけで、ディスクへ書かない**（Pythonは常に保存スレッドが書く）。再起動すると、この間に得たNominatimの結果は消える。取り込みが有効なら同期サイクルと同じキャッシュを共有し、保存スレッドが書く。⑨応答のキーの並びは辞書順（`items`の各要素が`lat lon source title`。JSONとしては同じ） | ①②は暴走・巨大な応答への歯止め。③は公開サーバーへの問い合わせを減らす。④⑤は非現実的な入力。⑧は本番の書き手がP5の切替までPythonなので、Rustが`.cache.json`を書かない | 該当の入力のとき。⑧は`GEOFRAME_INGEST=0`の`serve` | `geocode/tests.rs`（ループバックのスタブ＋偽の時計。フィクスチャ`geocode-python.json`は現行`handlers.Handler`の出力） | なし（決定。⑧はP5の切替で本番の書き手がRustになれば取り込みが有効になり解消） | [§18](#18-地名検索p3--t33) |

P1の検証の記録は[p1-acceptance.md](p1-acceptance.md)、P2の検証の記録は[p2-acceptance.md](p2-acceptance.md)。

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
| 同期1サイクルで品質を解析する枚数（0以上の整数。0は1として扱う。T2.6） | なし | `GEOFRAME_QUALITY_PER_CYCLE` | `GEOFRAME_FETCH_PER_CYCLE`と同じ（§16.1） |

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

`config.rs`の`UNSUPPORTED_ENV`に、現行の`config.py`にあるP2以降の項目（取り込み・タイル・AI・位置候補など）を**列挙だけ**してある（T2.1で`GEOFRAME_DRIVE_POLL_MIN`、T2.3で`GEOFRAME_PHOTOS_DIR`・`GEOFRAME_FETCH_PER_CYCLE`、T2.4で`GEOFRAME_DRIVE_KEY`・`GEOFRAME_DRIVE_FOLDER`、T2.5で`GEOFRAME_INFER_OFFSET_MIN`、T2.6で`GEOFRAME_QUALITY_PER_CYCLE`を読み始めたので、20個）。読まず、警告も出さない（`/etc/default/geoframe`に書いてあっても無視される）。`docs/reference/configuration.md`にある`GEOFRAME_*`が、読む項目（`SUPPORTED_ENV`）か未対応のどちらかに分類されていることを、テスト`every_documented_variable_is_classified`が検査する。Pythonに項目が増えたら、このテストが落ちて気づける。

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
- **`Conflicts=geoframe-server.service`（P5まで。検証環境だけ）**: ガードはPythonの非稼働を起動時に1回しか見ないので、その後にPythonが起動されたときにRustの書き込みが続かないよう、unitに付けた（§11.1.1）。
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
3. **Host検査**（§9.2）。不許可は403。`Host`の値は生バイトを**Latin-1として**読む（§17.1.2。UTF-8として読み直さない）。
4. **絶対形式のrequest-target**（`GET http://host/…`）は、`Host`が許可でも**全ルートで403**（`/healthz`・`/api/v1/photos`も。Pythonはホワイトリストに当たらず403）。Host検査の後に判定するので、`Host`が不許可なら先にHost用の403になる（Pythonの`_host_ok`が先なのと同じ）。
5. **ルートの無い`POST`は403**（`/healthz`・`/api/v1/photos`など`GET`のAPIも）。ルート（`/api/admin/setloc`・`setflag`・`rotate`、`/api/blog/save`・`export`。`webpath::normalize`した結果の完全一致）がある`POST`は、CSRF対策を含めて`post::handle`が検査する（§17。P1では全部403だった）。
6. ルート: `/healthz`、`GET /api/v1/photos`（§8）、それ以外は`site`（下記）。

`site`（`GET`/`HEAD`）は、現行の`do_GET`/`do_HEAD`と同じ順に判定する。

1. `GET`の`/api/admin/status`・`/api/admin/photos`は管理画面の読み取りAPI（T1.4、§10）。**生のパスの前方一致**で、現行と同じ。`GET`の`/api/admin/geocode`（地名検索。T3.3、§18）も同じ前方一致。そのあと、`/tiles/`（地図タイル、P3）と、`GET`の`/api/ai/`・`/api/blog/`（P3・P4）は、**503**と定型のJSON `{"ok":false,"error":"この機能は Rust 版ではまだ使えません"}`（`Cache-Control: no-store`）。現行の「未知のAPI」の403とは区別して、「禁止」ではなく「まだ無い」と分かるようにした。実装するたびに該当のルートをここから外す。`/api/admin/`の残り（`setloc`・`setflag`・`rotate`などの更新ルートへの`GET`、`/api/admin/`そのもの、大文字小文字やエンコードの違い）は、現行でも`do_GET`のどの分岐にも当たらず静的配信のホワイトリストで**403**になるので、T3.3で503をやめて静的配信の判定へ回した（`/api/admin/../static/…`のように`..`を含む形も現行は403で、`geocode-python.json`の`admin_gets`で突き合わせている）。P4に`/api/admin/`配下の`GET`は無い（AI検索は`/api/ai/`）。
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
| 未実装のルート（`/tiles/`、`/api/ai/`、`/api/blog/`のGET） | 応答する | 503と定型のJSON | P3・P4で実装する（`status`・`photos`はT1.4、`geocode`はT3.3で実装、§10・§18。`/api/admin/`の残りのGETはT3.3から現行どおり403） |
| `POST` | 既知のルートはContent-Type不一致でJSONの403、それ以外はHTMLの403 | 同じ（T3.1。P1では全部HTMLの403だった。§17） | 更新ルートはT3.1で実装 |
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

- 検査は**起動時の1回だけ**。そのあとにPythonが起動された場合はバイナリは検知しない（P5までは、検証環境にPythonの本番unitは無い）。その補いとして、`deploy/geoframe-rs.service`に**`Conflicts=geoframe-server.service`**を付けた（検証環境だけで使う unit。Pythonが起動されるとsystemdがRustを止め、Rustを起動するとPythonを止める。`geoframe-server.service`が無い機体では何も起きない）。バイナリを直接起動した場合（systemd配下でない）には効かない。P5の切替でこの行をどうするかは[plan.md P5](plan.md#p5-切替pr-6)の「ガードを外す」の5。
- `sync --dry-run`は**ガードを通さない**。行・写真・キャッシュ・ロックを書かない（SQLiteのWAL関連ファイルだけはできうる）ので、ガードは要らない。**ただし「本番機で実行してよい」という意味ではない**: 対象は検証環境の本番コピー（そのバックアップのコピー）だけで、本番の`catalog.db`には向けない（[plan.md §2](plan.md#2-守ること本番機の扱い)。dry-runでもWAL関連ファイルができ、鍵があればDriveに接続する）。読み取り専用の`serve`（`GEOFRAME_INGEST`が`0`）も同じ。
- **試験用の差し替えは、デバッグビルドだけ**: `GEOFRAME_TEST_STAGING_MARKER`（印のパス）と`GEOFRAME_TEST_PYTHON_STATE`（`<LoadState>/<ActiveState>`。`unavailable`は取得失敗）。`#[cfg(debug_assertions)]`の内側にあるので、配布するリリースビルド（musl）にはコードが無く、環境変数は無視される。差し替えても「印が要る」ことは外れない（指した先に通常ファイルが無ければ断る）。
- **P5で外す**手順は[plan.md P5](plan.md#p5-切替pr-6)。外すときに消すもの: `guard.rs`と`lib.rs`の`pub mod guard`、`ingest::start`の`start_with`／`guard.check()`、`IngestError::Refused`と`cli.rs`の`exit_for_ingest_error`、`tests/ingest_cli.rs`の`Staging`と`GEOFRAME_TEST_*`、`USAGE`・本節・§0の行・§6の項・`deploy/README.md §7.1`・`deploy/geoframe-rs.service`の記述。

### 11.2 設定

| 変数 | 既定 | 解釈 | 棚卸しとの対応 |
| --- | --- | --- | --- |
| `GEOFRAME_INGEST`（Rustだけ） | `0` | `0`／`1`だけ（他は起動を断る）。空文字は未設定 | [inventory.md §3.1](inventory.md) B0・B1（起動時の準備と同期サイクル）を動かすか。Pythonには無い |
| `GEOFRAME_DRIVE_POLL_MIN` | `10` | 分。小数も可（`0.5`は30秒）。Pythonは`float()`で秒に換算。**Rustは0以下・非有限・上限（525600分＝1年）超を起動の断りにする**（Pythonは0以下で空回りか例外） | [inventory.md §4.1](inventory.md)の`GEOFRAME_DRIVE_POLL_MIN`（P2・引き継ぐ） |

`GEOFRAME_DRIVE_POLL_MIN`は`UNSUPPORTED_ENV`から`SUPPORTED_ENV`へ移した。`GEOFRAME_INGEST`は`GEOFRAME_LOG`と同じRustだけの項目なので`docs/reference/configuration.md`（Pythonの`config.py`から生成）には無く、どちらの一覧にも載せない。`GEOFRAME_PHOTOS_DIR`・`GEOFRAME_FETCH_PER_CYCLE`はT2.3で読み始めた（§13.1）。`GEOFRAME_DRIVE_KEY`・`GEOFRAME_DRIVE_FOLDER`はT2.4で読み始めた（§14.1）。`GEOFRAME_INFER_OFFSET_MIN`はT2.5で読み始めた（§15.1）。`GEOFRAME_QUALITY_PER_CYCLE`はT2.6で読み始めた（§16.1）。

### 11.3 同期サイクルの骨組み

現行の`catalog.py:cycle()`の順に、**段（`Stage`）を並べた**もの。段は`index_local`→`index_drive`→`infer`→`enrich`→`local_derivatives`→`fetch_derivatives`→`quality`。T2.1の段は何もしない（`Placeholder`）で、T2.3〜T2.6で全部の段に中身が入った（`quality`はT2.6。§16）。

- **起動直後に1回、以降`GEOFRAME_DRIVE_POLL_MIN`ごと**。専用のstdスレッド`sync-worker`で動き、HTTPサーバーの起動を待たせない。SIGTERMで`StopFlag`を立て、待ちの最中ならすぐ、サイクルの最中なら今の段が終わってから止まる（最大10秒待ち、超えたらそのまま終了する。途中で止まってもDBはトランザクション単位なので壊れず、`.dl_*`の残骸は次回の起動で消す）。
- **段ごとに失敗を隔離する**: 段が`Err`を返しても、パニックしても、残りの段と次の周期へ進む。ログに出すのは段の名前と失敗の種類だけ（`StageError`はDB・I/Oの種類と定型文しか持たない。パニックのメッセージは出さず、場所だけをpanic hookが出す）。Pythonの`index_drive`段の「Drive認証に失敗したらDBの段だけ飛ばす」は、段が`Skipped(理由)`を返す形で表す（T2.4）。
- **ログは件数だけ**: 段ごとの件数（`added=3`のような固定の名前と数）と、サイクル末尾の`表示可能／取得待ち／全`（Pythonの`[cycle]`行と同じ集計）。
- **サイクルの前に、そのサイクル用の読み取り専用の接続を開く**（開けなければ次の周期へ持ち越す）。段は読むときは`ctx.read`、書くときは`ctx.writer()`（writer）を使う。

#### `--dry-run`

`geoframe sync --dry-run [--data-dir <dir>]`は、**書かずに「何をするか」の件数だけを出す**。書き込み接続を作らない（`ctx.writer`が`None`）ので、実DBへの書き込みは構造的に起きない。キャッシュは凍結して読むだけ。**段に渡す読み取り用の接続`ctx.read`は、実DBのメモリ上の写し**（`sync::simulation_copy`。SQLiteのバックアップAPIで作る。`rusqlite`の`backup`機能を足した）で、先の段が自分の計画を写しに適用する（`index_local`が追加・更新・削除を、`infer`が推定・取り消しを、`local_derivatives`が`display`を、実行と同じ書き込み関数で写しに書く。`enrich`は地理院に問い合わせないので、写しには書かず件数だけを数える）。後の段は「先の段が済んだあと」の状態で件数を数え、最後の`写真: 表示可能／取得待ち／全`も同じ状態の見込みになる（T2.3の再レビューの指摘。以前は後続の段が索引前の行を見て、新しい写真を数え落とした）。写しの大きさはDBと同じ（検証環境の本番コピーで約85 MiB。`sync --dry-run`は0.3秒ほどで終わる）で、dry-runのときだけ作る。**写しの間だけ`ctx.read`の元の接続で読み取りトランザクションを持ち、スナップショットを固定する**（バックアップは元が更新されると最初からやり直すので、更新の多いDBで終わらなくなるのを防ぐ。WALなら書き手を止めず、書き手の更新は写しに入らない。ロールバックジャーナルのDBでは写しの間だけ書き手が待つ）。さらに**30秒の期限**（`sync::COPY_DEADLINE`。本番相当で0.3秒の約100倍）を付け、ロックで進めない間は20 ms間隔で繰り返し、超えたら`geoframe: DB のメモリ上の写しを作れません: …`で終了コード1にする（`busy_timeout`の分までは超えうる）。**行・写真・キャッシュ・ロック・`tmp/`・`derivatives/`は変えないが、SQLiteが読み取りでも作る`catalog.db-wal`・`catalog.db-shm`は新しくできうる**（WALのDBを開くとできる。中身は書かれない。ヘルプ（`USAGE`）と起動の1行目にも書いてある）。**ガード（§11.1.1）は通さない**: 書かないからで、本番機で実行してよいという意味ではない。向ける先は検証環境の本番コピー（そのバックアップのコピー）で、本番の`catalog.db`には向けない（[plan.md §2](plan.md#2-守ること本番機の扱い)。WAL関連ファイルができ、鍵があればDriveに接続する）。出すもの:

```
dry-run: 何も書きません（行・ファイル・キャッシュ・ロックは変えません。SQLite の WAL 関連ファイルだけはできうります）
スキーマ: 作る表・索引 N / 足す列 N / place を NULL に戻す あり|なし / embedding の写し N
掃除: 消す .dl_* N 件 / 捨てる .cache.json の旧キー N 件
（リンクなどで掃除を飛ばす tmp/・derivatives/ があるときだけ）掃除を飛ばす tmp/・derivatives/（…）: N つ
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

`ingest::prepare_tmp_dir`（`drive.py:prepare_tmp_dir`と同じ）。`DATA_DIR/tmp/`を用意し、`tmp/`と`derivatives/`の`.dl_*`を消す。フル解像度・EXIF完全保持のDrive原本が残らないようにするため。消すのは**通常のファイルだけ**（ディレクトリ・シンボリックリンクは触らない。消せないものは無視）。`tmp/`・`derivatives/`自体がシンボリックリンク（など通常のディレクトリでないもの）のときは、その中を掃除しない（[§13.14](#1314-再々レビュー2026-10-24の指摘への対応)）。名前はログに出さず、件数だけ（`[fetch] 一時ダウンロードの残骸を…`）。`tile_cache_prune`（B4）はP3。T2.3で、続けて`derivatives/`の書きかけの`*.part`も消すようにした（[§13.5](#135-書きかけの-part-の掃除)）。

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
| 起動処理（`ingest.rs`） | 断られたら置き場・DB・ロックが作られず中身も変わらないこと、 `.dl_*`の掃除（通常ファイルだけ。リンク・ディレクトリは触らない。`tmp/`・`derivatives/`がリンクならリンク先を消さない）、DBの作成、ロック、再起動で何も変わらないこと、`plan`が書かないこと |
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

**レビュー（全体）の指摘への対応後（`Conflicts=`）。** unitに`Conflicts=geoframe-server.service`を足して`deploy/rust-deploy.sh`で配布した。

| 確認 | 結果 |
| --- | --- |
| `systemctl show geoframe-rs -p Conflicts` | `Conflicts=geoframe-server.service shutdown.target`（`shutdown.target`はsystemdが既定で足す）。`geoframe-server.service`は未導入（`LoadState=not-found`・`ActiveState=inactive`）のままで、`geoframe-rs`は`active`で起動した |
| 起動のログ・`GET /healthz` | `取り込みは無効です（DB に書きません）`（`ingest=0`）。200 |

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

**T2.4で実装した。実機iPhoneのHEICの確認と、原本の形式別の成功・失敗をPythonと比べる手順は[§14.11](#1411-鍵が置かれたあとの管理者の確認手順)。受入条件②（枠の占有）は[§14.6](#146-失敗が続く写真が枠を塞がない受入条件)、③（`create_new`＋`O_NOFOLLOW`）は[§14.5](#145-fetch_derivatives)で対応済み。以下は回したときの記録。**

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
| `GEOFRAME_PHOTOS_DIR`（既定`photos`） | `DATA_DIR`からの相対（絶対パスなら、そのパス）。現行の`(DATA_DIR / 値).resolve()`と同じ。`canonicalize`した先が`DATA_DIR`の**中のサブディレクトリ**でなければ`index_local`は失敗する（§0）。ディレクトリを作る前に親を検査するので、外を指す未作成のパスでも外には何もできない。ディレクトリ名`photos`は変えない（`display`・`path`列の相対パスが配信URLを兼ねる） |
| `GEOFRAME_FETCH_PER_CYCLE`（既定`25`） | `local_derivatives`が1サイクルで処理する枚数。T2.4で`fetch_derivatives`も同じ値を使う。0以上の整数だけ（それ以外は起動を断る。終了コード2） |
| ログ | 段ごとの件数だけ（`files added updated removed unreadable skipped …`、`original made failed icc_dropped`）。ファイル名・座標・撮影日時・例外の文字列は出さない |

### 13.2 `index_local`

**規則はPythonと同じ**で、違いは§0の行と下の表に挙げたものだけ。

1. **走査**: `PHOTOS_DIR`の下を再帰して、拡張子が`imaging::EXTS`（大文字小文字を区別しない）のファイルを集める。パスの要素のどれかが**ちょうど`gdrive`**なら除外する（旧ミラー先。部分一致ではなく、`gdrive2`・`mygdrive`は対象）。ドット始まりのファイル・ディレクトリも対象（Pythonと同じ）。`PHOTOS_DIR`が無ければ作る（`mkdir`。最後の1階層だけ。親が無ければ失敗）。**作る前に**、親を`canonicalize`して`DATA_DIR`の中かを確かめる（`..`やリンクを経由する未作成のパスは、外にディレクトリを作らずに断る。以前は`mkdir`してから検査していたので、段は失敗しても外にディレクトリが残った）。`--dry-run`は作らず、**置き場が無ければ空の走査として**同じ計画の関数を通す（実行は作って空の走査をし、既存のローカル行を削除するので、dry-runもその削除と消すderivativeを数える）。
2. **並び**: パスの要素の列の昇順（Pythonの`sorted(rglob("*"))`）。`a/b.jpg`は`a-b.jpg`より前。新しい行の`id`はこの順に振られる（`INTEGER PRIMARY KEY`なので`max+1`）。
3. **追加**: `photos(source='local', path, name, mtime, taken_at, lat, lon, loc_source, indexed_at)`。`path`は`DATA_DIR`からの相対（`/`区切り）、`name`はファイル名、`mtime`は`st_mtime`（`sec + nsec * 1e-9`の`double`。CPythonと同じ式で、比較テストでビット一致を確かめた）、`taken_at`は`imaging::extract_exif`の値（読めなければ空文字。NULLにしない）、`lat`・`lon`はGPSがあるときだけ、`loc_source`はGPSがあれば`exif`、無ければNULL、`indexed_at`は`YYYY-MM-DDTHH:MM:SS`（現地時刻・オフセット無し）で、段の先頭で1回だけ作って全行に使う。
4. **更新**: 既存の行（`path`が一致）で`mtime`の差が**1秒以上**（または`mtime`がNULL）のとき、`mtime taken_at lat lon loc_source`を上書きし、`place elev display`をNULLに戻し、`indexed_at`を更新する。**`name favorite hidden rotation title inferred_from infer_dt_s muni_cd pref city fetched_at`は触らない**（手動の位置は上書きされる。§0）。1秒未満の差は何もしない。
5. **削除**: 走査で見つからなかった`source='local'`の行（`path`がNULLの行も）を削除する。`display`が`derivatives/`で始まる行は、そのファイルも消す（`photos/`の原本は消さない）。消すのは行の削除を確定したあとで、名前は`DataDir::derivative_file`の検査（`derivatives/`直下の1ファイル、安全な名前、`derivatives`がリンクでない）を通ったものだけ。
6. **読めない写真**: 画像として読めない（`NotAnImage`・読み取りの失敗・512 MiB超・BigTIFF）写真は、行を作らず・更新せず、件数（`unreadable`）に数える。既に索引した写真が壊れても、走査では見つけているので行は消えない（Pythonと同じ）。EXIFが無い・壊れているのは失敗ではない（位置なし・時刻なし）。
7. **中断**: 停止の合図は、行・ファイルを**変える前**にだけ効く。確認する点は、走査でディレクトリを開くたび・計画で写真1枚ごと・計画の完了時（写真が0枚でも通る。最後の1枚のEXIF読み取り中に来た合図もここで拾う）・書き込みの始め・書き込みの途中で256件ごと・`COMMIT`の直前。どこで来ても`ROLLBACK`して何も変えず、段は`Skipped`（`停止の合図で中断しました`）で終わる（走査が途中の「見つからなかった」で行を消さない）。derivativeのファイル消しは`COMMIT`の**後**なので、`COMMIT`しなければ消さない。逆に`COMMIT`した後は、行がもう無くて後から拾えないので、合図が来ても最後まで消す。

**書き込みの形**: 走査・EXIFの読み取りはwriterの外で先に済ませ、計画（追加・更新・削除）を作ってから、`writer.call`1回の中で`BEGIN`〜`COMMIT`にまとめて書く。途中で落ちても中途半端な状態を残さない。`--dry-run`は計画の件数だけを出し、置き場の作成・実DBへの書き込み・ファイルの削除をしない（計画は§11.3の写しに適用して、後続の段に見せる）。実行とdry-runは**同じ計画の関数**（`IndexLocal::plan`）と**同じ書き込み関数**（`write_plan`）を通る。

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
- **失敗が続く写真は飛ばす**（T2.4で入れた。[§14.6](#146-失敗が続く写真が枠を塞がない受入条件)）: 変換に失敗した写真（`id`と`mtime`）は、30分・1時間・2時間…（上限12時間）のあいだ飛ばして、後続を枠に入れる。プロセスの中だけの記録で、ファイルが更新されれば（`mtime`が変わる）すぐ試す。`--dry-run`は覚えない。飛ばした数は`skipped_backoff`（あるときだけ出る）。
- `--dry-run`も、実行と同じ順に`resolve_existing`でパスを検査し（存在しない・`DATA_DIR`の外は`failed`）、`display=path`にする枚数（`original`）とderivativeを作る枚数（`made`）を数える。対象の行は、`index_local`の計画を適用した写しから選ぶ（新しく索引される位置つきの写真も数える）。違いは、derivativeを実際に作らない（変換の失敗は分からず`failed`に出ない・`icc_dropped`は常に0）ことと、`display`を書く先が写しであることだけ。出力の件数の組（`original made failed icc_dropped`）は実行と同じ。

### 13.4 `geoframe organize`

```
geoframe organize [--dry-run] [--data-dir <dir>]
```

位置の無いローカル写真を`DATA_DIR/_excluded/no_gps/`へ移し、行を削除する（Pythonの`server.py --organize`）。`index_local`を先に1回動かし、その結果を使う（失敗したら移さない）。書く実行は`GEOFRAME_INGEST=1`が要り、**`sync`と同じガード（§11.1.1）・ロック・移行・掃除を通す**。無いと終了コード2で断る。

- **対象**: `source='local' AND lat IS NULL`の行。**`index_local`→`infer`（§15.2）→移動の順**で、推定で位置が付いた写真は対象に入らない（Pythonの`index_local`→`infer_locations`→移動と同じ。錨が消えれば推定は取り消され、次の`organize`で移る）。T2.3〜T2.4はRustの`infer`が何もしなかったので、推定で位置が付きうる写真（撮影時刻があって、錨になる位置つきの写真が1枚でもあるもの）を移さず`held_back_until_infer`として数えていたが、T2.5で外した（出力の件数からも無くなった）。
- **ファイル**: `entry_path`（親を`canonicalize`して`DATA_DIR`の中か検査し、最後の要素のリンクは辿らない）で絶対パスにし、**通常のファイルだけ**を移す。リンク・ディレクトリは動かさず、行も残す（`skipped`）。ファイルが無い行は、Pythonと同じく行だけ消す（`row_only_removed`）。
- **移動先**: `_excluded`・`_excluded/no_gps`を1階層ずつ`symlink_metadata`で見て、リンクやファイルならディレクトリを作らずに断る（`DATA_DIR`の外へ出さない）。ファイル名はそのまま。**同名があっても上書きしない**: ハードリンクを作ってから元を消す（リンクは既にあれば失敗するので、同名のファイルを壊さない）。あれば`名前 (2).拡張子`、`(3)`…と探す。ハードリンクを作れないファイルシステムでは、無いことを確かめてから`rename`する。1つの移動に失敗しても次へ進む（`failed`）。**元の削除に失敗したら、作ったリンクを取り消す**（元のディレクトリが書き込み禁止のとき、リンクだけが残って行も残り、再実行のたびに`(2)`・`(3)`と増えて、候補を使い切ると権限を直しても移せなくなった）。取り消しにも失敗して残ったリンクは、次の実行で**退避先に同じ`inode`の名前があれば**それを使い、リンクを重ねずに元だけ消す（同じ実体への名前なので、中身は退避先に残る。別々の行が元からハードリンクだった場合も、実体は退避先に1つ残る）。
- 行の削除は、ファイルを移したあとに1行ずつ。
- `--dry-run`は何も動かさずに、実行と同じ分類（移す・行だけ消す・動かさない・調べられない）で`targets moved row_only_removed skipped failed`を数える。対象は、`index_local`と`infer`の計画を適用した写し（§11.3）の行なので、新しく索引される位置の無い写真も含み、推定で位置が付く写真は含まない。移動先は作らず（リンクを経由していないかの検査だけ行う）、移せるものとして数える（権限・名前の使い切りは試さない）。実行の出力も同じ5つの件数（対象が0件でも）。
- unitの`ReadOnlyPaths`は`photos`を読み取り専用にするので、`organize`はunitの外（`sudo -u geoframe geoframe organize`）で動かす。出力はファイル名を含まない。

### 13.5 書きかけの`.part`の掃除

T2.2の注意（§12.2）の対応。`ingest::start`の最後（ガード・ロック・移行・`.dl_*`の掃除のあと）で、`derivatives/`**直下**の次のファイルを消す。

- 通常のファイル（`DirEntry::file_type`はリンクを辿らない。シンボリックリンク・ディレクトリは消さない・辿らない）
- 名前が`.part`で終わり、`datadir::check_file_name`を通る（英数字・`_`・`-`・`.`、先頭が`.`でない、`..`を含まない）。Rustの`<出力名>.<pid>.<乱数>.part`もPythonの`<stem>.part`も対象
- `derivatives`自体がシンボリックリンクなら何もしない

消した数だけをログに出す（`[derivative] 書きかけの .part を N件削除しました`）。`--dry-run`は「消す書きかけの .part N 件」として数える。**判断**: 取り込みのロックを持つ間は、他に`.part`を作るプロセスがいない（Rustの別プロセスはロックで、Pythonはガードが止まっていることを確かめる）ので、残っているものはすべて書きかけの残骸と言える。中身はEXIFを持たない部分的なJPEGで、個人情報の問題はなく、容量を食うだけなので、起動時に1回の掃除で十分（サイクルの最中は掃除しない）。P3で管理画面の回転がderivativeを作るようになっても、`serve`の起動時の掃除は他のスレッドが動く前なので重ならない。

### 13.6 `--dry-run`

`geoframe sync --dry-run`の`index_local`・`local_derivatives`の段が、書かずに件数を出す（§11.3）: `index_local`は`files added updated removed unreadable skipped …`（置き場が無ければ空の走査として、既存のローカル行の削除を数える）、`local_derivatives`は`original made failed icc_dropped`（後続の段は索引後の写しで数える。§13.3）。掃除の行には`.part`の件数が増えた（`掃除: 消す .dl_* N 件 / 消す書きかけの .part N 件 / 捨てる .cache.json の旧キー N 件`）。リンクなどで掃除を飛ばす`tmp/`・`derivatives/`があるときは、`掃除を飛ばす tmp/・derivatives/（…）: N つ`の行も出る（[§13.14](#1314-再々レビュー2026-10-24の指摘への対応)）。`geoframe organize --dry-run`は`targets moved row_only_removed skipped failed`（`index_local`と`infer`のあとの写しで数える。§13.4）。**判断（再レビューの指摘5）**: 後続の段の件数は「索引前の状態で数える」案ではなく、メモリ上の写しに計画を適用する案にした。写しの作成はバックアップAPIの1回で済み、段の側は`ctx.read`の使い方を変えずに、書き込み関数を共有するだけで済んだ。

### 13.7 Pythonとの差（まとめ）

§0の4行に載せたものがすべて。要点:

- **現行のまま残した危ない挙動（P3で決める）**: `mtime`が変わると**手動で直した位置も上書きされる**。お気に入り・除外・回転・タイトルは残る。
- **安全側に変えたもの**: ファイルのリンクは実体が`DATA_DIR`の中のものだけ、`PHOTOS_DIR`が外・`DATA_DIR`そのものなら断る、消すderivativeの名前の検査、`organize`は上書きせず通常のファイルだけ動かす。
- **T2.5で解消したもの**: `organize`が推定で位置が付きうる写真を移さない制限（§15.7）。
- 意図して同じにしたもの: 走査順（`id`の採番順）、`gdrive`の完全一致、1秒の許容、読めない写真は飛ばす、削除時の`photos/`の原本は残す、`indexed_at`/`fetched_at`の形式、位置のあるローカル写真だけにderivativeを作る。

### 13.8 テスト

| 対象 | テスト |
| --- | --- |
| Pythonとの突き合わせ（`tests/local_index_python.rs`） | `tools/gen_local_index_fixture.py`が作る`local-index-python.json`（合成のJPEG・PNG・TIFF・HEICの木。ナノ秒つきの`mtime`、`gdrive`・隠しディレクトリ・日本語名・並びの境目・画像でないファイル。4段階の操作: 初回の索引・変化なし・更新と削除と追加と手動の書き込み・変化なし）を、Rustが同じ木と操作で`IndexLocal`に通し、`photos`の**全列と`id`**、書き換えられたか（`indexed_at`に入れた目印が残るか）、`derivatives/`のファイルの有無が一致することを確かめる。意図した差（壊れたGPSの写真）は別のテストで確かめる。CIの`app`ジョブが`--check`でPythonの結果を検査する（`pillow-heif`が必要） |
| `local.rs`の単体テスト | 追加の列、2回目に何も書かない、1秒の許容、更新で上書きする列と残す列、削除（`derivatives/`だけファイルも消す、危険な`display`は消さない）、`gdrive`・拡張子・隠しファイル、走査順、読めない写真、リンク（ディレクトリは降りない・外を指すファイルは読まない）、置き場が外・`DATA_DIR`そのもの、置き場の親が`DATA_DIR`の外（絶対パス・`..`・リンク。作る前に断り外に何も作らない）、`dry-run`（置き場が無くても削除を数え、実行と同じ件数）、停止（写真0枚、確認点を1回目から順に変えて「何も変えない」か「全部やった」のどちらかになること、256件ごとの確認）、ログに名前が出ないこと、`local_derivatives`（JPEG・HEIC、上限、失敗、`path`の逸脱、位置なし、`dry-run`）、`organize`（移動・同名・保留・`dry-run`・欠けた行・リンク・移動先がリンク、元の削除に失敗したときのリンクの取り消し・取り消せずに残ったリンクの再利用）、後続の段のdry-run（`index_local`→`local_derivatives`→`organize`を写しの上で通し、実行と同じ件数になること）、MD5の名前（Pythonの値）、大量の変更の計測（`--ignored`。§13.12） |
| `ingest.rs` | `.part`の掃除（対象と対象外、リンク、`derivatives`がリンク、ガードが断れば消さない） |
| `datadir.rs` | `derivative_file`・`entry_path` |
| `tests/ingest_cli.rs` | `sync --dry-run`の出力（`.part`の件数・新しい段の件数）、`organize`（INGESTなし・印なしで断る、`--dry-run`が何も変えず索引後の対象を数える、ガードを通すと移して行を消す。Driveの行は触らない。出力に名前が出ない） |

### 13.9 検証環境での確認（2026-10-24）

musl静的バイナリを`deploy/rust-deploy.sh`で配布して確かめた。検証環境の`catalog.db`は本番のバックアップのコピー（6,535行、すべて`source='drive'`）。**書き換える前に、検証環境の中で`sqlite3 .backup`のコピーを取った**（`.dump`のSHA-256を記録）。合成の写真は、`tests/fixtures/local-index-python.json`の操作を検証環境の中で再生して置いた（実写真は使わず、検証環境の外へも出していない）。

| 確認 | 結果 |
| --- | --- |
| (a) 写真を置く前の`--dry-run` | 本番コピーに対して、`index_local`は`files=0 added=0 updated=0 removed=0`、`local_derivatives`は`original=0 made=0`、掃除は`.dl_* 0 / .part 0 / 旧キー 0`。他の段は件数なし、`写真: 表示可能 5288 / 取得待ち 0 / 全6535`（本番コピーに対して変更ほぼ0。P2の受入の関門） |
| (b) 合成の写真19ファイル（`gdrive`配下・対象外の拡張子・壊れたファイル含む）を置いた`--dry-run` | `files=16 added=15 unreadable=1`（壊れたGPSの1枚はPythonと違い索引する。§0）。置き場は変わらない |
| (c) `GEOFRAME_INGEST=1`の`sync`1回目・2回目 | 1回目は`added=15`、`local_derivatives`は`original=8`（位置のあるJPEG）。**2回目は`added=0 updated=0 removed=0`、`made=0`**。`.dump`の差分は**15行（すべて`source='local'`の`INSERT`）で、ほかの表・行の差は0**。Driveの6,535行の中身（`SELECT *`のSHA-256）は変わらない |
| (d) 比較用Python（`geoframe-pyref`、別データ）との比較 | pyrefを止めて（DBは検証環境の中でバックアップ）、同じ木をpyrefの置き場に置き、pyrefのvenv（Python 3.12.3・Pillow 13.0.0・pillow-heif 1.8.1）の`index_local`・`local_derivatives`を通し、`photos`の`source='local'`の行をRustの行（壊れたGPSの1枚を除く）と**`id`の並び順・`path name mtime taken_at lat lon loc_source display …`の全列まで比べた: 14行すべて一致**（`indexed_at`・`fetched_at`は形式まで一致）。続けて変化の操作（更新・削除・追加）を流して、もう一度比べて**14行すべて一致**。HEICに位置を与えたときのderivativeの名前も同じ（`local_<md5>.jpg`、64x48のJPEG） |
| (e) `organize` | `INGEST`なしは終了コード2。`--dry-run`は`targets=5 held_back_until_infer=2`。実行で5枚を`_excluded/no_gps/`へ移して行を削除（`moved=5 failed=0`）。もう一度動かすと`targets=0`。時刻つきで錨がある2枚は残った |
| (f) `.part`の掃除 | `derivatives/`に書きかけの`.part`1つと、`.part`という名前のシンボリックリンク1つ（リンク先は別の場所の一時ファイル）を置いた。`--dry-run`は`消す書きかけの .part 1 件`（リンクは数えない）。`sync`が1件削除し、リンク先の別の場所の一時ファイルの中身は無傷（検証環境の`derivatives/`は作業用のユーザーから読めないので、リンク自体が残ることは直接見ていない。単体テスト`sweep_does_not_follow_or_remove_links`が確かめる） |
| (g) unitの制限下（`NoNewPrivileges`・`ProtectSystem=strict`・`ReadOnlyPaths`に`photos`・`MemoryDenyWriteExecute`）で`GEOFRAME_INGEST=1`のdrop-inで起動 | 起動時のサイクルが通り（`index_local`が`photos`が読み取り専用でも走査できる。置き場の`mkdir`は`AlreadyExists`で済む）、写真を1枚足して再起動すると`added=1`・`original=1`。`/api/v1/photos`は3,065＋合成のぶん増え、`/photos/<名前>`は200 |
| 後片付け | 合成の写真・`_excluded`・`derivatives/local_*.jpg`・Rustが足した`source='local'`の行を消し、drop-inを外して`ingest=0`（`取り込みは無効です`）に戻した。**`.dump`のSHA-256は配布前のコピーと一致**。`/healthz`は200。比較用Pythonは、DBを変更前のバックアップと同じ内容に戻して（`.dump`が一致）起動し直した。検証環境の中に作ったバックアップは確認後に削除した |

### 13.10 T2.5・T2.4へ回したもの

- **T2.5（実施済み。§15）**: `organize`の「推定で位置が付きうる写真は移さない」制限（§13.4）を外し、`index_local`→`infer`→移動の順にした。
- **T2.4（実施済み。§14）**: `fetch_derivatives`の取得待ちの占有（§12.10の受入条件②）は、`local_derivatives`にも同じ仕組みで入れた（§14.6）。
- **P3**: 手動の位置がmtimeの変更で上書きされる挙動（§0、[inventory.md §2.3](inventory.md)）を直すか決める。直すなら、`loc_source='manual'`の行は`lat lon loc_source`を更新しない・`display`だけをNULLにする、などの選択肢がある。

### 13.11 再レビュー（2026-10-24）の指摘への対応

| 指摘 | 対応 |
| --- | --- |
| 1. 置き場が無いとdry-runが削除を見落とす | 置き場が無いときは空の`Scan`として、実行と同じ計画の関数を通す（§13.2）。実行は置き場を作って空の走査をし、既存のローカル行を消す。dry-runも同じ削除と、消すderivativeを数える。件数一致は単体テスト`写真の置き場が無くてもdry_runは削除を数え_実行と同じ件数になる`で確かめた |
| 2. 停止の合図を受けても行・ファイルを消す経路 | 確認点を、走査中・写真ごと・計画の完了時・書き込みの始め・256件ごと・`COMMIT`の直前に増やし、確定前の合図は`ROLLBACK`して`Skipped`（§13.2の7）。derivativeのファイル消しは`COMMIT`の後なので、合図で確定しなければ消さない。試験は、確認点を1回目から順に変えて「全く変えない」か「全部やった」のどちらかになること（確認点は7つ）と、写真0枚、256件ごとの確認 |
| 3. `DATA_DIR`の外へ作ってから検査 | 作る前に親を`canonicalize`して`DATA_DIR`の中かを確かめる（§13.2の1）。絶対パス・`..`・リンクの3通りを、実行とdry-runの両方で試験し、外に何もできないことを確かめた。**他の箇所の確認**: `organize`の`_excluded/no_gps`は、1階層ずつ`symlink_metadata`で見てから作る（元から正しい順。dry-runでは作らない検査だけの形も足した）。`derivatives/`・`tmp/`は`DATA_DIR`直下の固定名で、`create_dir_all`が外へ作ることはない（リンクが外の実在ディレクトリを指す場合は作らず、作った後の`canonicalize`の照合で断る。宙に浮いたリンクは`create_dir_all`が失敗する）。`ingest::prepare_tmp_dir`は`tmp/`が外へのリンクだと、作りはしないが`.dl_*`の掃除がリンク先で走った（次の再レビューで`derivatives/`も同じと分かった）。この回は直していなかったが、次の再レビューで実害（リンク先の`.dl_*`を消す）が見つかったので**対応済み**（[§13.14](#1314-再々レビュー2026-10-24の指摘への対応)） |
| 4. `organize`の途中失敗でリンクが増え続ける | 元の削除に失敗したらリンクを取り消し、取り消せなかったものは次の実行が同じ`inode`で再利用する（§13.4）。試験は元のディレクトリを読み取り専用にして3回失敗させ（退避先が空のまま）、権限を戻すと`(2)`の無い名前で移ること、残ったリンクの再利用、別のファイルが同名のときは別名の側で再利用すること。rootなどパーミッションが効かない環境では最初のものを飛ばす |
| 5. 後続段のdry-runの件数 | メモリ上の写しに先の段の計画を適用する形にした（§11.3、§13.6）。`local_derivatives`のdry-runはパス検査も実行と共通。`organize --dry-run`も同じ。件数一致は単体テスト`dry_runの後続段は索引後の状態で数え_実行と同じ件数になる`と、検証環境（§13.13） |
| 6. 大量の変更のトランザクション | 計測して分割しないと判断した（§13.12） |

### 13.12 大量の変更の計測

合成の`Entry`を、writerの1回の呼び出し（1トランザクション）に渡して測った（`local/tests.rs`の`#[ignore]`のテスト。`cargo test --release -p geoframe --lib -- --ignored --nocapture 大量`）。本番機（N100）で、リリースビルド、SQLiteはWAL。

| 操作（1トランザクション） | tmpfs | NVMeのディスク（4回） |
| --- | --- | --- |
| 10,000件の追加 | 約10 ms | 約13 ms（初回の1回だけ約620 ms: 空のDBの作成を含む） |
| 10,000件の更新 | 約11 ms | 13〜15 ms |
| 10,000件の削除 | 約7 ms | 12〜14 ms |
| 更新の途中で停止の合図 → `ROLLBACK`して戻るまで | 約0.5 ms | 0.4〜0.5 ms |

- **writerの占有**は、最悪でも1万件で約15 ms（初回の空のDBでも1秒未満）。writerを使う他の呼び出し（P3の管理画面の更新）はこの間だけ待つ。読み取りはWALなので待たない。
- **停止への応答**は、256件ごとの確認で1 ms未満。確認の間隔はトランザクションを分割する代わりになる。
- **判断: 分割しない**。分けると、削除の判定（走査で見つからなかった行）と、追加・更新・削除の原子性が崩れる（途中で止まると、一部だけ反映された索引が残る）。1万件を桁で超える変更（数十万枚の初回の取り込みなど）はこの機体では想定しない。超える場合は、占有時間が件数に比例するので、その時点で分割を設計する（削除の判定は走査の完了後に1回だけ行い、追加・更新だけを分ける、など）。
- 計測していないもの: 計画の側（走査とEXIFの読み取り）の所要時間。これはwriterの外で行い、停止の確認は写真1枚ごとなので、応答は「1枚のEXIFを読む時間」で決まる。

### 13.13 検証環境での確認（2回目。2026-10-24）

再レビューの修正を入れたmusl静的バイナリを`deploy/rust-deploy.sh`で配布して確かめた。§13.9と同じ手順: 書き換える前に検証環境の中で`sqlite3 .backup`のコピーを取り（`.dump`と、`source='drive'`の全行の`SELECT *`のSHA-256を記録）、合成の写真（`tests/fixtures`のJPEG・HEICの複製。実写真は使わず、検証環境の外へも出していない）を置いて、`sync --dry-run`と`GEOFRAME_INGEST=1`の`sync`を同じ状態に対して続けて実行し、`index_local`・`local_derivatives`の段と`写真:`の行を比べた。

| 状態 | dry-run | 実行 |
| --- | --- | --- |
| 5枚を新しく置く（位置つきJPEG3・位置なしJPEG1・位置なしHEIC1） | `added=5`、`original=3`、`写真: … 全6540` | 同じ（出力が一致）。以前のdry-runは`original=0`になった |
| `organize --dry-run`（上の状態で、索引前） | `index_local added=5`、`targets=2 moved=2` | 実行は`targets=2 moved=2`（2枚が`_excluded/no_gps/`へ） |
| HEICと位置つきJPEGを足し、1枚を更新・1枚を削除 | `added=2`、次に`updated=1 removed=1`、`original=1` | 同じ |
| HEICに位置を与える（検証環境の`sqlite3`で更新） | `made=1` | 同じ。`derivatives/local_<md5>.jpg`ができる。2回目は変更0 |
| HEICの原本を消す | `removed=1 derivative_files_removed=1` | 同じ。derivativeも消える |
| 位置なしJPEGを足す → `organize --dry-run` / `organize` | `index_local added=1`、`organize targets=1 moved=1` | 出力が一致 |
| `photos/`ごと退避し、偽のderivativeを1つ`display`にする | `removed=3 derivative_files_removed=1`。`photos/`は作られない | 同じ件数。`photos/`が作られ、行3つとderivativeが消える |

- 2回目の変更は0（`added=0 updated=0 removed=0`、`made=0`）。Driveの行（6,535行）の`SELECT *`のSHA-256は、配布前のコピーと**一致**した。
- 後片付け: 合成の写真・`_excluded`・`derivatives/local_*.jpg`・`source='local'`の行を消し、`photos/`の権限を元（750）に戻した。**`.dump`のSHA-256は配布前のコピーと一致**。`GEOFRAME_INGEST`は設定されておらず（`取り込みは無効です`）、`/healthz`は200。検証環境の中に作ったバックアップは確認後に削除した。

### 13.14 再々レビュー（2026-10-24）の指摘への対応

| 指摘 | 対応 |
| --- | --- |
| 1. 起動時の`.dl_*`の掃除が、`tmp/`・`derivatives/`が外へのリンクだとリンク先のファイルを消す | 列挙の前に、`tmp/`・`derivatives/`を`symlink_metadata`（リンクを辿らない）で検査する（`ingest::sweepable_dirs`）。通常のディレクトリだけを掃除し、リンク・ファイルなど通常でないものは掃除を飛ばして、数だけを警告する（`[fetch] tmp/・derivatives/ のうち N つがリンクなどで…`。パスは出さない）。無い`derivatives/`は黙って外す。`.part`の掃除（`part_leftovers`）の`derivatives/`の検査も同じ関数にそろえた。`--dry-run`の件数（`count_leftovers`）も同じ検査を通り、飛ばす数を`掃除を飛ばす tmp/・derivatives/（…）: N つ`の行で出す（0のときは出さない）。単体テスト`linked_tmp_and_derivatives_are_not_swept`は、`tmp/`・`derivatives/`のそれぞれが外へのリンクのとき、リンク先の`.dl_*`・`.part`が残ること、もう一方の通常のディレクトリはふつうに掃除されること、実行とdry-runの件数を確かめる。`start`は`tmp/`の`create_dir_all`のあとに検査するので、宙に浮いたリンクは従来どおり作成の失敗（`IngestError::Io`）になる。**`DATA_DIR`自体**がリンクなのは正当な構成なので検査しない |
| 2. dry-runのDBの写しの再試行に終わりが無い | `sync::simulation_copy`が、①写しの間だけ元の接続で読み取りトランザクションを持ってスナップショットを固定し（バックアップがやり直しにならない）、②**30秒の期限**（`COPY_DEADLINE`）の`step`ループにした。期限を超えたらエラー（`SQLITE_BUSY`・`DB が使用中で、N秒以内にメモリ上へ写せませんでした`）で、`sync --dry-run`・`organize --dry-run`は終了コード1。**期限の判断**: 本番相当で0.3秒の約100倍。固定したスナップショットなら期限に当たるのは元が長く排他ロックされている異常なときだけなので、短すぎて正常な写しを落とすことも、長すぎて何も言わずに待ち続けることも避けられる値とした（`busy_timeout`の5秒の分までは超えうる）。以前の「5 msの間隔」は、ロックで進めなかったときだけの待ち（20 ms）に変えた（通常の`step`の合間には待たない）。単体テスト: ①WALのDB（約12 MiB）を別の接続が更新し続けても期限内に完了し、写しが整合していて、元の接続がトランザクションを持ち越さないこと（スナップショットを固定しないと同じ試験が期限で失敗することを確かめた）、②元が`BEGIN EXCLUSIVE`のままなら期限でエラーになり、ロックが解ければ写せること |

検証環境での確認（再々レビューの修正を入れたmusl静的バイナリを`deploy/rust-deploy.sh`で配布。DBは書き換えていない）:

- 本番コピーに対する`sync --dry-run`は3回とも終了コード0で、約0.14秒で完了した。出力は以前と同じ（`掃除: 消す .dl_* 0 件 / 消す書きかけの .part 0 件 / 捨てる .cache.json の旧キー 0 件`、`index_local`・`local_derivatives`は件数0、`写真: 表示可能 5288 / 取得待ち 0 / 全6535`）。`photos`の全行のSHA-256は実行の前後で同じ（6,535行）。
- `tmp/`・`derivatives/`が外のディレクトリへのリンクで、外に`.dl_*`と`.part`を置いた本番コピーのDBの置き場（検証環境の`/tmp`の中の使い捨て）で、`sync --dry-run`は`掃除を飛ばす tmp/・derivatives/（…）: 2 つ`を出し、`GEOFRAME_INGEST=1`の`sync`は`[fetch] tmp/・derivatives/ のうち2つがリンクなどで通常のディレクトリではないので、掃除を飛ばしました`を出して、外の`.dl_*`・`.part`は両方残った。使い捨てた置き場は削除した。
- `/healthz`は200。`/etc/default/geoframe`・unitに`GEOFRAME_INGEST=1`は無く、取り込みは無効のまま。


## 14. Driveの索引と取得（P2 / T2.4）

Google Driveのフォルダ（`GEOFRAME_DRIVE_FOLDER`）の画像を`photos`の行にし（`index_drive`）、原本をダウンロードして表示用のderivativeを作る（`fetch_derivatives`）。現行の`drive.py`の`drive_service`・`drive_resolve_folder`・`index_drive`・`fetch_derivatives`に対応する。棚卸しS2（[inventory.md §5](inventory.md)）。実装は`crates/geoframe/src/drive.rs`（認証・通信）、`drive/index.rs`・`drive/fetch.rs`（2つの段）、`backoff.rs`（失敗の記録）。画像の読み取りと変換は§12の部品を使う。

### 14.1 段と設定

| 項目 | 内容 |
| --- | --- |
| 段 | `index_drive`（`index_local`のあと）・`fetch_derivatives`（`local_derivatives`のあと）。順序は現行の`cycle()`のまま。どちらも失敗・パニックは隔離される（§11.3）。2つの段は`Arc<DriveShared>`でトークン・解決したフォルダID・「このサイクルで認証に失敗した」印を共有する（現行は1サイクルで`svc`を作って両方に渡す） |
| `GEOFRAME_DRIVE_KEY`（既定`drive_key.json`） | サービスアカウントの鍵JSONのパス。相対はカレントディレクトリ基準（§0）。**`deploy/geoframe-rs.service`が`/var/lib/geoframe/drive_key.json`を与える**（Python版のunitと同じ。`GEOFRAME_INGEST`が0の間は段が動かないので、鍵があってもDriveには接続しない）。**設定が持つのはパスだけ**で、中身は段の中で読む。起動ログにも`Debug`にも載せない（パス・フォルダ名ともテストで確かめている） |
| `GEOFRAME_DRIVE_FOLDER`（既定`GeoFrame`） | 取り込むフォルダ。**名前**、または20字以上の英数字・`_`・`-`なら**ID**（Pythonの`re.fullmatch(r"[A-Za-z0-9_-]{20,}")`と同じ）。IDなら名前の検索をしない。名前なら`mimeType='application/vnd.google-apps.folder' and trashed=false`で検索し、最初の1件を使う（解決したIDはプロセスの間覚える）。見つからなければ段の失敗（フォルダ名は出さない） |
| `GEOFRAME_FETCH_PER_CYCLE`（既定`25`） | `fetch_derivatives`が1サイクルで**試す**枚数（`local_derivatives`と同じ値）。失敗した写真も1枚と数える。飛ばした写真（§14.6）は数えない |
| 鍵が無いとき | `index_drive`・`fetch_derivatives`とも`Skipped`（`Drive の鍵が無いので飛ばします`）。**通信しない**。ローカルの段はそのまま動く（Pythonと同じ） |

### 14.2 認証

`Session::token`（`drive.rs`）。サービスアカウントの鍵JSONから`client_email`・`private_key`だけを読み、JWT（RS256。[`jwt.rs`](../../crates/geoframe/src/jwt.rs)。T2.1）の assertion（`iss`・`scope=https://www.googleapis.com/auth/drive.readonly`・`aud`・`iat`・`exp=iat+3600`）を作って、`https://oauth2.googleapis.com/token`へ`grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer`で送り、アクセストークンを得る。

- **交換先は定数**。鍵JSONの`token_uri`は読まない。通信先（トークンの交換先とAPIの基底URL）を持つ`Endpoints`の本番用は定数だけで、差し替えは`#[cfg(test)]`のコンストラクタしか無い（環境変数・設定・起動引数からは与えられない。配布するリリースビルドにコードが無い）。
- トークンは`expires_in`（上限1時間）から期限を計算し、**期限の60秒前に取り直す**。要求のたびに確かめるので、長い取得の最中に切れても取り直す。API が401を返したら、取り直して**1回だけ**やり直す。
- 鍵ファイル: 開く前に種類を見る（ディレクトリ・FIFOなど通常のファイルでないものは読まない。FIFOを開くと止まるため）。リンクは辿る（`DRIVE_KEY.exists()`と同じ）。64 KiBまで。JSONで、`client_email`・`private_key`が空でない文字列であること。権限が`0o077`のどれかを含む（所有者以外が読める）と、**プロセスで1回だけ**警告する（権限の数字だけを出す。`0600`にする）。
- **鍵・トークン・`Authorization`・URL・`ureq`のエラー文字列を、ログにもエラーにも出さない。** `DriveError`は固定の語・状態コード・`ErrorKind`・通信の失敗の種類（`dns`・`connect`・`tls`・`timeout`・`other`）だけを持つ（テスト`errors_never_carry_secrets_or_urls`・`log_lines_never_contain_...`）。`CachedToken`の`Debug`は中身を出さない。
- 結果の扱い（`index_drive`の入口）: 鍵が無い → `Skipped`。鍵が壊れている・署名が断られた（4xx）→ `Skipped`（`Drive の認証に失敗したので飛ばします`）で、**同じサイクルの`fetch_derivatives`も飛ばす**（Pythonと同じ）。Google側の不調（429・5xx）や通信の失敗 → **段の失敗**（認証の失敗ではない）。

### 14.3 通信

- クライアントは`ureq`（同期。T2.1）。一覧・トークンの応答は16 MiBまで（1ページ1000件で数百KB）。要求の時間は`net::agent()`の30秒、**原本のダウンロードは1枚15分まで**（要求ごとに延ばす）。
- **再試行**: 429・5xx・Driveが返す流量制限の403（本文に`rateLimitExceeded`・`userRateLimitExceeded`。本文は16 KiBまで読んで捨てる）は、`Retry-After`（秒。あれば）か指数バックオフ（1・2・4・8秒、1回の待ちは30秒まで）で、最大4回再試行する。待ちの最中も停止の合図を見る（合図が来たらすぐ中断）。権限の403・404などは再試行しない。再試行を使い切ったら`DriveError::Status`。**流量制限の403だけは、使い切ったあとも理由を保って`DriveError::RateLimited`にする**（`Status(403)`にすると権限の不足と見分けがつかず、写真の失敗に数えられる）。
- 一覧のページ送りは最大2000ページ。**同じ`nextPageToken`が続く**・`files`が配列でない・`nextPageToken`が文字列でない・`id`の無い画像は、一覧の失敗（途中まで取れた分は捨てる）。
- ダウンロードは`Content-Length`が上限（512 MiB）を超えれば読む前に断り、無くても読みながら数えて超えたら止める（書き込んだ分は呼び出し側が消す）。本文を読む途中の切断は`DriveError::Net(disconnected)`（通信の障害。段を中断）、**原本の本文の受信の時間切れは`DriveError::BodyTimeout`**（その写真が遅いだけ。要求を送って応答の頭を受けるまでの時間切れ`Net(timeout)`とは別。後者は障害）にする。
- **通信の失敗の分類**（`ureq` 3.5 のエラー。実装は`net.rs`の`Failure`の表と`drive.rs`の`read_error`）。**応答のヘッダーを受け取る前**に起きた`Io`の失敗は、種類によらずすべて通信の障害にする（相手が応答の前に切ると`ureq`は`Io(UnexpectedEof)`を返す。`ConnectionReset`・`ConnectionAborted`・`BrokenPipe`・`NotConnected`なども同じ）。

  | 起きる場所 | `ureq`のエラー | 分類 | 扱い |
  | --- | --- | --- | --- |
  | ヘッダーの前 | `HostNotFound` | `dns` | 障害（段を中断） |
  | 同上 | `ConnectionFailed`・`ConnectProxyFailed`・`Io`（`ConnectionRefused`・`NetworkUnreachable`・`HostUnreachable`・`NetworkDown`） | `connect` | 障害 |
  | 同上 | `Timeout(_)`・`Io`（`TimedOut`・`WouldBlock`） | `timeout` | 障害 |
  | 同上 | `Tls`・`Rustls`・`Pem`・`TlsRequired`・`Io`（`InvalidData`。rustlsの証明書エラー） | `tls` | 障害 |
  | 同上 | 上記以外の`Io`すべて（`UnexpectedEof`・`ConnectionReset`・`ConnectionAborted`・`BrokenPipe`・`NotConnected`など） | `disconnected` | 障害 |
  | 同上 | `Protocol`・`LargeResponseHeader`・`Http`・`BadUri`・`TooManyRedirects`・`BodyExceedsLimit`など | `other` | **写真の失敗**（`fail_download`）。応答は届いていて、要求ごとに決まった結果になりうる（特定の1枚だけ毎回壊れた応答になる）ので、障害にして段を止めると、その1枚が残りを永久に塞ぐ |
  | 原本の本文（ヘッダーのあと） | 切断（`UnexpectedEof`・`ConnectionReset`・`ConnectionAborted`・`BrokenPipe`・`NotConnected`など） | `disconnected` | 障害 |
  | 同上 | 時間切れ（`Timeout(_)`を包んだもの・`TimedOut`・`WouldBlock`） | `BodyTimeout` | **写真の失敗**（`fail_slow_download`） |
  | 同上 | 上記以外の読み出しの`io::Error` | `Io(kind)` | 写真の失敗（`fail_io`） |
  | 403の理由の本文（16 KiBまで読んで捨てる） | 切断・時間切れ・TLSなど（受け取れなかった） | `Net(…)`（`disconnected`・`timeout`・`tls`。時間切れも`BodyTimeout`にしない） | 障害。**権限の不足（`Status(403)`）と決めつけない**。数百バイトの理由の本文が届かないのは回線の不調 |
  | 同上 | 上記以外の読み出しの失敗すべて（`Protocol`＝不正なチャンク長、`Decompress`＝途中で終わるgzipなど。ヘッダーでは`other`になる種類） | `Net(reason_unreadable)`（`Failure::ReasonUnreadable`） | 障害（段を中断）。**ヘッダーの`Protocol`（写真の失敗）とは区別する**: 理由が読めないこと自体が、権限の不足とも流量制限とも決められない不調で、写真の失敗にすると待ちが付き、直ってからも取得が遅れる |
  | 同上 | 本文が16 KiBを超えた | （`false`） | 流量制限ではない403として扱う（権限の不足。写真の失敗） |
  | 一覧・トークンの本文 | 上のヘッダーの前と同じ分類 | `Net(…)` | 一覧・トークンの失敗（段の失敗） |

### 14.4 `index_drive`

一覧（`'<folder>' in parents and trashed=false`、`fields`=`nextPageToken,files(id,name,md5Checksum,mimeType,imageMediaMetadata(time,location))`、`pageSize=1000`。**サブフォルダは辿らない**。Pythonと同じ文字列で、テスト`index_asks_drive_exactly_what_python_asks`と`drive-index-python.json`が確かめる）のうち、MIMEが`image/jpeg`・`image/png`・`image/heic`・`image/heif`・`image/tiff`のものを対象にする。

- **一覧の順を保つ**: Pythonは`remote`（dict）の挿入順に`INSERT`するので、新しい行の`id`は一覧の順に振られる。同じIDが2回来たら、位置は最初、値は最後（dictの代入と同じ）。
- **追加**: `drive_id`の行が無いとき。`source='drive'`・`name`・`md5`（`md5Checksum`。無ければNULL）・`taken_at`（`imageMediaMetadata.time`を[`norm_time`](#123-exif撮影時刻gps向き)に通す。無い・合わなければ**空文字**）・`lat`/`lon`（`location.latitude`/`longitude`。**独立に取る**ので、緯度だけあれば`lat`あり・`lon`NULL）・`loc_source`（`lat`があれば`drive_meta`、無ければNULL）・`indexed_at`（`YYYY-MM-DDTHH:MM:SS`。計画の後に1回作って全行に使う）。
- **更新**: `drive_id`の行があって`md5`が違うとき（`NULL`同士は同じ）。`name md5 taken_at lat lon loc_source`を上書きし、`place elev display fetched_at`をNULL、`rotation=0`、`indexed_at`を更新する。**手動の位置も上書きされる**（Pythonと同じ。§0。直すかは**P3で決める**）。`md5`が同じなら何もしない（`name`が変わっていても）。`favorite hidden title inferred_from infer_dt_s muni_cd pref city`は触らない。古いderivativeのファイルは消さない（次の取得が上書きする。回転済みの別名は残る。Pythonと同じ）。
- **削除**: `source='drive'`の行で、一覧に無いもの（`drive_id`がNULLの行も）。確定したあとに`display`のderivativeも消す（`derivatives/<安全な名前>`だけ。§13.2の5と同じ関数`remove_derivatives`）。**一覧が0件でDriveの行が残っているときは削除しない**（`deletes_withheld`に数えて警告する。§0）。**0件でなくても、消す件数が50件（`MASS_DELETE_MIN`）を超え、かつ計画を作った時点のDriveの行（`source='drive'`）の20%（`MASS_DELETE_PERCENT`）を超えるときも、削除だけを保留する**（`mass_delete_withheld`に数えて、件数だけの警告を出す。名前は出さない。追加・更新は通常どおり行い、保留の判定は`plan`の中なので`--dry-run`にも同じ件数が出る）。例: 6,535行から1件だけ見える一覧は、6,534件の削除を保留する。値の根拠は、50件以下の入れ替えは普通の運用（Driveで数枚消した）で、手作業で50件以上を一度に消し、かつ2割を超えることは普通ない、という判断。**保留を外すのは`geoframe sync --allow-mass-delete`の1回だけ**（`--dry-run`と併せると、外した場合の件数を見られる）。環境変数・設定・unitで常時外せる形にはせず、`serve`の定期実行は常に保留する（毎サイクル警告が出る）。このフラグは0件の一覧の保留も外す（意図した全削除）。
- **途中で失敗したら何も変えない**: 全ページが取れてから計画を作る。Pythonも例外で`index_drive`全体が落ちるので同じ。停止の合図も、書く前にだけ効く（§13.2の7と同じ確認点: 一覧の各ページ・書き込みの始め・256件ごと・`COMMIT`の直前。確定前なら`ROLLBACK`して`Skipped`）。
- 計画（`plan`）と書き込み（`write_plan`）は、実行と`--dry-run`が同じ関数を通る。`--dry-run`は一覧を**読み**（通信する）、計画を§11.3の写しに適用して、後続の段（`fetch_derivatives`）と最後の`写真:`の行に索引後の状態を見せる。
- 件数: `remote pages added updated removed unchanged malformed deletes_withheld mass_delete_withheld derivative_files_removed derivative_files_unsafe derivative_files_failed`（`malformed`は`name`が無くて索引に入れなかったファイル）。

### 14.5 `fetch_derivatives`

`source='drive' AND display IS NULL`の行を`ORDER BY taken_at, id`で、`GEOFRAME_FETCH_PER_CYCLE`枚まで試す。**位置の有無では絞らない**（[inventory.md §6.4](inventory.md)）。1枚ごとに:

1. `imaging::derivative_name(drive_id, rotation)`（IDが`[A-Za-z0-9_-]`でなければここで断る。`rotation`がNULLなら0）と、`DataDir::download_tmp`（`tmp/.dl_<drive_id>`。IDの検査つき）。
2. **`create_new`＋`O_NOFOLLOW`＋権限0600で作る**（受入条件③）。既にある名前（シンボリックリンク・切れたリンクを含む）は辿らず・消さず、その写真の失敗（`fail_io`）にする。起動時の掃除（§11.6）が消す。**作った直後に、原本を消すガード（`Original`）を置く**。ガードは自分が新規作成に成功したものだけを持つ。
3. ダウンロード（§14.3。512 MiBまで）。
4. `imaging::store_derivative`で`derivatives/<drive_id>[.r<回転>].jpg`を作る（§12。ICCはsRGBへ変換して付けない）。
5. `UPDATE photos SET display=?, fetched_at=? WHERE id=?`（`fetched_at`は`YYYY-MM-DDTHH:MM:SS`）。1枚ずつ書くので、途中で止まっても進んだ分は残る。
6. **原本を明示的に消し（`Original::remove`。成功・写真の失敗のどちらでも）、結果を確かめる**。消せなかったら`original_remove_failed`に数え、固定の文言と写真の`id`だけの警告を出す（EXIF・GPS付きのファイルが`tmp/`に残っている。次の起動時の掃除で消える）。ガードの`Drop`は、早期`return`・パニック・停止の合図で抜けたときの補助で、まだ消えていなければ消す（結果は捨てる）。

失敗は**写真の失敗**と**段の中断**に分ける。写真の失敗は件数に数え、記録して次へ進む。段の中断（通信の不調）は、残りを試しても同じなので、止めて段の失敗（`sync`の終了コード1）にする。ここまでの件数は失敗の文面に付く。

| 失敗 | 扱い | 件数の名前 |
| --- | --- | --- |
| ダウンロードの404・403（権限）・その他の4xx | 写真の失敗 | `fail_download` |
| 本文の受信が1枚15分を超えた（`BodyTimeout`） | 写真の失敗 | `fail_slow_download` |
| ダウンロードが上限超え、`ImageError::FileTooLarge` | 写真の失敗 | `fail_too_large` |
| 一時ファイルの作成・書き込みの失敗（`AlreadyExists`を含む） | 写真の失敗 | `fail_io` |
| 画像として読めない／BigTIFF／画素数が上限超え | 写真の失敗 | `fail_not_image`／`fail_bigtiff`／`fail_too_many_pixels` |
| デコードの失敗 | 写真の失敗 | `fail_decode_jpeg`・`fail_decode_png`・`fail_decode_tiff`・`fail_decode_heic`・`fail_decode_other` |
| JPEGの書き出しの失敗、IDやrotationが不正、`drive_id`がNULL | 写真の失敗 | `fail_encode`・`fail_other` |
| 接続できない（DNS・接続・TLS）、**応答のヘッダーを受け取る前の切断などのI/Oの失敗**、**原本の本文の途中の切断**、**接続・応答の待ちの時間切れ**、**流量制限の403が続く**、**403の理由の本文の受信の失敗**（切断・時間切れ）、429・5xxが続く、認証の失敗、**取り直したトークンでも401**（サービスアカウント側の問題） | **段の中断**（写真の失敗に数えない・記録しない） | （失敗の文面） |
| 停止の合図 | 中断（失敗ではない） | — |

成功は形式別に`ok_jpeg`・`ok_png`・`ok_tiff`・`ok_heic`、ほかに`pending`（取得待ちの総数。位置の有無で絞らない）・`skipped_backoff`・`attempted`・`fetched`・`icc_dropped`・`original_remove_failed`（原本の削除に失敗した数。0でなければ`tmp/`の権限を確かめる）。**件数が0の分類は出さない**（`ok_*`・`fail_*`）。失敗した写真は`warn`で写真の`id`と`ImageError`・`DriveError`の文面（パス・名前・座標を含まない固定の語。HEICは構造の分類つき。§12.4）を出す。

`--dry-run`: ダウンロードも変換もしない。取得するはずの写真（`planned`）と、使えない写真（`unusable`。IDが不正など）を数え、`pending`・`skipped_backoff`も出す。成功したものとして写しの`display`を埋める（後続の段と`写真:`の行に見せる）。**鍵が無ければ実行と同じく`Skipped`**。

### 14.6 失敗が続く写真が枠を塞がない（受入条件②）

**問題**: 取得待ちを`LIMIT n`つきのSQLで選ぶと（Pythonの`fetch_derivatives`・`local_derivatives`）、恒久的に失敗する写真（壊れた原本、読めない形式、Driveで消えたファイル）が毎サイクル先頭の枠を埋め、後続の写真がいつまでも取得されない。フィクスチャ`drive-index-python.json`に、Pythonが`GEOFRAME_FETCH_PER_CYCLE`=1のとき、画像でない原本1枚のために**何サイクルでも同じ1枚だけを試し続ける**様子を記録している（`cycle2_limit1`・`cycle3_limit1`の`attempts`が両方`["idF"]`）。

**対応**（スキーマは変えない。Pythonと`catalog.db`を共有するため。列を足すとPythonが落とす）: **プロセスの中だけの失敗の記録**（`backoff.rs`の`FailureMemo`）。

- 失敗した写真は、次の再試行の時刻まで飛ばして、後続を枠に入れる。待ちは**30分（1回目）→1時間→2時間…と倍々で、上限12時間**。同期サイクルの間隔（既定10分）より長くして、次のサイクルでは必ず飛ばされるようにした。
- 成功すると記録を消す。記録は**4096件まで**（Driveで消えた行の記録が残り続けないように、いっぱいになったら期限の過ぎたものから、無ければいちばん早く過ぎるものを落とす）。
- キーは`fetch_derivatives`が`(drive_id, md5)`、`local_derivatives`が`(id, mtime)`。**Driveで原本が差し替わる・ローカルのファイルが更新される**（`md5`・`mtime`が変わる）と別のキーになり、**すぐ再び試す**。
- プロセスが再起動すれば忘れる（もう一度1回ずつ試す。枠の一部を使うだけ）。`--dry-run`は覚えない（実行はしていないので、次の実行に影響させない）。
- 選び方: `LIMIT`を付けず、`ORDER BY`の順に読み、記録にあるものを飛ばして、`GEOFRAME_FETCH_PER_CYCLE`枚そろったら止める。飛ばした数は`skipped_backoff`。
- **通信の不調・認証の失敗は写真の失敗に数えない**（記録しない）。ネットワークが落ちている間に全部の写真に待ちが付くと、直ってからも30分以上取得されないため。数えない障害は、接続・名前解決・TLS・**応答のヘッダーを受け取る前の切断などのI/Oの失敗**・**原本の本文の途中の切断**・**応答の待ちの時間切れ**・**流量制限の403（再試行を使い切ったあと）**・**403の理由の本文の受信の失敗（切断・時間切れ・TLSに加え、`Protocol`・`Decompress`など読み出しのどんな失敗も。上限超過だけは除く）**・429・5xx・認証の失敗（分類の表は§14.3）。数える（その写真の失敗）のは、原本の本文の受信が1枚15分を超えたときと、権限の403・404など。テスト: `a_rate_limit_that_outlasts_the_retries_stops_the_stage_without_blaming_the_photos`・`a_timeout_waiting_for_the_response_stops_the_stage_without_blaming_the_photos`（どちらも、障害中に試した写真が、復旧した次のサイクルで待たずに取得される）、`a_disconnect_before_the_response_headers_stops_the_stage_without_blaming_the_photos`・`a_disconnect_in_the_middle_of_an_original_stops_the_stage_without_blaming_the_photos`・`a_403_whose_reason_body_is_cut_off_is_an_outage_not_a_permission_error`・`a_403_whose_reason_body_times_out_is_an_outage_and_not_a_slow_download`・`a_403_whose_reason_body_is_malformed_chunked_is_an_outage`・`a_403_whose_reason_body_is_a_truncated_gzip_is_an_outage`（いずれも復旧後に待たずに取得される）・`a_disconnect_while_exchanging_the_token_is_an_outage_not_an_auth_error`、`a_body_that_takes_too_long_is_that_photos_failure_and_the_rest_continue`。分類の表は`head_phase_errors_follow_the_table`（`net.rs`）・`body_read_errors_follow_the_table`・`reason_body_failures_follow_the_table`が確かめる。

テスト: `photos_that_keep_failing_do_not_block_the_ones_behind_them`（枠2枚で、先頭3枚が恒久的に失敗する。1回目は先頭2枚を試して失敗、2回目はその2枚を飛ばして3枚目と良い写真1枚、3回目は失敗中の3枚を飛ばして残り、4回目は何も試さない。「待ち時間0」の記録＝毎回すべてを試す方針に差し替えると、良い写真が3サイクル続けて0枚のまま＝Pythonの挙動を再現する）、`a_failed_photo_is_tried_again_when_its_original_changes_or_the_wait_is_over`、`失敗が続く写真は枠を塞がず_後続の写真が作られる`（`local_derivatives`。`mtime`が変わればすぐ試す）、`backoff.rs`の単体テスト（倍々・上限・成功で消える・記録数の上限・溢れない）。

### 14.7 ログ・`--dry-run`

- ログ・件数に出すのは件数・状態名・写真の`id`・HTTPステータス・固定の語だけ。Driveのファイル名・フォルダ名・フォルダID・座標・撮影日時・鍵・トークンは出さない（テスト`log_lines_never_contain_names_keys_tokens_or_urls`が段の結果の文面を、`the_actual_log_output_never_contains_names_coordinates_times_keys_or_tokens`が**実際の`tracing`の出力**（全レベル。索引・取得・再試行・保留の警告・鍵の権限の警告・原本の削除の失敗・トークンの取り直しを通す）を捕まえて、スタブの合成の名前・座標・撮影時刻・トークン・鍵・URL・メール・フォルダ名・Drive IDが入らないことを確かめる）。`RemoteFile`・`Listing`・`Entry`・`Op`・`DrivePlan`の`Debug`は手書きで、件数と有無だけを出す（`{:?}`・パニックの表示・試験の失敗の表示からも漏れない。`derived_debug_output_hides_names_times_and_locations`）。
- `sync --dry-run`の`index_drive`は、鍵があれば**Driveの一覧を読みに接続する**（読み取りのスコープ。原本のダウンロードも書き込みもしない）。**ガードは通さない**（§11.1.1）ので、鍵のある機体では一覧を読みに行く（だから本番機では実行しない。検証環境の本番コピーに向ける）。`--help`と`--dry-run`の1行目にも書いてある。**通信を避けたいときは、存在しない鍵ファイルの絶対パスを`GEOFRAME_DRIVE_KEY`に明示する**（例: `GEOFRAME_DRIVE_KEY=/nonexistent/drive_key.json geoframe sync --dry-run`。鍵が無ければDriveの2段は通信せずに`省略`になる）。`--data-dir`を変えても鍵の設定は変わらないので、鍵のある機体では接続する。`GEOFRAME_DRIVE_KEY`を空文字にしても、既定の`drive_key.json`（カレントディレクトリ基準）に戻るだけで、通信を避けられない。

### 14.8 Pythonとの差（まとめ）

§0の3行（認証・`index_drive`・`fetch_derivatives`）と、`local_derivatives`の行（枠の占有）にまとめた。要点:

- **現行のまま残した危ない挙動（P3で決める）**: md5が変わると**手動で直した位置も上書きされる**。
- **安全側に変えたもの**: 一覧が空なら削除しない／鍵JSONの`token_uri`を読まない／原本の一時ファイルは`create_new`＋`O_NOFOLLOW`＋0600／ダウンロードに上限／消すderivativeの名前の検査／単一トランザクション／ログにファイル名を出さない。
- **枠の占有**（受入条件②）: 失敗を覚えて飛ばす。
- 意図して同じにしたもの: 一覧の問い合わせ（`q`・`fields`・`pageSize`）、サブフォルダを辿らない、MIMEの集合、一覧の順（`id`の採番順）、`lat`/`lon`を独立に取る、`taken_at`は空文字、`md5`が同じなら何もしない、`ORDER BY taken_at`、位置の有無で絞らない、`derivative_name`、`indexed_at`・`fetched_at`の形式、鍵が無ければDriveの段を飛ばす。

### 14.9 テスト

| 対象 | テスト |
| --- | --- |
| スタブ（`drive/stub.rs`） | ループバックの平文HTTPサーバー。**assertionのJWTを署名・`iss`・`scope`・`aud`・`exp`まで検証してトークンを発行**し、`files.list`（フォルダ名の検索・ページ送り）と`files.get?alt=media`を返す。故障の注入（状態コード・`Retry-After`・`Content-Length`無し）、トークンの失効、受けた要求の記録。応答はすべて合成 |
| 認証 | 鍵が無い／壊れている（JSONでない・項目が無い・PEMでない）で通信せずに飛ばす、署名の検証、トークンの共有（2つの段で1回）、期限の60秒前に取り直す、失効（401）で1回だけ取り直す、400は飛ばす・500は失敗（再試行つき）、ディレクトリ・FIFO・大きすぎる鍵、権限が緩い鍵でも読む |
| 索引 | 一覧の順で`id`が振られる・全列（`taken_at`の正規化、空文字、緯度だけ、MIMEの絞り込み）、Pythonと同じ問い合わせ、フォルダ名のエスケープ・見つからない、2回目は何も書かない（`indexed_at`も）、md5変更で戻る列・残る列、消える行とderivative（危ない`display`は消さない）、**一覧が途中で失敗・壊れていたら何も変えない**、429・5xx・流量制限の403は再試行／権限の403は再試行しない、**空の一覧は削除しない**、**大量の削除（50件超かつ2割超）は保留する（閾値の前後・1件だけ残る一覧・フラグで外れる・dry-run）**、重複ID、名前の無いファイル、`drive_id`がNULLの行、停止の合図、dry-run（一覧は読むが実DBは変わらず、後続の段に索引後の状態を見せ、実行と同じ件数） |
| 取得 | JPEG・PNG・TIFF・HEICの全形式、**原本が成功・失敗・パニックのどれでも残らない**（ガードの単体テスト）、リンクを辿らない・消さない・その写真だけの失敗、**先頭の枠より多い失敗があっても後続が取得される**、md5が変わると再び試す・待ちが過ぎると再び試す、通信の不調（流量制限の403・応答の待ちの時間切れ・5xx・401）は段を中断して写真の失敗に数えず、復旧した次のサイクルで待たずに取得される、本文の受信の時間切れ・404は写真の失敗、原本の削除の失敗が件数に出る、回転・不正なID（通信もファイル作成もしない）、枠の数と0、サイズ上限（`Content-Length`あり・なし） |
| Pythonとの突き合わせ（`matches_the_python_fixture`） | `tools/gen_drive_index_fixture.py`が、偽のDriveに**Pythonの`index_drive`・`fetch_derivatives`**を流した結果を作る（`svc`と`googleapiclient.http.MediaIoBaseDownload`を差し替え。HEICの変換に`pillow-heif`が要る）。10段階: 初回・変化なし・変更（md5の変更・削除・追加・名前だけの変更・手動の位置など）・変化なし・一覧が途中で失敗・取得4サイクル（枠が3・1・1・25）・空の一覧。Rustが同じ応答をスタブから返し、**`photos`の全列と`id`**、**Driveへの問い合わせ（`q`・`fields`・`pageSize`・`pageToken`）**、追加/削除の件数、derivativeのファイルの有無が一致することを確かめる。取得は、最初のサイクルが**同じ写真を同じ順に試し、同じ行・同じ寸法**になり、最後は同じ写真が取得済みになる。意図した差（空の一覧を削除しない、失敗を覚えて枠に後続を入れる）は、その差が出ることを確かめる。CIの`app`ジョブが`--check`でPythonの結果を検査する |
| バイナリ（`tests/ingest_cli.rs`） | 鍵が無い環境で、`sync --dry-run`・`sync`が`index_drive`・`fetch_derivatives`を`省略（Drive の鍵が無いので飛ばします）`として終了コード0で終えること（通信しない） |

### 14.10 検証環境での確認（鍵が無い状態。2026-10-25）

musl静的バイナリ（`static-pie linked`・`ldd`は静的・`GEOFRAME_TEST_*`の文字列なし）を`deploy/rust-deploy.sh`で配布して確かめた。検証環境の`catalog.db`は本番の2026-10-24 06:17のバックアップのコピー（6,535行、すべて`source='drive'`）。**検証環境にDriveの鍵は無い**（存在だけを`sudo test -f`で確かめた。中身は見ていない）。`sqlite3 -readonly … .dump`のSHA-256は配布前と各確認のあとで同じ。

| 確認 | 結果 |
| --- | --- |
| (a) 鍵が無い状態の`sync --dry-run`（`geoframe`ユーザー） | 終了コード0。`index_drive`・`fetch_derivatives`は`省略（Drive の鍵が無いので飛ばします）`、`index_local`・`local_derivatives`は件数0、`写真: 表示可能 5288 / 取得待ち 0 / 全6535`。**行の変更0** |
| (b) 鍵が無い状態の`GEOFRAME_INGEST=1`の`sync`（実行）と、`serve`の起動時のサイクル（drop-inで`GEOFRAME_INGEST=1`） | どちらも終了コード0（`serve`は起動してサイクルが完了）。Driveの2段は(a)と同じ`省略`。`.dump`のSHA-256は配布前と一致 |
| (c) 合成の鍵（テスト用の使い捨てのRSA鍵。どのサービスにも登録していない）を`GEOFRAME_DRIVE_KEY`で指して`sync --dry-run` | musl静的バイナリ・実際のGoogleへのTLSで assertion の署名とトークンの交換が動き、Googleが**HTTP 400で断った**。`[drive] 認証エラー: トークンの交換が HTTP 400 で断られました`の警告（鍵・URL・本文は出ない）と、`index_drive`・`fetch_derivatives`の`省略（Drive の認証に失敗したので飛ばします）`、終了コード0。権限644にすると`鍵ファイルが所有者以外にも読める権限です（644）`の警告が出た。確認後、合成の鍵は検証環境から削除した |
| 後片付け | drop-inを外して`ingest=0`（`取り込みは無効です`）に戻し、`/healthz`は200、`.dump`のSHA-256は配布前と一致 |

最後に、`geoframe-rs.service`へ`GEOFRAME_DRIVE_KEY=/var/lib/geoframe/drive_key.json`を足した最終のバイナリ・unitを配布し直して、(a)(b)をもう一度確かめた（unitの環境に`GEOFRAME_DRIVE_KEY`が入っていること、鍵が無いので`省略`、`.dump`のSHA-256は変わらず、`ingest=0`に戻して`/healthz`は200）。

(c)は、実際のDriveへの取り込みではない（鍵は合成で、Googleが断った）。**本物の鍵での確認は管理者が行う**（§14.11）。

### 14.11 鍵が置かれたあとの管理者の確認手順

検証環境の`/var/lib/geoframe/drive_key.json`（所有者`geoframe`・権限`0600`。`geoframe-rs`のunitがこのパスを`GEOFRAME_DRIVE_KEY`に与える）に鍵を置き、フォルダ名が`GeoFrame`でなければ`GEOFRAME_DRIVE_FOLDER`（`/etc/default/geoframe`か`geoframe-rs`のdrop-in）を設定してから、次の順に行う。本番機では行わない（本番の鍵・`/var/lib/geoframe`には触れない）。以下の「`$ENV`」は`sudo -u geoframe env TZ=Asia/Tokyo GEOFRAME_DRIVE_KEY=/var/lib/geoframe/drive_key.json GEOFRAME_DRIVE_FOLDER=<フォルダ名かID>`の略。

**(1) 本番コピーに対する冪等性（`--dry-run`）。Driveの一覧を読むだけで、何も書かない。**

```
$ENV GEOFRAME_DATA_DIR=/var/lib/geoframe /usr/local/bin/geoframe sync --dry-run
```

`index_drive: 完了 remote=… added=… updated=… removed=… unchanged=… deletes_withheld=0 mass_delete_withheld=0`と、`fetch_derivatives: 完了 pending=… skipped_backoff=0 planned=…`、最後の`写真:`の行を見る。**件数の意味は別々**で、同じ数にならない:

- `index_drive`の`remote`は、Driveのフォルダの画像の数（本番コピーは6,535行）。`unchanged`がほぼ`remote`。`added`・`updated`・`removed`は索引が**この実行で変える**行数（`--dry-run`は写しに適用するだけで、実DBは変わらない）。
- `fetch_derivatives`の`pending`は、**取得待ちの総数**（`source='drive' AND display IS NULL`。**位置の有無で絞らない**）。索引の`updated`・`added`の行はここに入る（`display`がNULLに戻る・NULLで入る）。`planned`は、そのうち**この1回で取得するはずの数**で、`GEOFRAME_FETCH_PER_CYCLE`（既定25）が上限。`pending`が25を超えれば`planned`は25になる（`skipped_backoff`は失敗の記録があって飛ばす数で、dry-runの新しいプロセスでは0）。`unusable`は取得できない行（IDが不正など）。
- 最後の`写真:`の取得待ちは別の数え方で、**位置のある行（`lat IS NOT NULL`）のうち`display`がNULLのもの**だけ（dry-runでは、`planned`の分を取得した後の写しで数える）。位置の無い取得待ちは含まれず、`pending`・`planned`とは一致しない。

**冪等性の判断**は、次の2つで行う（`写真:`の行では行わない）。

1. 索引: `added`・`updated`・`removed`が、バックアップ（2026-10-24 06:17）のあとにDriveへ足した・差し替えた・消した写真の数だけ（ほぼ0）であること。`updated`が多いと`display`が戻って再取得される（`md5`の違いの原因を調べる）。
2. 取得段: 実行の**前**に`sudo -u geoframe sqlite3 -readonly /var/lib/geoframe/catalog.db "SELECT COUNT(*) FROM photos WHERE source='drive' AND display IS NULL"`で取得待ち`P0`を控える。`pending`が`P0 + added + updated`にほぼ一致し（`updated`の行がもともと取得待ちなら二重には増えない。消えた行が取得待ちだった分は引く）、`planned`が`min(pending, 25)`であること。

そのほか:

- `deletes_withheld`・`mass_delete_withheld`が0でなければ、削除を保留した（0なら一覧が空、0以外なら消す行が多すぎる。フォルダ名・共有・鍵の権限・`GEOFRAME_DRIVE_FOLDER`を確かめる。意図した削除なら`sync --allow-mass-delete`の1回だけで外れる。§14.4）。
- 失敗するときは`段 index_drive: 失敗: Drive: …`（状態コードや通信の種類だけ。URL・ファイル名は出ない）か、`省略（Drive の認証に失敗したので飛ばします）`（鍵・assertionが断られた。`[drive] 認証エラー`の警告に状態コード）。

**(2) 原本の形式別の成功・失敗をPythonと比べる（受入条件①）。** 本番コピーの`derivatives/`からの再生成（T2.2の確認）は原本の互換性を裏付けないので、**実際の原本をDriveから落として**、同じ写真をPythonとRustの両方で変換して比べる。検証環境の本番コピーは書き換えず、**使い捨ての置き場**（`geoframe`所有・`0700`）に`catalog.db`の複製を2つ作ってそれぞれで動かす。

```
S=/var/tmp/gf-t24-compare; sudo install -d -o geoframe -g geoframe -m 0700 $S $S/rust $S/py
sudo -u geoframe sqlite3 /var/lib/geoframe/catalog.db ".backup $S/rust/catalog.db"
sudo -u geoframe sqlite3 /var/lib/geoframe/catalog.db ".backup $S/py/catalog.db"
# 比べる写真を選ぶ（形式ごとに最大20枚。IDS はカンマ区切りの写真の id）
pick() { sudo -u geoframe sqlite3 -readonly $S/rust/catalog.db "SELECT group_concat(id) FROM (SELECT id FROM photos WHERE source='drive' AND ($1) ORDER BY random() LIMIT 20)"; }
IDS=$(printf '%s,%s,%s,%s' "$(pick "lower(name) LIKE '%.heic'")" "$(pick "lower(name) LIKE '%.jpg'")" "$(pick "lower(name) LIKE '%.png'")" "$(pick "lower(name) LIKE '%.tif' OR lower(name) LIKE '%.tiff'")" | sed -E 's/,+/,/g; s/^,//; s/,$//')
for d in rust py; do sudo -u geoframe sqlite3 $S/$d/catalog.db "UPDATE photos SET display=NULL, fetched_at=NULL WHERE id IN ($IDS)"; done
```

（`$ENV`の中身にフォルダ名の空白が入るときは、引用符で囲んで渡す。）Rust側は**使い捨ての置き場に対して**1サイクル（`GEOFRAME_FETCH_PER_CYCLE`を選んだ枚数以上にする）:

```
$ENV GEOFRAME_INGEST=1 GEOFRAME_DATA_DIR=$S/rust GEOFRAME_FETCH_PER_CYCLE=80 /usr/local/bin/geoframe sync 2>&1 | grep -E "段 (index_drive|fetch_derivatives)|取得に失敗"
```

`fetch_derivatives: 完了 pending=… attempted=… fetched=… ok_jpeg=… ok_heic=… fail_…`と、失敗した写真の`id`と固定の語の行が出る（ファイル名は出ない）。`index_drive`が使い捨ての置き場に行を足す・直すことがあるので、(1)で`added`・`updated`が0であることを先に確かめておく。Python側は検証環境の比較用Python（`/opt/geoframe-pyref`の`venv`・`app`）で、同じ写真を`fetch_derivatives`で変換する。Pythonはファイル名を出力するので、**拡張子ごとの件数だけ**に畳んで見る:

```
$ENV GEOFRAME_DATA_DIR=$S/py <pyrefのPython> -c 'import sys; sys.path.insert(0,"/opt/geoframe-pyref/app"); from geoframe import drive; drive.prepare_tmp_dir(); drive.fetch_derivatives(drive.drive_service(), 80)' 2>&1 \
  | sed -nE 's/^  \[fetch\] .*\.([A-Za-z0-9]+) → derivative生成$/ok \L\1/p; s/^  \[fetch\] .*\.([A-Za-z0-9]+) 失敗: .*$/fail \L\1/p' | sort | uniq -c
```

両方の置き場で、**選んだ写真のうち`display`が入ったものを`id`で比べる**:

```
# comm は、両方の入力が同じロケールで整列されている必要がある。整列も comm も LC_ALL=C にそろえる
for d in rust py; do sudo -u geoframe sqlite3 -readonly $S/$d/catalog.db "SELECT id FROM photos WHERE id IN ($IDS) AND display IS NOT NULL" | LC_ALL=C sort > $S/$d.ok; done
LC_ALL=C comm -3 $S/rust.ok $S/py.ok          # 左だけ = Rustだけ成功、右だけ = Pythonだけ成功
sudo -u geoframe sqlite3 -readonly $S/rust/catalog.db "SELECT lower(substr(name, length(rtrim(name, replace(name,'.','')))+1)) ext, count(*), sum(display IS NOT NULL) FROM photos WHERE id IN ($IDS) GROUP BY ext"
```

判定: **Rustだけが失敗する写真が0**であること。あれば`id`で`fail_*`の分類（HEICは構造の分類つき）を見て、原因ごとに判断する。既知の差は、**BigTIFF**（`fail_bigtiff`。Pythonは開ける）と、**EOI（終端のマーカー）が無くゴミやゼロ埋めが続くJPEG**（`fail_decode_jpeg`。§0）だけ。Pythonだけが失敗する写真（Rustは成功）は問題ではない。結果は形式別の成功/失敗の件数と分類だけを記録する（ファイル名・座標は書かない）。**確認後、使い捨ての置き場は消す**（`sudo rm -rf $S`。変換後の画像と写真の位置を含む）。

**(3) 実機iPhoneのHEICを`img-probe`で分類する（グリッド・HDR・10ビットなど。§12.10）。** `img-probe`はDBにも`DATA_DIR`にも触れず、ファイル名を出さない。HEICの原本が必要なので、管理者がDriveのweb画面から数枚（できれば機種・撮影モード（通常・Live Photo・連写・HDR）の違うもの）をダウンロードし、検証環境の**使い捨ての置き場**（`geoframe`所有・`0700`。原本はGPSを含むので、検証環境の外へ出さない）に置く:

```
D=/var/tmp/gf-t24-heic; sudo install -d -o geoframe -g geoframe -m 0700 $D   # ここに HEIC を置く
sudo -u geoframe /usr/local/bin/geoframe img-probe $D/*.heic
sudo rm -rf $D
```

各行は連番と`ok format=Heic in=… out=… exif_orientation=… icc=…`、または`error heicをデコードできません（…）構造: brand=… primary=grid codec=hevc profile=… bit_depth=… chroma=… colr=… aux=gainmap items=…`。最後の`summary files=… ok=… failed=…`で件数を見る。**読めない形式（`error`）が出たら**、構造の行（ファイル名は含まれない）と`ok`のものの寸法を記録して、`zune-heic`が読めない形式かどうかを判断する（読めなければPoCの`libheif-rs`の経路を足す。[build.md §6](build.md)）。

**(4) 実DriveでのS2（合成のJPEGをDriveのフォルダに足す）。** 本番の取り込みを有効にする前に済ませる関門の一つ（[plan.md P5](plan.md#p5-切替pr-6)）。(1)〜(3)と同じ検証環境で、本番コピーの**複製**（使い捨ての置き場）に対して行い、本番コピーそのものには書かない。合成のJPEGは実写真ではない（`tools/gen_acceptance_photos.py`の`a01`。公開の代表点（東京駅）・架空の撮影時刻・ファイル名は連番）。

```
S=/var/tmp/gf-t24-s2; sudo install -d -o geoframe -g geoframe -m 0700 $S $S/photos
sudo -u geoframe sqlite3 /var/lib/geoframe/catalog.db ".backup $S/catalog.db"
G=$(mktemp -d); python3 tools/gen_acceptance_photos.py $G   # a01.jpg が合成のJPEG。他は使わない
```

1. **足す前の確認。** `$ENV GEOFRAME_DATA_DIR=$S /usr/local/bin/geoframe sync --dry-run`で`index_drive`の`added`・`updated`・`removed`がほぼ0であること（(1)が済んでいること）。`SELECT COUNT(*) FROM photos`を控える。
2. **Driveへ足す。** 管理者がDriveのweb画面から、`$G/a01.jpg`をDriveのフォルダ（`GEOFRAME_DRIVE_FOLDER`）へアップロードする（`drive.readonly`の鍵では書けない）。
3. **周期同期を1回。** `$ENV GEOFRAME_INGEST=1 GEOFRAME_DATA_DIR=$S /usr/local/bin/geoframe sync`で、`index_drive`の`added=1`と`fetch_derivatives`の`fetched=1`（`ok_jpeg=1`）になること。確認:
   `sudo -u geoframe sqlite3 -readonly $S/catalog.db "SELECT source, loc_source, display, lat IS NOT NULL FROM photos WHERE id > <控えた最大のid>"`が`drive`・`drive_meta`・`derivatives/<drive_id>.jpg`・`1`（`display`の`<drive_id>`はDriveのファイルID）であること。
4. **配信を確かめる。** `$ENV GEOFRAME_DATA_DIR=$S GEOFRAME_INGEST=0 /usr/local/bin/geoframe serve --listen 127.0.0.1:8099`を前面で起動し（別の端末で）`curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8099/derivatives/<drive_id>.jpg`が200（`Host`検査に通る名前で。`GET /api/v1/photos`の件数が1増える）、終わったら止める。
5. **もう一度の周期で変更0。** もう一度`sync`（または`sync --dry-run`）で、`index_drive`の`added`・`updated`・`removed`と`fetch_derivatives`の`pending`・`fetched`が0であること。
6. **後片付け。** Driveのフォルダから合成のJPEGを削除する（ゴミ箱へ。以後の(1)の件数に影響しない）。`sudo rm -rf $S "$G"`（変換後の画像・複製のDBを含む）。

結果は件数と合否だけを記録する（ファイル名・ファイルID・座標は書かない）。

### 14.12 T2.5・T2.7へ回したもの

- **T2.5（実施済み。§15）**: `infer`段（位置の推定）。`index_drive`が`display`をNULLに戻す（md5の変更）ことと推定は独立で、`infer`は錨の選び方に`display`を使わない（錨になるのは`loc_source`が`exif`・`drive_meta`・`manual`で時刻が読める写真だけ）。`infer`が`display`をNULLにするのは、推定を取り消す行だけ（[inventory.md §2.3](inventory.md)）。
- **T2.7（実施済み。[p2-acceptance.md](p2-acceptance.md)。ただし§14.11の(1)〜(4)の本物のDriveでの確認は鍵待ち。本番で取り込みを有効にする前の関門）**: 本番コピーでの`--dry-run`（§14.11の(1)の鍵なし部分）と実サイクル、同じ写真を比較用PythonとRustへ入れた`photos`の列ごとの比較、棚卸しS2の実機確認、結果の`p2-acceptance.md`への記録。**§14.11の(2)・(3)が受入条件①の実機での確認**。
- **P3**: md5の変更で手動の位置を上書きする挙動（§0）を直すか決める。直すなら`loc_source='manual'`の行は`lat lon loc_source`を更新せず`display`だけをNULLにする、などの選択肢がある（§13.10と同じ）。

### 14.13 レビュー（2026-10-25）の指摘への対応

T2.4のレビューの指摘7件への対応。コミットは`T2.4: …`の4つ（通信の障害の区別・大量削除の保留・原本の削除・`Debug`とログの検査）と、この文書。

| 指摘 | 対応 |
| --- | --- |
| 通信の障害が写真の失敗として記録される | 再試行を使い切った流量制限の403を`DriveError::RateLimited`にして理由を保ち、接続・応答の待ちの時間切れ（`Net(timeout)`）も障害にした。本文の受信の時間切れだけを`BodyTimeout`（`fail_slow_download`、写真の失敗）として区別する。障害中に試した写真が、復旧した次のサイクルで待たずに取得されるテストを足した（§14.6） |
| 0件以外の大量削除に保護が効かない | 消す件数が50件を超え、かつDriveの行の20%を超えるときは、削除だけを保留する（`mass_delete_withheld`）。外すのは`geoframe sync --allow-mass-delete`の1回だけ。判断: 閾値は「50件以下の入れ替えは普通の運用、手作業で50件以上を2割超まとめて消すことは普通ない」。フラグは0件の一覧の保留も外す（意図した全削除ができないままになる行き止まりを避ける）。環境変数・設定・unitでは外せない（§14.4） |
| 原本の削除の失敗を黙って捨てている | 成功・写真の失敗では`Original::remove`で明示的に消して結果を確かめ、失敗を`original_remove_failed`の件数と固定の文言の警告で報告する。`Drop`は早期`return`・パニックの補助（§14.5） |
| `Debug`がファイル名・時刻・座標を出す／ログの試験が実際のログを見ていない | `RemoteFile`・`Listing`・`Entry`・`Op`・`DrivePlan`の`Debug`を手書きにして件数と有無だけにした。`tracing`の出力を捕まえて検査する試験を足した（§14.7） |
| dry-runで通信を避ける方法の説明が誤り | 「存在しない鍵ファイルの絶対パスを`GEOFRAME_DRIVE_KEY`に明示する」に直した（§14.7、`deploy/README.md`） |
| 受入手順の取得件数の期待値が実装と合わない | `pending`・`planned`・`写真:`の取得待ちの意味を分け、冪等性は「索引の変更件数」と「取得段の`pending`・`planned`」で判断する手順にした（§14.11の(1)） |
| 成功IDの比較で`comm`の整列の条件を満たしていない | 両方の出力を`LC_ALL=C sort`で整列し、`LC_ALL=C comm`で比べる（§14.11の(2)） |

### 14.14 レビュー2回目（2026-10-25）の指摘への対応

14.13の「通信の障害が写真の失敗として記録される」に残っていた取りこぼし2件。

| 指摘 | 対応 |
| --- | --- |
| 応答のヘッダーの前に切断されると写真の失敗になる（`Io(UnexpectedEof)`が`other`になっていた） | ヘッダーを受け取る前の`Io`の失敗は、時間切れ・TLS・接続の拒否/到達不能以外のすべてを`Failure::Disconnected`（`disconnected`）にして障害にした。`ureq`の全エラーの分類の表を§14.3と`net.rs`の`Failure`に書いた。応答の頭が壊れている（`Protocol`など）ものは、要求ごとに決まった結果になりうるので写真の失敗のまま（理由は表）。原本の本文の途中の切断は今まで通り障害（`disconnected`）。テスト: ヘッダー前の切断・本文の途中の切断・トークン交換中の切断が、段を中断し、復旧後に待たずに取得される／認証の失敗にならない |
| 403の理由の本文の受信の失敗が握り潰され、権限の不足と同じ`Status(403)`になる | `is_rate_limit_body`を`Result<bool, DriveError>`にして、理由の本文の切断・時間切れ・TLSは`Net(…)`として返し、段を中断する。時間切れは原本の`BodyTimeout`（`fail_slow_download`）にしない。16 KiBを超える本文だけは流量制限ではない403として扱う。テスト: 本文の途中切断（`UnexpectedEof`）と時間切れ（`Timeout(Global)`）の2経路 |

### 14.15 レビュー3回目（2026-10-25）の指摘への対応

14.14で解消した「ヘッダー前の切断」は問題なし。「403の理由の本文の受信の失敗」に取りこぼしが残っていた。ヘッダーの`Protocol`・`LargeResponseHeader`を写真の失敗とする判断は妥当（変更なし）。

| 指摘 | 対応 |
| --- | --- |
| 403の理由の本文の受信の失敗が、エラーの種類によって写真の失敗になる（`Failure::from`が`Protocol`・`Decompress`を`Other`にし、`is_outage()`が偽になって段が続き、写真に失敗の待ちが付く。不正なチャンク長の403と、途中で終わるgzipの403で再現） | 専用の障害の種類`Failure::ReasonUnreadable`（`reason_unreadable`。`is_outage()`は真）を設けた。`is_rate_limit_body`は、上限超過（`BodyExceedsLimit`。流量制限ではない403として扱う、今のまま）以外の読み出しエラーでは必ず`Err`にして段を中断する。切断・時間切れ・TLSは従来の種類（`disconnected`・`timeout`・`tls`）を保ち、それ以外の`Other`になる種類だけが`reason_unreadable`になる（`reason_body_failure`）。ヘッダーの`Protocol`（写真の失敗）は変えていない。分類の表（§14.3）と§14.14の記述を更新。テスト: 不正なチャンク長・途中で終わるgzipの403が、後続の写真を試さず、障害の間は写真の失敗を記録せず、復旧後に待たずに取得される（修正前に落ちることを確認）。スタブの壊れた本文が実際に`ureq`の`Protocol`・`Decompress`を返すことも確かめる |

## 15. 位置の推定と地名（P2 / T2.5）

撮影時刻が近い位置つきの写真（錨）から、位置の無い写真の位置を推定し（`infer`）、位置のある写真に地名・標高・自治体を付ける（`enrich`）。現行の`geo.py`の`infer_locations`・`enrich_rows`・`muni_table`に対応する。棚卸しB1・B2・S3（[inventory.md](inventory.md)）。実装は`crates/geoframe/src/geo.rs`（時刻の解釈）・`geo/infer.rs`・`geo/enrich.rs`。試験用に`geo/stub.rs`（ループバックの地理院のスタブ）と`geo/tests.rs`。

どちらも**計画を作る関数と書く関数が分かれていて**、実行と`--dry-run`が同じ関数を通る（§11.3。`--dry-run`は書き込み接続を持たず、`infer`の計画だけメモリ上の写しに当てる）。

### 15.1 段と設定

| 項目 | 内容 |
| --- | --- |
| 段 | `infer`（`index_drive`の後）→`enrich`（その直後）→`local_derivatives`。順序は現行の`cycle()`のまま。失敗・パニックは隔離される（§11.3） |
| `GEOFRAME_INFER_MIN`（既定`15`） | 1段目の窓（分）。T1.4から読んでいる（管理画面の内訳の区切り）。推定の**最大の窓**は`max(この値, 12時間)`（現行の`INFER_WINDOWS_S[-1]`）。段階窓（15分→2時間→12時間）は「差を含む最小の窓」に振り分けるのと同値なので、1回の最近傍探索で済ませる。窓のラベルは`infer_dt_s`から表示側が計算する |
| `GEOFRAME_INFER_OFFSET_MIN`（既定`0`） | **T2.5で読み始めた**。カメラ時計のずれ（分）。推定する写真の撮影時刻に1回だけ足す（錨には足さない）。有限の数なら負も小数も可。それ以外は起動を断る（終了コード2。値は出さない） |
| 通信先 | 地理院の逆ジオコーダ・標高API・`muni.js`。**本番は定数のみ**で、環境変数・設定では変えられない（座標を任意のURLへ送らせない）。試験のビルドだけがスタブへ向ける |
| ログ | 段ごとの件数だけ。座標・地名・撮影時刻・URL・応答の本文を出さない |

### 15.2 `infer`

**規則はPythonと同じ**で、違いは§0の行と下に挙げたものだけ。

1. **錨**: `loc_source IN ('exif','drive_meta','manual')`で`taken_at`が空でなく、時刻が読める写真。`inferred`は錨にしない（推定が推定を呼ばない）。`manual`は錨になる（手動で1枚直すと、同じ旅の近い時刻の未解決の写真に波及する）。位置が付いていない錨（`lat`がNULL）も錨の数に入る（現行は`lat`を見ない）。
2. **対象**: `loc_source IS NULL OR loc_source='inferred'`。`exif`・`drive_meta`・`manual`は再推定しない（手動の位置が次のサイクルで上書き・取り消しされない。棚卸しS4）。
3. **選び方**: 対象の時刻に`GEOFRAME_INFER_OFFSET_MIN`を足し、時刻の差が最小の錨。**同順位は`id`の小さい方**（錨を`ORDER BY id`で並べ、より小さい差が出たときだけ入れ替える）。差が最大の窓**以内**（境界を含む）なら、`lat lon`を錨のものにし、`loc_source='inferred'`・`inferred_from`＝錨の`id`・`infer_dt_s`＝差の秒（切り捨て）にする。
4. **取り消し**: 錨が消えた・窓から外れた・時刻が読めなくなった対象で、`lat`が入っているものは、`lat lon loc_source inferred_from infer_dt_s display`をNULLにする。`display`をNULLにするので、Driveの写真は`fetch_derivatives`が取り直す。**derivativeのファイルは消さない**（管理画面が`derivatives/<drive_id>.jpg`を拾い直す）。
5. **地名の列には触らない**（Pythonと同じ）。位置が動いても`place elev muni_cd pref city`は古いまま残る。新しく位置が付いた行は`place`がNULLなので、`enrich`が付ける。**位置が動いたときに地名を付け直すか**は、手動の位置の扱い（[inventory.md §2.3](inventory.md)）と一緒にP3で決める。
6. **時刻**: `YYYY-MM-DD HH:MM`を`strptime`と同じ規則で読む（`geo.rs`の`naive_seconds`。月・日・時・分は1〜2桁、**日だけ空白埋めも読む**（`2024-05- 6 10:05`。`%d`の正規表現にだけある` [1-9]`。月・時・分・秒の空白埋め、`- 06`・`- 16`は読めない）、日付と時刻の間はPythonの`\s`に当たる文字（U+001C〜001Fを含む）1文字以上、暦として不正・範囲外・末尾の余分は失敗）。品質解析の`parse_taken_at`が使う秒つきの形（`naive_seconds_with_seconds`）にも同じ規則。Pythonの`strptime`の結果600通りと突き合わせた（`time_samples`。写真フィードの`parse_epoch`も同じ表と一致）。差は**`TZ`に依存しないnaiveな日時**（暦の日数×86400＋時分）で取る。夏時間の無い`TZ`（本番は`Asia/Tokyo`）ではPythonと一致する。ASCIIの数字だけ読む。

件数（段の出力）: `anchors targets inferred updated unchanged cancelled untimed`。`inferred`は新しく位置が付いた数（現行の戻り値）、`updated`は推定済みで錨や差が変わる数、`unchanged`は推定済みで値が変わらない数（書かない）、`cancelled`は取り消す数、`untimed`は時刻が無い・読めない対象の数。

**書き込み**は計画を1つのトランザクションにまとめる（writerの1回の呼び出し）。始めるとき・256件ごと・`COMMIT`の直前に停止の合図を見て、来ていれば`ROLLBACK`して`省略`で返る（`index_local`と同じ。§13.2）。`UPDATE`は`loc_source IS NULL OR 'inferred'`の行にだけ効く。

### 15.3 `enrich`

**規則はPythonと同じ**で、違いは§0の行と下に挙げたものだけ。

1. **対象**: `lat`・`lon`が数値で`place IS NULL`の行を`id`順に**1サイクル40件まで**（`place`がNULLが「未enrich」の印。空文字は「確定」）。
2. **キー**: `geo:<緯度3桁>,<経度3桁>`（`{:.3}`。PythonのCPython `f"{x:.3f}"`と同じ丸め。62個の値で突き合わせた）。値は`{place, elev, muni_cd, pref, city}`。
3. **国外**（緯度20〜46.5、経度122〜154.5の矩形の外）: キャッシュを引かず、通信せず、`place=''`・`elev=NULL`・`muni_cd=''`・`pref=''`・`city=''`で確定する。キャッシュにも同じ値を書く（Pythonと同じ）。待ちなし。
4. **国内**: キャッシュに`place`・`city`を持つ辞書があればそれを使う（通信しない・待たない）。無ければ逆ジオコーダ（`LonLatToAddress`）と標高APIを呼ぶ。
   - `place`＝応答の`results.lv01Nm`（字。都道府県・市区町村は連結しない。別の列）。
   - `muni_cd`＝`results.muniCd`（ASCIIの数字だけのとき。文字列でも整数でも）。先頭の0を落とした文字列で`muni_table`（`muni.js`から作る コード→[都道府県, 市区町村]。市区町村名の全角空白は除く）を引いて`pref`・`city`にする。表に無ければ空。
   - `elev`＝応答の`elevation`が数値のとき、**偶数丸め**（Pythonの`round`。`12.5`→`12`、`13.5`→`14`、`-0.5`→`0`）の整数。`"-----"`など文字列はNULL。
   - **応答が空でも確定する**（`place=''`）。海上や地理院のカバー外は何度問い合わせても埋まらず、確定させないと40件の枠を恒久的に占める。「応答あり」は、状態コードが2xxで本文がJSONとして読めたこと（形が違う・`results`が空でも「あり」）。
   - **片方のAPIだけ「応答が使えない」（4xx・JSONでない）なら、もう片方の応答で確定する**（Pythonと同じ）。**両方とも使えない**座標は、キャッシュも`photos`も更新せず次の行へ進み、プロセスの中で覚えて30分（失敗が続けば倍々、12時間まで）飛ばす。**片方が通信の障害のまま再試行が尽きたときは確定しない**（§15.4）。
   - **通信を試した行のあとに0.2秒待つ**（停止の合図で打ち切れる）。確定した行も、確定しなかった行も待つ（Pythonは確定した行だけ）。キャッシュ・国外の行は待たない。同じキーの2件目以降は、1件目の結果（キャッシュ）で済む。
5. **処理の順**: **`id`順の1パス**（Pythonと同じ）。国外の行はキャッシュを空で上書きし、国内の行は直前までのキャッシュを見る。`muni.js`は、最初に通信の要る行が来たときに、キャッシュの`muni_table`が無ければ取りに行き、取れたら保存する（Pythonは毎サイクルの先頭）。通信が止まった（`muni.js`が取れない・全体の障害・時間の上限）あとは、通信の要らない行（国外・キャッシュ）は済ませる。
   - **確定していない先の行があるキーは、同じキーの後の行を見送る**（順序を守る）。先の行が確定しないのは、座標による障害（覚えて飛ばす）・両方の応答なし（覚えて飛ばす）・段の中断・時間の上限で止まったとき。サイクルの中では`deferred`、**サイクルをまたぐ分**（覚えて飛ばしている先の行）は、候補を選ぶときに、バックオフ中の行のキーと同じキーの後の行を枠に入れずに`skipped_blocked`に数える。キーの集合は毎サイクル、バックオフ中の行から作り直す（記憶はプロセスの中だけ）。`--dry-run`も同じ選び方をする。
   - 代償: 先の行がずっと確定しない座標（両方の応答が決まって使えないなど）は、同じキーの後の行をその座標のバックオフが明けるたびの再試行まで止める（12時間おきまで延びる）。後の行を先に進めて空のキャッシュを作らせるよりこちらを選んだ。
   - 衝突の例: 緯度20.0のすぐ内側と外側（`(20.0001, 130.0)`と`(19.9999, 130.0)`）はどちらも`geo:20.000,130.000`。`id`順に国内・国外なら、国内の行は問い合わせて確定し、国外の行がキーを空で上書きする（そのあとの国内の同じキーの行は空に当たる）。国外・国内の順なら、国内の行は国外の空のキャッシュに当たり、問い合わせない。`geo-python.json`の`enrich_key_collision`が両方向を固定している。
6. **書き込み**は行ごと（writerの呼び出し）。`UPDATE`は`id`・`lat`・`lon`が計画のときと同じで`place`がNULLの行にだけ効く（enrichの最中に管理画面で位置が直されて地名が消えた行へ、古い座標の地名を書かない）。

件数（段の出力）: `candidates foreign cache_hit queried resolved unresolved deferred skipped_backoff skipped_blocked out_of_time unusable pending`。`queried`は**通信を試した行**（最初の要求を出した時点で数える。成否・途中の停止によらない）、`resolved`はそのうち確定した行、`unresolved`はそのうち確定しなかった行（応答が使えない・障害・停止の合図・時間の上限で打ち切った行。`queried = resolved + unresolved`）、`deferred`は通信できない間・時間の上限のあと・同じキーの先の行が未確定で見送った行（通信していない）、`skipped_backoff`は覚えていて飛ばした数、`skipped_blocked`はその行と同じキーの後の行を枠に入れなかった数、`out_of_time`は段の時間の上限に達したら1、`unusable`は`lon`がNULL・数値でない等で対象にしない数、`pending`は今サイクルを始める時点で未enrichの数（40件を超えた分も含む）。`--dry-run`は`queried resolved deferred out_of_time`の代わりに`would_query`を出す。

### 15.4 通信の失敗の分類

T2.4の`net.rs`の`Failure`（§14.3の表）に合わせる。`Failure::is_outage()`が偽のもの（`Other`）は「応答が使えない」、それ以外は「通信の障害」。

**通信の障害は再試行する**: 同じ要求を、0.5秒・1秒の待ちを置いて2回まで取り直す（待ちは停止の合図で打ち切る）。それでも続くときの扱いは、**座標によらないか**で分ける。

| 障害 | 種類 | 再試行のあとの扱い |
| --- | --- | --- |
| 全体 | `dns`・`connect`・`tls`、429 | 再試行のあと、**すぐ段を中断**。その座標は覚えない（直れば待たずに続ける） |
| 座標による場合もある | 5xx、`timeout`、`disconnected`、`body_unreadable`（下の表） | **その座標を覚えて30分以上飛ばし、次の座標へ進む**。**続けて3つの異なるキー（座標）**で起きたら段を中断する（同じ座標の写真は何枚あっても1つ）。応答が返った座標（確定した・応答が使えなかった）で数は0に戻る。覚えた座標は中断したあとも残る |

中断するときは、確定した行を残し、失敗の文面に種類と件数だけを載せる（URL・座標を含まない）。それ以外の行は次のサイクルで続きからやる。

**時間**: 1要求の上限は8秒なので、再試行込みの1つのAPIは最大で約25.5秒（8秒の時間切れを2回、3回目も8秒、待ち0.5秒＋1秒）、1行（逆ジオ＋標高）は**最大で約51秒**（両方で「2回時間切れ→3回目成功」が続く行も、片方が最終的に失敗する行も同じ）。成功扱いの行は連続の障害にならないので、中断の数え方だけでは止まらず、40行で約34分になりうる（レビュー指摘）。そこで**段全体の経過時間に上限を置く（2分。`STAGE_BUDGET`）**。超えたら、通信が要る未確定の行を次のサイクルへ回し（失敗にしない。`out_of_time=1`）、その行と同じキーの後の行は追い越させない。上限は次の要求（再試行を含む）を始める前に見るので、段の最大は**上限＋進行中の1要求（8秒）＋再試行の待ち（1秒）≒約2分10秒**（`muni.js`の15秒は上限の中に入る）。上限の値は、普通の行（2要求で1秒未満、40行で1分に届かない）の約2倍の余裕と、全体の障害の中断（3座標で最大約153秒だが、その前に上限が来る）、同期サイクルの間隔（既定10分）に収まることから決めた。上限で止まった行は記憶に入れないので、次のサイクルは先頭から試す。1行が約51秒なので、各サイクルで少なくとも1行は進む。現行は障害の行ごとに最大16秒で、40件なら10分を超える。

| 起きる場所 | 失敗 | 扱い |
| --- | --- | --- |
| 逆ジオ・標高。ヘッダーを受け取る前 | `dns`・`connect`・`timeout`・`tls`・`disconnected`（§14.3の表） | 通信の障害（上の表） |
| 同 | `Other`（`Protocol`・`BadUri`・`TooManyRedirects`など） | そのAPIは「応答なし」 |
| 状態コード | 429・5xx | 通信の障害（429は全体、5xxは座標による場合もある） |
| 状態コード | 3xx（リダイレクトを辿り切れない）・4xx | そのAPIは「応答なし」（要求ごとに決まった結果になりうるので、障害にして段を止めない） |
| 2xxの本文 | JSONでない・1 MiBを超える（`BodyExceedsLimit`） | そのAPIは「応答なし」（再試行しない） |
| 2xxの本文を読んでいる最中 | 切断・時間切れ・TLS（`Failure::from`が障害にするもの） | 通信の障害（`disconnected`・`timeout`・`tls`） |
| 同 | それ以外すべて（`Protocol`＝不正なチャンク長、`Decompress`＝途中で終わるgzipなど） | **通信の障害（`body_unreadable`。座標による）**。T2.4のDriveの403の理由の本文（`reason_unreadable`）と同じ考え方で、`Other`（写真の失敗扱い）にしない |
| 2xxで、JSONとして読めた | `results`が空・配列・`null`など形が違う | 「応答あり」で、空のまま確定 |
| `muni.js` | 障害・4xx・本文が解釈できない（表が空） | 通信の要る行を見送り、段を失敗にする。通信の要らない行は済ませる。キャッシュの`muni_table`があれば取りに行かない（再試行しない。キャッシュにあるのが通常） |

ヘッダーの前の`Protocol`（応答の頭が壊れている）は`Other`のまま（`net.rs`の表）。本文の`Protocol`・`Decompress`は、数百バイトの本文が読めない＝座標の中身でなく回線・相手の不調と言えるので障害にするが、特定の座標の応答だけが壊れる場合もあるので「座標による」に入れる。以前は本文の`Protocol`・`Decompress`も`Other`になり、座標ごとの失敗（応答なし）として数えていた。

**標高だけの障害（決定）**: 逆ジオが答えていても、標高が通信の障害のまま再試行が尽きたら、その座標は**確定しない**（逆ジオが障害で標高だけ答える場合も同じ。逆ジオが障害なら標高は呼ばない）。Pythonはもう片方の応答だけで確定してキャッシュする。理由は、確定すると`elev=NULL`（または`place=''`）が**キャッシュにも`photos`にも永久に残り**、海上のNULLと見分けがつかず、あとから直す方法が無いこと。代わりに、標高だけの長期障害の間は国内の未キャッシュの行が進まない（失敗した3座標を覚えて飛ばし、次のサイクルは続く3座標を試して止まる。国外・キャッシュの行は進む）。直れば欠けなく埋まる（覚えた座標は30分以上あとに取り直す）。「進捗」より「キャッシュの恒久性」を取った決定で、標高が表示で使われる（`GET /api/v1/photos`の`elev`）ことも理由。片方が「応答が使えない」（4xx・JSONでない）だけなら、Pythonと同じくもう片方の応答で確定する。

障害から復旧した次のサイクルは、全体の障害で止まった座標は待たずに、座標による障害で覚えた座標は30分以上あとに、続きから処理する。座標による障害で覚えた座標と同じキーの後の行は、その座標が確定するまで見送られるので、**直ったあとは障害が無かった場合と同じ地名に収束する**（§15.3の5。`enrich_key_collision`を障害つきで流すテストで、Pythonの`first`と一致することを確かめた）。

### 15.5 `--dry-run`とログ

- `infer`: 全部オフラインで計算でき、計画をメモリ上の写しに当てる（後続の段は推定後の状態を見る）。件数は`anchors targets inferred updated unchanged cancelled untimed`。取り込み済みの本番コピーで`inferred updated cancelled`が0に近いことが、P2の受入の一部（§15.8）。
- `enrich`: **地理院に問い合わせない**（検証環境の本番コピーに`--dry-run`を回しても、外へ通信しないように）。40件を、国外・キャッシュで済む・問い合わせるはず（`would_query`）に分けて数える。**実行と同じ`id`順で、書くはずのキャッシュを模擬する**（国外の行が空で上書きするキー、問い合わせるはずの行が作るキーは、あとの行にとってキャッシュにある。同じキーの2件目以降は`cache_hit`）。`photos`の写しには書かない（後続の段が地名を使わない）。`.cache.json`は凍結されていて何も書かれない。バックオフ中の行とそのキーの後の行を枠から外す選び方は実行と同じ（`skipped_backoff`・`skipped_blocked`。ただし`--dry-run`は別のプロセスなのでふつうは記憶が空）。`queried resolved deferred out_of_time`の代わりに`would_query`が出る。件数は、障害が無ければ実行の`queried`と一致し、`geo-python.json`ではPythonの逆ジオの要求の数と一致する。
- **停止の境界**: 停止の合図は、行と行のあいだ・行のあとの待ち・再試行の待ちで見る。**通信が進んでいる行は、逆ジオ・標高・キャッシュ・DBの更新まで済ませてから止まる**（要求の途中で捨てても片方の応答が残るだけで、得るものが無い。普通の行は1秒未満、再試行込みでは最大で約51秒＝§15.4）。再試行の待ちの途中で止まったときは、その行は確定せず、覚えもせずに戻る（次のサイクルで待たずに試す）。通信は試したので`queried`と`unresolved`に数える。段の時間の上限（§15.4）は停止の合図と違い、次の要求を始める前に見る。
- ログに出すのは件数と固定の語だけ。座標・地名・撮影時刻・URLは出さない。通信の失敗は種類（`dns`・`http 503`など）だけ。

### 15.6 Pythonとの差（まとめ）

§0の2行に載せたものがすべて。要点:

- **Pythonと同じにした（直さない）もの**: 地名の列を推定で触らない（位置が動いても古い地名が残る）、片方のAPIだけ「応答が使えない」（4xx・JSONでない）ならもう片方で確定する、手動の位置は錨にはなる、時刻のずれの補正は推定する側にだけ足す、国外は空で確定する、`id`順の1パス（同じキーの先の行がキャッシュを作る）。
- **安全側に変えたもの**: 値が変わらない行に書かない、書き込みの条件付き`UPDATE`、通信の障害は再試行し、全体の障害なら段を中断・座標による障害は覚えて飛ばし続けて3つの異なる座標なら中断、段全体の経過時間の上限（2分）、確定していない先の行があるキーは同じキーの後の行を追い越させない（サイクルをまたぐ）、**片方が通信の障害なら確定しない**（§15.4の決定）、`muni.js`が取れなければ地名を付けない、壊れた座標を覚えて枠を塞がせない、`lon`がNULLの行を対象にしない、応答の大きさの上限、通信を試した行はすべて待つ。
- **既知の差**: 全角の数字が混じった時刻（Pythonは読む場合がある）、夏時間のある`TZ`。

### 15.7 `organize`の絞り込みを外した

§13.4のとおり、`geoframe organize`は`index_local`→`infer`→移動の順になった（Pythonの`organize_photos`と同じ）。`local::organize`は`infer`が済んだ状態の`source='local' AND lat IS NULL`をそのまま選ぶ。`held_back_until_infer`は件数から無くなった。`--dry-run`は`index_local`と`infer`の計画を写しに当ててから数える。

### 15.8 検証環境での確認（2026-10-25）

検証環境（本番の2026-10-24のバックアップのコピー。`photos`6,535行・`inferred`2,162行・Driveの鍵は無い）に`rust-deploy.sh`で配布して（コミット`a4b36c7`）、比較した。座標・地名は書かない（件数だけ）。

| # | 確認 | 結果 |
| --- | --- | --- |
| (a) | 本番コピーで`geoframe sync --dry-run` | `infer: anchors=3125 targets=3409 inferred=0 updated=0 unchanged=2162 cancelled=0 untimed=31`、`enrich: candidates=0 pending=0`、サイクルは22 ms。**Pythonが推定・enrich済みの本番コピーで、変更は0**。推定済み2,162行すべてが「値が変わらない」（`lat`・`lon`・`inferred_from`・`infer_dt_s`がPythonの保存した値とビット単位で一致） |
| (b) | 実サイクル（`GEOFRAME_INGEST=1 geoframe sync`）の前にDBを検証環境の中でバックアップし、**わざと壊して**回す: 推定済み2,162行の`lat lon loc_source inferred_from infer_dt_s`をNULLにし、地名の5列をNULLにした13行（国内8・国外5）を作った | `infer: inferred=2162`、`enrich: candidates=13 foreign=5 queried=8`（実際の地理院に逆ジオ8＋標高8＋`muni.js`1回）。**実行後の`.dump`のSHA-256が、壊す前と完全に一致**（2,162行の推定と13行の地名がPythonの保存した値に戻った。`display`などの列も変わらない） |
| (c) | `.cache.json`（検証環境には元々無かった） | `geo:`13件＋`muni_table`。実物の`muni.js`を、Pythonの正規表現とRustの手書きの解析に通した表が**1,919件で完全に一致** |
| (d) | 後始末 | `.cache.json`とバックアップを消した（`.dump`のSHA-256は(b)の後のまま一致）。`geoframe-rs`は`GEOFRAME_INGEST`なしのまま稼働、`/healthz` 200、`/api/v1/photos` 3,065件 |

**レビュー指摘の修正後（コミット`efd3dd5`）の再確認**: 同じ検証環境に`rust-deploy.sh`で配布した（`/healthz` 200、`/api/v1/photos`は3,065件、`GEOFRAME_INGEST`なしのまま）。座標・地名は書かない（件数とハッシュだけ）。

| # | 確認 | 結果 |
| --- | --- | --- |
| (e) | 本番コピーで`geoframe sync --dry-run` | `infer: inferred=0 updated=0 unchanged=2162 cancelled=0`、`enrich: candidates=0 foreign=0 cache_hit=0 would_query=0 unresolved=0 pending=0`。(a)と同じ |
| (f) | 本番コピーの**コピー**（別のデータ置き場）で、地名の5列をNULLにした13行（国内8・国外5）に実サイクル（`GEOFRAME_INGEST=1 geoframe sync`）を流す | `enrich: candidates=13 foreign=5 cache_hit=5 queried=3 resolved=3 unresolved=0 deferred=0`（実際の地理院へ3座標。残りの国内5行は、同じキーのキャッシュに当たったと見られる）。13行の地名の5列のハッシュが、壊す前と**一致**。本番コピーの`.dump`のSHA-256は配布の前後で変わらない（検証用のコピーとバックアップは消した） |

**2回目のレビュー指摘の修正後（コミット`60c5de7`）の再確認**: 同じ検証環境に`rust-deploy.sh`で配布した（`/healthz` 200、`GEOFRAME_INGEST`なしのまま）。検証環境の本番コピーは触らず、`catalog.db`のコピーを別の一時ディレクトリに作って確かめた（コピーは確認後に消した）。座標・地名は書かない（件数だけ）。

| # | 確認 | 結果 |
| --- | --- | --- |
| (g) | 本番コピーのコピーで`geoframe sync --dry-run` | `infer: inferred=0 updated=0 unchanged=2162 cancelled=0`、`enrich: candidates=0 foreign=0 cache_hit=0 would_query=0 unresolved=0 skipped_backoff=0 skipped_blocked=0 unusable=0 pending=0`、サイクルは28 ms。**以前どおり0**（新しい件数`skipped_blocked`が増えた） |
| (h) | 本番コピーのコピーで、地名の5列をNULLにした13行（国内8・国外5）に実サイクル（`GEOFRAME_INGEST=1 geoframe sync`）を流す | `enrich: candidates=13 foreign=5 cache_hit=5 queried=3 resolved=3 unresolved=0 deferred=0 skipped_backoff=0 skipped_blocked=0 out_of_time=0`（実際の地理院へ3座標。サイクルは2.1秒）。13行の地名の5列が、壊す前と**一致** |

### 15.9 テスト

| 対象 | テスト |
| --- | --- |
| Pythonとの突き合わせ（`geo/tests.rs`の`pythonのinfer_locationsとenrich_rowsと全列が一致する`） | `tools/gen_geo_fixture.py`が作る`geo-python.json`。11ケース・23段階: 合成の行（座標は駅・役所など公開の代表点、時刻・地名は架空）にPythonの`infer_locations`・`enrich_rows`を段階的に流した結果と、Rustの`photos`の全列・`infer`が付けた数・地理院への要求の種類と座標と順序・`.cache.json`の`geo:`キーが一致する。窓の境界（15分・2時間・12時間ちょうどと、12時間1分）、同順位（錨が等距離・同じ時刻）、オフセット（−90・＋30・±0.01分）、`GEOFRAME_INFER_MIN`を1・1000にした窓、錨の削除・移動・手動への変化・全部の消失、時刻なし・空・暦の不正・末尾の余分・桁が足りない・空白2つ・全角の数字、国内・国外・矩形の角と外、**日の空白埋め（`2024-05- 6`）・空白埋めできない形（月・分・` 06`）の行（取り消されない／取り消す）**、**国内外でキーが衝突する行（`enrich_key_collision`。国内が先・国外が先・先にキャッシュがある・同じキー3件）**、応答が空・標高が文字列・片方だけ応答・両方なし・JSONでない・`null`・`results`が配列・自治体コードの先頭の0・数字でない・未知、偶数丸め、キャッシュ済み、同じキーの2件目、40件の枠。Pythonの`geo.http_json`だけを差し替えて、`geo.py`本体を流す。各段階で`--dry-run`も流し、`would_query`がPythonの逆ジオの要求の数と一致することを確かめる。CIで`--check` |
| `strptime`の突き合わせ（`pythonのstrptimeと時刻の読み方が一致する`） | 同じ`geo-python.json`の`time_samples`: 手書き45通りと乱数600通り（月・日の空白埋め、` 06`・` 16`、区切りの空白の種類、秒の有無・範囲、暦の不正など）を、`datetime.strptime`の`%Y-%m-%d %H:%M`・`%Y-%m-%d %H:%M:%S`に通した結果。`naive_seconds`・`naive_seconds_with_seconds`・写真フィードの`parse_epoch`が一致する（全角の数字は入れない。既知の差） |
| `muni.js`の解析・キー | 合成の断片（カンマを含む市区町村名、重なるコード、先頭に0が並ぶ、閉じない引用符など）をPythonの正規表現に通した表と一致。`{:.3}`はPythonの`f"{x:.3f}"`と62個の値（ちょうど中間・負・大きい値）で一致 |
| 単体テスト（`geo/tests.rs`） | 時刻の解釈、同順位、オフセット、取り消しと地名・お気に入りの残存、手動、値が変わらない行に書かない（トリガーで`UPDATE`を数える）、計画と書き込みの間に手動になった行を上書きしない、書き込みの途中の停止、`--dry-run`（通信しない・写しに当てる・件数）、国外、空で確定、待ちの回数、片方だけ応答・両方なし、壊れた座標の記憶と後続の処理、全体の障害（429）はすぐ中断して覚えない、座標による障害（5xx・切断・本文の途中の切断・壊れたチャンク・途中で終わるgzip×逆ジオ・標高）は覚えて飛ばし3座標で中断・復旧後に続きから、先頭だけ5xx・間に成功があれば中断しない、標高だけの長期障害は確定しない、一度だけの5xxは再試行で通る、本文を読む失敗の分類の表、再試行・停止の待ち、通信を試した行はすべて待つ、国内外のキー衝突（国内が先・国外が先・通信できない間の追い越し・単発の503で止まった行の追い越し・バックオフ中の行と同じキーの後の行を枠に入れない・3座標の中断をまたいでも追い越さず直れば`enrich_key_collision`のPythonの結果に収束する）、同じ座標の写真が3行障害でも中断しない、段の時間の上限（スタブの遅延と短くした上限で、打ち切り・同じキーの後の行を追い越さない・再試行に進まない・次のサイクルで続きから）、**時間切れの行を失敗の記憶に入れない**（1時間のバックオフの同じ`Enrich`を使い回し、応答を戻した直後のサイクルで`skipped_backoff=0`のまま確定する。上限のあとで要求を始めなかった行と、要求の途中で上限に達した行の両方。記憶に入れる形に変えると落ちることを確かめた）、`muni.js`の失敗、キャッシュの値の検査、`lon`が数値でない行、競合、40件、停止 |
| `local/tests.rs` | `organize`が推定で位置が付く写真を移さない・窓の外と時刻なしは移す・錨が消えれば次に移す |
| `tests/ingest_cli.rs` | `sync --dry-run`が`infer`・`enrich`の件数を出す、取り込み済みの形のDBで国外の1件が空で確定する |

### 15.10 レビュー（Sol）の指摘への対応

| 指摘 | 対応 |
| --- | --- |
| 必須: 処理順の変更で、国内の行が空の地名で永久に確定する | `id`順の1パスに戻した（§15.3の5）。通信できない間も、止まった行・見送った行と同じキーの行は追い越さない（`deferred`）。`--dry-run`も同じ順で、国外の行が上書きするキー・問い合わせる行が作るキーを模擬する。`enrich_key_collision`を`geo-python.json`に足した |
| 推奨: 単発・座標に依存する5xxが後続を長く止める | 2回まで再試行（0.5秒・1秒）。全体の障害（`dns`・`connect`・`tls`・429）はすぐ中断、座標による障害は覚えて飛ばして次へ、続けて3座標で中断（§15.4）。**標高だけの障害は確定しない**と決めた（理由は§15.4）。本文の`Protocol`・`Decompress`は`Failure::BodyUnreadable`として障害（座標による）に入れた |
| 推奨: 両方の応答が壊れた場合に待たず、問い合わせの件数にも入らない | 通信を試した行はすべて待つ。`queried`（試した）と`resolved`（確定した）を分けた |
| 推奨: `strptime`の互換に日付の空白埋めが抜けている | `%d`の` [1-9]`を再現した（`naive_seconds`と`naive_seconds_with_seconds`）。区切りの空白もPythonの`\s`（U+001C〜001F）に合わせた。`time_samples`で突き合わせ |
| 参考: enrichの停止の境界 | 変えずに§15.5へ明記した（通信が進んでいる行は確定してから止まる。再試行の待ちの途中なら確定せずに戻る） |

### 15.10.1 レビュー（Sol）の2回目の指摘への対応

| 指摘 | 対応 |
| --- | --- |
| 必須: 障害のときに同じキーの後続の行が追い越し、国内の行が空で永久に確定する（バックオフ中の行は候補から除くだけでキーを保持しない） | 確定していない先の行（座標による障害・両方の応答なし・段の中断・時間の上限）があるキーは、同じキーの後の行を見送る（`deferred`）。サイクルをまたぐ分は、候補を選ぶときにバックオフ中の行のキーを毎サイクル作り直し、同じキーの後の行を枠に入れず`skipped_blocked`に数える。`--dry-run`も同じ。試験: 単発の503、3座標目の障害による中断から次のサイクル、バックオフ中の行（両方の応答なし）と同じキーの国外の行。`enrich_key_collision`を障害つきで流し、直ったあとにPythonの`first`と同じ行・キャッシュに収束することを確かめた |
| 推奨: 「続けて3座標」が同じ座標の写真3行でも成立する | 連続の数を**異なるキーの集合**で数える（応答が返ったら空にする）。同じキーの後の行は見送るので、同じ座標の3行は1つ。試験: 同じ座標3行の障害のあとに正常な座標を置く（中断せず、正常な座標が進む） |
| 推奨: 再試行込みのサイクルの所要時間に上限が無く、文書の最大時間も過小 | 段全体の経過時間の上限を2分にした（§15.4）。次の要求（再試行を含む）の前に見て、超えたら通信が要る未確定の行を次のサイクルへ回す（失敗にしない。`out_of_time=1`）。文書の最大時間を、1行約51秒・段は上限＋約10秒に直した。試験: スタブの遅延と短くした上限 |
| 参考: 再試行の待ちで停止すると、通信済みでも`queried=0` | `queried`を最初の要求を出した時点で数える。停止・時間の上限で打ち切った行は`unresolved`に数える（`queried = resolved + unresolved`）。試験の期待値を直した |

### 15.10.2 レビュー（Sol）の3回目の推奨への対応（T2.7）

| 指摘 | 対応 |
| --- | --- |
| 推奨: 「時間切れの行を記憶しない」テストが、バックオフ時間ゼロ・次サイクルで新しい`Enrich`のため、誤って失敗の記憶に入れても通る | 2つのテスト（`段の経過時間の上限で打ち切り_残りは次のサイクルへ回す`・`上限のあとは再試行に進まない`）を、1時間のバックオフを持つ同じ`Enrich`を使い回し、応答を正常に戻したあとの**直後のサイクル**で`skipped_backoff=0`・`skipped_blocked=0`・`out_of_time=0`のまま対象の行が確定することを見る形にした（`enrich_with_long_backoff`）。前者は「上限のあとで要求を始めなかった行」（標高だけを遅くして、次の行を始める前に上限を過ぎさせる）、後者は「要求の途中で上限に達した行」。それぞれ、該当箇所で`remember_failure`を呼ぶ形に一時的に変えると落ちることを確かめた（変更は戻した） |

### 15.11 回したもの

- **T2.6**: `quality`段（簡易品質）。済み（§16）。
- **T2.7（実施済み。[p2-acceptance.md](p2-acceptance.md) §2）**: 検証環境での受入のうち、**同じ写真を比較用PythonとRustへ入れた`photos`の列ごとの比較**（`infer`・`enrich`を含む。地理院は本物）。Driveの本物での確認は管理者が行う。
- **P3**: 位置が動いたときに地名を付け直すか（§15.2の5）。手動の位置の上書き（§13.10・§14.12）と一緒に決める。

## 16. 簡易品質（P2 / T2.6）

写真の重複・類似を見つけ、表示から外してよい写真（完全重複の非推奨）を決める。現行の`quality.py`の`quality_cycle`・`quality_backfill`に対応する。棚卸しB3・B9（`--quality-scan`）・S7（[inventory.md](inventory.md)）。実装は`crates/geoframe/src/quality.rs`（段・DB・`--dry-run`・一括解析）、`quality/features.rs`（1枚の特徴量）、`quality/group.rs`（グルーピング。DBにも画像にも触れない純粋な関数）。試験は`quality/tests.rs`。

**表示への効き方**: 配信の`QUALITY_VISIBLE`（`GET /api/v1/photos`が`group_kind='exact'`で`recommended=0`の写真を外す）が`photo_quality`を読む（§8.1）。段の責任は`photo_quality`を作ることだけで、配信の側は変えていない。

### 16.1 段と設定

| 項目 | 内容 |
| --- | --- |
| 段 | `quality`。同期サイクルの**最後**（`fetch_derivatives`の後）。失敗・パニックは隔離される（§11.3） |
| `GEOFRAME_QUALITY_PER_CYCLE`（既定は`GEOFRAME_FETCH_PER_CYCLE`＝25） | 1サイクルで解析する枚数。0以上の整数だけ（負・小数・非数は起動を断る＝終了コード2。Pythonは負を`max(1, …)`で1にする）。**0は1として扱う**（Pythonの`max(1, limit)`）。`UNSUPPORTED_ENV`から`SUPPORTED_ENV`へ移した |
| 流れ | ①孤児の特徴量を消す→②未解析を`id`順に上の枚数まで解析→③未解析が0で`photo_quality_dirty=1`のときだけ再構築（Pythonと同じ） |
| ログ | 件数だけ（`removed pending analyzed failed remaining rebuilt rows`）。写真の名前・パス・失敗の詳しい文面は出さない |

### 16.2 算法の方針: Pythonが作った特徴量の行を有効として使う

**決めたこと**: `photo_quality_features`の行は、Pythonが作ったものも、Rustが作ったものも、同じく有効として使う。Rustは**新しく入った（または`display`・`md5`・`mtime`が変わった）写真だけ**を自分の算法で解析する。本番コピーの約6,500枚を解析し直さない。

**理由**: ①特徴量の行には**算法の印が無い**（`photo_quality`には`algorithm`があるが、`photo_quality_features`には無い）。「Pythonの行だけを無効にして作り直す」には列を足すしかなく、それはスキーマの移行（「何もしない」ことを試験で保証している。§11.7）とPythonへの切り戻し（P5）に触れる。②有効かどうかの判定は`display`・`md5`・`mtime`の`IS`比較だけで（Pythonと同じ）、算法には依らない。③全部解析し直すと次のような費用になる（下の計測）。

検証環境の本番コピー（`photos`6,535行すべてに有効な特徴量がある。`photo_quality`は559行・243グループ、`GET /api/v1/photos`は3,065件）の複製で、3通りを比べた（件数だけ。§16.9）:

| | A. Pythonの行を使う（採用） | B. 300枚だけRustで解析（混在） | C. 全6,535枚をRustで解析し直す |
| --- | --- | --- | --- |
| 解析する枚数 | 0（`pending=0`） | 300 | 6,535 |
| 所要時間 | — | 17秒 | **424秒**（`quality-scan`）。同期サイクルの既定（25枚／10分）では**262サイクル＝約44時間**（その間、新しい写真の重複判定は前回のまま止まる） |
| `photo_quality`の行数・グループ数 | 559・243（変わらない） | 559・243 | 560・243 |
| グループの顔ぶれが変わった写真 | 0 | 0 | 7（距離12の境界付近の組が入れ替わった） |
| 推奨が変わった行／順位が変わった行 | 0／0 | 4／17 | 2／14 |
| スコアが変わった行 | 0 | 38（最大68.5） | 301（最大43.5） |
| 完全重複の非推奨（＝配信から外れる）の集合 | 同じ（93） | 同じ | 同じ |
| `GET /api/v1/photos`の件数 | 3,065 | 3,065 | 3,065 |

Aは、Pythonの特徴量の行をそのまま入力にしたRustの再構築が、**Pythonの`photo_quality`と559行すべて同じ**になること（`group_id`・順位・推奨・スコア・理由・注意を含む）。B・Cでスコアが大きく動く行があるのは、グループ内の最小・最大で正規化する式が、鮮明度がほぼ同じ2枚では0.4%の差で順位を入れ替えるため（Rustの鮮明度はPythonより中央値で0.4%高い）。表示に効く完全重複の集合は、どの場合も変わらない。

**混在の安全性**: Pythonの行とRustの行が同じ再構築に入るので、pHashが別の算法どうしで比べられる。そこで、RustのpHashを**Pythonと同じ作り方**（グレーに変換→中央を正方形に切って32×32へLanczos→DCTの左上8×8→直流を除く63個の中央値より大きいか→先頭の係数が最上位ビット）にした。画素は一致しない（デコーダ・縮小の実装が違う）が、同じ写真のPythonの値とのハミング距離を、検証環境の本番コピーの6,535枚（`geoframe img-probe --quality`の出力と、Pythonが保存した`phash`）で測った:

| ハミング距離 | 0 | 2 | 4 | 6 | 8以上 |
| --- | --- | --- | --- | --- | --- |
| 枚数 | 6,134（93.9%） | 391（6.0%） | 9 | 1 | 0 |

最大6で、類似の判定の閾値（12）に届くものは0枚。SHA-256は6,535枚すべて一致（同じファイルのバイト列を読んでいる）。Pythonの値と**同じ閾値で別の算法の値を比べても、判定の境目に来る写真はごくわずか**で、混在のB（無作為の300枚をRustで解析）は、上の表の回ではグループの顔ぶれが1枚も変わらなかった。別の無作為の300枚で測り直した回（§16.9の(h)）では7枚が入れ替わった。2回は標本が違うので、この差の原因は切り分けていない。さらに、**同じSHA-256の写真は、指標（鮮明度・露出・コントラストなど）を`id`が最小の行にそろえてから順位をつける**（`group.rs`の`unify_same_content_metrics`）: 同じバイト列の指標は同じはずで、違うのは算法が違う行が混ざったときだけ。そろえないと、同一ファイルの2枚（Pythonの行とRustの行）のスコアが算法の差で食い違い、完全重複でどちらを推奨にするかが算法の差で決まる。Pythonの行だけの入力では何も変わらない（本番コピーで確かめた。§16.9）。

**pHashはそろえない**（指標を分ける）: 類似の結びつき（3分以内・距離12以下）は、各行の元のpHashで判定する。pHashまでそろえると、同じSHAの2枚目が別の写真と結びついていたのに、1枚目のpHashでは結びつかなくなり、グループの顔ぶれ・`kind`・配信の抑制がPythonと変わる。例（回帰テスト）: A・Bが同じSHAでpHash距離2・時刻差10分、CはBの1分後で、距離がA–B=2・B–C=12・A–C=14。Pythonは3枚の`similar`（B–Cがつながる）。BのpHashをAにそろえるとB–C=14でつながらず、`exact`のA・Bだけになり、Bが配信から消える。

**限界**: 混在のとき（同じSHAの写真が別の算法のpHashを持つ場合に限らず、別の写真どうしでも）、結びつきは各行のpHashで決まるので、その行が別の写真と距離12の境目にいると、算法の差で結びつきが変わりうる（同一画像のPython／Rust間の距離は最大6と計測したが、別の画像との距離12の境界は保証できない）。この影響は、Pythonの行とRustの行が混ざる場合だけで、Pythonだけの行・Rustだけの行では起きない。

### 16.3 特徴量（`quality/features.rs`）

| 項目 | 内容 |
| --- | --- |
| SHA-256 | **`display`が指すファイルのバイト列**（Driveの写真はderivative。ローカルの写真でブラウザが表示できるJPEG・PNGは原本）。Pythonの`sha256_file(path)`と同じ。16進・小文字。`ring`で計算 |
| 読む前の検査 | `DataDir::resolve_existing`（絶対パス・`..`・リンクの先が`DATA_DIR`の外は失敗）、通常のファイルか、512 MiB以下か（§12.5）。**FIFOなどで止まらない** |
| デコード | 内容で形式を判別（JPEG・PNG・TIFF・HEIC）→EXIFの向きを当てる（`exif_transpose`）→**Pillowと同じ整数式**でグレー化（`(R×19595+G×38470+B×7471+0x8000)>>16`。`image`の`to_luma8`は係数が違うので使わない）。ICC・アルファは使わない |
| pHash | 上の16.2の作り方。`ImageOps.fit`の切り出し（中央、縦横比1:1、小数の枠）を`fast_image_resize`の`crop`で、Lanczos3で32×32へ。DCTは係数の表をPythonと同じ式で作り、足す順もPythonと同じ。保存は`{:016x}` |
| 鮮明度 | 長辺512pxへLanczosで縮め（`target_dims`はPillowの`thumbnail`と同じ丸め）、`FIND_EDGES`（3×3、0〜255に収める、外周1画素は元のまま）→外周4画素を除く→母分散（`ImageStat.var`と同じ式）→`ln(1+var)` |
| 輝度・露出・白飛び・黒つぶれ・コントラスト | ヒストグラムから。Pythonの式をそのまま写した（`exposure_score`、`white_clip`＝248以上、`black_clip`＝8未満、`contrast`＝95%点−5%点） |
| 決定性 | 乱数・時刻・並列に依らない。同じバイト列は常に同じ値（単体テスト） |
| Pythonとの違い | **縮小は`reducing_gap`による箱型の縮小を再現しない**（Pillowの`thumbnail`は先に箱型で縮めてからLanczos。Rustは直接Lanczos）。鮮明度はRust／Pythonの比が中央値1.004（5〜95%点が1.000〜1.007、最小0.985・最大1.011）、輝度は中央値で0.1（0〜255）低い、コントラスト（整数）は95%点で1違う（16.2の計測）。値は`error=''`の行なら有効で、混在しても順位づけは相対値で行う |
| 失敗 | 画像として読めない・パスが不正・大きすぎる・通常のファイルでない: `error`に**種類だけ**を書いた行を作る（Pythonは`型名: 例外の文字列`を500文字まで。パスを含みうるので書かない）。この行も解析済み（画像が変わるまで再解析しない）で、グルーピングには入らない |
| `analyzed_at` | `+09:00`のようにUTCからのずれが付く（`datetime.now().astimezone().isoformat(timespec="seconds")`）。`sysinfo::local_isoformat_offset` |
| `algorithm` | `photo_quality.algorithm`は`rust-phash-v1:t3:h12`（Pythonは`poc-phash-v1:t3:h12`。`t3`＝時刻の窓3分、`h12`＝距離12）。再構築のたびに全行がこの値になる。特徴量の表には`algorithm`の列が無い。**`algorithm`は特徴量の由来ではなく、その`photo_quality`を作った実装の印**（Pythonも読まず、無効化にも使わない） |

### 16.4 グルーピング（`quality/group.rs`）

Pythonの`build_quality_rows`・`rank_group`と同じ。入力は`id`の昇順の特徴量。

1. **完全重複**: SHA-256が同じ写真を1つにつなぐ。
2. **類似**: 撮影時刻（`YYYY-MM-DD HH:MM:SS`、なければ`YYYY-MM-DD HH:MM`）の差が**3分以内（ちょうど3分を含む）**で、pHashのハミング距離が**12以下**の写真どうしをつなぐ（撮影時刻の順に、3分の窓の中で比べる）。時刻が読めない写真（空・NULL・暦として不正・秒が60以上・末尾の余分）は時刻では結びつかない。
3. **グループ**: つながった連結成分で2枚以上のものだけが行になる。`kind`は成分のSHAが1種類なら`exact`、それ以外は`similar`。
4. **スコア**: グループ内の相対値（鮮明さ62%・露出25%・コントラスト13%、白飛び・黒つぶれの減点、`round(…, 1)`）。最小と最大が`math.isclose`なら0.5。理由・注意の文言（`ぼけ傾向`・`露出要確認`・`白飛び多め`・`黒つぶれ多め`・`低コントラスト`／`グループ内で最も鮮明`・`鮮明さ良好`・`露出良好`・`白飛び・黒つぶれ少`・`総合スコア上位`）もPythonと同じで、`json.dumps(ensure_ascii=False)`と同じ`["…", "…"]`の形。
5. **順位**: スコアの降順、同点は`id`の昇順。**推奨**はSHAごとの最上位1枚（代表）のうち上位2枚（完全重複だけのグループは1枚）。信頼度＝代表の1位と3位（なければ最後）のスコアの差（代表が1枚だけなら100）。
6. **グループ番号**: `exact`が先、次にグループが大きい順、次に信頼度の高い順、次に先頭の写真の`taken_at`の文字列の昇順（安定ソート。同順位は成分の最小の`id`の順）。

`round(x, 1)`はPythonと同じ丸め（10進で最も近い値、中間は偶数側）を`format!("{x:.1}")`の解析で再現し、境界の値で突き合わせた。

### 16.5 再構築と書き込み

- **全員分そろってから**: 未解析（`display`が空でなく、行が無いか`display`・`md5`・`mtime`が違う）が0件で、`settings`の`photo_quality_dirty`が`'1'`のときだけ再構築する。未解析が残る間は前回の`photo_quality`を保つ（途中結果で表示候補が揺れない）。
- **`BEGIN IMMEDIATE`の1トランザクション**で、未解析の数・dirty・特徴量の読み取り・グルーピング・`DELETE FROM photo_quality`・全行の`INSERT`・`photo_quality_dirty='0'`をまとめる（Pythonは読んでから別に書く。Rustは読んだあとに他の書き込みが入って食い違わない）。途中で失敗しても、確定の前に停止の合図が来ても、何も変わらない（前回の結果とdirtyのまま）。
- 特徴量の書き込み（upsert）と`photo_quality_dirty='1'`は1つのトランザクション。孤児の削除と`dirty='1'`も1つ。
- 解析（重い）は書き込み接続の外で行う（writerを塞がない）。
- 再構築で`photo_quality`に書く列は、Pythonと同じ（`photo_id display source_md5 source_mtime content_sha256 group_id group_kind rank_in_group recommended score reasons_json issues_json algorithm analyzed_at`）。`source_md5`・`source_mtime`は`photos`の値をSQLiteの型のまま写す。

### 16.6 `--dry-run`と`geoframe quality-scan`

- **`sync --dry-run`**: 解析（重い）は**実行しない**。件数だけ: `orphans`（消す孤児）`pending`（未解析）`would_analyze`（今回解析する枚数）`remaining`（今回のあとに残る未解析）`would_rebuild`（0／1: 残りが0で、dirtyか今回の解析か孤児の削除がある）。今回解析する写真が無くて再構築になるときは、`rows`（書く行数）と`changed`（今の`photo_quality`から変わる行数。追加・削除・変更。`analyzed_at`・`algorithm`は比べない）も出す。**取り込み済みの本番コピーでは全部0**（`pending=0`、dirty=0）。`--dry-run`の写しでは、先の段（`local_derivatives`・`fetch_derivatives`）が取得するはずの写真に`display`が入るので、それらも`pending`に数えられる（実行でも同じサイクルの`quality`が解析するので、数え方は同じ。ただし`GEOFRAME_QUALITY_PER_CYCLE`を超える分は次のサイクル以降）。Driveの鍵が置かれた環境の`--dry-run`で`pending`が取得待ちの分だけ増えるのは、この読み方による。
- **`geoframe quality-scan [--dry-run] [--data-dir <dir>]`**（現行の`server.py --quality-scan`の`quality_backfill`）: 孤児を消し、未解析が0になるまで`GEOFRAME_QUALITY_PER_CYCLE`件ずつ解析し、再構築する。`GEOFRAME_INGEST=1`と、`sync`・`organize`と同じガード（検証環境の印・Pythonが非稼働）を通す。`--dry-run`は`would_analyze`が未解析の全部になる。書いたのに未解析が減らない（行が有効にならない）ときは、無限に回さず失敗にする。出力は件数だけ。
- `geoframe img-probe --quality <画像>…`: 画像から特徴量（SHA-256・pHash・指標）を出す確認用。DB・`DATA_DIR`に触れない。ファイル名は出さない。16.2の計測に使った。

### 16.7 Pythonとの差（まとめ）

§0の1行に載せたもの:

- **特徴量の行の算法**は混在する（Pythonの行を使い、Rustは新しい写真だけを解析する）。
- **Rustの鮮明度・露出・輝度は、Pythonと値が少し違う**（`reducing_gap`を再現しない）。pHashは同じ作り方で距離が小さい。
- **同じSHA-256の写真は、指標を`id`最小の行にそろえて順位をつける**（pHashはそろえない。類似の結びつきは各行の元のpHashで決める）。
- **混在の限界**: 同じSHAの写真が別の算法のpHashを持つとき、別の写真との距離12の境目の組は算法の差で結びつきが変わりうる（§16.2）。
- **失敗の行の`error`は種類だけ**（Pythonは例外の文字列）。
- `algorithm`は`rust-phash-v1:t3:h12`（特徴量の由来ではなく、結果を作った実装の印）。
- **Pythonへ切り戻した後の再構築**は、Rust製の特徴量も入力にする（§16.10）。
- 特徴量の書き込みと`dirty`、再構築の読み書きがそれぞれ1トランザクション。
- `GEOFRAME_QUALITY_PER_CYCLE`が負・小数・非数なら起動を断る。
- 時刻の読み方は§15.2の6と同じ規則（`strptime`に合わせる。ASCIIの数字だけ）。秒つきの形も試す。全角の数字が混じった時刻は読めない（既知の差。§0）。

### 16.8 テスト

| 対象 | テスト |
| --- | --- |
| Pythonとの突き合わせ（`quality/tests.rs`の`pythonのquality_cycleとquality_backfillと全列が一致する`） | `tools/gen_quality_fixture.py`が作る`quality-python.json`。21ケース・合成の`photos`と**合成の特徴量**（画像は使わない）にPythonの`quality_cycle`・`quality_backfill`を段階的に流した結果と、Rustの`photo_quality_features`・`photo_quality`・`settings`の全列・戻り値（`analyzed removed remaining rebuilt`）が一致する。完全重複、3分の境界（2:59・3:00・3:01、秒つき）、距離12・13、連鎖、類似の中の完全重複、3枚以上の推奨・信頼度・同点のid順、理由・注意の文言、読めない時刻、件数制限での分割、一括解析、失敗の行、孤児（削除・空・NULL）、無効化（`md5`が値に・`mtime`・`display`）、位置なし・`display`なし、グループの並び、dirtyの印が無いとき、前回の結果を保つ、再グルーピング、**同じSHAでpHashが違う写真の結びつき**（`same_sha_phash_differs`。A–B=2・B–C=12・A–C=14で3枚が`similar`）、乱数の3ケース（60・90・40枚）。CIで`--check` |
| 切り戻し（`quality/tests.rs`の`rustで作った結果をpythonへ切り戻すと…`） | 同じ`quality-python.json`の`switchbacks`（3ケース）。Python本体に、①Pythonが特徴量と`photo_quality`を作る→②新しい写真（同じバイト列の写し）をRustが解析して再構築する（Pythonの`build_quality_rows`に「同じSHAの指標を`id`最小の行にそろえる」を足して流した結果）→③Pythonで変化なしのサイクル→④`dirty=1`にしてPythonで再構築、を流して各段階の特徴量・`photo_quality`・`algorithm`・`dirty`と、②④の差を記録する。Rust側は、Rustの実際の結果が②と全列で一致すること、③が②を一切変えないこと、④でグループの顔ぶれ・`kind`・`group_id`が変わらず、変わるのは順位・推奨・スコア・理由・注意だけであること、指標が同じ写しなら何も変わらず、指標が違う完全重複の写しでは推奨が入れ替わることを確かめる。CIで`--check` |
| 特徴量の算出（`quality/tests.rs`） | 同じ入力なら同じ値（別スレッドでも）、再圧縮・縮小した同じ絵のpHashが近い、別の絵は遠い、ぼけ・白飛び、極端な縦横比、壊れた入力、Pillowと同じ整数式のグレー化、`round`の丸め |
| 段（実ファイル） | 解析→行→再構築、失敗の行（パス・ファイル名が入らない）、リンクで`DATA_DIR`の外を指す写真、件数制限と0、再構築が失敗しても前回の結果を保つ（トリガーで挿入を失敗させる）、停止の合図、`--dry-run`、一括解析と待ちが減らないときの停止、位置なし・`display`なし、同じSHAの指標をそろえる、同じSHAでpHashが違っても結びつきは各行のpHashで決める（回帰） |
| `tests/ingest_cli.rs` | `sync --dry-run`の品質の件数、`quality-scan`（ガードの拒否、`--dry-run`、実行） |
| `config.rs`・`cli.rs`・`sysinfo.rs` | `GEOFRAME_QUALITY_PER_CYCLE`、`quality-scan`の引数、`+09:00`付きの時刻の形 |

### 16.9 検証環境での確認（2026-10-25）

検証環境（本番の2026-10-24のバックアップのコピー。`photos`6,535行、特徴量6,535行・`photo_quality`559行、Driveの鍵は無い）に`rust-deploy.sh`で配布して（コミット`eb54361`）確かめた。**書き換える前に、検証環境の中でバックアップを取った**（`sqlite3 .backup`。`.dump`のSHA-256の先頭16桁`57f6dcef…`）。座標・ファイル名・撮影日時は出していない。

| # | 確認 | 結果 |
| --- | --- | --- |
| (a) | 特徴量の計測: 6,535枚のderivativeを`geoframe img-probe --quality`で解析し、Pythonが保存した値と比べた | SHA-256は6,535枚すべて一致。pHashの距離は0が6,134・2が391・4が9・6が1（最大6）。鮮明度の比（Rust／Python）は中央値1.004。**1枚あたり中央値62 ms・95%点88 ms**（長辺2560pxのJPEG。1スレッド、最大RSS 87 MiB）、全6,535枚で407秒。同期サイクルの既定の25枚は約1.6秒 |
| (b) | 本番コピーで`geoframe sync --dry-run` | `段 quality: 完了 orphans=0 pending=0 would_analyze=0 remaining=0 would_rebuild=0`（**ほぼ0どころか0**）。`infer`は`inferred=0 updated=0 cancelled=0`、`enrich`は`candidates=0`。`.dump`のSHA-256は実行の前後で一致 |
| (c) | `GEOFRAME_INGEST=1 geoframe sync`を1サイクル（本番コピーの実DBに対して） | `段 quality: 完了 removed=0 pending=0 analyzed=0 failed=0 remaining=0 rebuilt=0 rows=0`。**`.dump`のSHA-256が実行前・バックアップと完全に一致**（`photo_quality`559行・`algorithm=poc-phash-v1:t3:h12`のまま）、`/api/v1/photos`は3,065件のまま、`/healthz` 200。常駐の`geoframe-rs`は`GEOFRAME_INGEST`なし（読み取り専用）のまま。元に戻す作業は要らなかった |
| (d) | 複製A: dirty=1にして`--dry-run`→`quality-scan` | dry-runは`would_rebuild=1 rows=559 changed=0`。再構築のあとの`photo_quality`は**元の559行と全列で一致**（`algorithm`・`analyzed_at`を除く）。`/api/v1/photos` 3,065件。**同じSHAの特徴量をそろえる処理（§16.2）は、本番のPythonの行では何も変えない** |
| (e) | 複製B・C（§16.2の表） | 300枚だけRustで解析（混在）: 17秒、グループの顔ぶれ・完全重複の集合・E1の件数は変わらない。全6,535枚をRustで解析し直す: 424秒、7枚の組が入れ替わる。どちらもE1は3,065件 |
| (f) | 棚卸しS7: 複製Aに、単独の写真と同じバイト列の写真を2枚足して1サイクル | dry-runは`pending=2 would_analyze=2 would_rebuild=1`。実行は`analyzed=2 rebuilt=1 rows=562`。新しいグループは`exact`で、元の写真が推奨（順位1）、足した2枚が非推奨（順位2・3）。`/api/v1/photos`は足した直後の3,067件から**3,065件に戻った**（完全重複の非推奨が配信から消えた）。`photo_quality`の全行が`algorithm=rust-phash-v1:t3:h12`になった |
| (g) | 後始末 | 複製（`/var/tmp`の使い捨て）・バックアップ・スクリプトを消した。実DBは(c)のとおり不変 |
| (h) | レビュー指摘への対応後（コード`de90a57`。以後は文書だけ）を`rust-deploy.sh`で配布し、**検証環境の中でバックアップを取って**から確認 | 本番コピーの`sync --dry-run`は`段 quality: 完了 orphans=0 pending=0 would_analyze=0 remaining=0 would_rebuild=0`（実行の前後で`.dump`のSHA-256の先頭16桁`57f6dcef…`が一致）。常駐の`geoframe-rs`（`GEOFRAME_INGEST`なし）は`/healthz` 200、`/api/v1/photos`は3,065件。コピーA（dirty=1→`quality-scan`）は`photo_quality`が**559行・243グループで元と全列一致**（`algorithm`・`analyzed_at`を除く）、完全重複の非推奨（外れる数）は93、コピーの`/api/v1/photos`は3,065件・`/healthz` 200。コピーB（無作為の300枚の特徴量を消してRustで解析し直す。混在）を2回測った。**1回目は行数・グループ数・配信から外れる数・`/api/v1/photos`の件数だけを測り**（559・243・93・3,065件）、顔ぶれは測っていない（`group_id`の番号は信頼度の並びで振り直されるので、番号の比較は顔ぶれの比較にならない）。**2回目は顔ぶれを測った**: 標本は1回目と別の無作為の300枚で、行が560になり、**グループの顔ぶれが変わった行が7・推奨が変わった行が2・順位が15・スコアが29**、`kind`の変化は0、配信から外れる数は93。この7が算法の混在による境目の入れ替わりかどうかは、標本が違うので切り分けていない（決定的な根拠は、同じSHAでpHashが違う写真の結びつきをPythonの結果と全列で突き合わせる試験。§16.8） |
| (i) | (h)の後始末 | コピーとバックアップは消した。実DBの`.dump`は(h)の全体で`57f6dcef…`のまま不変 |

再現の手順（検証環境の中で。座標・ファイル名は出力に出ない）:

```sh
# 実DBに触れる前にバックアップ
sudo -u geoframe sqlite3 /var/lib/geoframe/catalog.db ".backup /var/tmp/catalog.pre.db"
# (a) 特徴量の計測: derivative の絶対パスを引数に渡す（出力は引数の順。Python が保存した phash と突き合わせる）
geoframe img-probe --quality --jobs 1 /var/lib/geoframe/derivatives/<ファイル>…
# (b) 書かずに件数: 品質の行は pending=0 would_rebuild=0 になる
sudo -u geoframe env GEOFRAME_DRIVE_KEY=/nonexistent/k.json geoframe sync --dry-run --data-dir /var/lib/geoframe
# (c) 実DBに1サイクル（前後の .dump の SHA-256 と /api/v1/photos の件数が同じ）
sudo -u geoframe sqlite3 -readonly /var/lib/geoframe/catalog.db .dump | sha256sum
sudo -u geoframe env GEOFRAME_INGEST=1 GEOFRAME_DRIVE_KEY=/nonexistent/k.json geoframe sync --data-dir /var/lib/geoframe
# (d)(e)(f) 複製で: バックアップを使い捨ての置き場に置く。derivatives/ はハードリンクで複製する（実体は増えない）
d=/var/tmp/qcopy; mkdir -p $d && cp /var/tmp/catalog.pre.db $d/catalog.db && cp -al /var/lib/geoframe/derivatives $d/ && chown -R geoframe $d
sudo -u geoframe sqlite3 $d/catalog.db "DELETE FROM photo_quality_features WHERE photo_id IN (SELECT photo_id FROM photo_quality_features ORDER BY random() LIMIT 300)"
sudo -u geoframe env GEOFRAME_INGEST=1 geoframe quality-scan --data-dir $d
```

### 16.10 Rustで作った結果をPythonへ切り戻す（P5のロールバック）

**`algorithm`の意味**: `photo_quality.algorithm`は、その行を作った**実装の印**（`rust-phash-v1:t3:h12`／`poc-phash-v1:t3:h12`）で、**特徴量の由来ではない**。Pythonは`algorithm`を読まず、全行を書き換えても特徴量を無効にしない（有効かどうかは`display`・`md5`・`mtime`の`IS`比較だけ）。Rustが作った特徴量の行（`photo_quality_features`には算法の印が無い）は、切り戻したPythonからもそのまま有効に見える。

試験は`tools/gen_quality_fixture.py`の`switchbacks`（Python本体を流す）と、Rust側の`rustで作った結果をpythonへ切り戻すと…`（§16.8）。流れは、Pythonが特徴量と`photo_quality`を作る→新しい写真（古い写真と同じバイト列の写し。算法が違うのでpHashと指標が少し違う）をRustが解析して再構築→**Pythonの変化なしのサイクル**→**`dirty`にしてPythonで再構築**→Rustが解析した特徴量の行を消してPythonに解析し直させる。

| 段階 | 起きること（試験で確認） |
| --- | --- |
| 切り戻した直後（Pythonの変化なしのサイクル） | **何も変わらない**。未解析が0で`dirty`でなければPythonは再構築しないので、Rustが作った`photo_quality`をそのまま保つ（全列が一致。`algorithm`は`rust-phash-v1:t3:h12`のまま）。配信は変わらない |
| 新しい写真が入る／何かで`dirty`になる | Pythonが未解析を解析し、全員分そろったら**全行を再構築**する。入力は`photo_quality_features`の全行で、**Rust製の特徴量も含む**。`algorithm`は全行`poc-phash-v1:t3:h12`になる |
| その再構築での変化 | **グループの顔ぶれ・`kind`は変わらない**（pHashは各行の元の値で比べる規則がRustとPythonで同じ。§16.2）。変わりうるのは、**同じSHAの写しの指標が算法の差で違うときの、順位・推奨・スコア・理由・注意**と、それで信頼度の並びが変わったときの**グループ番号`group_id`の振り直し**（試験の3ケースでは起きない）。Rustは同じSHAの指標を`id`最小の行にそろえるが、Pythonは行ごとなので、指標が高い方の写しが代表になりうる。完全重複のグループでは**表に出る（推奨の）写しが入れ替わる**（`/api/v1/photos`の件数は同じ。完全重複の非推奨が外れる数はグループの大きさで決まるため）。指標が同じ写し（同じ算法で作られた写し）なら何も変わらない |
| Rustが解析した行を消して、Pythonに解析し直させる | 同じバイト列のPythonの値になるので、**Pythonだけで作った場合の結果に戻る**。指標が同じ写しの組は変わらない。Rustの別算法のpHashが橋になっていた組（§16.2の限界）は、Pythonの値に戻ると結びつかず、グループが減りうる |

**切り戻しの手順**（[plan.md P5](plan.md#p5-切替pr-6)）:

1. Pythonを起動する。`photo_quality`には何もしなくてよい（変化なしのサイクルは結果を保つ）。`algorithm`を書き換える必要も、意味もない。
2. **Pythonだけで解析した結果に戻したいときだけ**、**切り戻す前に**、Rustが解析した特徴量の行を消す（`photo_quality_features`には印が無いので、Rustへ切り替えた日時以降の`analyzed_at`で選ぶ）: `DELETE FROM photo_quality_features WHERE analyzed_at >= '<Rustへの切替の日時>'`。Pythonが次のサイクルから`GEOFRAME_QUALITY_PER_CYCLE`枚ずつ解析し直し、**未解析が0になるまで前回の`photo_quality`（Rustの結果）を保つ**。全員分そろった時点で、Pythonの算法で再構築する。**これはRustの結果を保つ方法ではない**: Rustの別算法のpHashが橋になっていた組は、Pythonの値に戻ると離れ、グループの顔ぶれ・`kind`・配信から外れる数が変わりうる（試験`bridge_by_copy`では、`similar`3行が`exact`2行になり、配信から外れる数が0→1）。消さずに切り戻しても壊れない（変化は上の表のとおり）。
3. 確認:
   - 手順2をしない（特徴量を保ったまま、後で`dirty`の再構築が走る）とき: `GET /api/v1/photos`の件数が切り戻しの前後で同じ、`photo_quality`の行数・グループ数が同じ。
   - 手順2をしたとき: 件数・行数・グループの顔ぶれ・`kind`は変わりうる。解析し直しが終わった（未解析が0）あとに、`photo_quality`の行数・グループ数・配信から外れる数と`GET /api/v1/photos`の件数を記録し、Pythonだけで解析した場合の結果として受け入れる。

### 16.11 回したもの

- **T2.7（実施済み。[p2-acceptance.md](p2-acceptance.md) §1・§3）**: 検証環境での受入のうち、本番コピーの`--dry-run`（変更ほぼ0）と実サイクルの通し、棚卸しS7（同じバイト列の写真・3分以内のほぼ同じ写真を入れて、完全重複の非推奨がE1から消えること）。
- **P4**: AIワーカーが`photo_quality_features`を読む（`content_sha256`が`error=''`の行）。Rustで特徴量を作った行もそのまま使える。


## 17. 更新API（P3 / T3.1）

状態: T3.1 実装済み（2026-10-25）。E12 `POST /api/admin/setloc`（手動の位置）、E13 `POST /api/admin/setflag`（お気に入り・除外）と、POST の入口（ルーティング・CSRF 対策・エラー応答）。実装は`post.rs`（入口）と`update.rs`（E12・E13）。E14 回転は T3.2（実装済み。§17.8）、旅行ブログの保存・書き出し（E15・E16）は T3.6。T3.6 のルートは入口の検査（403・400・413）まで通し、処理は未実装として 503 を返す。巨大・不正な`Content-Length`はHTTP層が先に断るので、下の表の順序・状態コードどおりにならない（§17.1.1）。

### 17.1 順序と状態コード

外側から §9.1 の検査（501 → Host → 絶対形式）を通ったあと、現行 `do_POST` と同じ順序:

| 順 | 検査 | 外れたとき |
| --- | --- | --- |
| 1 | ルートが無い（`/api/blog/`配下も含む）。`webpath::normalize`した結果の完全一致なので、`/api/admin/setloc/`・`//api/admin/setloc`・`%73etloc`・`?x=1`付きも当たる | HTMLの403（`GET`の未知の`/api/blog/…`だけが404。`POST`は違う） |
| 2 | `Content-Type`の`;`より前を空白除去（Pythonの`str.strip()`）・小文字化して`application/json`。値は生バイトを**Latin-1として**読む（§17.1.2） | JSONの403 |
| 3 | `Origin`があり**空でなければ**、そのnetloc（Pythonの`urlparse(origin).netloc`）が`Host`（空白除去）と**文字列として同じ**（ポート込み・大文字小文字を区別）。`Origin`が無い・空なら通す。`urlparse`が`ValueError`にする形（角括弧の中がIPv6でない等）は断る（Pythonは応答を返さず接続を閉じる。§17.1.2） | JSONの403 |
| 4 | `Content-Length`がある（`Transfer-Encoding: chunked`は無いのと同じ） | JSONの400 |
| 5 | 64 KiB（65536）以下。**超えたら本文を読まず**413を返し、`Connection: close`で接続を閉じる | JSONの413（`上限65536バイト`） |
| 6 | JSONとして解釈できる | JSONの400 |
| 7 | 値がオブジェクト | JSONの400 |
| 8 | 各ルートの処理 | `BadRequest`400・`NotFound`404・（実行中）409・writerなし503・その他500 |

- **この表は`post::handle`の順序**。巨大・不正な`Content-Length`と`Transfer-Encoding`は、axum/hyperのHTTP層が4・5行目より先に断る（状態コード・本文が表と違う）。拒否の向きだけの差で書き込みは起きない（§17.1.1）。
- CORSヘッダは一切返さない。`OPTIONS`は501のまま（§9.1の1）。`HEAD /api/…`は今のまま403（P1で決めたもの）、`GET /api/admin/setloc`などの更新ルートへのGETは未実装の503のまま（T3.3で`/api/admin/`の503が狭まる）。
- 応答は`{"ok": false, "error": "…"}`（成功は各ルートの形）。`Content-Type: application/json; charset=utf-8`・`Cache-Control: no-store`。
- 文面（`error`）は現行と同じ。例外・SQLiteのエラー文字列は**応答に出さない**: 500は`内部エラーが発生しました`だけで、詳細（`WriteError`のDisplay＝SQLiteのメッセージ。表名・列名まで）は`[api] <ルート> 内部エラー: …`としてログにだけ出す。成功時は何もログに出さない（座標・`id`を出さない）。アクセスログは§9.6のまま（メソッド・ルートのテンプレート・状態コード・時間）。
- Pythonの`http.server`はHTTP/1.0で毎回接続を閉じるが、Rustは keep-alive する。本文を読まずに返す応答（403・400）の未読の本文は hyper が扱う（次の要求として解釈しない）。413だけは、Pythonと同じく**明示的に閉じる**（`Connection: close`）。
- 本文は`axum::body::to_bytes`で宣言の長さまで読む。宣言より短く切れたら400。遅い・読まないクライアントは`limits.rs`の無通信・停滞の上限（§9.3）が切る。

#### 17.1.1 HTTP層が先に拒否する場合

上の表は`post::handle`が見る順序。**axum/hyperは`Content-Length`と`Transfer-Encoding`をそれより前に（ハンドラに届く前に）検査して拒否する**ので、巨大・不正な値の要求は表の4・5行目に届かず、状態コードも本文も表と違う。HTTP層の検査は変えられない（変えるには自前のHTTPパーサーが要る）ので、**hyperが断る分の厳密な互換は求めない**。理由は、その場合は**書き込みが起きず**、Pythonが書く入力をRustが断る「拒否の向き」の差だけだから（管理者の決定）。**逆向き（Pythonが断るのにRustが書く）は直す**（管理者の方針）。見つかったのは次の2つで、どちらも`post.rs`が書く前に断る:

- **`int()`の桁数の上限**: hyperは`Content-Length`を「ASCIIの数字だけ（`u64`に収まる値）」で読み、先頭の`0`をいくつ足しても通す（`0`を4298個＋`37`など）。Pythonの`int()`は、符号・前後の空白・`_`を除いた**10進の桁数（先頭の`0`も数える）が4300（`sys.int_info.default_max_str_digits`。Python 3.11以降と、3.10.7・3.9.14・3.8.14の既定）を超えると`ValueError`**で、`do_POST`は400（`Content-Length が必要です`）にする。Pythonで確かめた境界: 4300桁（`0…037`）は通る、4301桁・4302桁は400。前後に空白を足した4300桁は通り（空白は数えない）、4301桁は400。`+`付きの4301桁は400。`post::content_length`が同じ上限（4300桁超は`None`＝400）にする。hyper通過後に残る値は「ASCIIの数字だけ」（前後の空白は`httparse`が取り除き、`+`・`_`・全角などの数字・`0x`・`1e3`・`3_7`はhyperが断る）なので、Pythonの`int()`が読む形のうち**hyperも通すのは数字だけの文字列で、断る条件は桁数だけ**。桁数の検査は413の判定より前（4301桁なら値が上限超過でも413でなく400）。環境変数`PYTHONINTMAXSTRDIGITS`で上限を動かした本番のPythonがこれより緩くても、Rustは厳しいだけ（拒否の向き）。
- **`http.server`の行の大きさ**: Pythonは要求行が65536バイト（`POST ` + 対象 + ` HTTP/1.1\r\n`）を超えると414、ヘッダ1行（`名前: 値\r\n`）が65536バイトを超えるか、ヘッダが100行以上（終わりの空行も数える＝ヘッダ99個まで）だと431で、`do_POST`に届かない。hyperの既定（head全体約400KiB・ヘッダ100個まで）は緩く、Rustは書いてしまう。`axum::serve`はhyperの上限を変えられない（変えるには自前の接続ループが要る）ので、`post::head_limits`（`server::site`の`POST`、`post::handle`の前）が、hyperが整えたあとの値から長さを見積もって、書く前に431・414（`Connection: close`、定型JSON）にする。見積もりは現行以下（`名前: 値\r\n`の`: `を空白1つと数える）なので厳しい向きにずれる。**残る穴（意図して許す）**: 値の前後の空白（hyperが取り除く）と、hyperが捨てる重複した`Content-Length`の行は見えないので、それだけ長い・多い要求をPythonは断り、Rustは通すことがある。通っても、書くのは正しく検査された更新だけ。

実際のTCP接続（`axum::serve`＝`serve`と同じhyper/axumのスタック。`LimitedListener`は通さない）で確かめた結果。Pythonはフィクスチャ（現行の`handlers.Handler`）の結果で、毎回接続を閉じる（HTTP/1.0）。「閉じる」は応答のあと同じ接続に`GET /healthz`を足して応答が来ないこと:

| 入力（`Content-Type: application/json`。本文は有効なJSON） | Python | Rust 状態 | Rust 本文 | Rust 接続 | Rust 書き込み |
| --- | --- | --- | --- | --- | --- |
| `Content-Length: 65536`（本文も65536バイト） | 200 | 200 | 通常 | 保つ | する（同じ） |
| `65537`（本文は送らない） | 413 | 413 | 定型JSON（`post.rs`。`Connection: close`） | **閉じる** | しない |
| `18446744073709551615`（`u64`の最大値） | 413 | **431** | なし | 閉じる | しない |
| `18446744073709551616`（`u64`を超える） | 413 | **400** | なし | 閉じる | しない |
| 数字でない（`abc`） | 400 | 400 | なし（Pythonは定型JSON） | 閉じる | しない |
| 数字でない＋`Content-Type: text/plain` | **403** | **400** | なし | 閉じる | しない |
| 負（`-1`）・空 | 400 | 400 | なし | 閉じる | しない |
| `+N`・`3_7`（Pythonの`int()`が読む形。Nは本文の長さ37） | **200（書く）** | **400** | なし | 閉じる | しない |
| `N, N` | 400 | 400 | なし | 閉じる | しない |
| 重複・同じ値（`N`と`N`） | 200 | 200 | 通常 | 保つ | する（同じ） |
| 重複・最初が正しく後ろが違う（`N`と`99999`、`N`と`70000`） | **200（書く）** | **400** | なし | 閉じる | しない |
| 重複・最初が巨大（`99999`と`N`） | 413 | 400 | なし | 閉じる | しない |
| `0`を4298個＋`37`（4300桁。本文は37バイト） | 200（書く） | 200 | 通常 | 保つ | する（同じ） |
| `0`を4299個＋`37`（4301桁）・4302桁 | **400**（`Content-Length が必要です`） | 400（同じ文面） | 定型JSON（`post.rs`） | 保つ | しない |
| 4300桁の`0…0`＋`65537`（本文は送らない） | 413 | 413 | 定型JSON | 閉じる | しない |
| 4301桁の`0…0`＋`65537`（本文は送らない） | **400**（413でない） | 400（同じ文面） | 定型JSON | 閉じる（本文が来ない） | しない |
| 前後に空白を足した4300桁・4301桁 | 200（書く）・400 | 200・400 | 通常・定型JSON | 保つ | する・しない（同じ） |
| 前後にタブを足した`37` | 200（書く） | 200 | 通常 | 保つ | する（同じ） |
| `+`付きの4301桁 | 400 | 400 | なし（hyperが`+`を先に断る） | 閉じる | しない |
| 重複・最初が4301桁の`0…037`、後ろが`37` | **400** | 400（同じ文面） | 定型JSON | 保つ | しない |
| 重複・最初が`37`、後ろが4301桁の`0…037` | 200（書く） | 200 | 通常 | 保つ | する（同じ） |
| ヘッダ1行が65536バイト（`X-A: `＋値＋CRLF）・65537バイト | 200（書く）・**431** | 200・431 | 通常・定型JSON（`post.rs`） | 保つ・閉じる | する・しない（同じ） |
| ヘッダ99個・100個（Host・Content-Type・Content-Length・Connectionを含む） | 200（書く）・**431** | 200・431 | 通常・定型JSON | 保つ・閉じる | する・しない（同じ） |
| 要求行が65536バイト（クエリで延ばす）・65537バイト | 200（書く）・**414** | 200・414 | 通常・定型JSON | 保つ・閉じる | する・しない（同じ） |
| `Transfer-Encoding: chunked`のみ | 400（`Content-Length が必要です`） | 400（同じ文面） | 定型JSON | 保つ | しない |
| `Transfer-Encoding: chunked`と`Content-Length` | 400（`JSONとして解釈できません`） | 400（`Content-Length が必要です`） | 定型JSON | 閉じる | しない |

- 431はhyperが巨大な値に付けるもので、本文を読まずに接続を閉じる。`u64`を超える整数の400はhyperの構文エラー。どちらもhyperの版に依存するので、試験は固定した版の値を期待値にし、版を上げて変わったら落ちて気づく。
- hyperは`Content-Length`を厳密に（ASCIIの数字だけ）読み、複数あれば**同じ値のときだけ**1つにまとめる（数値で比べ、残すのは最初の行）。Pythonは`int()`で`+`・アンダースコア・前後の空白を読み、重複は最初の値だけを使う。この差が、上の「Pythonは書く・Rustは断る」の行になる。
- `Transfer-Encoding: chunked`はhyperが`Content-Length`を外すので、`post::handle`は`Content-Length`が無いものとして400にする（Pythonはチャンクの枠も含めて`Content-Length`の長さだけ読み、JSONにならず400）。どちらも書かない。
- 413だけは`post::handle`が返し、本文を読まず`Connection: close`で閉じる（§17.1の5行目）。ほかのHTTP層の拒否は、hyperが接続を閉じる。
- 試験は、ポートを0で取り、実時間に頼らない（応答・EOF・リセットは即座に来る。止まれば20秒の上限で失敗にする）。

#### 17.1.2 CSRFの判定のヘッダの値の読み方

`Content-Type`・`Origin`・`Host`の値を読む規則は`csrf.rs`（純関数）に集めた。**Pythonが断るものをRustが通すことは、`Origin`の前後の空白・タブ（下の表の最後。hyperのフレーミング）を除いて1件も許さない**（管理者の方針）。

**Latin-1で読む（方式の選択）**。現行の`http.server`はヘッダの生バイトを Latin-1 の`str`として読み（`http.client.parse_headers`）、`strip()`・`lower()`・`urlparse()`をその`str`に掛ける。UTF-8として読み直す（`String::from_utf8_lossy`）と、`Content-Type: \xC2\xA0application/json`（UTF-8のNBSPが前に付く）が`\u00A0`1文字になって`trim()`に取り除かれ、Pythonは`\xC2`が残って403にするのにRustは通して書いた（レビューの指摘1）。そこで`csrf::latin1`で**1バイト＝1文字**に直してから、Pythonと同じ規則で判定する。「非ASCIIのバイトを含む値は403」にする方式にしなかったのは、Pythonが通す正当な値（Latin-1の`\xA0`・`\x85`の前後の空白、UTF-8の名前を`Origin`と`Host`の両方にそのまま載せる非ブラウザ）を断らないため。空白は Python の`str.strip()`のもの（Latin-1の範囲では`\t\n\x0b\x0c\r`・`\x1c`〜`\x1f`・空白・`\x85`・`\xa0`）。`Host`の検査（`hostcheck`）も同じ読み方にした（`guard`が`csrf::latin1`で読む）。

**`netloc`の取り出し**。この環境のPython 3.13の`urllib.parse.urlsplit`の移植（`csrf::netloc`）: 先頭の制御文字・空白（U+0000〜U+0020）を除き、タブ・CR・LFを取り除き、`scheme:`（先頭が英字で`:`までが英数字と`+-.`）を読み飛ばし、`//`から次の`/`・`?`・`#`までを取る。角括弧がある`netloc`は、Pythonの`_check_bracketed_netloc`・`_check_bracketed_host`と同じ形だけ通す: 角括弧の前に何も無い（`userinfo@`は除く）・`]`の後ろは空かポート（`:`）だけ・中が`v`で始まれば`v16進数.1文字以上`（IPvFuture）、そうでなければ**IPv6であること**（`ipaddress.IPv6Address`の移植。`%`のゾーンIDは空や`%`入りが不可、`/`が不可、IPv4形式の末尾は先頭`0`なしの4つの10進）。IPv4・空・ゾーンIDだけ（`[::1%]`）は`ValueError`。レビューの指摘2（`Host: [127.0.0.1]`・`Origin: http://[127.0.0.1]`、`[::1%]`）はこれ。Pythonの`_checknetloc`（非ASCIIの`netloc`にNFKCを掛けて`/?#@:`が現れたら`ValueError`）は移植しない: Latin-1の範囲（U+0080〜U+00FF）にNFKCで`/?#@:`を生む文字が無いため（Python 3.13で全256文字を確かめた）。

**Pythonが例外のとき**。`_csrf_ok`の中の`urlparse`の`ValueError`は捕まえられず`do_POST`を抜け、`socketserver`が接続を閉じる。**実際のサーバーで確かめた結果、応答は返らない**（状態コードが付かない。フィクスチャでは0）。Rustは403（クロスオリジンの定型JSON）を返す。拒否の向きの差で、書き込みは起きない。

**Pythonと違うもの**（`Origin`の前後の空白・タブだけが許す向き、あとはRustだけが断る拒否の向き）:

| 入力 | Python | Rust |
| --- | --- | --- |
| 値に制御文字（0x00〜0x08・0x0a〜0x1f・0x7f）。`application/json`の後ろの`\x0b`、前の`\x0c`・`\x1c`、`Origin`の前の`\x01`など | 読む。`strip()`が取り除く（`Content-Type`・`Host`）か`urlparse`が取り除く（`Origin`の前の`\x00`〜`\x20`）ので200のことがある。後ろの`\x0b`の`Origin`は403 | httparseが要求ごと**400**（`post.rs`に届かない）。差分試験では、Pythonが通す件のうち`Content-Type` 25件・`Origin` 238件・`Host` 30件がこれ |
| 折り返し（obs-fold。値の次の行が空白で始まる） | 続きを値に連結して読む（200のことも403のこともある） | **400** |
| ヘッダとして読めない行（名前とコロンの間の空白: `Origin : …`、`Bad Name: x`）。`email`のパーサはそこで**ヘッダの読み込みを打ち切り、後ろのヘッダ（`Host`・`Origin`・`Content-Length`など）が全て消える** | `Content-Length`が消えれば400、`Content-Type`が消えれば403、**`Content-Length`の後ろにこの行があれば、その後ろの`Origin: http://other.test`が消えて200で書く** | **400** |
| `Origin`の角括弧が`ValueError`になる形 | 応答なしで接続を閉じる（0） | **403**（定型JSON） |
| `Origin`の値の前後の空白・タブ（RFC 9110のOWS） | 先頭は取り除く。**末尾の空白は残して`netloc`に入り403**（タブは`urlparse`が捨てるので、空白が1つでもあれば403。`http://localhost/ `のように`path`の後ろなら200）。角括弧のあとの空白は例外で接続を閉じる | hyper（httparse）が両端のSP/HTABを取り除くので、取り除いた値で判定して**200で書く**（**許す向き**。ブラウザは`Origin`に空白を付けられず、付けられるクライアントは`Origin`を省けばPythonでも通るので、守りは変わらない。下記） |

**`Origin`の前後の空白・タブ（既知の許す向きの差）**。Pythonは`Origin`の値の先頭の空白・タブだけ取り除き（`email`のパーサの`lstrip`。`urlsplit`も先頭の空白を除く）、末尾は残す。`http://localhost␠`（␠は空白）の`netloc`は`localhost␠`で`Host: localhost`と一致せず403。hyper（httparse）はRFC 9110どおり値の両端のOWS（SP・HTAB）を取り除いてから渡すので、`post.rs`には元の値が見えず、Rustは`http://localhost`として200で書く。値の前後の空白の除去はHTTPのフレーミングの処理で、§17.1.1と同じく文書に明記して許す。生のバイト列を接続の層で走査して補うことはしない（全接続の経路に複雑さと副作用を足すため。以前の実装は、接続に印を残して以降の正しい`Origin`のPOSTも403にしていた）。守りが弱くならない理由: ①ブラウザは`Origin`に空白を付けられない（`Origin`は禁止ヘッダで、ブラウザが決まった形で付ける）。②空白を付けられる非ブラウザのクライアントは`Origin`を送らなければよく、`Origin`が無ければPythonでも通る（無い・空なら通す仕様）。③`Origin`は`Host`と同じでなければ403のままで、取り除かれるのは前後の空白・タブだけ（`http://other.test␠`は403）。先頭の空白・タブはPythonも取り除くので差にならない。

**差分試験**（`tests/csrf_python.rs`、フィクスチャ`csrf-python.json`、`tools/gen_csrf_fixture.py`、CIで`--check`）:

- `content_type` 642件: 前後の空白・タブ・制御文字・NBSP・Latin-1の0x80〜0xFFの全バイト・UTF-8の空白類（NBSP・全角空白・EM SPACE・BOM・NEL・行区切り）、大文字小文字、`;`の前後、別の中身。
- `origin` 6667件: scheme（有無・大文字・`null`・`file://`・`x-y.z+w://`・先頭が数字・`-`）、userinfo、ポート（有無・`:`だけ・`:0`・先頭`0`・`:80x`）、path・query・fragment・`;`・末尾の`/`・`?`・`#`、角括弧（IPv6・IPv4・ゾーンID・空・閉じ忘れ・二重・前後の余り・IPvFuture・IPv4射影・先頭`0`のIPv4）、`Host`の大文字小文字・ポート・空白・空・無し、前後・途中の1バイト（170種）、IPv6の書式を1文字ずつ崩した500個（種を固定した乱数）。
- `mixed` 245件: `Content-Type`・`Origin`・`Host`の有無と中身の組み合わせ（`Content-Type`が先に判定される順序）。
- `host` 1659件: `_host_ok`（待ち受けポート8080・許可する名前は`localhost`だけ）。
- `http` 84件: 実際の`ThreadingHTTPServer`に生バイトで`POST`した結果（状態コード・書き込みの有無）と、Rustのサーバーへ実際のTCPで送った結果の一致。表にある差だけ（`DIFFERENT_STATUS`）。

試験は、各件で hyper の変換（値の両端の空白・タブを取り除く、制御文字なら要求ごと400）を再現して`post::csrf_verdict`・`hostcheck::host_allowed`を呼び、**Pythonの判定と同じ種類**（`content_type`／`origin`・`error`→403）になることを要求する。Pythonが通すのにRustが断るのは、制御文字を含む値（HTTP層が400）だけで、その件数と、含まれるバイトが制御文字だけであることも固定する。**既知の差**（`is_known_ows_difference`）は、`Origin`の値の末尾が空白かタブで、Pythonが断り（`origin`の403か、例外）Rustが通したものだけ。9213件のうち`origin`に70件・`mixed`に3件・`content_type`と`host`に0件の計73件で、件数も固定する（増えたら新しい差、減ったら読み方の変化に気づく）。さらに各件で、hyperが取り除く前の生の値を同じ判定（`csrf::verdict`）に渡すとPythonと同じ結果になることを要求し、原因が前後の空白・タブの除去だけであること（`netloc`や角括弧の読み違いが紛れ込まないこと）を固定する。これ以外の件は、Pythonと同じ判定を要求するので、許す向きの差は0件。実際のTCPの84件では10件（`KNOWN_OWS_DIFFERENCES`。`origin_trailing_space`・`origin_trailing_spaces`・`origin_trailing_tab_space`・`origin_trailing_space_tab`・`origin_leading_and_trailing_space`・`origin_trailing_space_bracket`・`origin_trailing_space_ip`・`origin_trailing_space_empty_netloc`・`origin_name_uppercase_trailing_space`・`origin_duplicate_first_trailing_space`）が、Pythonは403（`..._bracket`は例外で応答なし）・Rustは200で書く。名前を挙げた件だけがこの向きを許され、載せた件がもう同じになったり最初の`Origin`の末尾が空白・タブでなくなったりしたら落ちる。同じ接続の後続の要求が前の`Origin`に引きずられないことも確かめる。フィクスチャは Python 3.11 と 3.13 で同じ結果（CI は 3.12）。

### 17.2 E12 `setloc`・E13 `setflag`

`update.rs`。検査は現行の`_req_id`・`_req_coord`と同じ境界・同じ文面。

- `id`: 整数で正（`bool`・文字列・小数・`null`・欠落は`id は整数で指定してください`、0以下は`id が範囲外です`）。`i64`を超える整数は入力として正しく、`lat`/`lon`（`setflag`は`favorite`/`hidden`）の検査とwriterの確認（503）のあとで、**SQLを実行せずに**`id=… は存在しません`の404（§17.5①。`rowid`が`i64::MAX`の行があっても更新しない）。
- `lat`（-90〜90）・`lon`（-180〜180）: 数値のみ（整数も可で`float`に直す。`bool`・文字列・`null`・欠落は`… は数値で指定してください`）。閉区間。範囲外は`lat が範囲外です（-90.0〜90.0）`。検査の順は`id`→`lat`→`lon`。
- E12が更新する列（現行の`admin_setloc`と同じ。フィクスチャで`photos`の変化を全列比較）: `lat lon`、`loc_source='manual'`、`inferred_from infer_dt_s`と`place elev muni_cd pref city`をNULL。`display`・`favorite`・`hidden`・`rotation`などには触れない。地名は`enrich`が`lat IS NOT NULL AND place IS NULL`で次のサイクルに拾い直す。`manual`は`infer`の再評価の対象外で、ほかの未解決の写真の錨になる（棚卸しS4）。応答`{ok,id,lat,lon,loc_source:'manual'}`、対象なしは404。
- E13: `favorite`・`hidden`はJSONの`bool`だけ（`null`・数値・文字列は`… は true/false で指定してください`）。どちらも無ければ400。指定した列だけ書く。応答`{ok,id,favorite,hidden}`（更新後の両方）、対象なしは404。`favorite`→`hidden`の順に検査し、片方が不正なら何も書かない。
- 検査（400）を先に済ませ、通ったものだけ writer の有無を見る（§17.4）。

### 17.3 同期サイクルとの競合

書き込みは全部 writer の単一接続・専用スレッドを通る（§11.4）ので、段の1回の書き込み（1トランザクション）の**途中**に更新APIは入れない。入れるのは「段が計画を読んでから writer に書きを積むまで」の間（`index_local`は写真の走査とEXIFの読み取りの間、`index_drive`はDriveの一覧取得の間、`enrich`は地理院への問い合わせの間、`fetch_derivatives`は原本のダウンロードの間）。段ごとの結果:

| 段 | 割り込みで起きうること | 守り（Rust） | Python | 試験 |
| --- | --- | --- | --- | --- |
| `index_local` / `index_drive` | `UPDATE`は`mtime`・`md5`が変わった行の`lat lon loc_source`などを上書きし、`DELETE`は消えた行を消す。`favorite`/`hidden`/`rotation`は`SET`句に無いので**残る**。手動の位置は、ファイルが変わった行では上書きされ、消えた行はお気に入り・除外ごと消える | **競合では失われない**: 更新APIが計画の前でも後でも結果は同じ（ファイルから決まる値で上書き・削除）。失われるのは[inventory.md §2.3](inventory.md)の①②で、**T3.7でユーザーの決めた方針に合わせる**。それまでは現行と同じ。計画に入らない行（ファイルが変わっていない行）の手動の位置・お気に入りは残る | 同じ（Pythonは1枚ずつ読んで書くので窓は狭いが、結果は同じ） | `local::tests::計画のあとに更新apiが入っても…`、`drive::tests::index_drive_plan_survives_…` |
| `infer` | 計画（読み取り）のあとに、対象の行が手動になった・錨が手動で動かされた | `Set`/`Clear`の`UPDATE`は`loc_source IS NULL OR 'inferred'`の行にだけ効く（P2）。**T3.1で追加**: `Set`は、錨が計画のときの位置・種類（`exif`/`drive_meta`/`manual`）のままのときだけ。動かされた錨の古い位置を写さない（次のサイクルが新しい錨で付け直す） | `WHERE id=?`だけ: **手動の位置を推定で上書きし、`Clear`は取り消す**（位置が失われる） | `geo::tests::計画のあとに更新apiで直された位置…`（`Set`・`Clear`・錨の移動） |
| `enrich` | 地名の問い合わせ中に位置が直された（`place`がNULLに戻る） | `UPDATE`は`lat=? AND lon=? AND place IS NULL`（P2）。座標が変わった行には古い座標の地名を書かず、次のサイクルが新しい座標で付ける | `WHERE id=?`だけ: 古い座標の地名が新しい位置に書かれ、`place`が埋まったまま**再enrichされない**（誤った地名が残る） | `geo::tests::enrichの途中で更新apiが…` |
| `fetch_derivatives` / `local_derivatives` | 取得の途中で位置・お気に入りが変わる | 書くのは`display fetched_at`だけ（`WHERE id=?`）で、利用者の列に触れない。割り込みの影響なし。回転（T3.2）は`display`/`rotation`を書くので、取得との競合は§17.8.4 | 同じ | `drive::tests::fetch_keeps_what_the_update_api_wrote…`、`local::tests::derivativeの作成中に…` |
| `quality` | 解析の途中で位置・お気に入りが変わる | 書くのは`photo_quality_features`・`photo_quality`・`settings`だけで`photos`に書かない。影響なし | 同じ | `quality::tests::解析の途中で更新apiが…` |

割り込みの再現は、`Writer`の試験用の口（`interrupt_next_call`: 次の`call`が依頼を積む直前に1回だけ閉包を呼ぶ。`cfg(test)`）で、**段の`run`が計画を読んだあと**に更新API（`update::set_loc`・`set_flag`を本物の writer 経由で）を呼ぶ。段が書いたあとの`photos`の全列を確かめる。`enrich`は地理院のスタブの応答の中から呼ぶ。条件を外すと落ちることは確認済み（`infer`の2つの条件）。

### 17.4 writer の共有と、取り込みが無効なとき

- `AppState::writer()`は`Catalog`が持つ writer（`cli.rs`が`GEOFRAME_INGEST=1`のときだけ付ける。§11.4）の複製を返す。同期サイクルのスレッドと**同じ接続・同じキュー**。更新は`spawn_blocking`の中で`Writer::call`する。
- `None`（`GEOFRAME_INGEST=0`、またはガード（§11.1.1）で取り込みを断ったので writer が作られない）のとき、更新APIは**何も書かず**503（`書き込みが無効なので更新できません（この Rust 版は取り込みが無効です）`。値・パス・環境変数名は含めない）。この503は`catalog.db`への書き込みに限る。地図タイルのキャッシュや`.cache.json`はDBではないので、後のタスク（T3.3・T3.4）では取り込みが無効でも動かす。
- P2〜P4のガード（検証環境の印・`geoframe-server`の停止の確認）は変えていない。ガードを通らないと writer が作られず、更新APIは書かない。

### 17.5 Pythonとの差（まとめ）

§0の2行に同じ。①`i64`超の`id`は、入力の検査と writer の確認のあとで SQL を実行せずに404（Pythonは500。最大の`rowid`の行があっても更新しない）。②JSONの`NaN`・`Infinity`は400（文面が違う）。③パスの`\`は読み替えず403。④`setflag`は1回の`UPDATE … RETURNING`。⑤writerが無いと503。⑥競合の守り（`infer`・`enrich`はPythonが失う）。⑦巨大・不正な`Content-Length`はHTTP層が先に断る（§17.1.1。状態コード・本文が違うが、書き込みは起きず拒否の向きの差）。⑧**逆向き（Pythonが断るのにRustが書く）は見つかった分を直した**: 4300桁を超える`Content-Length`（Pythonの`int()`の上限。`post::content_length`が400）と、行の大きさ（要求行65536バイト超は414、ヘッダ1行65536バイト超・100個以上は431。`post::head_limits`）。残る穴は§17.1.1の最後（値の前後の空白と、hyperが捨てる重複した`Content-Length`の行の分）。⑨**CSRFの判定の値は生バイトをLatin-1として読む**（§17.1.2）。UTF-8のNBSP付き`Content-Type`と、角括弧の中がIPv4・ゾーンIDだけの`Origin`は、Pythonが断る（角括弧の例外は応答なしで接続を閉じる）のにRustが通していたので直し、差分試験（9213件）で、`Origin`の前後の空白・タブ（既知の差）以外にPythonが断るものをRustが通す差が0件であることを固定した。Rustだけが断るのは、制御文字・折り返しのHTTP層の400。許す向きの差は`Origin`の前後の空白・タブ（hyperがOWSとして取り除く）の1種類だけで、差分試験の73件・実HTTPの10件に固定した（§17.1.2。ブラウザは付けられず、付けられるクライアントは`Origin`を省けばPythonでも通るので守りは変わらない）。

### 17.6 テスト

- `tests/update_api.rs`: `tools/gen_update_fixture.py`（`app/tests/update_api_fixture.py`の167ケース）が現行`handlers.Handler`に順にPOSTした状態コード・応答のJSON・`photos`の変化（変わった行の変わった列）と、同じDBを作り直したRustのサーバーの結果が一致する。ケース: 各境界値（閉区間・範囲外・`bool`・文字列・`null`・欠落・`id` 0/負/文字列/小数）、存在しない`id`、更新する列、ルーティング（正規化・クエリ・大文字小文字・未知）、CSRFの各分岐と順序（Content-Type→Origin→Content-Length→413→JSON→オブジェクト）、64 KiBちょうどと1バイト超、BOM・不正UTF-8・末尾ゴミなど。意図した差（19件）は`known_differences`で、差が消えたら落ちる。HTTP層の境界（`http_*`、§17.1.1）のケースを含む。
- `tests/csrf_python.rs`: `tools/gen_csrf_fixture.py`（`app/tests/csrf_fixture.py`）が現行の`_csrf_ok`・`_host_ok`を実際のヘッダ解析で動かした9213件の判定と、実際のHTTPサーバーへの84件の結果。Pythonが断るものをRustが通さない（既知の差を除く。§17.1.2）、実際のTCPで状態コードと書き込みの有無が一致する（表にある差だけ）、既知の差（`Origin`の前後の空白・タブ）が表の通りであること。`src/csrf.rs`の単体テスト。
- 同ファイル（`update_api.rs`）: `content_length_boundaries_at_the_http_layer`（実際のTCP接続で§17.1.1の表の状態コード・接続の切断・書き込みの有無を固定し、Pythonの結果はフィクスチャと突き合わせる）、`an_id_beyond_i64_never_updates_the_row_with_the_largest_rowid`（`rowid`が`i64::MAX`の行があっても、それを超える`id`の`setloc`・`setflag`は404で何も書かない）。
- 同ファイル: writerなしの503（書かない・定型文・400/403は変わらない）、Hostの検査がルートより先、`Origin`とポート、`OPTIONS`が501でCORSヘッダなし、`chunked`は400、413で接続が閉じる（keep-aliveのソケットで本文を送らずに確認）、500の本文に例外が出ない、同時の更新がすべて書かれる。
- `src/update/tests.rs`: E12・E13の境界と文面、writerなし、`i64`を超える`id`が最大の`rowid`の行を更新しない回帰。`access_log.rs`: POSTのログにメソッド・ルート・状態コードと内部エラーの種類だけが出て、座標・`id`・Originの値は出ない。
- §17.3の割り込みの試験（`geo`・`local`・`drive`・`quality`の`tests`）。
- `app/tests/test_update_api_fixture.py`: フィクスチャが最新であること、検査の順序、Pythonの癖の記録。

### 17.7 回したもの

- **T3.2**: 済み（§17.8）。
- **T3.7**: 索引（`index_local`・`index_drive`）が手動の位置・お気に入り・除外を失う経路（§17.3の1行目）の方針。
- **T3.8**: 検証環境での受入（比較用Python `:8090`に同じ要求を送って応答とDBの変化を比べる）。

### 17.8 E14 `rotate`（T3.2）

状態: T3.2 実装済み（2026-10-25）。`POST /api/admin/rotate`、JSON `{id, delta}`。Driveの写真の表示用画像（derivative）を時計回りに`delta`度回して別名で保存し、`display`・`rotation`を書き換え、古いファイルを消す。実装は`rotate.rs`（流れ・DB）と`imaging`（`rotate_jpeg`・`write_atomic`・`lock_derivatives`）。入口（CSRF・ルーティング・エラー応答）は§17.1。

#### 17.8.1 検査・流れ・応答

| 順 | 処理 | 外れたとき |
| --- | --- | --- |
| 1 | `id`（E12・E13と同じ`req_id`。`i64`超の扱いも同じ） | 400 |
| 2 | `delta`は整数の90・180・270・-90だけ（`bool`・文字列・`null`・配列・欠落・小数は`delta は 90 / 180 / 270 / -90 で指定してください`） | 400 |
| 3 | writerが無い（取り込みが無効） | **ファイルに触れる前に**503（§17.4） |
| 4 | `id`が`i64`超 | SQLを実行せず404 |
| 5 | 回転どうし・取得との排他（`lock_derivatives`）を取り、行を読む（writer経由）。`id`が無い | 404（`id=… は存在しません`） |
| 6 | `source != 'drive'`か`drive_id`が空（NULL・空文字） | 400「回せるのはDriveの写真だけです」 |
| 7 | `display`が空・`derivatives/<安全な名前>`でない・無い・通常のファイルでない（リンク・ディレクトリ・FIFO） | 400「表示用の画像がまだありません」 |
| 8 | 新しい`rotation = (旧rotation（NULLは0）+ delta) mod 360`（負の剰余は非負。`45`や`1000`のような90の倍数でない値の行もそのまま足す）。名前は`derivative_name(drive_id, rotation)`（0なら`<drive_id>.jpg`に戻る）。`drive_id`が名前に使えない | 500（定型文。詳細はログにだけ） |
| 9 | 回して`.part`経由で保存（`create_new`＋`O_NOFOLLOW`、`fsync`、`rename`。`DATA_DIR/derivatives`の外に出ない）。画像が読めない・書けない | 500、何も残さない |
| 10 | DBを条件付きで更新（§17.8.4） | 409（行が消えていれば404） |
| 11 | 旧ファイルと新ファイルの`display`が違えば旧ファイルを消す（`derivatives/`直下の通常ファイルだけ） | 消せなくても成功。ログは固定の文言だけ |

応答`{ok:true, id, rotation, thumb:"/"+display}`（`display`は`derivatives/<名前>`）。エラーの文面は現行と同じ。検査の順は現行と同じ（`id`→`delta`→行→`source`/`drive_id`→`display`）で、Rustの503（3）は`delta`の検査のあと・行の検索の前（E12・E13と同じ）。

#### 17.8.2 画像

- `imaging::rotate_jpeg`: 派生画像をデコードして`manual_orientation(delta)`だけを当て、JPEG品質85・4:2:0・ハフマン表の最適化なし（P2の`derivative`と同じ設定）で書く。Pillowの`im.rotate(-delta, expand=True).save(.., "JPEG", quality=85)`に当たる。**EXIFの向きを当てない**（Pillowの`rotate`も当てない。EXIFの向きが付いたJPEGは、Pythonも向きを無視して回す）、**縮小しない**（派生画像は作ったときに2560pxに収まっている）、**ICCを変換せず付けない**。グレーはグレーのまま。出力に**EXIF・GPS・ICCを付けない**（エンコーダに渡さない。Pythonの`save`も付けない）。
- 向きは時計回り: 左上の色は右上へ移る（`+90`で四隅が`[青,赤,黄,緑]`）。寸法はPillowと完全一致（`-90`＝反時計回り、`180`は同じ寸法）。画素はデコーダ（zune-jpeg対libjpeg）・再エンコードの差で一致しない。測定（なだらかな合成画像400×260・品質92の入力、Pillow 13.0との比較）: 平均の絶対差1.4〜2.5、PSNR 34〜40 dB。Rust自身の「回した元の画素」との比較は平均の差3未満（`rotate/tests.rs`）。
- `imaging::write_atomic`: P2の`make_derivative`の書き出しを関数に分けたもの（挙動は同じ）。既存のリンク（切れたリンクも）に作らず別の名前で作り直し、出力先がリンクならそのリンク自体を置き換えて先を書き換えない。

#### 17.8.3 失敗の後始末

| 失敗 | 結果 |
| --- | --- |
| 画像が読めない・書き出せない（`.part`の作成・`fsync`・`rename`） | 自分の`.part`と新しいファイルを残さない。旧ファイルとDBは元のまま。500 |
| DBの更新が失敗（SQLiteのエラー・writerの停止） | 新しいファイルを消す（DBが今そのファイルを指しているなら消さない）。旧ファイルとDBは元のまま。500 |
| 条件が外れた（§17.8.4） | 同じ後始末。409／404 |
| 新しい名前が今の`display`と同じ（`display`と`rotation`が食い違った行） | Pythonと同じにそのファイルへ上書きする（旧ファイルは消さない）。DBの更新が失敗・条件が外れたら、読んでおいた元の内容を書き戻す |
| DB更新のあと旧ファイルを消せない | 応答は成功（Pythonの`unlink(missing_ok=True)`と同じ）。ログは「古い表示用ファイルを削除できませんでした」だけ（ファイル名・`drive_id`・`id`を出さない）。旧ファイルは孤児で残る |

#### 17.8.4 競合

writerは単一接続なので、DBへの書き込みどうしは割り込めない。ファイルとDBを別々に変える回転は、次の守りで`display`と実在するファイルの食い違いを防ぐ。

- **排他**: `imaging::lock_derivatives()`（プロセスで1つのMutex）を、回転は「行を読む」から「旧ファイルを消す」まで、取得（`fetch_derivatives`）は「derivativeを作る」から「`display`をDBに書く」まで持つ（原本のダウンロードの間は持たない）。writerの依頼はロックを取らないのでデッドロックしない。取得がロックを持つのは1枚のderivativeを作る間（デコード・縮小・保存。大きなHEICで数秒）なので、回転はその間待たされうる（管理画面の1操作が数秒遅れる程度）。
- **条件付きの更新**: `UPDATE photos SET display=?, rotation=? WHERE id=? AND display IS ? AND rotation IS ? AND md5 IS ? AND fetched_at IS ?`（読んだ時点の値。`IS`でNULLも一致）。`md5`・`fetched_at`も条件に入れるのは、`display`が一度NULLにされて取得が同じ名前（`<drive_id>.jpg`）で**変わった原本から**作り直すと、`display`・`rotation`が元に戻って見えるため（ABA）。外れたら何も書かず、新しいファイルを消して409。

| 場面 | どうなるか | 守り | 試験 |
| --- | --- | --- | --- |
| 回転が2本同時（同じ写真） | ロックで1本ずつ。累積して反映（90を6本→回転は{0,90,90,180,180,270}がそれぞれ1回、最後は180・`<drive_id>.r180.jpg`だけ） | `lock_derivatives` | `rotate::tests::同じ写真への回転が同時に来ても…`、`tests/rotate_api.rs`（実際のHTTPで同時に4本） |
| 取得が同じ写真のderivativeを書いている最中 | 取得はderivativeを作る前にロックを取るので、回転の終わりまで待つ。回転が読んだ`display`はその間に変わらない。取得の候補は`display`がNULLの行だけなので、回転が対象にできる行（`display`あり）は取得の対象外 | `lock_derivatives`（取得側） | `drive::tests::fetch_waits_for_a_rotation_in_progress_and_leaves_no_orphan`（取得が待つこと、回転が負けたあとの取り直しで`display`が実在するファイルを指し、孤児が残らないこと） |
| 同期サイクルが`display`をNULLにした直後（`infer`の取り消し・`md5`の変更） | 回転が読んだ後なら、条件が外れて409。新しいファイルは消える。旧ファイルは残り、取得が同じ名前で作り直す（`rotation`は`md5`の変更で0に戻る）。`display`が消えたファイルを指すことも、古い原本の回転済みの画像が`display`に入ることもない | 条件付きの更新 | `rotate::tests::途中でdisplayがnullになったら…`、`…md5が変わって索引が…`、`…同じdisplayに作り直されても…`（ABA） |
| 同じ名前で取得が作り直した | DBが今指しているファイルは、回転が負けても消さない | 後始末は`current display != 新しい名前`のときだけ消す | `rotate::tests::dbが今指しているファイルと新しい名前が同じなら…` |
| 行が消えた（`index_drive`） | 404。新しいファイルを消す | 条件付きの更新＋`SELECT` | `rotate::tests::途中で行が消えたら…` |
| 品質の解析が旧ファイルを読んでいる最中 | 解析は読んだ時点の`display`を特徴量の行に書く。回転で`display`が変わると行は「未解析」に戻り（`f.display=p.display`の結合）、品質はその古い結果から作られず、次のサイクルが新しいファイルを解析し直す。旧ファイルが読む前に消えていたら、失敗の行が`display`＝旧ファイルで付くが、同じく次のサイクルで解析し直される | `quality.rs`の`display`の突き合わせ（P2。変更なし） | `quality::tests::解析の途中で回転が入っても…`（読む前に消える／読んだあとに回る、の2通り） |
| 書き込みの失敗 | §17.8.3 | | `rotate::tests::dbの更新に失敗したら…`、`書き出しに失敗したら…` |

Pythonは、①同期サイクルが`display`をNULLにしたあとでも古い画像を`display`に書き戻し（取得は取り直さない）、②回転どうしの片方を失い、③無条件に`src`を消す（同じ名前で作り直されたファイルを消しうる）。**残る穴**: `index_drive`が一覧から消えた行と一緒に消すderivativeは、計画を読んだ時点の`display`のファイルなので、その直後（数ミリ秒）に回転が入ると新しいファイルが孤児で1つ残る（増え続けない。Pythonも同じ）。`md5`の変更で`rotation`が0に戻る経路が古い回転済みのファイルを残すこと（[inventory.md §2.3](inventory.md)③）は、T3.7で方針を決めるまでPythonと同じ。

#### 17.8.5 Pythonとの差（まとめ）

§0の2行に同じ。①小数の`delta`は400。②`display`がリンク・`derivatives/<安全な名前>`でない・通常のファイルでないとき400。③`drive_id`が名前に使えないとき500。④writerが無ければ503。⑤条件付きの更新で409。⑥ロックによる直列化。いずれも安全側の差か、Pythonが失う場面を守る差で、状態コード・応答・DB・ファイルがPythonと一致するのは、フィクスチャの57ケースのうち`known_differences`の5件（`id`が`i64`超・リンクの`display`・`derivatives/`の外の`display`・小数の`delta`2件）を除く52ケース。

#### 17.8.6 テスト

- `tests/rotate_api.rs`: `tools/gen_rotate_fixture.py`（`app/tests/rotate_fixture.py`）が、合成のderivative（JPEGの4色の象限、グレー、EXIF・GPS付き）を置いた現行`admin_rotate`に57ケースを順に流した結果（状態コード・応答のJSON・`photos`の変化・ファイルの増減と画像の寸法・四隅の色・EXIFの有無）と、同じDB・同じ画像を作り直したRustのサーバーの結果が一致する。ケース: 4方向の累積（90を4回で元の名前・元の向き）、`-90`・`180`・`270`、NULL・0・負・90の倍数でない`rotation`、新しい名前が今の`display`と同じ行、ローカルの写真・`drive_id`がNULL／空・`display`がNULL／ファイル無し／ディレクトリ／画像でない、`delta`の各種の不正値、`id`の各種の不正値、検査の順序、意図した差5件。`.part`が残らないことも確かめる。writerなしの503（ファイルもDBも変わらない）、同時の4本の回転、棚卸しS6（回したあと、新しいURLがE23で200・古いURLが404・写真フィードE1の`file`が新しい名前）。
- `src/rotate/tests.rs`: 検査の境界、JPEGの設定（ベースライン・4:2:0・EXIF/GPS/ICCなし・グレーは1成分）、画素の近さ、失敗の後始末（`rename`の失敗・書き込めないディレクトリ・DBのトリガーによる失敗・同じ名前の書き戻し・旧ファイルを消せない）、競合（§17.8.4）、リンク（`display`・`derivatives/`・新しい名前の位置。外のファイルが読まれず消されず作られない）、`drive_id`の不正。旧ファイルを消せない試験のログは固定の文言だけで、ファイル名を含まない。
- `drive::tests`（取り直しで回転が同じ名前・同じ向きになる、取得がロックで待つ）、`quality::tests`（解析の途中の回転）。`app/tests/test_rotate_fixture.py`: フィクスチャが最新であること、Pythonの癖の記録（四隅の色の向き、EXIFの向きを当てないこと、リンクの`display`が辿られること）。
- 割り込みは`rotate::rotate_with`の`between`（新しいファイルを書いたあと・DBを更新する前に1回呼ぶ閉包）。条件（`md5`・`fetched_at`）を外す、`current display`の確認を外す、取得のロックを外すと、それぞれの試験が落ちることを確認した。

#### 17.8.7 回したもの

- **T3.7**: 回転済みの写真が`md5`の変更で`rotation`0に戻り、古い回転済みのファイルが残る経路（[inventory.md §2.3](inventory.md)③）。
- **T3.8**: 検証環境での回転の受入（比較用Python `:8090`に同じ要求を送る）。

## 18. 地名検索（P3 / T3.3）

状態: T3.3 実装済み（2026-10-25）。`GET /api/admin/geocode?q=…`（[inventory.md](inventory.md) E4）。管理画面の手動位置指定が「地名で近くまで飛ぶ」ために使う。実装は`geocode.rs`（検索・変換・1秒1件）、`server.rs`の`api_admin_geocode`（ルーティング・同時数・応答）、`cache.rs`（メモリだけのモード）。位置候補の座標化（P4）も同じ`Geocoder::nominatim_search`を国コードつきで使う。

### 18.1 ルーティングとクエリ

- **生のパスの前方一致**（`/api/admin/geocode`で始まれば`geocodeXYZ`も当たる。先頭の`//`は畳んでから判定）。`status`・`photos`と同じ。`HEAD`は403（静的のホワイトリストに当たらない）。`POST`は更新ルートではないので403。順序は現行の`do_GET`と同じ（`/tiles/` → `/api/v1/photos` → `status` → `photos` → `geocode`）。
- `q`は`parse_qs`の最初の値（`pyquery::parse_qs_first`。空の値は無かったことになる。`+`は空白、`%XX`を戻し、不正なUTF-8はU+FFFD）。無ければ空文字。
- 検索語は前後の空白を落とし（`pyquery::py_strip` = Pythonの`str.strip()`。Rustの`trim`に`\x1c`〜`\x1f`を足したもの。全コードポイントをフィクスチャの`str.isspace`と突き合わせた）、**コードポイントで2文字未満**なら通信せずに`{"ok":true,"items":[]}`。
- 応答は`Content-Type: application/json; charset=utf-8`・`Cache-Control: no-store`。失敗は空の配列で握りつぶし、応答に理由を載せない。

### 18.2 検索の流れ（Pythonの`geocode_search`と同じ）

| 段 | 内容 |
| --- | --- |
| 1 | 地理院の地名検索（`https://msearch.gsi.go.jp/address-search/AddressSearch?q=<quote(q)>`）を**毎回**引く。結果はキャッシュしない |
| 2 | 先頭の**10件**を見る（使えない要素を飛ばした後ろは繰り上がらない）。座標`[経度, 緯度]`が数値（JSONの真偽値もPythonの`int`として通る）で2つ並んだものを`{title, lat, lon, source:"gsi"}`にする。**座標の範囲は見ない**。`title`は前後の空白を落とし、空・無し・偽の値なら検索語 |
| 3 | 要素・`geometry`・`properties`が**真の非オブジェクト**、または`title`が**真の非文字列**（数・配列・`true`）だと、Pythonは`.get`・`.strip`が例外になって握りつぶし、**地理院の結果全体を空にする**（それまでに集めた要素も捨てる）。Rustも同じ（座標が使えない要素は、`geometry`・`properties`の型を見る前に飛ばされるので毒にならない）。偽の値（`null`・`0`・`""`・`[]`・`{}`）は`{}`扱い |
| 4 | 結果が空（0件・失敗・毒）なら、Nominatimを引く（下） |
| 5 | Nominatimの結果は、`lat`・`lon`を`float()`に通し（文字列・数・真偽値。`null`・配列・オブジェクト・欠落・読めない文字列はその要素だけ飛ばす）、**緯度−90〜90・経度−180〜180**に収まるものだけを`{title: display_name, lat, lon, source:"osm"}`にして**先頭の10件**（有効なものを数える） |

`float()`の寛容さ（前後の空白、数字の間の`_`、ASCII以外の10進数字、`inf`・`nan`、`1.`・`.5`・指数）は`pyquery::py_float`が再現する（`str.isspace`のASCIIの`\x1c`〜`\x1f`は空白にならない、全角の`．`は不可、など。フィクスチャの`float_samples`で突き合わせた）。URLのエンコードは`py_quote`（`urllib.parse.quote`）。

### 18.3 Nominatim は全体で1秒1件

- `Geocoder`の`nominatim_last`（前の通信が**終わった**時刻）のMutexを、**待ちから通信・キャッシュへの保存まで持ち続ける**（Pythonの`_nominatim_lock`と同じ）。複数の要求が同時に来ても、上流への要求は直列になり、前の通信が終わってから`NOMINATIM_MIN_INTERVAL`（1秒）たつまで待つ。**通信が失敗しても終わった時刻を記録する**（Pythonの`finally`）。最初の1件は待たない。キャッシュに当たれば待たない・時刻も変わらない。
- 時計は`Clock`（`now`・`sleep`）。本番は実時間（`std::thread::sleep`）、試験は偽の時計（待たずに進め、待ちを記録する）。
- 待つ間にロックを持つので、**ブロッキングのスレッドで動かす**（`spawn_blocking`）。tokioのワーカーは塞がない。同時に処理する検索は**8件**（`MAX_IN_FLIGHT`）で、空きが無い要求は待たせずに**503**（定型の本文`{"ok":false,"error":"地名検索が混み合っています。…"}`。管理画面は`!d.ok`を「見つかりませんでした」と表示する）。2文字未満の語は空きを使わない。Pythonは上限なしに待たせる。最悪の待ちは、Nominatimの1件が最長8秒＋待ち1秒なので8×9秒ほど。
- 待っている間に別の要求が同じ語をキャッシュに入れたら、待ったあとに**キャッシュを見直して**通信しない（Pythonは見直さず、同じ語を同時に2回検索すると2回問い合わせる）。キャッシュへの保存はロックの中で済ませる。
- クライアントが切断しても、処理は最後まで進む（キャッシュに入り、空きは終わったときに戻る）。

### 18.4 `.cache.json`（メモリキャッシュ）

- キー`geocode_osm:<検索語>`（国コードを渡したときは`geocode_osm:<国コード>:<検索語>`）。検索語は`strip`済みでURLエンコード前。値は`{title, lat, lon, source}`の配列で、**0件も入れる**。通信の失敗・JSONでない・配列でない応答は入れない。**配列でない既存の値はミス**として取り直して上書きする。配列なら中身を問わずそのまま返す（先頭10件に切らない）。`geo:`・`muni_table`とは別のキーで、起動時の旧キー破棄（`purge_legacy`）は`geocode_osm:`を消さない。
- **取り込みが有効（`GEOFRAME_INGEST=1`）**: `serve`は`ingest.cache`（同期サイクルの`enrich`と同じ`Arc<Cache>`）をそのまま使う。`Cache`の状態はMutexで守られ、`get`・`put`は短い。保存は`cache-saver`（変更があれば最短60秒間隔）と停止時の最後の保存が行う（§11.5）。地名検索が保存の最中に`put`しても、スナップショットはロックの下で取るので失われない。
- **取り込みが無効（`GEOFRAME_INGEST=0`。既定）**: `serve`は`.cache.json`を**読んでメモリに入れるだけ**で、**ディスクへは書かない**（`Cache::load_memory_only`。`put`はメモリに入り、保存・`purge_legacy`・`.cache.json.tmp`の作成は一切しない）。同じ語を何度もNominatimに問い合わせないための、プロセスの中だけの記憶で、**再起動すると消える**。Pythonは常に保存スレッドが書くが、本番の書き手がP5の切替までPythonで、Rustの`serve`は`.cache.json`に触れない方針（§11.1）に合わせた。ファイルが無い・壊れているときは空から始める。
- `AppState`の既定は、ファイルを読まない空のメモリだけのキャッシュ（試験の`AppState::new`が実ファイルに触れないように）。`cli`の`serve`が実際のキャッシュに差し替える。

### 18.5 通信

- クライアントは`ureq`（`net::agent()`の複製）。リクエストごとに**User-Agent: `GeoFrame/0.2`**（Pythonの`http_json`と同じ。P2の他の通信は`geoframe/<版>`）、**全体で8秒**（Pythonの`timeout=8`は1回の読み書きごと）。リダイレクトは`ureq`の既定（10回）。状態コードが2xx以外・本文がUTF-8でない・JSONでない（BOMつきを含む）・本文が**1 MiB**を超える、は失敗。Pythonは本文の上限が無い。`ureq`の上限は「ちょうど」でも超過になるので`+1`して、1 MiBちょうどまでを読む。
- 接続先は定数（`geocode::Endpoints::production`）。Pythonと同じく設定では変えられない。試験のビルドだけ、ループバックのスタブへ向けられる（`Endpoints::stub`。配布するバイナリにコードが無い。P2の`GsiEndpoints::stub`と同じ流儀）。**結合テスト（`tests/`）は本番の接続先に向いたまま**なので、`q`は1文字以下か`q`を付けない要求だけを送る（通信のある試験は`geocode/tests.rs`）。
- 本番の`Nominatim`は、利用規約（1秒1件・識別できるUser-Agent・結果のキャッシュ）が前提。User-Agentに連絡先は入っていない（Pythonと同じ`GeoFrame/0.2`）。公開サーバーに識別されないと遮断されうるので、本番で使う前に`GEOFRAME_TILE_UA`と同様に連絡先入りにするか決める（T3.8の確認後。今は変えない）。

### 18.6 ログ

失敗は`[geo] 地名検索失敗（地理院）: <種類>`・`（Nominatim）`と、地理院の応答の形が想定外だったときの1行だけ。種類は`net::Failure`（`dns`・`connect`・`tls`・`timeout`・`disconnected`・`body_unreadable`・`other`）、`http <状態コード>`、`bad_response`（UTF-8でない・JSONでない・大きすぎる）。**検索語・地名・座標・応答の中身・URLは出さない**（Pythonは`q=`を出す）。アクセスログはメソッド・ルート（`/api/admin/geocode`）・状態コード・時間だけ。試験は、検索語・地名・座標・URLを含む失敗の場面で、出力がそれらを含まないことを確かめる（`geocode::tests::failure_logs_never_contain_…`、`tests/access_log.rs`）。

### 18.7 `/api/admin/`配下の`GET`の整理（T3.1の残り）

T3.1は更新ルート（`setloc`・`setflag`）への`GET`をP1の503のままにしていた。現行は`do_GET`に`status`・`photos`・`geocode`の3つ以外の`/api/admin/`の分岐が無く、静的配信のホワイトリストで**403**になる（`setloc`・`setflag`・`rotate`、`/api/admin/`、`/api/admin/Status`、`%73tatus`のようなエンコード、`..`を含む形を現行で確かめた）。T3.3で503の接頭辞（`/api/admin/`）を外し、同じ静的配信の判定に回した。**P4に`/api/admin/`配下の`GET`は無い**（AI検索は`/api/ai/`、旅行ブログは`/api/blog/`）ので、今後`/api/admin/`で503を返すルートは無い。`/api/ai/`・`/api/blog/`・`/tiles/`の`GET`は引き続き503（T3.4・T3.5・P4で実装）。

### 18.8 Pythonとの差（まとめ）

§0の1行に同じ。①本文は1 MiBまで。②同時に8件まで、超えたら503。③待つ間に同じ語がキャッシュされたら通信しない。④`NaN`・`Infinity`・f64に収まらない数は本文全体が不正（Pythonは読む）。⑤`display_name`が文字列でないときは検索語（Pythonは`str()`）。⑥ログに検索語を出さない。⑦取り込みが無効なら`.cache.json`に書かない。⑧応答のキーの並びが辞書順。それ以外（流れ・URL・User-Agent・件数・値・キャッシュのキーと保存の条件・1秒1件の待ち）はフィクスチャで一致を確かめた。

### 18.9 テスト

- `src/geocode/tests.rs`:
  - `python_fixture_*`: `tools/gen_geocode_fixture.py`（`app/tests/geocode_fixture.py`の32ケース・152ステップ）が、現行`handlers.Handler`にGETを順に流し、`urllib.request.urlopen`と`geo.time`を合成の応答・偽の時計に差し替えて記録した、状態コード・応答のJSON・上流への要求（種類・クエリ文字列・User-Agent・タイムアウト）の順・`time.sleep`の引数・メモリキャッシュの全体と、Rustが同じ応答を返すループバックのスタブ・偽の時計で流した結果が一致する。ケース: 検索語の取り出しと2文字未満（空白・`\x1c`〜`\x1f`・全角・NBSP・ゼロ幅スペース）、`q`の重複・`+`・`%XX`・不正なUTF-8・前方一致・`//`、URLのエンコード、地理院の値・毒要素・先頭10件・非リスト・失敗13種（404・500・429・切断・JSONでない・空・不正なUTF-8・BOM・途中で切れたJSON・204）、Nominatimの値・飛ばす要素・`float()`の寛容さ・先頭10件・失敗とキャッシュしないもの、キャッシュの再利用・Pythonが書いた値の扱い・キー、1秒1件の待ちの値（通信中の時間・失敗も待ちを起こす・キャッシュと地理院は待たない）、国コード（正規化・キー・URL）。あわせて、更新ルートや`..`を含む形への`GET`が403であること（21件）。
  - `python_fixture_small_functions_match`: `float()`・`urllib.parse.quote`・`str.isspace`（全コードポイント）の出力と`py_float`・`py_quote`・`py_strip`。`python_fixture_constants_match`: User-Agent・タイムアウト・件数・間隔・本番の接続先。
  - 差・Rustだけの性質: 本文の上限、`NaN`などで本文全体が不正、`display_name`が文字列でない、**同時の検索でNominatimへの要求が1秒に1件を超えない**（6スレッドから直接・5本のHTTPで。偽の時計で各要求の受信時刻と待ちを記録して確かめる。実時間に頼らない）、同じ語の同時検索が1回の問い合わせで済む、同時数の上限（応答を止めて2本目が503、2文字未満は答える、終われば空きが戻る）、実時計の下限、ログに検索語などが出ない、`.cache.json`（取り込み無効: ファイルを変えず・`.tmp`も作らずメモリのキャッシュで済む、有効: 同期サイクルが`geo:`を書き続けている間でも互いの更新を失わず保存される）。
- `src/cache.rs`・`src/pyquery.rs`: メモリだけのモード。`tests/admin_api.rs`・`access_log.rs`・`static_files.rs`・`update_api.rs`: `/api/admin/`の残りの`GET`が403、`geocode`が`q`なし・1文字で空の200（通信しない）。
- `app/tests/test_geocode_fixture.py`: フィクスチャが最新であること、Pythonの癖（毒要素で地理院の結果全体が空、先頭10件を先に切る、待ちの値、キー）の記録。
- 確認した変異: `py_strip`を`trim`にする、Nominatimの時刻を通信の前に記録する、のそれぞれで試験が落ちる。

### 18.10 回したもの

- **T3.8**: 検証環境での実際の通信の確認（地理院・Nominatimへの実通信、比較用Python `:8090`に同じ要求を送る）。取り込みが有効・無効の両方で`.cache.json`の挙動を見る。
- 本番で使う前: Nominatimへ送るUser-Agentの連絡先（§18.5）。
