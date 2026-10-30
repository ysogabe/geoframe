# P2の受入確認（検証環境での記録）

- 日時: 2026-10-25に実施
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）/ 計画: [plan.md](plan.md) §4 P2 / タスク: [tasks.md](tasks.md) T2.7 / 前フェーズの記録: [p1-acceptance.md](p1-acceptance.md)
- 対象のソース: コミット`35d677d`の実装（T2.5の「時間切れの行を記憶しない」テストの補強を含む。`crates/`の実装部分は`T2.6`から無変更）。比較用の合成の写真の生成スクリプト`tools/gen_acceptance_photos.py`とこの記録は、この記録を含むコミット
- バイナリ: `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`（`static-pie linked`、16,564,016 B、sha256 `a1fdee065aab9498…`）。`deploy/rust-deploy.sh`で配布（検査・転送・入れ替え・確認OK）
- 検証環境（lxc-geoframe。Ubuntu 24.04、2コア、メモリ4GiB）。本番機ではない。接続先の値はGit管理外のメモにあり、ここには書かない。本番機（geoframe-server）・本番の`/var/lib/geoframe`・miharashiの本番プロセスには触れていない
- **写真の値・ファイル名・位置・地名・撮影日時は書かない。件数・ハッシュの先頭・合否だけ**（合成の写真は公開の代表点と架空の時刻。実写真ではない）
- 比較用Pythonは`geoframe-pyref`の`/opt/geoframe-pyref`（Python 3.12.3・Pillow 13.0.0・pillow-heif 1.8.1。本番と同じ`GEOFRAME_EMBED_BACKEND=local`）

**追記（2026-10-25、T3.10）**: 以下はP2実施時の記録。鍵待ちだった実Driveの冪等性・原本比較・iPhone HEIC・未登録行の索引から配信までの確認は[drive-acceptance.md](drive-acceptance.md)で完了した。元の実施記録は残す。

## 0. 結果の要約

