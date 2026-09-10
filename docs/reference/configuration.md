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
並べています。外部連携を有効にする手順は [外部連携](../integrations/) を参照してください。

- `GEOFRAME_DATA_DIR`（`config.py`: `DATA_DIR`） — 既定値 なし（未設定時は `ROOT / "var"`）。ソース（geoframe/ static/ vendor/）は ROOT 配下、書き換わるデータ （catalog.db・derivatives/・photos/・各種キャッシュ）は DATA_DIR 配下に置く。本番では GEOFRAME_DATA_DIR=/var/lib/geoframe のようにデータだけ別の場所へ移せる。※.env の読み込みは ROOT 側のまま。DATA_DIR は .env で指定できる必要があり、 .env を DATA_DIR 配下に置くと「読むために DATA_DIR が要る」鶏卵になるため。※derivatives/ photos/ blog_exports/ というディレクトリ名は変更しないこと。DBの display 列が「DATA_DIR/display＝実ファイル」かつ「/display＝配信URL」の 二重の意味を持っており、名前を変えると既存の全行が解決不能になる。
- `GEOFRAME_PHOTOS_DIR`（`config.py`: `PHOTOS_DIR`） — 既定値 `photos`。※PHOTOS_DIR は DATA_DIR 配下であること（drive.py が relative_to(DATA_DIR) で DBに入れる相対パスを作るため、外に出すと索引できない）
- `GEOFRAME_PORT`（`config.py`: `PORT`） — 既定値 `8080`。HTTPの待ち受けポート。キオスク端末は http://<機体IP>:<この値>/ を開く
- `GEOFRAME_HOST`（`config.py`: `HOST`） — 既定値 `0.0.0.0`。開発時は GEOFRAME_HOST=127.0.0.1 でこの端末だけに限定できる。実機の家庭内LAN配信は従来互換の 0.0.0.0 を既定とする。
- `GEOFRAME_FETCH_PER_CYCLE`（`config.py`: `FETCH_PER_CYCLE`） — 既定値 `25`。同期1サイクルで取り込む写真の枚数。初回のDrive全件取り込みもこの数ずつ進む
- `GEOFRAME_QUALITY_PER_CYCLE`（`config.py`: `QUALITY_PER_CYCLE`） — 既定値 `str(FETCH_PER_CYCLE)`。品質解析も同期1サイクルで処理量を制限する。未指定時はderivative生成数と揃え、 通常運用では新しく取り込んだ分を同じサイクル内で追いつけるようにする。
- `GEOFRAME_AI_ENABLED`（`config.py`: `AI_ENABLED`） — 既定値 `1`。AI写真選定はDrive同期と別スレッドで少量ずつ進める。無効化する場合だけ0を指定する。
- `GEOFRAME_AI_EMBED_PER_CYCLE`（`config.py`: `AI_EMBED_PER_CYCLE`） — 既定値 `12`。AI選定の1サイクルでembeddingする枚数
- `GEOFRAME_AI_JUDGE_PER_CYCLE`（`config.py`: `AI_JUDGE_PER_CYCLE`） — 既定値 `2`。AI選定の1サイクルでGemini比較まで進めるグループ数
- `GEOFRAME_AI_POLL_S`（`config.py`: `AI_POLL_S`） — 既定値 `60`。ai_workerが次のサイクルまで待つ秒数。30秒未満は30秒へ切り上げる
- `GEOFRAME_AI_TIME_WINDOW_MIN`（`config.py`: `AI_TIME_WINDOW_MIN`） — 既定値 `15`。同一場面候補とみなす撮影時刻の窓（分）。この範囲内の写真だけを類似比較する
- `GEOFRAME_AI_SIMILARITY`（`config.py`: `AI_SIMILARITY`） — 既定値 `0.82`。同一場面候補とみなすembeddingのcosine類似度しきい値
- `GEOFRAME_AI_SHORTLIST_MAX`（`config.py`: `AI_SHORTLIST_MAX`） — 既定値 `6`。1グループでGemini比較に回す最大枚数（2〜6へクランプ）
- `GEOFRAME_AI_SEARCH_PER_MIN`（`config.py`: `AI_SEARCH_PER_MIN`） — 既定値 `30`。/api/ai/search の流量上限（分あたり）。LAN内の任意ページから連打GETで 全embedding行の毎回フルスキャン（3テーブルJOIN、グローバルDBロック内）を 誘発できるため、tiles.pyのスライディング窓と同型で抑制する。
- `GEOFRAME_DRIVE_KEY`（`config.py`: `DRIVE_KEY`） — 既定値 `drive_key.json`。Google Driveサービスアカウント鍵JSONのパス（ROOT＝app/ からの相対）
- `GEOFRAME_DRIVE_FOLDER`（`config.py`: `DRIVE_FOLDER`） — 既定値 `GeoFrame`。取り込み対象のDriveフォルダ名
- `GEOFRAME_DRIVE_POLL_MIN`（`config.py`: `DRIVE_POLL_S`） — 既定値 `10`。Driveの変更確認間隔（分）。値は秒へ換算して使う
- `GEOFRAME_INFER_MIN`（`config.py`: `INFER_WINDOW_S`） — 既定値 `15`。位置推定の錨に使う撮影時刻の窓（分）。この範囲内の位置つき写真から推定する
- `GEOFRAME_INFER_OFFSET_MIN`（`config.py`: `INFER_OFFSET_S`） — 既定値 `0`。カメラ時計のずれ補正（分）。撮影時刻が恒常的にずれる機体で調整する
- `GEOFRAME_ADSB_URL`（`config.py`: `ADSB_URL`） — 既定値 `https://ads-b.dam-lab.work/tar1090/data/aircraft.json`。航空機情報の取得先（readsb/tar1090互換のaircraft.json）
- `GEOFRAME_MATCH_KM`（`config.py`: `MATCH_KM`） — 既定値 `80`。海外行き先、および国内だが都道府県解決に一時的に失敗した場合の フォールバック半径としてのみ使用する（詳細: adsb.py の3分岐マッチング）
- `GEOFRAME_MLIT_KEY`（`config.py`: `MLIT_KEY`） — 既定値 （空文字）。不動産情報ライブラリ（要APIキー）。地震速報時の防災レイヤ表示に使用
- `GEOFRAME_TILE_UA`（`config.py`: `TILE_UA`） — 既定値 `GeoFrame/0.1 (personal photo frame)`。OSM tile usage policy 準拠に必須。実運用では連絡先入り（例 "GeoFrame/0.1 (me@example.com)"）を GEOFRAME_TILE_UA に設定しておくこと。既定値のままだとブロックされうる
- `GEOFRAME_TILE_CACHE_MAX_MB`（`config.py`: `TILE_CACHE_MAX_MB`） — 既定値 `16384`。3D PHOTO JOURNEYの高ズーム航空写真を保持できるよう、全レイヤー共有で16GiB。実機で容量を抑えたい場合は GEOFRAME_TILE_CACHE_MAX_MB で上書きする。
- `GEOFRAME_TILE_FETCH_PER_MIN`（`config.py`: `TILE_FETCH_PER_MIN`） — 既定値 `240`。上流フェッチ（＝キャッシュミス）だけを対象にした流量上限（枚/分）。キャッシュヒットは無制限。/tiles/ は無認証なので、LAN内の任意のWebページが <img src="http://<frame-ip>:8080/tiles/…"> を大量に並べるだけで上流を叩かせられる（クロスオリジンの<img>はSOPで止まらない）。上限に達したら上流へ行かず透明PNGを返すことで、OSM tile usage policy上の bulk downloading とみなされて自宅グローバルIPがBANされる事態とディスク枯渇を防ぐ。既定240の根拠: コールド起動時の1画面が実測116枚。通常の初回描画＋パン1回分に 約2倍の余裕を持たせた値（gsiの上流同時接続4でも詰まらない）。
- `GEOFRAME_HA_URL`（`config.py`: `HA_URL`） — 既定値 `http://homeassistant.local:8123`。Home Assistant連携（.envのHOME_ASSISTANT_API_KEY または GEOFRAME_HA_TOKEN）※平文のままホスト名を指定した場合（下の既定値の homeassistant.local など）は、 mDNS/DNS応答を偽装されてトークンを盗まれる経路になるため起動時に拒否し、連携を開始しない。https か、名前解決を伴わないIPリテラル（127.0.0.1 等）なら許可する
- `GEOFRAME_HA_TOKEN`（`config.py`: `HA_TOKEN`） — 既定値 なし（未設定時は `HOME_ASSISTANT_API_KEY`）。HAの長期アクセストークン。GEOFRAME_HA_TOKEN を優先し、無ければ HOME_ASSISTANT_API_KEY を読む
- `HOME_ASSISTANT_API_KEY`（`config.py`: `HA_TOKEN`） — 既定値 （空文字）。HAの長期アクセストークン。GEOFRAME_HA_TOKEN を優先し、無ければ HOME_ASSISTANT_API_KEY を読む
- `GEOFRAME_HA_MOTION_ENTITY`（`config.py`: `HA_MOTION`） — 既定値 （空文字）。例: binary_sensor.display_motion
- `GEOFRAME_IDLE_MIN`（`config.py`: `IDLE_MIN`） — 既定値 `10`。無人この分数で画面スリープ
- `GEOFRAME_SLEEP_CMD`（`config.py`: `SLEEP_CMD`） — 既定値 （空文字）。例: xset dpms force off（N100実機用）
- `GEOFRAME_WAKE_CMD`（`config.py`: `WAKE_CMD`） — 既定値 （空文字）。例: xset dpms force on
- `GEOFRAME_HA_ENV_ENTITIES`（`config.py`: `HA_ENV_ENTITIES`） — 既定値 （空文字）。ROOM ENV表示に使うHAエンティティ（カンマ区切り。.envで指定）
- `GEOFRAME_HA_SCREEN_MODE_ENTITY`（`config.py`: `HA_SCREEN_MODE_ENTITY`） — 既定値 （空文字）。HA側の input_select（選択肢: auto / force_on / force_off）を既存のポーリングで 読むだけの構成にしている。こちら側に書き込み用のHTTPエンドポイントは足さない （このサーバーは無認証なので、外から画面を消せる口を増やしたくない）。未設定なら常に auto 相当＝従来の人感センサー任せの挙動に戻る（rollback経路）。例: GEOFRAME_HA_SCREEN_MODE_ENTITY=input_select.geoframe_screen_mode
- `GEOFRAME_HA_SCREEN_DECAY_MIN`（`config.py`: `HA_SCREEN_DECAY_MIN`） — 既定値 `5`。HAへ疎通できないままこの分数を超えたら、オーバーライドを捨てて auto へ戻す。外出先で force_off にした直後にHAが落ちて画面が二度と点かない、を避ける保険
- `GEOFRAME_SCREEN_STATUS_CMD`（`config.py`: `SCREEN_STATUS_CMD`） — 既定値 （空文字）。実機の画面状態を読むコマンド（例: /opt/geoframe/deploy/kiosk/screen-ctl.sh status）。※標準出力の on/off だけを見る。あのスクリプトは正常な「off」判定でも exit 1 を 返す仕様なので、終了コードで成否を判定してはいけない
- `GEOFRAME_HA_SCREEN_RECONCILE_SEC`（`config.py`: `HA_SCREEN_RECONCILE_SEC`） — 既定値 `90`。画面の意図と実機状態を照合する間隔（秒）
- `GEOFRAME_JMA_AREA`（`config.py`: `JMA_AREA`） — 既定値 `120000`。気象庁（無認証）。GEOFRAME_JMA_AREA: 予報区コード（既定120000=千葉県）
- `GEOFRAME_DEBUG_API`（`config.py`: `DEBUG_API`） — 既定値 （空文字）。開発用API（/api/ha/entities・/api/mlit/probe）のガード。既定は無効
- `GEOFRAME_ALLOWED_HOSTS`（`config.py`: `ALLOWED_HOST_NAMES`） — 既定値 （空文字）。許可するホスト名。IPリテラル宛（192.0.2.7等）は_host_ok側で無条件に許可するため、 ここには名前だけを入れる。GEOFRAME_ALLOWED_HOSTS はTailscaleや固定DNS名で 運用する場合の追加指定用（例: GEOFRAME_ALLOWED_HOSTS=geoframe.local:8080,frame.lan）

## `config.py` 以外が読む環境変数

以下は各モジュールが直接読みます（`config.py` を経由しないため上の一覧には現れません）。
設定方法と既定の挙動は、それぞれの変数の説明先を参照してください。

- `GOOGLE_APPLICATION_CREDENTIALS` — `ai_search.py`
- `GOOGLE_CLOUD_LOCATION` — `ai_search.py`
- `GOOGLE_CLOUD_PROJECT` — `ai_search.py`
- `GOOGLE_CLOUD_PROJECT_ID` — `ai_search.py`
