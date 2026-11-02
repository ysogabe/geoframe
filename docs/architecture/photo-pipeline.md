# 現行写真パイプライン

更新日: 2026-11-01（B: 一覧用サムネイル `/thumb/` の節を追加。2026-10-30 の P5b で Python版の削除に合わせて、実装の記述をRust版へ直した。AIの詳細は[rust-migration/ai-config.md](../rust-migration/ai-config.md)ほか）

旅行ブログ連携の調査から出発し、写真の取り込み・品質解析・AI選定・表示APIまでの現行仕様を
まとめた文書です。

## 写真カタログ

- DBは `GEOFRAME_DATA_DIR/catalog.db`（既定は `./var/catalog.db`）。SQLite WALモードで、書き込みは1本の接続で行う。
- 写真は `photos.id` が内部主キー。Drive写真は `drive_id`、ローカル写真は `path` で原本を参照する。
- Google DriveがDrive写真の原本。サービスアカウントの読み取り専用権限で一覧と `imageMediaMetadata` を取得する。
- 表示用画像は `GEOFRAME_DATA_DIR/derivatives/`（既定は `./var/derivatives/`）の長辺2560px JPEG。`photos.display` にデータルートからの相対パスを記録する。
- 撮影時刻は `taken_at`（`YYYY-MM-DD HH:MM`）。元EXIF文字列、UTC、タイムゾーンは現在のDBには分離保存されていない。
- 位置は `lat` / `lon`。由来は `loc_source`（`exif`、`drive_meta`、`inferred`、`manual`）。推定元は `inferred_from`、時間差は `infer_dt_s`。
- 地名は `place` / `pref` / `city`、標高は `elev`。
- Driveの変更検知に `md5`、ローカル変更検知に `mtime` を使う。表示用derivativeのSHA-256、64-bit pHash、鮮明度・露出・クリッピングは`photo_quality_features`へ保存する（原本ハッシュではない）。
- 事前検証した品質解析は本体へ実装済み。同期サイクルは新規・更新写真だけを`GEOFRAME_QUALITY_PER_CYCLE`件ずつ解析し、全表示写真の特徴量が最新になった時だけ`photo_quality`を原子的に再生成する。途中では前回の完成結果を維持する。
- 初回や保守時は `geoframe quality-scan`（`GEOFRAME_INGEST=1`が要る）で未解析写真を一括バックフィルできる。通常運用ではサーバー起動中の同期ワーカーが自動処理する。
- 表示APIは完全重複の非代表を除外する。AI未解析・比較待ち・API失敗・別場面判定は従来どおり表示可能にする。同一場面としてGemini比較が完了したグループだけ非推奨を抑止し、`favorite`は常にAI判定より優先する。Rust版は、抑止するメンバーの世代（特徴量の`display`・`md5`・`mtime`・内容SHA）が今の写真と合い、同じグループに有効で表示可能な推奨写真があるときだけ抑止する（代表を削除・更新・非表示にしたあと残りが隠れ続けない。[ai-index.md](../rust-migration/ai-index.md)）。解析後に原本または表示画像が変わった結果は、`md5` / `mtime` / `display`照合により自動的に無効になる。元の写真が横倒し・逆さまのまま保存されている（EXIFの向きも無い）ものは、`/admin`の↻で時計回りに90°ずつ回して直す。回した角度は`photos.rotation`に残し、derivativeは`<drive_id>.r<角度>.jpg`の別名で作り直す（`/derivatives/`はブラウザにキャッシュさせているため）。`display`が変わるので、品質解析・embedding・同一場面の比較はその写真の分だけ作り直される。Drive側で内容が変わったときは`rotation`を0に戻す。
- AI選定はDrive同期と独立した`ai_worker`が増分実行する。SHA-256完全重複を除いた各写真をembeddingし（embeddingの接続先・モデルは`GEOFRAME_AI_CONFIG`のJSONで決める。画像と検索語が同じ空間に入る）、撮影時刻±15分とcosine類似度（既定は0.82。`GEOFRAME_EMBED_BACKEND=local`のときは0.83）で同一場面候補を作る。各グループは鮮明度・露出・コントラスト・クリッピングの固定の式（学習済みの順位付けモデル`GEOFRAME_RANK_MODEL`があれば、品質特徴とembeddingから点数を出す線形モデル）で順位を付けて最大6枚へ絞り、Gemini Flash-Liteが人物・犬猫・開眼・顔切れ・水平・美的品質を比較する。見せ方は`photo_ai_groups.judge_order`（`rank`／`shuffled`）と`presented_json`（見せた写真IDの順）に残す（写真を順位でなく並べ替えて見せる割合を決める`GEOFRAME_AI_JUDGE_SHUFFLE_RATE`は旧Python版だけの項目で、Rust版は読まない）。
- AI派生データは`photo_ai_features`、`photo_ai_groups`、`photo_ai_group_members`へ保存する。`photo_ai_features`は今のモデルのembeddingと判定を1行に持ち、embeddingのモデルを切り替えても写真の中身（SHA-256）が同じなら判定は残す。モデル別のembeddingは`photo_embeddings`にキャッシュし、切り替えて戻したときに作り直さない（キャッシュからの分は1サイクルの上限に数えない）。途中のグループは`pending`、完成は`judged`、一時失敗は`error`。グループと表示候補はトランザクションで確定する。
- AI処理量は`GEOFRAME_AI_EMBED_PER_CYCLE`（既定12）、`GEOFRAME_AI_JUDGE_PER_CYCLE`（既定2）、`GEOFRAME_AI_POLL_S`（既定60秒）で制限する。`GEOFRAME_AI_ENABLED=0`で停止できる。
- ユーザー評価は `favorite` と `hidden`。rating・表示回数はない。
- `GEOFRAME_INFER_OFFSET_MIN` は位置推定時のカメラ時計補正に使う。写真ごとの補正値・根拠は未保存。
- 位置なし写真の撮影地候補は`ai_worker`が写真選定の後に別処理として増分実行する（Rustでは`ai/location_clue.rs`・`ai/location_suggest.rs`）。まず同じ機体のDecisionサーバー（gpu-evalの常駐のd3-lite。同じ機体のLAN入口経由で呼び、接続先は`GEOFRAME_AI_CONFIG`のJSONの`local_base_url`）が写真ごとに「撮影地の手がかりがあるか」と被写体の種類を判定し、`photo_loc_clues`へ保存する。同じ日の写真（12時間以上の空きか日付の変わり目で区切る）をまとめ、手がかりあり（`clue`≥0.2または`kind=landmark`）の上位6枚だけをクラウドのGemini（モデルは`GEOFRAME_AI_CONFIG`のJSONで決める）へ送って撮影地を推定させる。判定の済んでいない写真がある日、手がかりのある写真が無い日は送らない。Decisionが応答しない間は判定が進まないので、家の中のペット写真などが判定なしでGoogleへ送られることはない。Decisionはgpu-evalの常駐なので起動を気にしなくてよい（常駐をやめて必要な間だけ起動する運用に戻す場合の毎朝5:00のタイマーは[デプロイ §2.3](../../deploy/README.md#23-撮影地の手がかり判定を毎朝動かす任意)）。推定した地名はNominatimで座標にし、`photo_loc_runs`（問い合わせ1回分）と`photo_loc_suggestions`（写真ごとの候補）へ保存する。候補は`photos.lat/lon`に書かず、管理画面の位置指定で候補の場所へ地図を動かすだけにする。確定は人が地図をクリックしてSAVEしたとき（`manual`）に限る。処理量は`GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE`（既定10枚）と`GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE`（既定2日）、`GEOFRAME_LOC_SUGGEST=0`で停止できる。
- 外部AIは写真選定と撮影地候補にだけ使用する。用途ごとの送り先は`GEOFRAME_AI_CONFIG`のJSONで決める（`GEOFRAME_EMBED_BACKEND`は旧データの識別で、送り先を切り替えない）。画像と検索語のembeddingを`provider: local`にすると、EXIF除去・縮小したderivativeと検索文はLAN内のembeddingサービスへだけ送り、クラウド（Agent Platform）へ送るのは、クラウドを許した用途（同一場面の比較評価に回す候補（最大6枚）と、撮影地の推定）だけになる。`ai_caption`列は未使用で、旅行ブログ本文は引き続き決定的な事実アウトラインを生成する。

## API・UI

- サーバーはRustの単一バイナリ（`geoframe serve`。axum・tokio）。家庭内LAN利用を前提とし、Host検証と同一オリジンPOST制限がある。
- 表示クライアント向けの写真一覧は `GET /api/v1/photos`（[写真フィードの契約](photo-feed-contract.md)）、管理用ページング一覧は `GET /api/admin/photos`。
- AI検索は`GET /api/ai/search`、進捗は`GET /api/ai/status`、管理画面の写真タブ（`/ai-search`も同じ画面）。通常の管理画面STATUSにもembedding・比較待ち・表示抑止数を表示する。検索結果の写真にも由来・近い時刻の位置のヒント・撮影地候補を付ける（管理一覧と同じ）。
- 管理用一覧（`GET /api/admin/photos`）は、位置なし写真にAIの撮影地候補（`suggest`）を付ける。`filter=suggest`で候補のある位置なし写真だけを返す。STATUSにはDecisionの接続状態、手がかり判定の進み具合、推定済みの日数を表示する。表示クライアント向けの写真フィードには候補を含めない。管理用一覧は、どのフィルタでも位置なしの写真に近い時刻の位置のヒント（`hint`）を付ける。`GET /api/admin/status` の `db.filters` は、フィルタごとの件数を返す。
- 管理画面のライブラリタブは `GET /api/admin/library/{trips,photos,points,calendar,places}`（GETの完全一致。`Cache-Control: no-store`）で、旅（撮影日時が48時間を超えて空いたところで区切る）・サムネイルの格子・地図の点（上限20,000点。超えると新しい順に切って`truncated`）・日付と場所ごとの件数を返す。除外した写真は`hidden=1`を付けたときだけ含める。新しい表は使わないので、読み取り専用の起動でも動く。設計は[admin-library/design.md](../admin-library/design.md)。
- 管理画面の選別タブは、品質解析（`photo_quality`。今の写真と世代が合う行だけ）の2枚以上のグループを1組1段で出し、人が代表を決める。`GET /api/admin/curation/groups`（`state=unconfirmed|confirmed|all`・`kind=all|exact|similar`・`ids=`・`page`・`per`。GETの完全一致。`Cache-Control: no-store`）が組ごとに状態（`unconfirmed`・`confirmed`・`recheck`）・メンバー・`token` を返し、`POST /api/admin/curation/confirm`（代表の写真、または解析の推奨のまま確認済みにする `null`）と `POST /api/admin/curation/reset`（人の記録を消す）が書く。書き込みは他のPOSTと同じ経路（Content-Typeと同一OriginのCSRF検査・本文64 KiB・`INGEST=0`は503）で、`token`（メンバーの集合と各写真の世代・解析の推奨・人の記録の印）が今のグループと違えば何も書かず409 `group_changed`（画面はその段を読み直す）。代表に選べるのはフィードに出せる写真（除外でなく位置がある）だけで、違えば400 `representative_not_visible`。人の記録は専用の表 `photo_curation` に写真ごとに持ち、`quality-scan` の再構築で消えない。人が選んだ代表は `GET /api/v1/photos` の `quality_recommended` と exact グループの推奨外の除外に反映される。状況タブの `db.quality.curation_pending` が未確認・再確認の組数。設計は[admin-library/design.md](../admin-library/design.md)。
- 管理画面の運用タブは、同期ワーカー（`serve` の定期サイクルと同じ1本のスレッド）に運用の要求を入れ、進み具合を見せる。`GET /api/admin/ops`（GETの完全一致。`Cache-Control: no-store`。HEAD・POSTは403）は実行中・待ちの要求、直近の定期サイクル、次の予定、履歴（新しい順に10件）、大量削除の保留（`hold`。件数・理由・`token`・`releasable`。行のIDは出さない）を返す。メモリ上の掲示板と保留の記憶の写しだけで、DBは読まない（再起動で消える）。段の失敗は結果の語（`failed`）だけで、文面（パス・SQLiteのメッセージ）は出さずログにだけ残る。`INGEST=0`でも200で、`ingest:false` と空の項目だけを返す。操作は `POST /api/admin/ops/sync`（定期サイクルと同じ段を1回）・`/quality-scan`（`geoframe quality-scan` と同じ一括解析）・`/drive-release`（本文 `{"token":"<16桁の小文字hex>"}`。画面が見た保留の行の集合に限って大量削除の保留を解除する）で、受け付けたら200 `{ok, request:{id, kind, state:"queued"}}`（`drive-release` は承認した件数 `approved` つき）を返し、実行はワーカーが順に行う（結果は `GET` の履歴で読む）。要求は1つだけ待てる。失敗は `{ok:false, error:<符号>}`: 409 `busy`（待ちがある・同じ種類が実行中）、409 `too_soon`（Driveを読む `sync`・`drive-release` は前回の終了から60秒以内）、`drive-release` はさらに409 `no_hold`（保留が無い）・409 `not_releasable`（一覧が空のための保留 `empty_listing`。画面から解除せず、CLIの `--allow-mass-delete` だけ）・409 `hold_changed`（`token` が今の保留と違う。実行の直前にもワーカーが照らし直し、違えば実行せず履歴に `hold_changed`）、`token` の形が違えば400 `bad_params`。`INGEST=0` と停止の合図のあとは503。入口の検査（Content-Typeと同一OriginのCSRF検査・本文64 KiB）は他のPOSTと同じ。**`token` は認可ではなく、古い画面で解除してしまうのを防ぐ印**（認証の無いLANでは `GET` を読める相手は誰でも得られる）。相手の正当性は[ADR 0008](../decisions/0008-photo-platform-and-miharashi-display.md)の前提（家庭LAN・外部公開時はCloudflare Access）に任せる。設計は[admin-ops/design.md](../admin-ops/design.md)。
- 管理画面の設定タブは `GET /api/admin/config`（GETの完全一致。読み取り専用）で、起動時に読んだ設定を項目ごとの出どころ（引数・環境変数・既定）つきで返す。AI用途の送り先・モデル・今日（UTC）の送信数・無効の理由も含み、ユーザーの決定で鍵のパスや接続先も設定どおりに出す（鍵の中身は出さない）。
- 管理画面の地名検索（`GET /api/admin/geocode`）は地理院のアドレス検索が0件のときだけNominatim（OpenStreetMap）で引き直し、海外の地名も探せる。Nominatimへの問い合わせは全体で1秒1件に絞り、結果はキャッシュする。
- 旅行ブログ機能は、`GEOFRAME_BLOG=1`のときだけ有効（既定は無効）。`GET /api/blog/*` から写真カタログを読み取り、正確なGPSをレスポンスへ含めない。
- レビュー状態は再構築可能な写真索引と混ぜず、`GEOFRAME_DATA_DIR/travel_blog/drafts/` のJSONへ保存する。
- エクスポートは `GEOFRAME_DATA_DIR/blog_exports/<trip_id>/`。画像は再エンコードしてEXIFを除去する。

## 一覧用サムネイル（`GET /thumb/<id>.jpg?v=<gen>`）

管理画面の一覧（ライブラリ・選別）が、長辺2560pxの表示用画像を読まずに済むように、長辺320pxのJPEG（品質80）を要求時に作って配る。設計は[admin-library/design.md](../admin-library/design.md)。環境変数は足していない（大きさ・上限・同時数は定数）。

- 経路: `<id>`は数字だけ（`^[1-9][0-9]{0,17}\.jpg$`の1段。先頭の0・末尾の`/`・2段以上は404）。`v`は12桁の小文字hex。どちらも違えば404。クライアントの文字列は`<id>`と`v`以外使わない。静的配信の白名簿より前に判定する。アクセスログのラベルは`/thumb/{id}`。
- 世代`gen`: `display|md5|mtime|THUMB_VERSION`のSHA-256の先頭12 hex（NULLは空）。回転・原本の更新で`display`や`mtime`が変わるとURLが変わるので、`Cache-Control: private, max-age=604800`で長く持たせる。サムネイルの大きさや品質を変えたら`THUMB_VERSION`を上げる。
- 元ファイル: `photos.display`（`derivatives/`か`photos/`で始まり、`..`・絶対パス・NUL・ドット始まりの要素を含まないもの）。`display`が無ければ`derivatives/<drive_id>.jpg`（`drive_id`は英数字・`_`・`-`だけ）。元ファイルも返すファイルも領域（`derivatives/`・`photos/`・`thumbs/`）の中に限り、`/derivatives/`と同じ検査をする（領域とファイルをリンクごと解決し、外へ出るリンクとディレクトリは403、その他の特殊ファイルは404）。
- キャッシュ: `GEOFRAME_DATA_DIR/thumbs/<id>-<gen>.jpg`。`.part`に書いてから`rename`する（`derivatives/`と同じ）。成功したら同じ写真の古い世代のファイルを消す（消す直前にDBの世代を引き直し、自分の世代が今の世代のときだけ）。起動時に`thumbs/`の書きかけの`.part`も掃除する。応答は`ETag: "<gen>"`・`Last-Modified`・`Cross-Origin-Resource-Policy: same-origin`を付け、`If-None-Match`が一致すれば304。
- **生成する条件**: `GET`で、`v`がDBから計算した今の世代と一致し、書き込み側がある（`GEOFRAME_INGEST=0`でない）ときだけ。それ以外は生成しない: `HEAD`と古い`v`は、キャッシュがあればそれ、無ければ404。`Sec-Fetch-Site`が`cross-site`・`same-site`なら403（HTTPSで配るときだけ効く重ねがけ）。`INGEST=0`でキャッシュが無いときは、下の「元の画像を返す」と同じ。`gen`は同一オリジンの管理APIを読まないと分からないので、外部のページの`<img>`が連番のIDで生成を起こすことはできない。
- 資源: 同じ写真は写真IDごとのロックで1つだけ作る。同時に作るのは2つまで。ロックと枠を合わせて待ちは最長10秒・待つ要求は最大32件で、超えたら503と`Retry-After: 5`。取ったロックと枠は`spawn_blocking`の生成が終わるまで持つので、要求が切れても復号が枠を超えて走らない。サムネイル専用の復号の上限は、入力64 MiB・5,000万画素（表示用画像の作成は512 MiB・約1.79億画素）で、全体を復号する前にファイルの大きさとヘッダの寸法で断る。
- 失敗の扱い:
  - 復号できない・上限を超える写真は404（`Cache-Control: no-store`）。画面は「画像なし」にする。同じ世代は最大10,000件の記憶に入れ、起動中は作り直さない（ロックも枠も取らずに404。写真が変わって世代が変われば試す）。
  - サムネイルは作れたが`thumbs/`に書けないとき（ディスクの満杯・権限・`thumbs/`がリンク）と、書き込み側が無くてキャッシュも無いときは、元の表示用画像をそのまま返す（拡張子がjpg/jpeg/pngのときだけ。`no-store`。`Last-Modified`・304は付けない）。書けない失敗のあと60秒は、どの写真も生成を試みない。
  - ログ（WARN）は失敗の初回に1行で、写真IDと分類（`decode`/`limit`/`write`）だけ。パスやエラーの文面は出さない。
- `GET /api/admin/photos`・検索API（`/api/ai/search`の写真）・`POST /api/admin/rotate`は、`thumb_url`（`/thumb/<id>.jpg?v=<gen>`。`thumb`が空なら空）で版付きのURLを返す。世代は`thumb_generation`と同じ入力（DBの`display`・`md5`・`mtime`）で作る。
- 外部公開のとき、`/thumb/`は管理用の経路で、Cloudflare Accessの保護の外に出してはいけない（連番のIDで除外した写真も引ける）。

## 既知の不足

- `taken_at_raw` / UTC / local / timezone / offset / normalization confidence の分離がない。
- SHA-256とembeddingはいずれも表示用derivative由来で、Drive原本そのもののハッシュではない。
- AIバックフィル中は未解析写真を表示可能なままにするため、全件完了まで選定率は段階的に上がる。
- 撮影地候補の関門の基準（`clue`≥0.2または`kind=landmark`）は、位置なし写真64枚に1人が付けたラベルで決めた暫定値。ラベルと基準の判定は[tools/loc_clue_eval](../../tools/loc_clue_eval/README.md)（測り直すスクリプトはP5bで削除した）。Geminiの確信度は当てにならないため、候補は必ず人が確認する。
- 海外写真の粗い地名が未解決の場合がある。ブログ候補では旅程の場所ラベルへフォールバックする。
- 公開は自動化しない。静的HTMLを書き出した後、利用者が内容を確認して別途公開する。
