# 実装計画: GEOFRAMEのRust単一バイナリ化

- 状態: 実施中（2026-10-24 作成）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)
- 要求: Issue #54 の本文（受入条件・非目標）
- 設計: [Rust単一バイナリの写真管理APIと外部AI Provider](../architecture/rust-api-and-ai-providers-proposal.md)
- タスク表: [tasks.md](tasks.md)

Issue #54 と設計案をそのまま要求・設計として扱い、この文書は「どの順に、どこで、どう確かめながら作るか」だけを決める。

## 1. 進め方の決定

| 項目 | 決定 | 理由 |
| --- | --- | --- |
| PRの単位 | **フェーズごとに1つ**（P0、P1、…）。各PRでmainへマージする | 全体は数か月規模。最後に1本にすると差分が大きすぎてレビューできない |
| Rustの置き場所 | リポジトリ直下に`Cargo.toml`（workspace）、`crates/geoframe/`（バイナリ兼ライブラリ1つ）。P0の計測用だった`crates/geoframe-imgpoc`は、**T2.2で本体（`crates/geoframe/src/imaging`）へ移して削除した**（コミット`c41c4a9`に残る） | 移行完了後にPythonを消すと、直下がCargo workspaceという最終形になる。crateの分割は必要になってから |
| 本番への影響 | **Rustをmainへマージしても本番は変わらない** | 本番の`deploy-pull.sh`はgitの取り込みと`requirements.txt`変更時の`pip install`だけを行い、Pythonの`server.py`を起動する。Rustのビルドも起動もしない。本番の切替はP5で明示的に行う |
| 公開API | 既存のパスと応答形を維持する。`/api/v1/photos`と`/derivatives/`は契約、管理APIは現行の管理画面が動く形 | 管理画面のHTML/JSを流用し、作り直しを避ける |
| 静的ファイル | 管理画面（`app/static`・`app/vendor`）はバイナリへ埋め込む | 「バイナリ＋設定＋データ」で起動する受入条件のため |
| 設定 | 現行の`GEOFRAME_*`環境変数名と`/etc/default/geoframe`を引き継ぐ | systemd unitと運用手順の差し替えを最小にする |
| DB | 現行の`catalog.db`スキーマをそのまま読む。追加はテーブル・列の追加だけ（既存列は消さない） | 手動GPS・お気に入り・除外・回転を守る。旧Pythonへ戻せる状態をP5まで保つ |
| 画像ライブラリ | P0の計測で決め、**T2.2で本体に入れた**: miharashiと同じ`image = "0.25.10"`（PNG・TIFFのデコード）＋JPEGのデコードは`zune-jpeg`（厳格モード）、縮小は`fast_image_resize`、エンコードは`jpeg-encoder`、ICCは`moxcms`、EXIFは自前（[画像PoC](image-poc.md)、[ビルド §8](build.md)、[server.md §12](server.md)） | 知見の共有。HEICは`zune-heic`で純Rust・musl静的のまま扱える（**実機のHEICでの確認はT2.4**）。逃げ道の`libheif-rs`は、T2.2で**コードには残さない**ことにした（CIで一度もコンパイルされず腐るため。必要になったらPoCの実装を参照して足す。[ビルド §6](build.md)） |

## 2. 守ること（本番機の扱い）

- **geoframe-n100が本番機**で、開発もこの機体で行う。ここでは次をしない。
  - `deploy/deploy.sh`・`deploy-pull.sh`の実行、`geoframe-server`の停止・再起動
  - `/var/lib/geoframe/catalog.db`を書き込み可能で開くこと、Rustを本番DBへ向けること
- 本番データの持ち出しは**読み取りのバックアップ**（`/var/backups/geoframe/latest.db.gz`、`sqlite3 .backup`相当）からだけ行う。
- 家庭写真・位置情報・秘密情報をGit・PR・CIログへ載せない。テストは合成した写真とDBで行う。実データでの比較結果は件数・一致率など集計だけを記録する。
- クラウドAI（Gemini）の呼び出しと、Google Driveへのアクセスは、検証環境では既定で無効にする。有効にするときは都度ユーザーに確認する。

