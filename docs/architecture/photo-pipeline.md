# 現行写真パイプライン

更新日: 2026-10-13（ローカルembeddingの追加）

旅行ブログ連携の調査から出発し、写真の取り込み・品質解析・AI選定・表示APIまでの現行仕様を
まとめた文書です。

## 写真カタログ

- DBは `GEOFRAME_DATA_DIR/catalog.db`（開発時の既定は `app/var/catalog.db`）。SQLite WALモードで、`geoframe.store` が単一接続とロックを管理する。
- 写真は `photos.id` が内部主キー。Drive写真は `drive_id`、ローカル写真は `path` で原本を参照する。
- Google DriveがDrive写真の原本。サービスアカウントの読み取り専用権限で一覧と `imageMediaMetadata` を取得する。
- 表示用画像は `GEOFRAME_DATA_DIR/derivatives/`（開発時は `app/var/derivatives/`）の長辺2560px JPEG。`photos.display` にデータルートからの相対パスを記録する。
- 撮影時刻は `taken_at`（`YYYY-MM-DD HH:MM`）。元EXIF文字列、UTC、タイムゾーンは現在のDBには分離保存されていない。
- 位置は `lat` / `lon`。由来は `loc_source`（`exif`、`drive_meta`、`inferred`、`manual`）。推定元は `inferred_from`、時間差は `infer_dt_s`。
- 地名は `place` / `pref` / `city`、標高は `elev`。
- Driveの変更検知に `md5`、ローカル変更検知に `mtime` を使う。表示用derivativeのSHA-256、64-bit pHash、鮮明度・露出・クリッピングは`photo_quality_features`へ保存する（原本ハッシュではない）。
- 事前検証した品質解析は本体へ実装済み。同期サイクルは新規・更新写真だけを`GEOFRAME_QUALITY_PER_CYCLE`件ずつ解析し、全表示写真の特徴量が最新になった時だけ`photo_quality`を原子的に再生成する。途中では前回の完成結果を維持する。
- 初回や保守時は `python server.py --quality-scan` で未解析写真を一括バックフィルできる。通常運用ではサーバー起動中の同期ワーカーが自動処理する。
- 表示APIは完全重複の非代表を除外する。AI未解析・比較待ち・API失敗・別場面判定は従来どおり表示可能にする。同一場面としてGemini比較が完了したグループだけ非推奨を抑止し、`favorite`は常にAI判定より優先する。Rust版は、抑止するメンバーの世代（特徴量の`display`・`md5`・`mtime`・内容SHA）が今の写真と合い、同じグループに有効で表示可能な推奨写真があるときだけ抑止する（代表を削除・更新・非表示にしたあと残りが隠れ続けない。[ai-index.md](../rust-migration/ai-index.md)）。解析後に原本または表示画像が変わった結果は、`md5` / `mtime` / `display`照合により自動的に無効になる。元の写真が横倒し・逆さまのまま保存されている（EXIFの向きも無い）ものは、`/admin`の↻で時計回りに90°ずつ回して直す。回した角度は`photos.rotation`に残し、derivativeは`<drive_id>.r<角度>.jpg`の別名で作り直す（`/derivatives/`はブラウザにキャッシュさせているため）。`display`が変わるので、品質解析・embedding・同一場面の比較はその写真の分だけ作り直される。Drive側で内容が変わったときは`rotation`を0に戻す。
- AI選定はDrive同期と独立した`ai_worker`が増分実行する。SHA-256完全重複を除いた各写真をembeddingし（`GEOFRAME_EMBED_BACKEND`。既定の`gemini`はGemini Embedding 2、`local`はこのサーバーのCPUで動くembeddinggemma-2。どちらも768次元で、画像と検索語が同じ空間に入る）、撮影時刻±15分とcosine類似度（Geminiは0.82、embeddinggemma-2は0.83）で同一場面候補を作る。各グループは鮮明度・露出・コントラスト・クリッピングの固定の式（学習済みの順位付けモデル`GEOFRAME_RANK_MODEL`があれば、品質特徴とembeddingから点数を出す線形モデル）で順位を付けて最大6枚へ絞り、Gemini Flash-Liteが人物・犬猫・開眼・顔切れ・水平・美的品質を比較する。Geminiは最初に見せた写真を推奨しやすいため、写真は順位どおりに見せるが、`GEOFRAME_AI_JUDGE_SHUFFLE_RATE`（既定0.2）の割合のグループだけgroup_idから決まる順に並べ替えて見せる。見せ方は`photo_ai_groups.judge_order`（`rank`／`shuffled`）と`presented_json`（見せた写真IDの順）に残し、並べ替えたグループの判定を順位付けモデルの学習と確認に使う。
- AI派生データは`photo_ai_features`、`photo_ai_groups`、`photo_ai_group_members`へ保存する。`photo_ai_features`は今のモデルのembeddingと判定を1行に持ち、embeddingのモデルを切り替えても写真の中身（SHA-256）が同じなら判定は残す。モデル別のembeddingは`photo_embeddings`にキャッシュし、切り替えて戻したときや`python -m geoframe.prefill_embeddings`で先に作った分は計算し直さない（キャッシュからの分は1サイクルの上限に数えない）。途中のグループは`pending`、完成は`judged`、一時失敗は`error`。グループと表示候補はトランザクションで確定する。
- AI処理量は`GEOFRAME_AI_EMBED_PER_CYCLE`（既定12）、`GEOFRAME_AI_JUDGE_PER_CYCLE`（既定2）、`GEOFRAME_AI_POLL_S`（既定60秒）で制限する。`GEOFRAME_AI_ENABLED=0`で停止できる。
- ユーザー評価は `favorite` と `hidden`。rating・表示回数はない。
- `GEOFRAME_INFER_OFFSET_MIN` は `geoframe.config.INFER_OFFSET_S` として読み込み、位置推定時のカメラ時計補正に使う。写真ごとの補正値・根拠は未保存。
- 位置なし写真の撮影地候補は`ai_worker`が写真選定の後に別処理として増分実行する（`geoframe/loc_suggest.py`）。まず同じ機体のDecisionサーバー（gpu-evalの常駐のd3-lite。同じ機体のLAN入口`http://127.0.0.1:8100/llm-decision`経由で呼び、要求に`X-Client-Name: geoframe`を付ける。`GEOFRAME_DECISION_URL`）が写真ごとに「撮影地の手がかりがあるか」と被写体の種類を判定し、`photo_loc_clues`へ保存する。同じ日の写真（12時間以上の空きか日付の変わり目で区切る）をまとめ、手がかりあり（`clue`≥0.2または`kind=landmark`）の上位6枚だけを`GEOFRAME_LOC_SUGGEST_MODEL`（既定`gemini-3.8-flash`）へ送って撮影地を推定させる。判定の済んでいない写真がある日、手がかりのある写真が無い日は送らない。Decisionが応答しない間は判定が進まないので、家の中のペット写真などが判定なしでGoogleへ送られることはない。Decisionはgpu-evalの常駐なので起動を気にしなくてよい（常駐をやめて必要な間だけ起動する運用に戻す場合の毎朝5:00のタイマーは[デプロイ §2.3](../../deploy/README.md#23-撮影地の手がかり判定を毎朝動かす任意)）。推定した地名はNominatimで座標にし、`photo_loc_runs`（問い合わせ1回分）と`photo_loc_suggestions`（写真ごとの候補）へ保存する。候補は`photos.lat/lon`に書かず、管理画面の位置指定で候補の場所へ地図を動かすだけにする。確定は人が地図をクリックしてSAVEしたとき（`manual`）に限る。処理量は`GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE`（既定60枚）と`GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE`（既定2日）、`GEOFRAME_LOC_SUGGEST=0`で停止できる。
- 外部AIは写真選定と撮影地候補にだけ使用する。`gemini`では検索時に検索文、取り込み時にEXIF除去・縮小したderivativeをAgent Platformへ送る。`local`ではembeddingをこのサーバーで計算し、Agent Platformへ送るのは比較評価に回す同一場面の候補（最大6枚）だけになる。`ai_caption`列は未使用で、旅行ブログ本文は引き続き決定的な事実アウトラインを生成する。

## API・UI

- サーバーはPython標準ライブラリの `ThreadingHTTPServer`。家庭内LAN利用を前提とし、Host検証と同一オリジンPOST制限がある。
- 表示クライアント向けの写真一覧は `GET /api/v1/photos`（[写真フィードの契約](photo-feed-contract.md)）、管理用ページング一覧は `GET /api/admin/photos`。
- AI検索は`GET /api/ai/search`、進捗は`GET /api/ai/status`、管理画面は`/ai-search`。通常の管理画面STATUSにもembedding・比較待ち・表示抑止数を表示する。
- 管理用一覧（`GET /api/admin/photos`）は、位置なし写真にAIの撮影地候補（`suggest`）を付ける。`filter=suggest`で候補のある位置なし写真だけを返す。STATUSにはDecisionの接続状態、手がかり判定の進み具合、推定済みの日数を表示する。表示クライアント向けの写真フィードには候補を含めない。
- 管理画面の地名検索（`GET /api/admin/geocode`）は地理院のアドレス検索が0件のときだけNominatim（OpenStreetMap）で引き直し、海外の地名も探せる。Nominatimへの問い合わせは全体で1秒1件に絞り、結果はキャッシュする。
- 旅行ブログ機能は `GET /api/blog/*` から写真カタログを読み取り、正確なGPSをレスポンスへ含めない。
- レビュー状態は再構築可能な写真索引と混ぜず、`GEOFRAME_DATA_DIR/travel_blog/drafts/` のJSONへ保存する。
- エクスポートは `GEOFRAME_DATA_DIR/blog_exports/<trip_id>/`。画像は再エンコードしてEXIFを除去する。

## 既知の不足

- `taken_at_raw` / UTC / local / timezone / offset / normalization confidence の分離がない。
- SHA-256とembeddingはいずれも表示用derivative由来で、Drive原本そのもののハッシュではない。
- AIバックフィル中は未解析写真を表示可能なままにするため、全件完了まで選定率は段階的に上がる。
- 撮影地候補の関門の基準（`clue`≥0.2または`kind=landmark`）は、位置なし写真64枚に1人が付けたラベルで決めた暫定値。ラベルと測り直すスクリプトは[tools/loc_clue_eval](../../tools/loc_clue_eval/README.md)。Geminiの確信度は当てにならないため、候補は必ず人が確認する。
- 海外写真の粗い地名が未解決の場合がある。ブログ候補では旅程の場所ラベルへフォールバックする。
- 公開は自動化しない。静的HTMLを書き出した後、利用者が内容を確認して別途公開する。