| 項目 | 結果 |
| --- | --- |
| §1 本番コピーでの冪等性（P2の受入の関門） | **合格（Driveの2段を除く）。** `sync --dry-run`で、実行した段の変更は0。`index_drive`・`fetch_derivatives`は鍵が無く省略されたので**Driveの2段は未確認（鍵待ち）**。複製での実サイクル2回のあと、`.dump`のSHA-256が元と一致（`57f6dcef…`） |
| §2 同じ写真をPythonとRustへ入れた比較 | **合格。** `photos`の全26列×21行が一致（`id`・推定・地名・`display`を含む）。品質の顔ぶれ・`kind`・グループ番号・配信から外れる数も一致。差は2つで、どちらもPythonとの既知の差で説明できた（HEICのderivativeの画素、実行時刻） |
| §3 棚卸しのE2E | S1・S3・S7は通った（RustとPythonで同じ結果）。S2は**スタブのDriveの範囲**（本物のDriveは鍵待ち）。S4はP3 |
| §4 miharashiでの表示の継続 | **合格。** 取り込みを有効にしたRust（複製）で、定常200枚・停止と再開300枚とも終了コード0。`compare_with_python.py`は想定外の差0 |
| §7 検証環境の最終状態 | `ingest=0`・`/healthz` 200・本番コピーの`.dump`は配布前と一致・使い捨てのものは全部削除 |
| 説明できなかった差 | **なし** |
| 未実施（鍵待ち） | 本物のDrive（S2の実機・[server.md §14.11](server.md#1411-鍵が置かれたあとの管理者の確認手順)の(1)〜(4)）。**本番で取り込みを有効にする前の関門**（[plan.md P5](plan.md#p5-切替pr-6)） |

## 1. 本番コピーでの冪等性（受入の関門）

検証環境の`/var/lib/geoframe`は本番の2026-10-24 06:17のバックアップのコピー（`photos`6,535行すべて`source='drive'`、`photo_quality`559行、`photo_quality_features`6,535行、ローカルの写真は0）。**本番コピーそのものには書かず**、検証環境の中で`sqlite3 .backup`の複製を取って（`.dump`のSHA-256は元と同じ）、複製の`derivatives/`はハードリンクで作った。Driveの鍵が無いので、`index_drive`・`fetch_derivatives`は`省略（Drive の鍵が無いので飛ばします）`になる。

### 1.1 `sync --dry-run`（全段の件数）

本番コピーそのもの（読むだけ）と、複製の両方で、同じ結果:

| 段 | 結果 |
| --- | --- |
| `index_local` | `files=0 added=0 updated=0 removed=0`（`unreadable`・`skipped`・`derivative_files_*`も0） |
| `index_drive` | 省略（鍵なし）。**未確認**（§6） |
| `infer` | `anchors=3125 targets=3409 inferred=0 updated=0 unchanged=2162 cancelled=0 untimed=31` |
| `enrich` | `candidates=0`（`would_query=0 unresolved=0 skipped_backoff=0 skipped_blocked=0 unusable=0 pending=0`） |
| `local_derivatives` | `original=0 made=0 failed=0` |
| `fetch_derivatives` | 省略（鍵なし）。**未確認**（§6） |
| `quality` | `orphans=0 pending=0 would_analyze=0 remaining=0 would_rebuild=0` |
| スキーマ・掃除 | 作る表・索引0、足す列0、`place`をNULLに戻す: なし、消す`.dl_*`0、消す`.part`0、捨てる`.cache.json`の旧キー0 |
| 写真 | 表示可能5288 / 取得待ち0 / 全6535（28 ms未満） |

### 1.2 実サイクル2回（`GEOFRAME_INGEST=1 geoframe sync`、複製に対して）

| 回 | 結果 |
| --- | --- |
| 1回目 | 終了コード0。全段の件数は1.1と同じ（`enrich`は`queried=0`、`quality`は`analyzed=0 rebuilt=0 rows=0`）。`.dump`のSHA-256の先頭16桁`57f6dceff3b48bc5`が元と**一致** |
| 2回目 | 同じ。`.dump`は`57f6dceff3b48bc5`のまま**一致** |

**行の変更0・derivativeの再取得0・`.dump`完全一致。** 本番コピーに対して、Rustが取り込み済みのデータを一切動かさない。

## 2. 同じ写真を比較用PythonとRustへ入れて`photos`を列ごとに比べる

T2.3で行ったローカルの比較（[server.md §13.9](server.md#139-検証環境での確認2026-10-24)）を、推定・地名・品質まで通しにした。

### 2.1 手順

- **合成の写真15枚**（[`tools/gen_acceptance_photos.py`](https://github.com/ysogabe/geoframe-ambient-display/blob/a804623029ad67d415692d08ea3182fd765eb2a4/tools/gen_acceptance_photos.py)。乱数から作った画像。座標は駅・城・歌劇場の公開の代表点、撮影時刻は架空。`mtime`は固定）を、RustとPythonの`photos/`に同じバイト列・同じ`mtime`で置いた。内訳: GPSありのJPEG3枚（東京駅・皇居（長辺3200px）・京都駅（EXIFの向き6））、GPSなしのJPEG5枚（GPSありの写真の10分後・1時間30分後・8時間後・どの合成の写真からも9日後・撮影時刻なし）、同じバイト列のJPEG2枚（1枚は`sub/`の下）、GPSありで2分違いのほぼ同じJPEG2枚、GPSありのHEIC1枚とその5分後のGPSなしのJPEG1枚、国外（シドニー）のJPEG1枚。
- **Rust**: 検証環境の中の複製（§1と同じ`catalog.pre`のコピー）に対して、`sync --dry-run`のあと`GEOFRAME_INGEST=1 geoframe sync`を**2サイクル**。
- **Python**: 別の複製（同じ`catalog.pre`のコピー、`derivatives/`はハードリンク）に、`pyref`のvenvで`from geoframe.catalog import cycle; cycle()`を**2サイクル**（比較用Pythonのサービスのデータ`/var/lib/geoframe-pyref`は触らない）。
- `.cache.json`は両方に比較用Pythonのもの（`muni_table`だけ）を置いた。**地理院は本物に問い合わせた**: Rustの`enrich`は`candidates=14 foreign=2 cache_hit=6 queried=6 resolved=6`（逆ジオ6＋標高6の要求。dry-runの`would_query=6`と一致）。
- そのあと、追加の6枚（§3のS1・S3・S7用）を両方に置いて、取り込みを有効にした`serve`（`GEOFRAME_DRIVE_POLL_MIN=0.1`）とPythonの`server.py`（同じ間隔）のサイクルに任せ、もう一度全部を比べた。

### 2.2 結果

| 比較 | 結果 |
| --- | --- |
| dry-runと実行の一致（Rust） | `index_local added=15`、`infer inferred=5`、`enrich would_query=6`（実行は`queried=6`）、`local_derivatives original=13 made=1`、`quality would_analyze=14 would_rebuild=1`（実行は`analyzed=14 rebuilt=1 rows=563`）。写真は表示可能5302 / 全6550で同じ |
| 2サイクル目（Rust） | `added=0 updated=0`、`inferred=0`、`enrich candidates=0`、`made=0`、`quality pending=0`。変更0 |
| **`photos`のローカル行** | 15行（追加分を足して21行）・パスの集合が一致。**全26列（`id`・`path name mtime taken_at lat lon loc_source place elev muni_cd pref city display inferred_from infer_dt_s`ほか）の不一致0**。`indexed_at`・`fetched_at`は値でなく形式（数字を9に置いた形）まで一致 |
| Driveの6,535行 | 両方とも変化なし（実行時刻を除いて一致） |
| 位置の推定 | 推定された行は両方とも同じ。バケットは±15分・±2時間・±12時間が1行ずつ以上（窓どおり）。合成の写真から遠い1枚は、本番コピーの既存の写真が窓の内側にあったので、両方とも**その既存の写真を錨に**推定された（値は書かない）。時刻なしの1枚は両方とも推定されない |
| 地名 | 国内の行は同じ地名の列が付き、国外の1枚は両方とも空で確定。時刻なしの行は`place`がNULLのまま |
| **`photo_quality`のローカル行** | 21行時点で8行・`kind`（`exact`/`similar`）・順位・推奨が一致 |
| `photo_quality`の全体 | 行数567・グループ数247・`kind`の内訳（`exact` 189・`similar` 378）が一致。`photo_id`ごとの全列（`group_id`・順位・`recommended`・`score`・`reasons_json`・`issues_json`を含む。実装の印の`algorithm`と`analyzed_at`は除く）の不一致0 |
| 特徴量（`photo_quality_features`のローカル行） | pHashの距離は20行すべて0。`content_sha256`は20行中19行が一致、不一致の1行はHEICのderivative（下の差） |
| `GET /api/v1/photos` | 3,065件 → 追加後3,078件（15枚から、位置がなく推定もされない1枚と、完全重複の推奨外1枚を除く13枚が増える）→ 3,083件（S1〜S7用の追加後）。Rust・Pythonとも同じ件数 |
| `tools/compare_with_python.py`（Rust複製 :8081とPython複製 :8092、3,083件の時点） | 一覧のJSONは3,083件で並びも一致。値の差は`fetched_at`の18件だけ（形式は同じ）。`/api/admin/status`・`/api/admin/photos`の222通り・管理画面・拒否・メソッド・Hostは一致。画像は3,082/3,083件がバイト一致 |

### 2.3 差の説明

| 差 | 理由 | 分類 |
| --- | --- | --- |
| HEICのderivative1枚のバイト列（`content_sha256`も）。寸法は同じ（1600x1200）、画素の平均絶対差は0.003（64x48に縮めた輝度、0〜255）、pHash距離は0 | デコーダ・縮小フィルタ・エンコーダがPillowと違う。ICCの扱いも違う: Rustは対応するICCで画素をsRGBへ変換し、出力にICCを付けない。Pythonは画素を変換せず、ICCを捨てる（[server.md §12.2](server.md)・[§0](server.md)の「画像処理（T2.2）」） | 既知の差 |
| `/api/v1/photos`の`fetched_at`（ローカルのderivative・原本を作った時刻） | 実行時刻はRustとPythonで別のサイクルで書かれる。形式（`YYYY-MM-DDTHH:MM:SS`）は同じ | 実行時刻 |
| `photo_quality.algorithm`・`analyzed_at`（比較から除いた） | 実装の印と実行時刻。Pythonは`algorithm`を読まない（[server.md §16.10](server.md#1610-rustで作った結果をpythonへ切り戻すp5のロールバック)） | 実行時刻・実装の印 |

**説明できなかった差はない。** 手動GPSが上書きされる経路、位置が動いたときの地名の付け直しは、この比較の入力に含めていない（§5）。

## 3. 棚卸しのE2Eシナリオ

[inventory.md §5](inventory.md)のシナリオを、検証環境の取り込み有効のRust（複製。`GEOFRAME_DRIVE_POLL_MIN=0.1`の`serve`）で通した。比較用Python（同じ間隔の`server.py`）にも同じ写真を置き、結果を突き合わせた。

| ID | 内容 | 結果 |
| --- | --- | --- |
| S1 | ローカル写真の追加→表示 | GPSと撮影時刻つきのJPEGを`photos/`に置く → 次のサイクル（約6秒）で`photos`に`source='local'`・`loc_source='exif'`・`display='photos/…'`の行ができ、`GET /api/v1/photos`が1件増え（3,078→3,083の一部）、`GET /photos/…`が200（バイト数は元ファイルと同じ109,794 B）。Pythonも同じ |
| S3 | GPSなし写真の位置補完 | GPSなし・錨の10分後の写真 → 次のサイクルで`loc_source='inferred'`・`infer_dt_s=600`・バケット±15分。`GET /api/admin/photos?filter=inferred`に出て、`/api/admin/status`の`infer_buckets`の±15分が987→988（Pythonも同じ）。一覧では`inferred=true`。窓の違う3枚（±2時間・±12時間）も§2のとおり |
| S7 | 重複の選別 | 同じバイト列の2枚 → `photo_quality`に`exact`のグループができ、推奨外の1枚が`/api/v1/photos`から消えて`exact_suppressed`が94→95（一覧は+1枚だけ）。3分以内のほぼ同じ写真の2枚 → `similar`のグループ。両方とも一覧に残る（Pythonの規則と同じ）。Pythonと`kind`・順位・推奨・グループ番号まで一致 |
| S2 | Driveの写真（GPS有）の追加 | **本物のDriveは鍵待ち**（§6）。スタブのDriveで確かめた範囲: `files.list`（`imageMediaMetadata`つき）と`files.get`を差したテスト`drive::tests`（61件）と、Pythonの`index_drive`・`fetch_derivatives`の結果と全列を突き合わせる`matches_the_python_fixture`（`tools/gen_drive_index_fixture.py --check`）。失敗が続く写真があっても後続を取得できること（受入条件②）、`tmp/.dl_*`を`create_new`＋`O_NOFOLLOW`で作ること（受入条件③）、通信の障害を写真の失敗に数えないことも、スタブのテスト（[server.md §14.9](server.md#149-テスト)）。検証環境の本番コピーでは、鍵がないので2段を飛ばし、他の段が動くことは§1・§2で確認済み |
| S4 | 手動GPSと再推定の波及 | **P3の対象**（更新APIがまだ無い）。手動GPSの上書き・取り消しの扱いはP3で決める（§5） |

## 4. miharashiでの表示の継続

[`tools/miharashi-feed-check/`](../../tools/miharashi-feed-check/README.md)（miharashiのコミット`1bc73a6`。本番の`miharashi.service`・`/etc/miharashi`には触れていない）を、**取り込みを有効にしたRust（複製の`catalog.db`、`GEOFRAME_INGEST=1`・`GEOFRAME_DRIVE_POLL_MIN=0.5`の一時的なunit）**に向けた。この間もRustは30秒おきに同期サイクルを回している（変更を伴う段は0）。

| 試験 | 結果 |
| --- | --- |
| 定常（200枚） | **終了コード0**。写真185枚＋旅・季節のカード15枚（計200）。`Retrying`0回。11秒。表示対象は3,029枚 |
| 停止と再開（300枚。10秒後に`systemctl stop`、8秒後に`start`） | **終了コード0**。写真274枚＋カード26枚（計300）。`Retrying`4回・`Online`1回で自動回復（キー操作なし）。最初の`Retrying`から次の写真まで15.1秒。33秒 |
| `tools/compare_with_python.py`（Rust複製 :8081と、変更していない比較用Python :8090） | `/api/v1/photos`は**3,065件でJSON完全一致（型も区別、並びも一致）**。管理画面の読み取りAPI 222通り一致、画像3,065/3,065件がバイト一致、静的配信・拒否・メソッド・Hostも一致。**想定外の差0項目** |
| 取り込みの副作用 | 一時的なunitのログでERROR 0。WARNは比較ツールが送る拒否の見本（Host違い・未実装メソッド）だけ。複製の`.dump`のSHA-256は試験の最後も`57f6dceff3b48bc5`のまま（取り込みを有効にしたまま、本番コピーに対する取り込みは何も変えない） |

## 5. P3以降へ回したもの

[server.md §0](server.md)で「P3で決める」とした、現行（Python）のままの挙動。P2はPythonと同じにしてあり、今回の比較の入力にも含めていない。**P3のT3.7（2026-10-25）で①〜⑤を決めて実装した**（下の各行の末尾。結果は[inventory.md §2.3](inventory.md)）。

| 内容 | 詳細 | 回す先 |
| --- | --- | --- |
| ローカル写真の`mtime`が変わると、手動で直した位置（`loc_source='manual'`）も`exif`かNULLに上書きされる | [server.md §13.7・§13.10](server.md#137-pythonとの差まとめ) | P3（更新APIと一緒に決める）→ **T3.7で手動の位置を残す形に変更済み** |
| Driveの`md5`が変わると、手動の位置が`drive_meta`かNULLに上書きされる | [server.md §14.8・§14.12](server.md#148-pythonとの差まとめ) | P3 → **T3.7で手動の位置を残す形に変更済み** |
| 位置が動いたとき（推定の取り消し・変更、手動の位置）に、地名（`place elev muni_cd pref city`）を付け直すか | [server.md §15.2の5・§15.11](server.md#152-infer) | P3 → T3.7の決定の範囲外で今のまま（残した課題） |
| S4（手動GPSと再推定の波及） | 更新API（地名検索・SAVE）がP3 | P3（T3.8で確かめる） |
| 次の**一次データが消える・変わる経路**。Rustは今のPythonと同じ挙動のままで、P3で「今の仕様を残すか、守る仕様にするか」を決める（[inventory.md §2.3](inventory.md)）: ①索引からの削除（Drive・ローカルから消えた行）と`organize`で、お気に入り・除外（`hidden`）・手動の位置も行ごと消える ②Driveの更新（`md5`の変更）で回転が`0`に戻る ③削除→再追加で写真IDが変わり、旅行ブログの下書きの参照が切れる（書き出しが400） ④大量削除の保留（50件超かつDriveの行の20%超）は、意図した削除も配信に残す。保留の件数と`--allow-mass-delete`での解除方法を管理画面で知らせる案 **→ T3.7（2026-10-25）で決定・実装**: ①は手動の位置を残す、②③は今のまま、④は書き出しが使えない写真を飛ばして`skipped`で知らせる、⑤は管理画面に表示（[inventory.md §2.3](inventory.md)の「P3で決めたこと」） | P3（T3.7で完了） |
| AIワーカーが`photo_quality_features`を読む | [server.md §16.11](server.md#1611-回したもの) | P4 |
| Rustで作った品質の結果をPythonへ切り戻す手順の試験（本番での最終確認） | [server.md §16.10](server.md#1610-rustで作った結果をpythonへ切り戻すp5のロールバック) | P5 |
| `GEOFRAME_INGEST=1`のガード（検証環境の印・Pythonの非稼働）を外し、本番の設定に`1`を足す | [server.md §11.1.1](server.md#1111-本番への誤指定の防止ガード) | P5（切替） |

## 6. P2時点の未実施（T3.10で確認済み）

P2実施時は検証環境にDriveの鍵が無く、次は行っていなかった。**T3.10で確認済み**（[drive-acceptance.md](drive-acceptance.md)。S2はDriveへの新規アップロードに代え、複製DBの未登録行で確認。10ビット・HDRなど標本に無いモードは未確認）。手順は[server.md §14.11](server.md#1411-鍵が置かれたあとの管理者の確認手順)にある。本番機では行わない。

| 確認 | 内容 | 手順 |
| --- | --- | --- |
| 本物のDriveでの冪等性 | 本番コピーに対する`sync --dry-run`で`index_drive`の`added`・`updated`・`removed`がほぼ0、`fetch_derivatives`の`pending`が取得待ちの総数と合うこと（§1の`index_drive`・`fetch_derivatives`の行は「省略」のまま） | §14.11の(1) |
| 原本の形式別の成功・失敗をPythonと比べる（T2.4の受入条件①） | 使い捨ての置き場でRustとPythonが同じ原本を変換し、Rustだけが失敗する写真が0であること | §14.11の(2) |
| 実機iPhoneのHEIC（グリッド・HDR・10ビットなど。T2.2から持ち越し） | `geoframe img-probe`で分類 | §14.11の(3) |
| S2の実機（Driveにフォルダの写真を追加→サイクル→`loc_source='drive_meta'`・`display='derivatives/<drive_id>.jpg'`・E23が200） | **合成のJPEG**（`tools/gen_acceptance_photos.py`のa01。公開の代表点・架空の時刻）をDriveのフォルダへ足し、周期同期→行・derivative・`GET /derivatives/…`が200→もう一度の周期で変更0（終わったらDriveから消す） | §14.11の(4)と[inventory.md §5](inventory.md)のS2 |

## 7. 検証環境に加えた変更（戻した）と最終状態

| 変更 | 戻し方・結果 |
| --- | --- |
| `/usr/local/bin/geoframe`（`.prev`は直前の世代）を`rust-deploy.sh`で配布し直した | そのまま（`geoframe-rs`は`ingest=0`のまま稼働） |
| 使い捨ての置き場`/var/tmp/gf-t27/`（`catalog.db`の複製3つ・`derivatives/`のハードリンク・合成の写真・`.cache.json`の写し）と、一時的なunit（`gf-t27-rs`・`gf-t27-py`はtransient、`gf-t27-rs2`は`/etc/systemd/system/gf-t27-rs2.service`）、`/tmp`の比較スクリプト | **すべて削除**（`daemon-reload`済み。ハードリンクは複製側の名前を消しただけで、本番コピーの`derivatives/`の実体は消えていない） |
| 比較用Python（`geoframe-pyref`、:8090）のデータ`/var/lib/geoframe-pyref` | **触れていない**（Python側の複製は使い捨ての置き場に作った） |

最終状態（配布後・すべての試験のあと）:

| 項目 | 結果 |
| --- | --- |
| `geoframe-rs` | active、起動ログは`ingest=0`・`取り込みは無効です（DB に書きません）` |
| `GET /healthz` | 200 |
| `GET /api/v1/photos` | 3,065件 |
| 本番コピー（`/var/lib/geoframe/catalog.db`）の`.dump`のSHA-256 | `57f6dceff3b48bc59c1e4b5eb861c48101f2fe9f59591eae45b826ecc2528977` — **配布前と一致**。`photos`6,535行、ローカルの行0 |
| 比較用Python（:8090） | active、`/api/v1/photos` 3,065件 |
| 一時的なunit・使い捨ての置き場 | 0 |

## 8. 結論

P2の受入（[plan.md](plan.md) P2）の、検証環境で確かめられる部分は満たした。

- 本番コピーに対して、Rustの`sync --dry-run`は、実行した段の変更が0（`index_drive`・`fetch_derivatives`の2段は鍵が無く省略。Driveの2段は未確認で鍵待ち）、実サイクルを2回回しても`.dump`が元と**完全一致**。
- 同じ写真（GPSあり・なし、近い撮影時刻、同じバイト列、ほぼ同じ写真、HEIC、国外）を比較用PythonとRustに入れると、`photos`の全列・推定・地名・品質の顔ぶれ・`kind`・配信から外れる数が一致する。差はHEICのderivativeの画素と実行時刻だけで、既知の差で説明できる。
- 取り込みを有効にしたRustで、miharashiの取得処理が定常・停止と再開とも続き、Pythonとの全件比較の差は0。
- 本物のDrive（S2の実機・原本の形式別の比較・実機のHEIC）は、鍵が置かれてから管理者が行う（§6）。残り（手動GPSの扱い、地名の付け直し、本番の切替）はP3以降。