## 3. 検証環境（lxc-geoframe）

LAN内のLXDホスト上の`lxc-geoframe`を、移行完了まで検証環境として使う。

| 項目 | 内容 |
| --- | --- |
| 接続 | macvlanでLAN直結のSSH。LXDホストからは`lxc exec lxc-geoframe`。接続先の値はGit管理外のメモ（`local-notes/rust-migration-staging.md`）に置く |
| OS・資源 | Ubuntu 24.04（glibc 2.39）、2コア、メモリ4GiB。ビルド機のn100はDebian 13（glibc 2.41）なので、**n100でglibc向けにビルドしたものはそのまま動かない可能性がある**（P0で確かめる） |
| データ | `/var/lib/geoframe`に本番のコピー: `catalog.db`（2026-10-24 06:17のバックアップ）、`derivatives/`、`blog_exports/`、`travel_blog/`。`drive_key.json`・`models/`・`tile_cache/`はコピーしない |
| 旧環境 | 2026-08時点の旧版は`/opt/geoframe.old-202608`・`/var/lib/geoframe.old-202608`・`/etc/default/geoframe.old-202608`・`/root/old-units-202608/`へ退避し、unitは無効化済み。不要になったら消す |
| 比較用のPython | 同じコピーから作った別のデータ（`/var/lib/geoframe-pyref`）で現行Pythonを`127.0.0.1:8090`に起動し、AIとDriveを無効にする。PythonもDBへ書き込む（起動時の移行・位置推定）ため、Rustと同じDBは使わない |
| Rust | `geoframe-rs.service`として`:8080`で起動。データは`/var/lib/geoframe` |
| 更新 | n100でビルド→`scp`→`systemctl restart`のスクリプト（接続先は環境変数で指定し、IPをリポジトリへ書かない） |

各フェーズのPRは、検証環境で受入確認を済ませてからマージする。

## 4. フェーズとPR

### P0: 棚卸しと技術PoC（PR 1）

- 現行の全HTTPエンドポイント、DBテーブルと書き込み元、バックグラウンド処理（同期・品質・AI・位置候補・タイル掃除・キャッシュ保存）、設定項目を棚卸し（[棚卸し](inventory.md)）し、各フェーズへの割り当てと代表的なE2Eシナリオを決める。
- Cargo workspaceの骨組みとCI（`cargo fmt --check`・`clippy -D warnings`・`test`）。既存のPythonのCIは残す。
- ビルドと配布: `x86_64-unknown-linux-musl`＋`rusqlite`の`bundled`で完全静的なバイナリにできるか。できればそれを標準にし、できなければ配布先と同じglibcでビルドする方法を決める。
- 画像PoC: 検証環境にある本番コピーのderivatives（JPEG）から約100枚と、それをHEICへ変換したものを使い、検証環境の上でデコード→EXIF向き補正→長辺2560px→JPEG出力の画質・時間・最大RSS・リンクされるライブラリを計測する。HEICの候補（libheif連携など）を比較する。結果は数値だけを残す。
  - ユーザー指示（2026-10-24）によりGoogle Driveの原本は使わない。実機のHEIC固有の構造（グリッド分割・HDRゲインマップ・深度など）はP2のDrive取り込みの実装時に確かめる。
- `/api/v1/photos`のserde型とJSON Schemaを定義し、合成DBで契約テストを書く。
- miharashiの既存`image`実装と共有できる純粋な処理の整理: 今は共有できる関数はなく、共有crateは実際の重複が出てから決める。T2.2で本体へ移すときに改めて検討したが、結論は変わらない（[画像PoC](image-poc.md)の「miharashiとの共有」・§10）。
- 画像Embedding APIはlocal-model-lab #93で評価中（別リポジトリ、OPEN）。P4の開始前に結果を取り込む。P1の妨げにはならない。

#### P0での設計案のレビュー結果

