# Rust単一バイナリ化 タスク

- 要求: [Issue #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 設計: [設計案](../architecture/rust-api-and-ai-providers-proposal.md) / 計画: [plan.md](plan.md)
- ブランチ: ローカルはT3の`t3/issue-54-implementation-plan`のまま、リモートはフェーズごとに分ける（P0は同名で PR #57、P1は`feat/rust-migration-p1`で PR #58、どちらも squash merge 済み。P2は`feat/rust-migration-p2`へpush）
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

## P2: 写真取り込み

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
| T2.4 | Driveの索引: 認証（`drive.readonly`）、フォルダの解決（名前かID）、`files.list`のページ送りと`imageMediaMetadata`、追加・更新・削除の規則（Pythonと同じ）、`fetch_derivatives`（`tmp/.dl_*`経由、失敗でも原本を残さない、`GEOFRAME_FETCH_PER_CYCLE`）。スタブのDrive APIでのテスト | AC2, AC7 | T2.1, T2.2 | full | 本物のDrive確認待ち | 0baf711 | 4d3a3e6..5f1be84 | レビュー（Sol）3回: 必須 1→2→1（通信の障害を写真の失敗に数えない: 流量制限・接続の時間切れ・ヘッダー前の切断・403の理由の本文が読めない）、推奨6（大量削除の保留`--allow-mass-delete`、原本の削除の確認、Debugの伏せ、手順書の修正）を反映。3回目の必須は5f1be84で直し、管理者が差分を確認。スタブのDriveでPythonの`index_drive`・`fetch_derivatives`と全列一致（fixture）。失敗が続く写真はプロセス内の記録で飛ばす（受入②、ローカルにも適用）。一覧0件では削除しない（Pythonは全行削除）。本物のDriveでの確認（受入①・冪等性・実機HEIC）は鍵が置かれてから管理者が行う（[server.md §14.11](server.md)）。棚卸しS2。本番コピーでの冪等性（再取得0）を`--dry-run`で確かめる。**受入条件（T2.2のレビューから）**: ①Driveの原本の形式別の成功件数と失敗の分類をPythonと比べる（T2.2で使ったJPEG 50・HEIC 8枚はderivativeからの再生成で、原本の互換性を裏付けない）、②失敗が続く写真があっても後続の写真を取得できることをスタブで確かめる（Pythonと同じ`LIMIT`の取得待ちSQLだと、恒久的に失敗する写真が先頭の枠を埋めうる）、③`tmp/.dl_*`は`create_new`＋`O_NOFOLLOW`で作る。詳細は[server.md §12.10](server.md) |
| T2.5 | 位置の推定と地名: `infer_locations`（段階窓・`GEOFRAME_INFER_OFFSET_MIN`・錨が消えたときの取り消し）、`enrich_rows`（GSIの逆ジオ・標高・`muni.js`、1サイクル40件、0.2秒待ち、応答が空なら空で確定、国外は空で確定） | AC2 | T2.1 | full | レビュー中 | a2e7341 | a4b36c7..2e952b1 | 棚卸しS3。合成の行でPythonの`infer_locations`・`enrich_rows`と全列・地理院への要求順・キャッシュのキーが一致（fixture）。本番コピーのdry-runで推定・取り消し・enrichとも0、推定2,162行と地名13行を消して1サイクル回すと`.dump`が元と完全一致。`organize`の`held_back_until_infer`を廃止 |
| T2.6 | 簡易品質: 特徴量（SHA-256・pHash・鮮明度・露出）、孤児の削除、全員分そろってからの再グルーピング（完全重複・3分以内かつpHash距離12以下）、`BEGIN IMMEDIATE`での全差し替え、`--quality-scan` | AC2 | T2.1, T2.2 | full | 完了 | 460626f | eb54361..5f11220 | レビュー（Sol）2回: 必須0。推奨（同じSHAでそろえるのは指標だけでpHashは行ごと＝配信の抑制をPythonと同じに、Pythonへの切り戻しの試験`switchbacks`と手順）を反映、2回目の推奨（解析し直す切り戻しの説明）は管理者が文書を修正。方針: Pythonの特徴量の行を有効として使い、新しい写真だけRustで解析（全部やり直すと約44時間・7枚の組が変わる）。グルーピングはPythonと20ケースで全列一致（fixture）、本番コピーで解析0・再構築559行全一致、1サイクル後も`.dump`不変・3,065件。棚卸しS7。旧スコアとの一致は不要、同じ入力で同じ結果 |
| T2.7 | 検証環境での受入: 本番コピーでの`--dry-run`（変更ほぼ0）と実サイクル、同じ写真を比較用PythonとRustへ入れて`photos`の列ごとの比較、棚卸しS1・S2・S3・S7、miharashiでの表示の継続、結果を`p2-acceptance.md`へ | AC1, AC2, AC5 | T2.3〜T2.6 | full | 未着手 | | | |

## P3〜P5（開始時に分け直す）

| ID | 内容 | AC | 種類 | 状態 |
|---|---|---|---|---|
| P3 | 管理機能（手動GPS・お気に入り・除外・回転・地名検索・タイル・旅行ブログ・更新APIのCSRF対策） | AC1, AC2 | full | 未着手 |
| P4 | AI Provider・永続ジョブ・Embeddingインデックス・位置候補・同一場面・AI検索 | AC2, AC3, AC4, AC7 | full | 未着手 |
| P5 | バックアップ・復元演習、シャドー比較、本番切替、ロールバック演習、Python依存の撤去 | AC1, AC2, AC5 | full | 未着手 |
