# 設定リファレンス（環境変数）

> [!NOTE]
> この文書は `app/geoframe/config.py` から自動生成しています。手で編集せず、
> 変数や説明を変える場合は `config.py` の代入と直前のコメントを直したうえで
> `python3 tools/gen_env_reference.py` を実行してください。CIが差分の有無を検査します。

## 読み込み順序

- `app/.env` を読み込みます。書式は `KEY=VALUE` で1行1項目、`#` 始まりの行は無視します。
- **環境変数として直接指定した値が優先**されます（`.env` は未設定の項目だけを埋めます）。
- systemd の `Environment=` / `EnvironmentFile=` でも指定できます。

## 一覧

既定値は未設定時の値です。式で書かれている場合は `config.py` の記述をそのまま示します
（`ROOT` は `app/`、`DATA_DIR` はデータルートを指します）。定義順（＝機能のまとまり順）に
並べています。

- `GEOFRAME_DATA_DIR`（`config.py`: `DATA_DIR`） — 既定値 なし（未設定時は `ROOT / "var"`）。ソース（geoframe/ static/ vendor/）は ROOT 配下、書き換わるデータ （catalog.db・derivatives/・photos/・各種キャッシュ）は DATA_DIR 配下に置く。本番では GEOFRAME_DATA_DIR=/var/lib/geoframe のようにデータだけ別の場所へ移せる。※.env の読み込みは ROOT 側のまま。DATA_DIR は .env で指定できる必要があり、 .env を DATA_DIR 配下に置くと「読むために DATA_DIR が要る」鶏卵になるため。※derivatives/ photos/ blog_exports/ というディレクトリ名は変更しないこと。DBの display 列が「DATA_DIR/display＝実ファイル」かつ「/display＝配信URL」の 二重の意味を持っており、名前を変えると既存の全行が解決不能になる。
- `GEOFRAME_PHOTOS_DIR`（`config.py`: `PHOTOS_DIR`） — 既定値 `photos`。※PHOTOS_DIR は DATA_DIR 配下であること（drive.py が relative_to(DATA_DIR) で DBに入れる相対パスを作るため、外に出すと索引できない）
- `GEOFRAME_PORT`（`config.py`: `PORT`） — 既定値 `8080`。HTTPの待ち受けポート
- `GEOFRAME_HOST`（`config.py`: `HOST`） — 既定値 `0.0.0.0`。開発時は GEOFRAME_HOST=127.0.0.1 でこの端末だけに限定できる。実機の家庭内LAN配信は従来互換の 0.0.0.0 を既定とする。
- `GEOFRAME_FETCH_PER_CYCLE`（`config.py`: `FETCH_PER_CYCLE`） — 既定値 `25`。同期1サイクルで取り込む写真の枚数。初回のDrive全件取り込みもこの数ずつ進む
- `GEOFRAME_QUALITY_PER_CYCLE`（`config.py`: `QUALITY_PER_CYCLE`） — 既定値 `str(FETCH_PER_CYCLE)`。品質解析も同期1サイクルで処理量を制限する。未指定時はderivative生成数と揃え、 通常運用では新しく取り込んだ分を同じサイクル内で追いつけるようにする。
- `GEOFRAME_AI_ENABLED`（`config.py`: `AI_ENABLED`） — 既定値 `1`。AI写真選定はDrive同期と別スレッドで少量ずつ進める。無効化する場合だけ0を指定する。
- `GEOFRAME_AI_EMBED_PER_CYCLE`（`config.py`: `AI_EMBED_PER_CYCLE`） — 既定値 `12`。AI選定の1サイクルでembeddingする枚数
- `GEOFRAME_AI_JUDGE_PER_CYCLE`（`config.py`: `AI_JUDGE_PER_CYCLE`） — 既定値 `2`。AI選定の1サイクルでGemini比較まで進めるグループ数
- `GEOFRAME_AI_POLL_S`（`config.py`: `AI_POLL_S`） — 既定値 `60`。ai_workerが次のサイクルまで待つ秒数。30秒未満は30秒へ切り上げる
- `GEOFRAME_AI_TIME_WINDOW_MIN`（`config.py`: `AI_TIME_WINDOW_MIN`） — 既定値 `15`。同一場面候補とみなす撮影時刻の窓（分）。この範囲内の写真だけを類似比較する
- `GEOFRAME_EMBED_BACKEND`（`config.py`: `EMBED_BACKEND`） — 既定値 `gemini`。画像と検索語のembeddingを計算する場所。gemini=Agent PlatformのGemini Embedding 2、 local=このサーバーのCPUでembeddinggemma-2（写真をGoogleへ送らない。requirements-local-ai.txtが必要）
- `GEOFRAME_LOCAL_EMBED_MODEL_DIR`（`config.py`: `LOCAL_EMBED_MODEL_DIR`） — 既定値 なし（未設定時は `DATA_DIR / "models" / "embeddinggemma-2"`）。localのembeddinggemma-2のモデルディレクトリ。起動時にネットワークから取得しないため、 サービスから読める場所（既定はデータディレクトリの下）へ事前に置く
- `GEOFRAME_LOCAL_EMBED_THREADS`（`config.py`: `LOCAL_EMBED_THREADS`） — 既定値 `4`。localのembeddingに使うCPUスレッド数。同じPCの他の処理と分け合う
- `GEOFRAME_AI_SIMILARITY`（`config.py`: `AI_SIMILARITY`） — 既定値 なし（未設定時は `("0.83" if EMBED_BACKEND == "local" else "0.82")`）。同一場面候補とみなすembeddingのcosine類似度しきい値。既定はモデルごとに違う （embeddinggemma-2の0.83は、実写真でGemini Embedding 2の0.82と同じ分位点に当たる値）
- `GEOFRAME_AI_VISUAL_SIMILARITY`（`config.py`: `AI_VISUAL_SIMILARITY`） — 既定値 なし（未設定時は `("0.91" if EMBED_BACKEND == "local" else "0.90")`）。検索結果で「同じ構図」に畳むcosine類似度しきい値。既定はモデルごとに違う
- `GEOFRAME_RANK_MODEL`（`config.py`: `RANK_MODEL_PATH`） — 既定値 なし（未設定時は `DATA_DIR / "models" / "rank_model.json"`）。同一場面グループの中の順位に使う学習済みモデル（JSON）。無ければ鮮明度・露出などの 固定の式で順位を付ける。家庭の写真の評価から作るため、リポジトリには入れない
- `GEOFRAME_AI_SHORTLIST_MAX`（`config.py`: `AI_SHORTLIST_MAX`） — 既定値 `6`。1グループでGemini比較に回す最大枚数（2〜6へクランプ）
- `GEOFRAME_AI_JUDGE_SHUFFLE_RATE`（`config.py`: `AI_JUDGE_SHUFFLE_RATE`） — 既定値 `0.2`。Gemini比較で写真を順位どおりでなく並べ替えて見せるグループの割合（0〜1）。Geminiは最初に 見せた写真を推奨しやすいため、一部をgroup_idから決まる順に並べ替え、偏りのない判定を 順位付けの学習と確認に使う
- `GEOFRAME_LOC_SUGGEST`（`config.py`: `LOC_SUGGEST_ENABLED`） — 既定値 `1`。位置なし写真の撮影地候補（loc_suggest.py）を作るか。AI_ENABLED=1のときだけ動く。0で止める
- `GEOFRAME_DECISION_URL`（`config.py`: `DECISION_URL`） — 既定値 `http://127.0.0.1:8092`。撮影地の手がかりの有無を判定するDecisionサーバー（gpu-evalのd3-lite）。応答しない間は Geminiへ写真を送らない（家の中のペット写真などを外へ出さないための関門）
- `GEOFRAME_LOC_SUGGEST_MODEL`（`config.py`: `LOC_SUGGEST_MODEL`） — 既定値 `gemini-3.8-flash`。撮影地を推定するGeminiモデル。3.1-flash-liteは店名から所在地を決めつけやすかったため3.8-flashを使う
- `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE`（`config.py`: `LOC_SUGGEST_CLUE_PER_CYCLE`） — 既定値 `60`。1サイクルでDecisionに判定させる写真の枚数（1枚0.6秒前後）
- `GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE`（`config.py`: `LOC_SUGGEST_GEMINI_PER_CYCLE`） — 既定値 `2`。1サイクルでGeminiに撮影地を推定させる日の数（1回20秒前後）
- `GEOFRAME_AI_SEARCH_PER_MIN`（`config.py`: `AI_SEARCH_PER_MIN`） — 既定値 `30`。/api/ai/search の流量上限（分あたり）。LAN内の任意ページから連打GETで 全embedding行の毎回フルスキャン（3テーブルJOIN、グローバルDBロック内）を 誘発できるため、tiles.pyのスライディング窓と同型で抑制する。
- `GEOFRAME_DRIVE_KEY`（`config.py`: `DRIVE_KEY`） — 既定値 `drive_key.json`。Google Driveサービスアカウント鍵JSONのパス（ROOT＝app/ からの相対）
- `GEOFRAME_DRIVE_FOLDER`（`config.py`: `DRIVE_FOLDER`） — 既定値 `GeoFrame`。取り込み対象のDriveフォルダ名
- `GEOFRAME_DRIVE_POLL_MIN`（`config.py`: `DRIVE_POLL_S`） — 既定値 `10`。Driveの変更確認間隔（分）。値は秒へ換算して使う
- `GEOFRAME_INFER_MIN`（`config.py`: `INFER_WINDOW_S`） — 既定値 `15`。位置推定の錨に使う撮影時刻の窓（分）。この範囲内の位置つき写真から推定する
- `GEOFRAME_INFER_OFFSET_MIN`（`config.py`: `INFER_OFFSET_S`） — 既定値 `0`。カメラ時計のずれ補正（分）。撮影時刻が恒常的にずれる機体で調整する
- `GEOFRAME_TILE_UA`（`config.py`: `TILE_UA`） — 既定値 `GeoFrame/0.1 (personal photo frame)`。OSM tile usage policy 準拠に必須。実運用では連絡先入り（例 "GeoFrame/0.1 (me@example.com)"）を GEOFRAME_TILE_UA に設定しておくこと。既定値のままだとブロックされうる
- `GEOFRAME_TILE_CACHE_MAX_MB`（`config.py`: `TILE_CACHE_MAX_MB`） — 既定値 `16384`。タイルキャッシュの上限（全レイヤー共有）。容量を抑えたい場合は GEOFRAME_TILE_CACHE_MAX_MB で上書きする。
- `GEOFRAME_TILE_FETCH_PER_MIN`（`config.py`: `TILE_FETCH_PER_MIN`） — 既定値 `240`。上流フェッチ（＝キャッシュミス）だけを対象にした流量上限（枚/分）。キャッシュヒットは無制限。/tiles/ は無認証なので、LAN内の任意のWebページが <img src="http://<frame-ip>:8080/tiles/…"> を大量に並べるだけで上流を叩かせられる（クロスオリジンの<img>はSOPで止まらない）。上限に達したら上流へ行かず透明PNGを返すことで、OSM tile usage policy上の bulk downloading とみなされて自宅グローバルIPがBANされる事態とディスク枯渇を防ぐ。既定240の根拠: コールド起動時の1画面が実測116枚。通常の初回描画＋パン1回分に 約2倍の余裕を持たせた値（gsiの上流同時接続4でも詰まらない）。
- `GEOFRAME_ALLOWED_HOSTS`（`config.py`: `ALLOWED_HOST_NAMES`） — 既定値 （空文字）。許可するホスト名。IPリテラル宛（192.0.2.7等）は_host_ok側で無条件に許可するため、 ここには名前だけを入れる。GEOFRAME_ALLOWED_HOSTS はTailscaleや固定DNS名で 運用する場合の追加指定用（例: GEOFRAME_ALLOWED_HOSTS=geoframe.local:8080,frame.lan）

## `config.py` 以外が読む環境変数

以下は各モジュールが直接読みます（`config.py` を経由しないため上の一覧には現れません）。
設定方法と既定の挙動は、それぞれの変数の説明先を参照してください。

- `GOOGLE_APPLICATION_CREDENTIALS` — `ai_search.py`
- `GOOGLE_CLOUD_LOCATION` — `ai_search.py`
- `GOOGLE_CLOUD_PROJECT` — `ai_search.py`
- `GOOGLE_CLOUD_PROJECT_ID` — `ai_search.py`