- 設計案から変えない点: 責務の境界（GEOFRAMEは推論を持たず外部HTTP APIへ）、`catalog.db`の保護、共有は必要が出てから、`/api/v1/photos`と`/derivatives/`の契約の維持。
- P0で決まった点: ①musl静的ビルドを標準にする（[ビルドと配布](build.md)）。②HEICは`zune-heic`が第一候補（[画像PoC](image-poc.md)）。③ICCをsRGBへ変換して書き出すのは、現行Pythonからの意図した変更。
- 設計案の移行手順4「miharashiの実クライアントで後方互換を確認」は、P0では契約テスト（miharashiの読み方の再現）までとし、実クライアントでの確認はP1の受入で行う。

### P1: Rust API基盤（PR 2）

- 設定の読み込み、SQLite接続（読み取り。ブロッキング処理は`spawn_blocking`か専用スレッド）、ログ（写真内容・GPS・秘密情報を出さない）。
- `GET /api/v1/photos`（並び・除外・タイトル・旅番号を現行どおり）、`/derivatives/`ほかの静的配信（ホワイトリスト、ドットファイル拒否、パストラバーサル防止、HEAD）、Host検査、管理画面ページ。
- 管理画面が開く読み取りAPI（`/api/admin/status`・`/api/admin/photos`など）とヘルス確認。
- systemd unit（現行と同じハードニング）、検証環境への配布スクリプト。
- 受入: 検証環境で、比較用Pythonと`/api/v1/photos`のJSONを全件比較して一致すること、`/derivatives/`が同じバイト列を返すこと。miharashiの取得処理（`ureq`＋`serde_json`）が同じ形で読めること。さらに、検証環境でmiharashiの実プロセスの写真の取得先をRustへ向け、一覧の取得と写真の読み込みが続くこと（P0の設計案レビューで先送りした実クライアントでの確認）。

### P2: 写真取り込み（PR 3）

- ローカル`photos/`とGoogle Driveの索引（`drive.readonly`、撮影時刻・GPSのメタデータ）、derivative生成（JPEG/HEIC）、EXIF、位置の推定、逆ジオコーダ、簡易品質判定（完全重複・近似の判定。旧スコアとの一致は不要）。
- 索引・取り込みがDBへ書くので、**取り込み用の書き込み接続（writer）をP2で追加し**、unitの`ReadOnlyPaths`を必要な分だけ解除する（[server.md §2.2](server.md)）。
- 検証環境でDriveを使うには`drive_key.json`のコピーが要る。**P2の開始時にユーザーへ確認する。**

### P3: 管理機能（PR 4）

- 手動修正などの更新APIをwriterの上に追加する（writer自体はP2で入っている）。手動GPS、お気に入り、除外、回転、地名検索（地理院→Nominatim）、地図タイルのプロキシ、旅行ブログ（下書き・書き出し）、AI検索の画面。更新APIのCSRF対策（JSON必須・Origin検査・本文上限）を現行どおり。
- 受入: 検証環境で管理画面の全操作を行い、変更がDBに残ること。

### P4: AI Providerとジョブ（PR 5）

- 永続ジョブ（状態・再試行時刻・回数上限・入力ハッシュ・処理版）、local-model-labのLAN入口（本番機の`:8100`。接続先は設定で与える、`X-Client-Name: geoframe`、`/llm/services/<id>`のavailable確認、lease中は後回し）、Geminiアダプター（タスクごとの明示ポリシーがあるときだけ写真を送る）。
- Embeddingはmodel・前処理版・次元・ベクトル空間IDごとに分けて保存し、異なる空間を混ぜない。画像Embedding APIはlocal-model-lab #93の結果に合わせる。
- 位置候補（Decision→Gemini）、同一場面のグループ化と代表選択、AI検索。

### P5: 切替（PR 6）

