# 設定リファレンス（環境変数）

> [!NOTE]
> この文書は**手で保ちます**（Python 版の `config.py` から作っていた生成物ではありません）。
> Rust のコードが読む `GEOFRAME_*` の名前は、すべて「Rust が読む環境変数」の節に載せます。
> 載っていない名前をコードに足したり、載せた名前をコードから外したりすると、
> `cargo test`（`crates/geoframe/tests/env_reference.rs`）が落ちます。
> 試験の中だけで使う名前は、その試験の除外リストに書いてあります。

## 読み込み順序

- 優先順位は **起動引数 > 環境変数 > 既定値**。起動引数があるのは待ち受け（`--listen <addr:port>`）とデータ置き場（`--data-dir <path>`）だけです。
- **`.env` は読みません。** 本番は systemd の `EnvironmentFile=`（共通は `/etc/default/geoframe`、Rust だけの項目は `/etc/default/geoframe-rs`）と `Environment=` で渡します。手元で試すときは `set -a; . ./環境ファイル; set +a` してから起動します。
- 環境変数が空文字のときは「未設定」と同じに扱います。
- `TZ` は読みません（時刻は文字列のまま扱います）。
- 値（鍵・トークンを含む設定）は、この文書にも PR にも書きません。ここに書くのは名前と意味だけです。

## Rust が読む環境変数

既定値は未設定時の値です。「Rust だけ」は、旧 Python 版には無かった項目です。

### 基本・取り込み

- `GEOFRAME_DATA_DIR` — 既定値 `./var`（起動したカレントディレクトリ基準）。データ置き場（`catalog.db`・`derivatives/`・`photos/`・各種キャッシュ）。本番では `/var/lib/geoframe` のようにソースと別の場所に置く。`derivatives/`・`photos/`・`blog_exports/` というディレクトリ名は変えないこと（DB の `display` 列が「`DATA_DIR/display` ＝実ファイル」かつ「`/display` ＝配信 URL」の二重の意味を持つため）。起動引数 `--data-dir` が優先。
- `GEOFRAME_HOST` — 既定値 `0.0.0.0`。開発時は `127.0.0.1` にするとこの端末だけに限定できる。起動引数 `--listen` は host と port の両方を置き換える。
- `GEOFRAME_PORT` — 既定値 `8080`。HTTP の待ち受けポート。
- `GEOFRAME_ALLOWED_HOSTS` — 既定値 空（`localhost` だけ許可）。許可する Host 名をカンマ区切りで足す。IP リテラル宛は無条件に許可するので、ここには名前だけを入れる（Tailscale や固定 DNS 名で開くとき。例: `geoframe.example.test:8080`）。
- `GEOFRAME_LOG` — Rust だけ。既定値 `info`。ログの詳しさ。
- `GEOFRAME_INGEST` — Rust だけ。既定値 `0`（無効）。`1` で取り込み（DB への書き込みと同期サイクル）を有効にする。`0` の間は、書き込みの API（旅行ブログの保存・書き出しなど）は書かずに 503 を返す。
- `GEOFRAME_DRIVE_POLL_MIN` — 既定値 `10`。同期サイクルの間隔（分）。
- `GEOFRAME_PHOTOS_DIR` — 既定値 `photos`。ローカル写真の置き場（`DATA_DIR` からの相対。絶対でも、解決した先が `DATA_DIR` の中なら可）。
- `GEOFRAME_FETCH_PER_CYCLE` — 既定値 `25`。同期 1 サイクルで derivative を作る写真の枚数。初回の Drive 全件取り込みもこの数ずつ進む。
- `GEOFRAME_QUALITY_PER_CYCLE` — 既定値 `GEOFRAME_FETCH_PER_CYCLE` と同じ。同期 1 サイクルで品質を解析する枚数（0 は 1 として扱う）。
- `GEOFRAME_DRIVE_KEY` — 既定値 `drive_key.json`（相対はカレントディレクトリ基準。本番は絶対パス）。Google Drive のサービスアカウント鍵 JSON のパス。設定が持つのはパスだけで、中身は読み込み時にしか触れない。
- `GEOFRAME_DRIVE_FOLDER` — 既定値 `GeoFrame`。取り込む Drive のフォルダ（名前、または 20 字以上の英数字なら ID）。
- `GEOFRAME_BLOG_PLANS_DIR` — Rust だけ。既定値 未設定（バイナリに埋め込んだ旅程を使う）。あれば埋め込みの代わりに、このディレクトリの `*.json` を旅程として読む（再ビルドなしで差し替えるとき。相対はカレントディレクトリ基準）。

### 位置推定・地図

- `GEOFRAME_INFER_MIN` — 既定値 `15`。位置推定の窓（分）。この範囲内の位置つき写真から推定する。管理画面の内訳の区切りにも使う。
- `GEOFRAME_INFER_OFFSET_MIN` — 既定値 `0`。カメラ時計のずれ補正（分。負でもよい）。推定する写真の時刻に足す。
- `GEOFRAME_TILE_UA` — 既定値 `GeoFrame/0.1 (personal photo frame)`。地図タイル・地名検索（地理院・Nominatim）の上流へ送る User-Agent。OSM の利用規約のため、実運用では連絡先入りにする（例 `GeoFrame/0.1 (admin@example.test)`）。既定値のままだとブロックされうる。ログには出さない。
- `GEOFRAME_TILE_CACHE_MAX_MB` — 既定値 `16384`。タイルキャッシュの上限（全レイヤー共有）。管理画面のディスク表示と掃除に使う。
- `GEOFRAME_TILE_FETCH_PER_MIN` — 既定値 `240`。上流取得（＝キャッシュミス）だけを対象にした毎分の上限（全体。0 は上流へ行かない）。キャッシュヒットは無制限。上限に達したら上流へ行かず透明 PNG を返す（`/tiles/` は無認証で、LAN 内の任意のページから上流を叩かせられるため）。

