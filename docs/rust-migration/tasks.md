# Rust単一バイナリ化 タスク

- 要求: [Issue #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 設計: [設計案](../architecture/rust-api-and-ai-providers-proposal.md) / 計画: [plan.md](plan.md)
- P0=PR #57、P1=#58、P2=#59、P3=#60、P4=#61はsquash merge済み。P4にはT3.10の補足を含む。P4の後の補足（T4.10）はPR #62。
- PRはフェーズごとに1つ。P2以降のタスクは、そのフェーズの開始時に細かく分け直す。
- 種類: `ops`は自分で行う運用作業、`quick`/`full`はビルドのスキルに任せる実装。

AC番号はIssue #54の受入条件の順（AC1 公開契約・永続データ、AC2 E2E、AC3 停止・lease中の動作と再試行、AC4 ベクトル空間、AC5 単一バイナリ・CI・systemd・依存、AC6 契約型・互換テスト、AC7 秘密・位置・写真を載せない）。

## P0: 棚卸しと技術PoC（PR #57、2026-10-24 マージ）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T0.1 | 検証環境の用意: lxc-geoframeの旧版退避、SSH、本番データのコピー、比較用Python（`:8090`、AI・Drive無効） | AC1 | - | ops | 完了 | - | - | 比較用Python（main e0f15fe、`geoframe-pyref.service`）の`/api/v1/photos`は本番と同じ3,065件 |
| T0.2 | 棚卸し`docs/rust-migration/inventory.md`: エンドポイント・DBテーブルと書き込み元・バックグラウンド処理・設定・E2Eシナリオ、各フェーズへの割当 | AC2 | - | quick | 完了 | 66ce058 | a0c2fca..fcea60b | レビュー（Opus）: 必須0、推奨4・参考3を反映（「ピアソン」は誤記でなく指摘不要） |
| T0.3 | Cargo workspaceの骨組み（`crates/geoframe`）、CIにfmt・clippy・test、musl静的ビルドの可否と検証環境での起動確認 | AC5 | - | full | 完了 | 8809e31 | 06df8ad | musl静的（static-pie, 約4MB）で検証環境動作。レビュー（Sol）: 必須0。推奨「WAL DBの読み取りテスト」はT1.1へ回す |
| T0.4 | 画像PoC: 実写真約100枚でデコード・EXIF向き・2560px・JPEG出力・HEICの計測、依存ライブラリの一覧。結果を文書へ（数値のみ） | AC5, AC7 | T0.3 | full | 完了 | 0f97acb | 912bb60..14152cf | 結論: HEICは純Rustのzune-heicでmusl静的のまま扱える。レビュー（Solが利用上限のためOpusで代替）: 必須0、推奨7・参考7を反映。CIのYAML構文エラー（ジョブ名のコロン）は管理者が修正。ユーザー指示（2026-10-24）でDriveは使わずローカルのファイルを使う: 検証環境の本番コピーのderivatives（JPEG）と、それを変換したHEIC。実機HEIC固有の問題（グリッド・HDR等）はP2で確認 |
| T0.5 | `/api/v1/photos`のserde型・JSON Schema・合成DBの契約テスト | AC6, AC7 | T0.3 | full | 完了 | 79d45d9 | e8598d0..6badbde | レビュー（Sol）2回: 1回目 必須2（実在地名、Pythonの`$`）・推奨2・参考1をすべて反映、2回目 指摘なし。本番コピーに`taken_at` NULLの行は無い |
| T0.6 | 全体レビューの指摘の反映: miharashiとの共有の整理、設計案レビューの記録、未決事項の更新、PoC crateの扱い | AC5, AC6 | T0.1〜T0.5 | quick | 完了 | 51fc0b6 | 51fc0b6..d3f0ac4＋管理者の文書修正 | P0全体レビュー（要求との突き合わせ、Solが利用上限のためOpusで代替）2回: 1回目 必須1・推奨6、2回目 必須0（推奨2・参考2を管理者が反映、P1受入に実クライアント確認を明記）。security-review: High/Medium 0、AC7でIPの記載を指摘→51fc0b6で削除 |

## P1: Rust API基盤（PR #58、2026-10-24 マージ）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1.1 | 設定（`GEOFRAME_*`）、SQLite読み取り層、ログ方針、ヘルス確認 | AC5, AC7 | P0 | full | 完了 | c41c4a9 | dc13801..52c7d8f | 検証環境の本番コピーでスキーマ検査を通過・/healthz 200（管理者確認）。レビュー（Solが利用上限のためOpusで代替）: 必須0、推奨4・参考を反映（パニック文言の固定、開きっぱなしのトランザクションのROLLBACK、/healthz専用接続、順番待ち10秒上限） |
| T1.2 | `GET /api/v1/photos`（除外・並び・タイトル・旅番号） | AC1, AC6 | T1.1 | full | 完了 | fae0fe6 | 9ae7fd0..5d8acc9 | 検証環境の本番コピー3,065件でPythonとJSON完全一致（管理者確認）。レビュー（Sol）: 必須0。推奨「全角数字の時刻」は文書に既知の差として記録しP2のインデクサで解消、参考「iterdumpの書式依存」は見送り |
| T1.3 | 静的配信（ホワイトリスト・ドットファイル・トラバーサル・HEAD）、Host検査、管理画面ページ、静的ファイルの埋め込み | AC1, AC5 | T1.1 | full | 完了 | 48c9aaa | 8be867f..1d47c29 | 検証環境の本番コピーで表示用画像40枚がPythonとバイト一致、管理画面・ブロック対象も同じ応答（管理者確認）。レビュー（Sol）2回: 1回目 必須1（遅いクライアントでFD枯渇→同時配信32・停滞30秒・無通信60秒・接続256）・推奨2（Host重複→400、絶対形式の拒否を全ルートへ）を反映、2回目 必須0・推奨2はT1.4で反映 |
| T1.4 | 管理画面の読み取りAPI（`/api/admin/status`・`/api/admin/photos`。棚卸しE2・E3） | AC1 | T1.1 | full | 完了 | 03e4e33 | f484997 | 検証環境の本番コピーでstatus（時刻・ディスク除く）とphotos 64通りがPythonと完全一致（管理者確認。比較用PythonはEMBED_BACKEND未設定で本番と違う→T1.5でそろえる）。T1.3の推奨2件も反映。レビュー（Sol）: 指摘なし |
| T1.5 | systemd unit、検証環境への配布スクリプト、Pythonとの全件比較スクリプト、miharashiの実プロセスでの取得確認 | AC1, AC5, AC6 | T1.2, T1.3 | full | 完了 | 56e0665 | 355ba86..2a0c7e5 | 検証環境で常駐（geoframe-rs.service）。全件比較の想定外の差0（写真一覧3,065件を型まで一致、画像3,065件バイト一致、管理API 222通り）。miharashiの実バイナリ（png --renderer cpu 60/60）とFeedで取得・デコード失敗0。障害試験・ロールバック・誤配布の拒否を確認。レビュー（Sol）4回: 必須 4→3→1→0（配布スクリプトの失敗時の復元、比較の判定の厳密化、本番への誤配布の防止）、修正3回 |
| T1.6 | P1全体レビューの推奨の反映: 書き込み層の担当フェーズ（P2で取り込み用writer、P3で更新API）の統一、Pythonとの差の索引、miharashi確認ツールの判定 | AC1, AC6 | T1.1〜T1.5 | quick | 完了 | 3d29b2e | e745ed3..22903d3 | P1全体レビュー（要求との突き合わせ、Sol）: 必須0・推奨3。security-review: High/Medium 0、参考1（ツールの.gitignore）は3d29b2eで対応。レビュー（Opus）: 必須0、推奨2（確認ツールは`Feed::spawn`の経路で`photo:`を出さないので数える仕組みを外しRetryingで判定、`lon` NULLの行は本番コピーで差0）・参考4を反映。直したツールで検証環境の定常200枚・停止再開300枚とも終了コード0（管理者確認） |

## P2: 写真取り込み（PR #59、2026-10-25 マージ）

開始時の調査（件数のみ）: 本番コピーの`photos`は全6,535行が`source='drive'`で、ローカルの`photos/`は空。HEICは1,735行（すべてDrive）。国外（日本のおおよその矩形の外）で位置のある1,267行は`place`がすべて空で、オフライン逆ジオコーダは本番に入っていない（Rustも入れず「空で確定」にする）。

P2で守ること（各タスクの委任文に入れる）:
- **本番コピーでの冪等性が受入の関門**: 取り込み済みの本番コピーに対してRustの1サイクルを回しても、行の変更・derivativeの再取得はほぼ0であること。確かめるため、同期サイクルに書き込まない計画だけのモード（`--dry-run`）を作る。
- **M1の罠**（[inventory.md §2.4](inventory.md)）: 空DBから作るスキーマにはALTERで足された列をすべて含める。既存DBへの移行は何もしないことを示す（`place=NULL`で全行の再enrichを起こさない）。
- 時刻の形式（`taken_at`は`YYYY-MM-DD HH:MM`、`indexed_at`/`fetched_at`はオフセット無し、`analyzed_at`は`+09:00`付き）、差分はnaiveな日時で取る（[inventory.md §6.1](inventory.md)）。
- ログにファイル名・座標・Driveのファイル名・例外文字列を出さない。`.cache.json`は位置を含むのでリポジトリ・テストに入れない（[inventory.md §6.3](inventory.md)）。
- テストではDrive・GSI・Nominatimをスタブにする。実際の通信は検証環境の受入だけ。
- 手動GPS・お気に入りが消える経路（[inventory.md §2.3](inventory.md)）はP2ではPythonと同じにし、Pythonとの差の候補として[server.md §0](server.md)に載せる。直すかはP3（更新APIのフェーズ）で決める。
- writerを本番の`catalog.db`へ向けない: 配布スクリプトの検証環境の印と本番への誤配布の拒否を維持する。

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T2.1 | 書き込みの土台: musl静的のまま外向きHTTPS（rustls・DNS）とサービスアカウントのJWT（RS256）が動くことの確認（検証環境からGSIへ1件）、writer（単一接続・専用スレッド・チャネル）、空DBからのスキーマ作成と既存DBへの移行（M0〜M7相当、何もしないことのテスト）、同期サイクルの骨組み（`GEOFRAME_DRIVE_POLL_MIN`、段ごとの失敗の隔離、`--dry-run`）、起動時の掃除（`tmp/`・`.dl_*`）、`.cache.json`の読み込み・旧キー破棄・保存（`geo:`を引き継ぐ）、unitの`ReadOnlyPaths`の解除 | AC1, AC5 | P1 | full | 完了 | fdde8e1 | c7b25e6..956306c | HTTPSはureq＋rustls（証明書埋め込み）、JWTはringで、musl静的のまま検証環境からGSIへ成功。`GEOFRAME_INGEST`（既定0）で取り込みを有効にし、P5まではガード（検証環境の印と`geoframe-server`の`ActiveState`がinactive/failed）を満たさなければ書く前に止まる。本番コピーで`INGEST=1`起動後も`.dump`のSHA-256が一致（移行は何もしない）、全件比較の差0。レビュー（Sol）3回: 必須 2→1→0（本番への誤指定の拒否、ROLLBACK失敗後の接続の破棄、`not-found/active`の拒否）、推奨・参考は修正3回で反映（停止時の保存とロックの順序、状態行の重複の拒否など）だめならP2の通信の作りを変える（[build.md](build.md)の「DNSを入れる段階で見直す」） |
| T2.2 | 画像処理の本実装: PoCからderivative生成（EXIFの向き→手動回転→長辺2560→JPEG q85、`.part`経由、回転後は別名）、EXIF（撮影時刻・GPS）、HEIC（zune-heic）を`crates/geoframe`へ移し、`crates/geoframe-imgpoc`とCIのPoCジョブを消す。実機iPhoneのHEIC（グリッド・HDR等）の確認 | AC5, AC7 | T2.1 | full | 完了 | 57714f2 | 63b0594..edcb075 | レビュー: 1回目（Sol）必須3（HEICの宣言長での巨大確保、`.part`のリンク追従、XMPだけの向き）・推奨4をすべて反映。2回目はSolとOpusの子タスクがどちらも安全確認で止まった（不正なファイルの検査を扱うため）ので、管理者（実装していないOpus）が`heic_guard.rs`と`.part`の作り方を読んで確認し、必須0。CI通過。EXIFはPillowの読み方に合わせて自前（Pythonと43ケース突き合わせ）、縮小寸法はPillowと170件一致、本番コピー由来のJPEG 50・HEIC 8枚で寸法不一致0・EXIF/GPS残り0。実機iPhoneのHEICはDrive鍵が無いためT2.4へ。実機HEICはDriveにしかない（1,735行）。Drive鍵の扱い次第で確認の方法が変わる |
| T2.3 | ローカルの索引: `index_local`（追加・更新・削除、`gdrive`を含むパスの除外）、`local_derivatives`、`--organize` | AC2 | T2.1, T2.2 | full | 完了 | 8d8c95b | 0915a9c..b0160e9 | 棚卸しS1。レビュー（Sol）3回: 必須 4→1→0（dry-runの削除の見落とし、停止後の削除、作ってから検査、organizeのリンク残り、起動時の掃除がリンク先を消す）、推奨（dry-runは写しに計画を当てて後続段を数える、写しの期限）も反映。合成の木でPythonと`photos`の全列・id一致（fixture）、検証環境で比較用Pythonと14行全列一致、Drive行は不変。T2.5までは`organize`が推定対象の写真を移さない（`held_back_until_infer`） |
| T2.4 | Driveの索引: 認証（`drive.readonly`）、フォルダの解決（名前かID）、`files.list`のページ送りと`imageMediaMetadata`、追加・更新・削除の規則（Pythonと同じ）、`fetch_derivatives`（`tmp/.dl_*`経由、失敗でも原本を残さない、`GEOFRAME_FETCH_PER_CYCLE`）。スタブのDrive APIでのテスト | AC2, AC7 | T2.1, T2.2 | full | 完了 | 0baf711 | 4d3a3e6..5f1be84 | レビュー（Sol）3回: 必須 1→2→1（通信の障害を写真の失敗に数えない: 流量制限・接続の時間切れ・ヘッダー前の切断・403の理由の本文が読めない）、推奨6（大量削除の保留`--allow-mass-delete`、原本の削除の確認、Debugの伏せ、手順書の修正）を反映。3回目の必須は5f1be84で直し、管理者が差分を確認。スタブのDriveでPythonの`index_drive`・`fetch_derivatives`と全列一致（fixture）。失敗が続く写真はプロセス内の記録で飛ばす（受入②、ローカルにも適用）。一覧0件では削除しない（Pythonは全行削除）。本物のDriveでの確認（受入①・冪等性・実機HEIC）は**T3.10で完了**（[drive-acceptance.md](drive-acceptance.md)）。棚卸しS2。本番コピーでの冪等性（再取得0）を`--dry-run`で確かめる。**受入条件（T2.2のレビューから）**: ①Driveの原本の形式別の成功件数と失敗の分類をPythonと比べる（T2.2で使ったJPEG 50・HEIC 8枚はderivativeからの再生成で、原本の互換性を裏付けない）、②失敗が続く写真があっても後続の写真を取得できることをスタブで確かめる（Pythonと同じ`LIMIT`の取得待ちSQLだと、恒久的に失敗する写真が先頭の枠を埋めうる）、③`tmp/.dl_*`は`create_new`＋`O_NOFOLLOW`で作る。詳細は[server.md §12.10](server.md) |
| T2.5 | 位置の推定と地名: `infer_locations`（段階窓・`GEOFRAME_INFER_OFFSET_MIN`・錨が消えたときの取り消し）、`enrich_rows`（GSIの逆ジオ・標高・`muni.js`、1サイクル40件、0.2秒待ち、応答が空なら空で確定、国外は空で確定） | AC2 | T2.1 | full | 完了 | a2e7341 | a4b36c7..93783d5 | レビュー（Sol）3回: 必須 1→1→0（同じ丸めキーで国内の行が空の地名で確定する＝処理順・障害のあいだの追い越し）、推奨（再試行と座標ごとの障害、段の上限2分、待ちと件数、strptimeの空白埋め）を反映。3回目の推奨（時間切れを記憶しないことのテストの補強）はT2.7で。棚卸しS3。合成の行でPythonの`infer_locations`・`enrich_rows`と全列・地理院への要求順・キャッシュのキーが一致（fixture）。本番コピーのdry-runで推定・取り消し・enrichとも0、推定2,162行と地名13行を消して1サイクル回すと`.dump`が元と完全一致。`organize`の`held_back_until_infer`を廃止 |
| T2.6 | 簡易品質: 特徴量（SHA-256・pHash・鮮明度・露出）、孤児の削除、全員分そろってからの再グルーピング（完全重複・3分以内かつpHash距離12以下）、`BEGIN IMMEDIATE`での全差し替え、`--quality-scan` | AC2 | T2.1, T2.2 | full | 完了 | 460626f | eb54361..5f11220 | レビュー（Sol）2回: 必須0。推奨（同じSHAでそろえるのは指標だけでpHashは行ごと＝配信の抑制をPythonと同じに、Pythonへの切り戻しの試験`switchbacks`と手順）を反映、2回目の推奨（解析し直す切り戻しの説明）は管理者が文書を修正。方針: Pythonの特徴量の行を有効として使い、新しい写真だけRustで解析（全部やり直すと約44時間・7枚の組が変わる）。グルーピングはPythonと20ケースで全列一致（fixture）、本番コピーで解析0・再構築559行全一致、1サイクル後も`.dump`不変・3,065件。棚卸しS7。旧スコアとの一致は不要、同じ入力で同じ結果 |
| T2.7 | 検証環境での受入: 本番コピーでの`--dry-run`（変更ほぼ0）と実サイクル、同じ写真を比較用PythonとRustへ入れて`photos`の列ごとの比較、棚卸しS1・S2・S3・S7、miharashiでの表示の継続、結果を`p2-acceptance.md`へ | AC1, AC2, AC5 | T2.3〜T2.6 | full | 完了 | 823f237 | 35d677d..67360d4 | [p2-acceptance.md](p2-acceptance.md)。本番コピーのdry-runは全段0、実サイクル2回で`.dump`不変。合成21枚で比較用Pythonと`photos`全26列・`photo_quality`567行・配信件数が一致。S1・S3・S7通過、miharashi定常200・停止再開300とも終了コード0、全件比較の想定外の差0。本物のDriveでの確認（S2・§14.11）はT3.10で完了（[drive-acceptance.md](drive-acceptance.md)の確認範囲）。T2.5のテスト補強も実施 |
| T2.8 | P2全体レビューの推奨の反映: dry-runの対象を検証環境に統一（本番DBに向けない）、受入の「全段0」の限定、本物のDrive確認をP5の関門に、一次データが消える経路のP3への引き継ぎ、ICCの説明、unitに`Conflicts=geoframe-server.service`、文書のユーザー名 | AC1, AC2, AC7 | T2.1〜T2.7 | quick | 完了 | 0d2750c | 15c306b | P2全体レビュー（要求との突き合わせ、Sol）: 必須1・推奨2・参考1。security-review（Opus）: High/Medium 0、Low 2（Python停止の確認が起動時だけ、文書のユーザー名） |

## P3: 管理機能

開始時の整理: 管理画面・AI写真検索・旅行ブログの画面（`app/static/`）はP1でバイナリに埋め込み済みで、読み取りAPI（E1〜E3）もP1で済んでいる。P3で足すのは、外部通信のあるGET（E4 地名検索、E11 地図タイル）、旅行ブログの読み取り（E7〜E10）、更新API（E12〜E16）と、その横断の仕組み（CSRF対策・エラー応答）。AI写真検索の画面のうちP3で動くのはE13（お気に入り・除外）だけで、検索そのもの（E5・E6）はP4。番号は[inventory.md §1.1](inventory.md)。

P3で守ること（各タスクの委任文に入れる）:
- 更新APIはP2のwriter（単一接続・専用スレッド）だけを通して書く。writerが動いていない（`GEOFRAME_INGEST=0`、またはガードで断られた）ときは、更新APIは書かずに定型文の503を返す。P2〜P4のガード（[server.md §11.1.1](server.md)）を弱めない。
- 同期サイクルの段と更新APIが同時に動いても、利用者の変更（手動の位置・お気に入り・除外・回転）が失われないこと。段が計画を読んでから書くまでの間に更新APIが割り込む場合を試験で確かめる。
- CSRF対策とエラー応答は今のPythonと同じ順序・同じ状態コード（[inventory.md §1.2](inventory.md)）。CORSヘッダを返さず、`OPTIONS`は501。未知のPOSTパスは403、`/api/blog/`配下の未知パスだけ404。500の本文に例外文字列を出さない。
- 外部への通信（地理院・Nominatim・地図タイル）はテストではスタブ。実際の通信は検証環境の受入だけ。User-Agent（`GEOFRAME_TILE_UA`）・流量の上限・同時数は今と同じ。
- ログに写真のファイル名・座標・地名・検索語・旅程の中身を出さない（[inventory.md §6.3](inventory.md)）。旅行ブログの候補（E9）は粗い地名だけを返し、正確な位置を返さない。
- Pythonと同じ要求を比較用Python（検証環境の`:8090`）にも送り、応答とDB・ファイルの変化を比べる（P1・P2と同じ全件比較の考え方）。
- 一次データが消える・変わる経路（[inventory.md §2.3](inventory.md)の①〜⑤）は、T3.7でユーザーの決めた方針に合わせる。それまでのタスクはPythonと同じ挙動のまま。

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T3.1 | 更新APIの土台と簡単な更新: POSTのルーティング、CSRF対策（Content-Type・Origin・Content-Length・64KiB超は本文を読まず413で接続を閉じる・JSON・オブジェクトのみ）、エラー応答の対応、writerをサーバーと共有する形（無ければ503）、E12 手動の位置（地名列などのNULL化）とE13 お気に入り・除外。同期サイクルとの競合の試験 | AC1, AC7 | P2 | full | 完了 | 588516e | 83cdeeb..c1e314c | 棚卸しS4（更新側）・S5。レビュー（Sol）4回: 必須 2→1→2→0（i64超のidで別の行を更新、Content-Lengthの4300桁、Content-TypeのLatin-1読み、Originの角括弧）。CSRFの判定はPythonの`_csrf_ok`・`_host_ok`で作った9213件と実TCP 84件の差分試験で、許す向きの差を0件に固定（既知の差はOriginの前後のSP/HTABだけ＝HTTPのOWSの除去、管理者判断でrawheadの生バイト走査を外した）。同期サイクルとの割り込みで手動の位置を失わないよう`infer`に錨の条件を追加（Pythonは失う）。最終レビューの推奨（新しいTCP試験の応答待ちにtimeout）と参考（試験の起動に`LimitedListener`）はT3.2の最初に入れる |
| T3.2 | E14 回転: Drive由来でderivativeがある写真だけ、回したderivativeを別名（`.r90`など）で作る→`display`/`rotation`を更新→旧ファイルを消す。`.part`の作り方はP2と同じ。取得（`fetch_derivatives`）・品質の解析との競合 | AC1 | T3.1 | full | 完了 | d1231e8 | b225257..d576825 | 棚卸しS6。b225257・751cccfはT3.1の最終レビューの推奨・参考。レビュー（Sol）: 必須0、推奨1（取得とのロックの試験が実時間のsleep頼み→ロック待ちの観測で同期、d576825、管理者が差分を確認）。回転と取得はプロセスで1つのロックで直列化、DBの更新は読んだ時点のdisplay・rotation・md5・fetched_atのままのときだけ（外れたら409）。同時の回転で回転を失う・取り消し直後に古い画像を書き戻すのはPythonの穴で、Rustは守る。小数のdelta（`90.0`）は400（Pythonは`.r90.0.jpg`を作る）、displayがsymlinkやderivatives直下でないものは400 |
| T3.3 | E4 地名検索: 地理院→0件のときだけNominatim（全体で1秒1件）、2文字未満は空、失敗は空配列、`.cache.json`のメモリキャッシュ | AC1, AC7 | T3.1 | full | 完了 | aca10ed | ee08961..0f848d9 | レビュー（Sol）2回: 必須 1→0（gzipの展開後に本文の上限が効かない→`net::read_body_limited`でcrate全体の6か所＝地名検索・enrich・Drive一覧/トークン/403の理由・selfcheckを直し、展開後が空のgzipには圧縮前の上限`wire_limit`も）。推奨（結合テストの既定の接続先を落ちるスタブに、実時計の試験、試験の前提）を反映、修正2は管理者が差分を確認。取り込み無効のときは`.cache.json`を読むだけで書かない（P5までは本番の書き手はPython）。検索語200文字・`geocode_osm:`5000件の上限、`/api/admin/`配下の未実装のGETは403（Pythonと同じ）。**T3.10で決定・確認済み**: `GEOFRAME_TILE_UA`を地名検索にも共用（[drive-acceptance.md](drive-acceptance.md) §6） |
| T3.4 | E11 地図タイルのプロキシとB4 キャッシュの掃除: パスの検査、OSM/GSIの最大ズームと同時数、上流取得は毎分240枚まで、失敗・流量超過は透明PNG、`X-Tile-Cache`とCache-Control、`tile_cache/`の上限（`GEOFRAME_TILE_CACHE_MAX_MB`）で古い順に90%まで縮める | AC1, AC7 | T3.1 | full | 完了 | 4a23a87 | 6e1b716..1a97be6 | レビュー（Sol）: 必須0、推奨3のうち2を反映（掃除の走査・削除の途中でも停止を確かめる、座標の4300桁の上限。1a97be6、管理者が差分を確認）、TOCTOU（走査後に親ディレクトリをリンクへ差し替え）はDATA_DIRに書ける権限が前提で守りの境界の外として見送り、server.md §19.6に記載（P2のderivativesと同じ前提）。掃除は取り込みと独立のタイマー（起動時＋1時間ごと）。`tile_cache`の途中がリンクなら読み書きしない（別ディスクはマウントで）。検証環境の`GEOFRAME_TILE_UA`はユーザーが設定し、OSMの実取得はP5の前までの持ち越し（plan.md P3）。掃除の同時実行は比較用Pythonが別のDATA_DIRのため該当なし（T3.8） |
| T3.5 | 旅行ブログの読み取り E7〜E10: 旅程JSONの置き場所（設定）、`trip_id`の検査、日ごとの候補（`hidden`除外・粗い地名のみ）、保存済みの下書きか生成した初期案 | AC1, AC7 | T3.1 | full | 完了 | a194bde | 4aa2986..1f265b4 | レビュー（Sol）3回: 必須 2→2→0（区間IDの`5`/`5.0`をPythonの辞書のキーの等価性に、fixtureの失敗メッセージに実際のtrip_idを出さない、旅程の埋め込みと配布前の検査で途中のフォルダのリンク・未追跡・異常時の名前のログ）、推奨（埋め込みを管理されたファイルに限る）を反映。旅程はバイナリに埋め込み、`GEOFRAME_BLOG_PLANS_DIR`で置き換え。候補・下書きの応答に座標・都道府県・ファイル名が出ないことを全応答で検査。T3.6へ: 保存時の`published`→`draft`、unitの`ReadOnlyPaths`から`travel_blog`・`blog_exports`を外す |
| T3.6 | 旅行ブログの保存と書き出し E15・E16: 本文の検査（章・画像数・文字列の上限）、下書きは一時ファイル→置き換え、書き出しは作業領域→`.previous`経由の置き換え、画像は1800pxで再エンコードしEXIF・GPSを落とす、実行中は409、`dry_run`。unitの`ReadOnlyPaths`から`travel_blog`・`blog_exports`を外す | AC1, AC5, AC7 | T3.5 | full | 完了 | 444193d | dcc0c64..0342aa3 | 棚卸しS10。レビュー（Sol）2回: 必須 2→0（POSTの処理中に無通信の期限を止める`ConnBusy`を本文の受信前に取っていた→本文を読んで検査したあとspawn_blockingを待つ間だけ、起動時の下書きの掃除が親のリンクを辿る→`is_sweepable_dir`）、推奨（長いPOSTの試験を合図で同期）を反映。取り込み無効のときは保存と書き出しを503（dry_runは何も書かないので200）。`serde_json`の`preserve_order`をcrate全体に入れ、下書き・manifestをPythonと同じバイト列に（全JSON応答のキー順が組み立てた順に変わる。契約・比較の試験は通過）。unitの`ReadOnlyPaths`は`photos`だけ。**T3.8で**: 比較用Pythonは別のDATA_DIRにする（同じだと書き出しの排他が効かない） |
| T3.7 | 一次データが消える・変わる経路①〜⑤への対応（2026-10-25にユーザーが決定）: ①原本（Driveのmd5・ローカルのmtime）が更新されても`loc_source='manual'`の位置を残す ②④写真の行の削除は今のまま、旅行ブログの書き出しは参照できない写真（行が無い・`hidden`・表示用の画像が無い）を知らせて残りを書き出す ③原本の更新で`rotation`を0に戻すのは今のまま ⑤大量削除の保留は管理画面に件数と解除のコマンド（`geoframe sync --allow-mass-delete`）を表示し、画面からは解除しない | AC1 | T3.1〜T3.6 | full | 完了 | 6f7bb28 | 06fcca4..ffe6331 | ③は変更なし（Pythonと同じ）。①はinventory §2.3・server.md §0にPythonとの差として記録。①は更新のSQLを`CASE WHEN loc_source='manual'`の条件付き（書く時点で判定、計画のあとのsetlocも守る）。②④は応答に`skipped:[{photo_id,reason:missing|hidden|no_image}]`、全部飛んだら本文だけ。⑤は`/api/admin/status`の`sync.mass_delete_hold`と管理画面STATUSタブの注意（プロセス内の記憶）。手動でない行の「位置が動いたときの地名の付け直し」は範囲外で今のまま。レビュー（Sol）: 必須0、推奨1（dry_runが大きさの上限を見ない→実際の書き出しと検査を共有、ffe6331、管理者が差分を確認） |
| T3.8 | 検証環境での受入: 管理画面・AI写真検索（E13）・旅行ブログの全操作を行い、変更がDB・ファイルに残ること。比較用Pythonと同じ要求で応答と変化を比べる。棚卸しS4〜S6・S10、miharashiの表示の継続、結果を`p3-acceptance.md`へ | AC1, AC2 | T3.1〜T3.7 | full | 完了 | 83b7c5f | 3ae4fa8 | [p3-acceptance.md](p3-acceptance.md)。棚卸しS4・S5・S6・S10、CSRF 12通り、取り込み無効の503と不変、旅行ブログの保存・書き出しが比較用Pythonと一致（差はT3.7の決定のskippedだけ）。全件比較の想定外の差0、操作後のcatalog.dbの`.dump`がRustとPythonで一致、miharashi定常200・停止再開300とも終了コード0。検証環境はDBとファイルを戻してINGEST=0で常駐（3,065件）。**持ち越し（P5の本番切替の前まで。plan.md P3・p3-acceptance.md §7.1）**: OSMのタイル（**2026-10-25に済み**）、⑤の保留の表示の実機（**T3.10で完了**、[drive-acceptance.md](drive-acceptance.md) §5）、画面の目視（**2026-10-25にユーザーが済み**。旅行ブログは作り直す予定のため今の動作でよい）。P5の前に比較用Pythonの`/opt/geoframe-pyref`を更新（埋め込みのadmin.js・blog-admin.jsが古い） |
| T3.9 | P3全体レビューの反映: 受入の持ち越し項目（画面の目視・OSMのタイル・Nominatimの実通信とUser-Agent・⑤の実機）を期限付きでplan.md・p3-acceptance.mdにそろえる、server.mdの食い違い（Driveの回転のリセット、ブログのGETの実装済み、500の詳細のログ） | AC1, AC2, AC7 | T3.1〜T3.8 | quick | 完了 | 24847e3 | 49d76d3 | P3全体レビュー（要求との突き合わせ、Sol）: 必須1（画面の目視とOSMが未確認のままT3.8を完了としていた→持ち越しを明記、マージは止めない）・推奨2（Nominatimの実通信の持ち越し、文書の食い違い）。security-review（Opus）: High/Medium 0 |

## P3の持ち越しの確認・補足（P4ブランチ）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T3.10 | 地名検索にもGEOFRAME_TILE_UAを使い、検証環境でNominatimの実通信と本物のDriveの取り込み・大量削除の保留表示を確認する | AC1, AC2, AC7 | T3.8 | full | 完了 | d7252b7 | 0acfc2c | [drive-acceptance.md](drive-acceptance.md)。通常レビュー（Sol）指摘0、fmt/clippy/739テスト成功。実Drive6535件の冪等性、46原本のRust/Python全件成功、iPhone原本5枚、未登録1行の索引→配信、2000行の保留→解除と画面、Nominatim実通信・キャッシュを確認。一時鍵・原本・複製は削除、元DBと画像は不変 |

## P4: AI Provider・永続ジョブ（PR #61、2026-10-29 マージ）

- PRD: [p4-prd.md](p4-prd.md)、設計: [p4-design.md](p4-design.md)。2026-10-26にユーザーが推奨順での実装を承認。
- 作業ブランチ: `feat/rust-migration-p4`。P4として1つのPR。先にT4.1a〜c、T4.2、T4.3を進める。
- local-model-lab #93は実装開始時もOPEN（2026-10-26再確認）。画像＋文Embeddingの本接続・P4全体の完了は契約確定後。後半の外部依存を、前半を止める理由にしない。
- 各タスクはt3-full-buildの管理モードで、実装Sonnet・通常レビューSol。security-reviewはP4のPR前に全体で1回。
- 2026-10-27のユーザー指定: Claudeが利用できない間は、実装がSolでも通常レビューを別のSol子タスクで簡易に実施する。変更点と認証・送信許可・予算・停止・秘匿の重要境界を確認し、全workspaceや既存範囲の不要な再実行は省く。進行中の修正が完了してから、新しいレビュー子タスクへ修正後コミットを渡す。
- 2026-10-27に#93のOPEN・契約未確定を再確認。T4.3の次は独立して進められるT4.6を、認証／通信、Decisionの写真別処理、日単位候補／座標化の3タスクへ分ける。要求・ACは元のT4.6のまま。実クラウド送信の許可はこの分割や実装では追加しない。
- 2026-10-27にlocal-model-labのPR #101反映後の画像対応BF16を独立確認し、[#93を完了](https://github.com/ysogabe/local-model-lab/issues/93#issuecomment-6444034979)。採用契約は同リポジトリの`c9b05b590d838b81a6b2f939af34e409f16469ad`に固定する。ユーザーが実装継続を承認。T4.4はアダプター・画像索引のaと、検索API・対話キャッシュのbへ分割する。要求・ACは変えず、完了にはLXCでの実API・並行応答確認も必要。
- 2026-10-28にT4.4a/bの実装・独立レビュー・LXC実API受入が完了。T4.5はa（同一場面・順位・新結果repository）、b（ローカル周期処理・選別/API接続）、c（任意のGemini比較）へ分割する。元の要求・ACは変えず、旧結果との部分重複を自動置換しない。クラウドが未許可でもローカルのグループ・代表選択を使える形にし、実クラウド送信の許可は追加しない。

| ID | 内容 | AC（P4-PRD） | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T4.1a | 追加ai_jobsスキーマ、冪等enqueue、claim・結果確定の原子性、入力失効、後回し・失敗・中断の上限、起動回収のrepository | AC1, AC2, AC4, AC10a | P3 | full | 完了 | 7a16640 | 8359ebb..cc6867c | レビューSol2回: 必須1→0（A→B→Aで再試行履歴を失う→terminalと途中のカウンタ・待ち時刻を保持）。全769テスト成功・2件ignore、fmt/clippy・musl成功。LXC複製で初回のai_jobs/索引追加だけ、2回目移行0・既存全表不変。Provider停止はT4.1b/T4.2、停止開始記録はT4.1cへ |
| T4.1b | AI設定JSON・用途別ポリシー、変更と冪等性の分離、永続の周期／日予算、無効設定時の配信継続 | AC1, AC4, AC5, AC10a | T4.1a | full | 完了 | d240b87 | f7f7fdf..e45f4fe | 通常レビューSol2回: 必須3・推奨1→指摘0。周期変更・cleanup後の時計巻戻り・期限後返信を修正。全821テスト成功・2件ignore、独立89試験成功、fmt/clippy・musl成功。LXC複製で不正設定/負周期でもfeed継続・既存DB不変。起動時pause整理・解除CLI・admin表示はT4.1c |
| T4.1c | AIワーカーの起動条件・LockHold、SIGTERM時の共通期限、停止後回収、ガード付きai-jobs状態／retry CLI | AC1, AC3, AC4, AC5, AC10a | T4.1a, T4.1b | full | 完了 | 79fffe2 | cf201d6..1cddeda | 通常レビューSol2回・Opus代替1回: 必須3→1→0。停止後送信/claim、保存I/O期限、writer接続終了までのLockHoldを修正。全883テスト成功・2件ignore、fmt/clippy・musl成功。LXCでCLI排他/旧DB不変、回帰44件と追加23件成功。実handler登録・Provider共通待ちは後続 |
| T4.2 | AI専用HTTP・Discovery/available・送信先制限・用途別許可・Decision、非同期処理からの安全な呼出し | AC2, AC3, AC4, AC5, AC10a | T4.1c | full | 完了 | 65ffea4 | b4c9e18..96412e1 | レビューSol3回: 必須2→2→0。停止Gateとwait表示、RGB由来の自生成JPEGだけを送る型へ修正。全949テスト成功・2件ignore、fmt/clippy・musl成功、型境界とallocation0を独立確認。LXCでAI158/176件・最終画像/Decision/Provider27件成功、waitのCLI/API/実STATUSと期限切れ非表示確認。実handler/Vertex/#93は後続 |
| T4.3 | 空間別のベクトル保存・active_space、有効世代JOIN、旧選別維持と代表不在時の安全な表示、状態の読み取り | AC4, AC6, AC7, AC9, AC10a | T4.1c | full | 完了 | 103babf | a0c0954..31e4ce2 | レビューSol2回: 必須3→0（破損vectorの公開・同SHA複製の世代・未知理由の生出力を修正）。全998テスト成功・2件ignore、fmt/clippy・musl成功、独立97試験成功。LXC索引35件・旧選別10件成功。feed3065不変、suppressed差455件を可視代表不在で説明。合成6500×768の公開約222〜279ms・最大RSS約30MiB。実Embeddingは後続 |
| T4.4a | 採用済みBF16画像／文アダプター、固定契約による空間識別、画像ジョブ・世代CAS・索引の自動切替 | AC2, AC3, AC4, AC6, AC10b | T4.2, T4.3, #93の契約 | full | 完了 | 6fe7fd7 | b297f6b..3205f09 | 別Sol簡易レビュー2回: 必須3→0。永続lineage・再probe期限・processor範囲を修正。全1146テスト成功・既存ignore2、fmt/clippy・musl成功。LXC合成38試験成功、実LANへ合成4枚/3SHAの保存・公開、Rustの日本語3件すべて最上位一致、再起動・同SHA世代再利用・旧DB不変を確認。追加表1（dry-run+1）。HTTP検索/LRUとRust HTTP経由の並行15秒はb |
| T4.4b | E5/E6の本接続、対話用枠・15秒の全体期限・検索語LRU、空間切替と旧DBの互換 | AC2, AC3, AC6, AC7, AC10b | T4.4a | full | 完了 | bf5d316 | 0429165..46b1c2b | 途中レビューの構図設定不正による配信停止を親が小修正。再開した別Solレビューは必須・推奨0、独立7試験成功。全1173成功・既存ignore2、fmt/clippy・musl成功。LXC対話24＋設定21成功。実HTTPで日本語3件最上位一致、背景8件中query約33ms、共有待機中hit200/miss503・解除復帰、readonly/旧DB不変・429・不正設定時配信200を確認。追加schema0 |
| T4.5a | 同一場面の連結成分・ルール／rank_model順位、世代別の新グループrepository・旧結果との重複分類 | AC4, AC6, AC8, AC9, AC10a | T4.3, T4.4b | full | 完了 | 5e18624 | f5f26c6..64d3860 | 別Solレビュー2回: 必須2→0（参照集合と保存結果の完全性を修正）、推奨のヘッダー型分類も反映。全1210成功・既存ignore2、fmt/clippy・musl成功。最終小修正は専用36件・clippyで確認。LXC専用36＋6500×768測定1成功、Python切り戻し実行。snapshot331ms・compute233ms・writer590ms。追加3表は明示APIのみで起動dry-run増分0、周期・選別接続はb |
| T4.5b | ローカル周期処理・新選別ビュー・E5/E6接続、現世代の失効と旧選別の安全な維持 | AC3, AC4, AC6, AC8, AC9, AC10a | T4.5a | full | 完了 | 64d3860 | b0469ae..5288f5f | 実装Sol（b0469ae）。Solレビューは利用上限で中断し、Opusレビュー4回: 必須1（読取ごとの全vector読み）→1（guard分離で古い場面を期限なく採用）→1（Readyでなくなった写真の参照が残る）→0。修正はSonnet（de61560・b004c09・008a759・5288f5f）。推奨は全反映（AI無効時の起動回収、Partialは情報のみ、Exactは旧群の要約を継承、群の並びはkeep_score尺度、db.ai.suppressedはPython互換、16列表の移行、別空間の試験）。全1244成功・ignore4、fmt/clippy・musl成功。LXCでgroups68・location_suggest50成功（Pillowを一時導入し後で削除）。6500×768の読取（LXC中央値）: 管理STATUS 1249→39ms、E5 1422→170ms、E6 889→381ms、写真一覧41→3ms。既知の境界はai-scenes.md |
| T4.5c | 用途別許可に従う任意のGemini比較・有償結果の再利用・世代CASでの適用 | AC2, AC3, AC4, AC5, AC8, AC9, AC10a | T4.5b, T4.6a | full | 完了 | 0894bc2 | 78a6b04..aa9b0ec | 実装Sonnet（78a6b04・f30ad4a・8a08880）。Opusレビュー2回: 必須0・0。推奨は全反映（9e8a85c: 有償回答のCASを送った写真だけに絞りwriter内6510枚で14→0.17ms、場面の状態が変わっても入力が同じなら回答を履歴に残し再課金しない、E6の群の中でGeminiの推薦を代表・並びに使う、許可を外したら表示も並びも使わない。aa9b0ec: writer内CASの直接試験ほか）。未許可はtoken・DNS・POSTとも0、送る画像はRGBから作り直しEXIF/GPSなし。フィードの抑止には使わない（既定、旧photo_ai_*表は書かない）。Exactは再送しない、Partialは管理確認のまま。全1271成功、scene_judge 26＋計測1。LXCでscene_judge25・関連122成功、writer内CAS 0.31ms（旧方式47ms）。実クラウド送信の許可・日の上限はユーザーの判断で未設定 |
| T4.6a | Vertexの明示認証・固定HTTPS宛先・token分離と401更新、用途別クラウド通信境界 | AC2, AC5, AC10a | T4.1c, T4.2 | full | 完了 | dd5b4a3 | e1fea35..1092c7e | 実装Sol（Sonnetは利用上限で開始前中断）。ユーザー指定のSol簡易レビュー2回: 必須1・推奨1→0。署名付き応答とPython生成設定の互換を修正。全1029テスト成功・2件ignore、fmt/clippy・musl成功。LXC cloud31件・Drive74件成功。初回LXCの期限/wire試験も修正しwriter待ちを全体期限へ制限。新規handler・実クラウド送信は後続 |
| T4.6b | Decision用の画像準備・質問・clue解釈、写真別reconcileと世代CAS、failedを手がかりなしとして記録 | AC2, AC4, AC5, AC8, AC10a | T4.1c, T4.2 | full | 完了 | 3eae4f9 | 8be70ed | 別Sol簡易レビュー指摘0、独立32試験成功。全1056テスト成功・2件ignore、fmt/clippy・musl成功。LXC写真別25件・画像2件成功、実serveで初回1POSTと正常保存、再起動再送0、photos不変・health200・SIGTERM約0.12秒以内を確認。日候補はc2で接続済み。親の追加受入で実LAN Decisionへ合成1枚を通し、正常clue・attempts0・一次列不変を確認 |
| T4.6c1 | 日集合・送信順・固定promptと応答型、世代別有償回答repository・Python互換run_id／部分集合再利用 | AC2, AC4, AC8, AC9, AC10a | T4.6a, T4.6b | full | 完了 | 8c0b906 | 40ce3f0..df12428 | 別Sol簡易レビュー2回: 必須1→0（成功分のfailure継承を修正）。全1083テスト成功・2件ignore、fmt/clippy・musl成功、限定独立11試験成功。LXC domain/repository27件成功。世代別回答履歴・旧done保留・Python互換と再利用を確認。実送信・handler登録はc2 |
| T4.6c2 | 日候補worker・CloudClient接続と別job座標化、Geocoder共有・候補CAS・Python切り戻し | AC2, AC4, AC5, AC8, AC9, AC10a | T4.6c1 | full | 完了 | b9bd93f | 5f92c86..5e9bc80 | 別Sol簡易レビュー2回: 必須1→0（cleanup失敗後のattempts継承迂回を修正）。全1108テスト成功・2件ignore、fmt/clippy・musl成功、限定独立6試験成功。LXC追加24件＋故障注入1件成功。保存済み回答からの再開・共有Geocoder・候補CAS／API／SAVE・Python互換を確認。実クラウド許可とP4全体受入は別 |
| T4.7 | LXCで通しの受入、Provider停止／復帰、miharashi・管理操作、Python切り戻しと結果の記録 | AC1〜9, AC10a, AC10b | T4.1a〜T4.5, T4.6a〜b, T4.6c1〜c2 | ops | 完了 | fb10168 | 5c24316..7c5bedd | 受入はOpus。[p4-acceptance.md](p4-acceptance.md)。配布用muslで本物の写真とLANのEmbedding・Decisionを使用。6,439件のEmbeddingで7回の再起動でも再送・重複0、Provider停止・無応答中もフィード・画像・STATUS・位置修正は200で復帰後に再開、無応答中のSIGTERMは10.1秒で停止、miharashi定常200・停止再開300とも終了0、日本語の意味検索0.36〜0.44秒（背景の再Embedding中も最大0.55秒）、Python切り戻しで人手の列は全行不変・フィード差は回転した代表の群だけ。直すべき不具合0、見つかった4件のうち2件はT4.8で修正。実クラウド（Vertex/Gemini）はユーザーの許可待ちで未確認。検証LXCは受入前の状態に戻した（`.dump`一致、3,065件） |
| T4.8 | 受入で見つかったことの修正: 期限を過ぎた後回しのジョブが待ちの後ろに回る、公開前の索引の進み具合が見えない | AC1, AC2, AC6, AC7 | T4.7 | quick | 完了 | 7c5bedd | f550d3f..3e4d9b3 | 実装Sonnet。claimの並びを、ジョブ自身の不良でない後回し（defer_count=0）は登録時刻で並べる（索引・スキーマ変更なし、7,000件で1回0.83ms）。E5の`pipeline.building`と`ai-jobs status`に公開前の件数・成功率・公開条件を出す（BLOBを読まない）。全1,279成功、故障注入6通りで試験が落ちることを確認。残る2件（初回のDecisionの全件再判定で約4時間CPU、初回Embedding中の意味検索503で約48分）はP5の手順・判断事項 |
| T4.9 | P4全体レビュー・security-reviewの反映: 構築後の追加写真で索引の公開が待たされる、Gemini比較の失敗上限32件が後続を止める、P5への引き継ぎ、位置なし写真の一覧の重さ（画像を読む）、撮影地推定の準備の毎周期デコード、AI状態のキャッシュ、検索の回数計上の順、AI鍵の権限、失敗行の投影 | AC1, AC2, AC5, AC6, AC7, AC9, AC10a | T4.1a〜T4.8 | full | 完了 | 618358f | f6aeddd..7893e59 | P4全体レビュー（要求との突き合わせ、Sol）: 必須3（索引公開の判定を構築開始時のsnapshotに合わせる、失敗キーを候補選びで飛ばす、P5の計画への引き継ぎ）・推奨1（途中段階の文書）。security-review（Opus）: High 0・Medium 2（一覧はDBだけで判定し画像を読まない、推定の画像は送る直前に1件ずつ作る）・Low 8のうち4を反映（AI状態の5秒キャッシュと重みの再読込、検索語の検査を回数より先に、AI鍵の権限・所有者が違えば使わず配信は続ける、送信項目の文書化）。修正後の最終レビュー（Opus）: 必須0・推奨3を反映（中断の上限で失敗したキーの失敗行の投影、鍵を拒否した理由の警告、キャッシュ試験の分離）。実装Sonnet。全1,293成功。検証LXCでlib全1,100件成功（2コアで全件を並列に流すとプロセスのファイル数の上限1,024に当たるので、上限を上げ、リポジトリのfixtureとPillowを一時的に置いて実行。終了後に削除）。P5の条件（初回Embeddingで意味検索503が約48分、Decisionの全件再判定で約4時間CPU、実クラウドの許可、鍵の権限、PythonのAI有効の切り戻し）は[plan.md](plan.md)のP5へ |

## P4の後の補足（P5の前）

- 2026-10-29のユーザーの決定: Decisionの全件再判定で本番機のCPUを約4時間使うことを承認。切替のあと意味検索が約48分止まることを受け入れる。Geminiへの接続を許可。検証環境は本番と同じ鍵を使い続けてよい。
- 実クラウドの確認は同日に完了（[p4-acceptance.md](p4-acceptance.md) §8.1）。

| ID | 内容 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|
| T4.10 | Provider全体の停止で止まる用途を`ai-jobs status`と管理画面のSTATUSに出す（`ai_task_blocks`）。待ちがあるときのワーカーの周期を短くする（`GEOFRAME_AI_BACKLOG_POLL_S`既定5秒、手がかりの1周期の件数を60→10） | full | 完了 | a2924d5 | 42009d7..0075a1e | PR #62。実装Sonnet。security-review（Opus）: 指摘0。レビュー（Sol）2回: 必須1→0。座標化の残りは短い待ちにし、クラウドに送る用途は予算の周期を早めないよう外したまま（Solも同意）。検証LXC（77dd1e4、約2時間40分）: ダミー鍵で起こした停止で、同じ鍵の両用途が「停止中: Provider全体」と待ちの件数を表示。解除で戻り、手がかりは1周期10件・約2分おきで、合間にEmbeddingが進んだ。持ち越し: 鍵の権限で止まったときも画面は「認証エラー（401）」と出る（理由コードの共有。journalには本当の理由が出る）。周期の開始・終了のログ行が無い |

## P5: 切替（実装中）

- PRD: [p5-prd.md](p5-prd.md)、設計: [p5-design.md](p5-design.md)。2026-10-30にユーザーが承認。
- 2026-10-29のユーザーの決定: Pythonの撤去はP5に含める。Pythonへ戻すときはAIを無効にする。クラウドAIは切替の数日後に有効にする。切替の日時は演習のあとに決める。
- PRは2つ: P5a（T5.1〜T5.4。切替の前にマージ）、P5b（T5.6a〜b。切替のあと）。作業ブランチ: P5aは`feat/rust-migration-p5`。
- 各タスクはt3-full-buildの管理モード（実装Sonnet・通常レビューSol）。security-reviewはPRの前に全体で1回。
- 設計のレビュー: Sol（設計）・Opus（安全面）を2回ずつ。必須は1回目14件・2回目8件で、すべて反映。

| ID | 内容 | AC（P5-PRD） | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T5.1 | ガードの見直し（印の検査を外しPython版の検査は残す、終了コード、rootと`catalog.db`が無いCLIの拒否）、`geoframe-rs.service`の見直し（置き場所・`After=`・設定のファイル・StartLimit・ReadOnlyPaths・制限）、`rust-deploy.sh --production`とその試験、`deploy-pull.sh`の拒否、`geoframe-admin.sh` | AC1, AC2 | - | full | 完了 | 859070f | feccb6b..7f737d4 | 実装Sonnet（feccb6b..2221433、修正7248346）。レビュー（Sol）2回: 必須2→0（`--production`で試験用の環境変数・確認先の上書きが効く→固定値＋`GEOFRAME_RS_*`があれば断る、件数の下限の桁で比較が壊れる→1〜9桁）。推奨（deploy-pullの状態の厳密な解析、server.mdの試験表、環境変数の拒否を1つずつ試す）を反映。設計からの外れ: 読み取りだけの`serve`もrootで断る、deploy-pullは状態を取れないときも断る。deployの試験3本をCIに追加。実systemdでの確認（制限一式・Conflicts/After・ホスト名の形）はT5.4 |
| T5.2 | STATUSの`db.loc.clue_pending`をRustの判定の待ちを含む値に直す | AC8 | - | full | 完了 | 1f0bb79 | cdbbb5d..98aca91 | 実装Sonnet（cdbbb5d、修正e77cd2b・3125954）。レビュー（Sol）3回: 必須1→1→0（総待ちをtimerの条件にしていた→`clue_runnable`・`clue_pause`を足しtimerが使う、fallbackの写真のA→B→Aで待ちが残る→保存行をすべて照合）。推奨（STATUSでファイルを読まない・30秒キャッシュ、起動後の一時的な0に停滞の猶予）を反映。見送り: 1写真に多数の世代がある場合の計測（回転・差し替えの回数は少なく、30秒キャッシュがある）、品質待ちと一時的な後回しを区別するキー（品質待ちだけが残ると約16分で終了コード1。文書に記載）。旧Pythonの判定済みDBでの確認はT5.4 |
| T5.3 | 手順書`cutover.md`と`deploy/README.md` §7の本番向けの記述 | AC3〜AC6, AC9 | T5.1, T5.2 | full | 完了 | 1eaa6c2 | ca527eb..9587dc7 | [cutover.md](cutover.md)。実装Sonnet（ca527eb・7b97d99、修正644ccec..5b2ffbf・8546e74..8ba42fe・91964bd）。レビュー（Sol）4回: 必須7→7→2→0（タイマーの再開・展開の失敗・鍵の権限・戻したあとの状態の控えと比較・品質の2つの場合・増えた写真の判定・画像の欠けを前提未達と明記、hiddenの再出現・代表の品質条件・/procの読めないfd・演習のtimer・シャドー比較のキー単位の許容・後片付けの順序・品質の入力に特徴量とtaken_at）。推奨（fuserの代わり、値を出さない、manual_hintを本番と検証環境で分ける、非ゼロの規則の一本化、T5.4のチェックリスト§15、合格条件の例外の書き方）を反映。**T5.4で確定するもの**: §10.5の画像の欠けの直し方（確定まで本番切替に進まない）、/procをrootで、cache_load.pyを本番のvenvで、Conflicts=の両方向、systemd-analyze security |
| T5.4 | 検証環境での演習（設計§演習の1〜9）と`p5-acceptance.md` | AC1〜AC5, AC8 | T5.3 | ops | 実装中 | cf41c53 | | |
| T5.5 | 本番の切替（ユーザーの立ち会い）と記録 | AC6 | P5aのマージ | ops | 未着手 | | | |
| T5.6a | 埋め込みの元を`web/`へ移し、Rustの試験からPythonの実行を外す | AC7 | T5.5 | full | 未着手 | | | |
| T5.6b | Pythonの実装・道具・CIのジョブ・Python版のunitと配布の撤去、文書の書き直し、設定の参照の試験 | AC7 | T5.6a | full | 未着手 | | | |