- 本番`catalog.db`のバックアップと復元の演習、検証環境でのシャドー比較、本番でPythonを止めてRustを起動、スモークテスト、ロールバックと再切替の演習。
- `deploy/`の手順・unit・READMEをRust用に改め、PythonのCIと依存を外す。Python実装の削除は切替後に安定を確かめてから。
- 本番の切替は**ユーザーの立ち会いのもとで**行う。
- **取り込みのガードを外す**（T2.1で入れた暫定。[server.md §11.1.1](server.md#1111-本番への誤指定の防止ガード)）。P2〜P4の間、`GEOFRAME_INGEST=1`の`serve`・`sync`は、検証環境の印`/etc/geoframe/staging`が無い、または`geoframe-server`が非稼働と確認できないと、書き込みの前に終了コード2で断る。本番機には印が無いので、**このガードを外さない限り、本番でRustは取り込みを始められない**。切替の手順で次をこの順に行う:
  1. 本番で`geoframe-server`を止めて`disable`し、停止を確認する（`systemctl is-active`が`inactive`）。Pythonの書き込みが無いことの担保は、ガードではなくこの手順が持つ。ガードは`LoadState`を見ず`ActiveState`だけで判定する（unitファイルを消しても動いているサービスは止まらない）ので、**止める前にunitファイルを消さない**（消すなら停止の確認のあと）。
  2. コードからガードを外す（またはガードの印を本番用に置き換え、`GEOFRAME_INGEST=1`に加えて明示のフラグを要る形にする）。消すもの: `guard.rs`・`lib.rs`の`pub mod guard`・`ingest::start`の`guard.check()`（`start_with`）・`IngestError::Refused`と`cli.rs`の`exit_for_ingest_error`・`tests/ingest_cli.rs`の`Staging`と`GEOFRAME_TEST_*`・ガードのテスト。残った箇所は`grep -rn "guard\|検証環境の印\|GEOFRAME_TEST_" crates deploy docs`で探す。
  3. 文書を直す: server.md（§0の索引の行・§6の項・§11.1.1・§11.9）、`deploy/README.md §7.1`、`deploy/geoframe-rs.service`と`rust-deploy.sh`の説明、`USAGE`。
  4. 検証環境と同じバイナリで、本番のコピーを使った通しの演習を済ませてから本番へ入れる。

## 5. 受入条件とフェーズの対応

| Issue #54の受入条件 | 主なフェーズ |
| --- | --- |
| 公開契約・写真ID・手動修正・お気に入りの保存、miharashi表示の継続 | P1（読み取り）、P3（更新）、P5（本番） |
| 写真追加→位置補完→表示→選別→検索→管理画面操作のE2E | P2〜P4で各部分、P5で通し |
| local-model-lab停止・lease中も閲覧・管理でき、失敗ジョブは再試行可能 | P4 |
| Embeddingのベクトル空間を混在させない | P4 |
| Rustバイナリ＋設定＋データで起動、Python不要、CI、systemd・ロールバック手順、HEIC等の依存を明示 | P0（CI・依存調査）、P1（unit）、P5（本番・手順） |
| 写真フィードの契約型とmiharashi互換テスト、共有crateも独立ビルド | P0・P1 |
| 秘密情報・位置情報・家庭写真をテストデータ・ログ・PRに載せない | 全フェーズ（各PRの安全面レビューで確認） |

## 6. 未決事項（決める時期）

- ~~HEIC変換のバックエンドと配布形態 — P0の計測後~~ → `zune-heic`を本体に入れた（T2.2、[画像PoC](image-poc.md)）。実機HEICでの確定はT2.4の受入（実装済み。確認の手順は[server.md §14.11](server.md#1411-鍵が置かれたあとの管理者の確認手順)。鍵が置かれてから管理者が行う）
- ~~musl静的ビルドかglibcビルドか — P0~~ → **musl静的ビルドを標準にする**（[ビルドと配布](build.md)。HEIC・アロケータを入れる段階で見直す）。HTTPS（rustls）とDNSはT2.1で、musl静的のまま検証環境から通ることを確かめた（[build.md §7](build.md)）
- 画像Embedding APIのJSON契約・CPU/GPU配置 — local-model-lab #93、P4の開始前
- Embeddingインデックスの保存形式 — P4の開始前
- 検証環境へのDrive鍵のコピー — P2の開始時にユーザーへ確認