### AI

AI は用途（画像の embedding・検索語の embedding・写真の比較・撮影地の手がかり・撮影地の推定）ごとに、`GEOFRAME_AI_CONFIG` の JSON で routing・モデル・上限を決める。旧設定だけでは Rust は推論を始めない。詳細は [docs/rust-migration/ai-config.md](../rust-migration/ai-config.md)。

- `GEOFRAME_AI_CONFIG` — Rust だけ。既定値 未設定（AI の推論は動かない）。AI 設定の JSON のパス。routing・モデル・Provider・上限・予算はこのファイルが持つ。ファイルの形式は ai-config.md。中身（鍵のパスを含む）は文書に書かない。
- `GEOFRAME_AI_ENABLED` — 既定値 `1`。`1` 以外は全用途を無効にする（AI 検索は 503）。
- `GEOFRAME_AI_POLL_S` — 既定値 `60`。AI ワーカーの周期（秒）。30 未満は 30、上限は 86400。
- `GEOFRAME_AI_BACKLOG_POLL_S` — Rust だけ。既定値 `5`。仕事が残っているときの次の周期までの待ち（秒。1〜`GEOFRAME_AI_POLL_S` に収める。不正な値は既定へ戻し、AI は無効にしない）。
- `GEOFRAME_AI_EMBED_PER_CYCLE` — 既定値 `12`。画像 embedding の 1 周期の上限（枚）。
- `GEOFRAME_AI_JUDGE_PER_CYCLE` — 既定値 `2`。写真の比較（`photo_judge`）の 1 周期の上限（グループ数）。
- `GEOFRAME_AI_SEARCH_PER_MIN` — 既定値 `30`。検索語の embedding（`/api/ai/search`）の毎分の上限。
- `GEOFRAME_LOC_SUGGEST` — 既定値 `1`。`1` 以外は撮影地の手がかり・推定の用途を無効にする。
- `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE` — 既定値 `10`（旧 Python 版は `60`）。撮影地の手がかり判定の 1 周期の上限（枚）。Rust のワーカーは 1 スレッドが用途を順に処理するので、60 件続けると他の用途が長く止まるため 10 にしている。
- `GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE` — 既定値 `2`。撮影地の推定（クラウド）の 1 周期の上限（日の数）。
- `GEOFRAME_EMBED_BACKEND` — 既定値 `gemini`。管理画面の AI 件数が数える embedding の識別（`gemini` または `local`）。embedding を計算する場所を決める項目ではなく（Rust は `GEOFRAME_AI_CONFIG` の JSON で決める）、旧データの表示用の識別と、下の既定値の切り替えに使う。
- `GEOFRAME_AI_TIME_WINDOW_MIN` — 既定値 `15`。同一場面候補とみなす撮影時刻の窓（分）。この範囲内の写真だけを類似比較する。
- `GEOFRAME_AI_SIMILARITY` — 既定値 `0.82`（`GEOFRAME_EMBED_BACKEND=local` のときは `0.83`）。同一場面候補とみなす embedding の cosine 類似度のしきい値。
- `GEOFRAME_AI_SHORTLIST_MAX` — 既定値 `6`。1 グループで比較に回す最大枚数（2〜6 に収める）。
- `GEOFRAME_AI_VISUAL_SIMILARITY` — 既定値 `0.90`（`local` のときは `0.91`）。検索結果で「同じ構図」に畳む cosine 類似度のしきい値。0〜1 の有限の数だけ受け付ける。
- `GEOFRAME_RANK_MODEL` — 既定値 `DATA_DIR/models/rank_model.json`。同一場面グループの中の順位に使う学習済みモデル（JSON）。無ければ鮮明度・露出などの固定の式で順位を付ける。家庭の写真の評価から作るため、リポジトリには入れない。

## 旧版のみ（削除済み。Rust は読まない）

次は旧 Python 版（P5a のマージのコミット `a804623` まで存在）が読んでいた名前です。Rust は読まず、設定しても無視します（警告も出しません）。猶予期間中に Python 版へ戻すときだけ意味があります。Rust のコードがこれらの名前を読むようになったら、「Rust が読む環境変数」の節へ移します（試験が検査します）。

- `GEOFRAME_AI_JUDGE_SHUFFLE_RATE` — 既定値 `0.2`。写真の比較で、順位どおりでなく並べ替えて見せるグループの割合。
- `GEOFRAME_LOCAL_EMBED_MODEL_DIR` — 既定値 `DATA_DIR/models/embeddinggemma-2`。CPU の local embedding のモデルディレクトリ。
- `GEOFRAME_LOCAL_EMBED_THREADS` — 既定値 `4`。local embedding に使う CPU スレッド数。
- `GEOFRAME_DECISION_URL` — 既定値 `http://127.0.0.1:8100/llm-decision`。撮影地の手がかりを判定する Decision サーバー。Rust は `GEOFRAME_AI_CONFIG` の `local_base_url` に固定のパスを付けて呼ぶ。
- `GEOFRAME_LOC_SUGGEST_MODEL` — 既定値 `gemini-3.8-flash`。撮影地を推定する Gemini のモデル。Rust は `GEOFRAME_AI_CONFIG` の JSON で決める。

あわせて、旧 Python 版は `app/.env`（`GEOFRAME_*` を書いたファイル）と、Google の認証の環境変数（`GOOGLE_APPLICATION_CREDENTIALS`・`GOOGLE_CLOUD_PROJECT` など）を読んでいました。Rust はどちらも読みません（Google の認証は `GEOFRAME_AI_CONFIG` の JSON の `credential_path` が指す鍵のファイルで渡します）。
