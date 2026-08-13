# hogo-meter API 契約 — Mac(libcurl) と Tab5(vendored) のズレを検出するための表

## この文書の目的

この契約は **2 回実装される**。

| | HTTP の実装 | JSON の実装 |
|---|---|---|
| Mac (`native-display`) | libcurl (`CurlHttpClient`) | nlohmann/json |
| Tab5 (hogo-meter firmware) | `HTTPClient` / `WiFiClientSecure` | ArduinoJson v7 |

同じソースが両方でコンパイルされることは設計上ありません（`docs/PORTABILITY.md`）。
つまり **2 つの独立した実装が、同じ 1 つのサーバ契約を満たさなければならない**。
片方だけがズレても、ズレた側でしか症状が出ないので、テストでは捕まりません。

そこでこの文書は、行ごとに **値 / firmware 側の file:line / dashboard 側の file:line** を
併記します。実行可能な写しが `tests/contract/test_hogo_client.cpp` です。

---

## ★契約の裁定者は firmware ではなく dashboard API

**firmware は一クライアントに過ぎません。** 「上流の firmware がこう書いているから」は
契約の根拠として不十分です。firmware が独自に安全側へ倒している箇所（`shouldLight` の
既定 true など）と、サーバが保証している箇所は別物です。

判断に迷ったら **dashboard の route / repository / validation を読むこと**。
この文書の「dashboard」列が空の行は「firmware 側のローカルな判断であって、
サーバが保証していない」ことを意味します。

---

## 基準コミットと引用ファイルの SHA-256

上流は生きたリポジトリなので、file:line 引用は黙って腐ります。
**この表と食い違ったら、上流を読み直してこの表を直してください。**

- リポジトリ: `<path-to-hogo-meter>` （★読み取り専用）
- 基準コミット: **`eadc9953b21a3928820118cb1a93505a8d9717a6`**
- 採取日: 2026-07-30
- 引用したファイルはすべて `git status --short` で **クリーン**（ワークツリーの変更なし）を
  確認済み。したがって下記の SHA-256 は基準コミットの内容と一致します。
  - 参考: 同リポジトリには無関係な未コミット変更が 1 件あります（`.serena/project.yml`）。
    引用ファイルには含まれません。

再取得の手順（★読むだけ。変更・git 操作・ビルド・実機接続はすべて禁止）:

```bash
git -C <path-to-hogo-meter> log -1 --format=%H
shasum -a 256 <上の表のファイル>
```

| ファイル | SHA-256 |
|---|---|
| `firmware/lib/hogo_common/server_sync.h` | `0f02bf017772d613b189af3ca4b03a66998719cd2b20c8768a57ed8e9a764216` |
| `firmware/lib/hogo_common/server_sync.cpp` | `4daab99a4da7fb52c9369abe4a00cf1735f2337337f93eabf91abc6ab067b6e0` |
| `firmware/lib/hogo_common/ui_model.cpp` | `e87add3714aa753672cc5c4bce0698f353866c4eb9a3eda0e6d69424f92df42e` |
| `firmware/lib/hogo_common/app_types.h` | `a70e40dcb7534a9b4c28fa571e2a2ea8b120ed285ecec2009d73576c965736b7` |
| `firmware/lib/hogo_common/ca_certs.h` | `438ea534ecad07c73437142b134b585a451f497c0f06f0242efd861d630b0997` |
| `firmware/variants/tab5/main.cpp` | `a248a70aa8408912baae738cb580b49c7d555ca30d69969edb057fb711dc01e1` |
| `firmware/variants/tab5/ui.h` | `2921c57e530eed1d609db04be378fb8aa99b3acf247269eab80de1810c4b44b8` |
| `dashboard/src/lib/schedule-status.ts` | `243a19f23cf3bd723d2cf164fc4e34dbf9fdc27de7c4acebbfe9bffb654569c6` |
| `dashboard/src/lib/types.ts` | `23d5bf69b0c8f6379f29dc8cc2515b076177c882de1d0f07b67c02d4f2211af9` |
| `dashboard/src/lib/validation.ts` | `7cde82920582250efe5354098e496f45a92434916d51050f9f0c9309ca8bdb9b` |
| `dashboard/src/lib/device-auth.ts` | `04fa05a3368a314e96a8664854b1585f05e3d855f13c5831668d3ff0f413533f` |
| `dashboard/src/lib/repositories/care-item-repository.ts` | `0d105cdc4070152e3a9245d218733c8df3d8593c1569b1945d1d55604b52e213` |
| `dashboard/src/lib/repositories/device-repository.ts` | `0f66fc141ed83bdfdefac6a784fe7cb13367b3ecafc4b76ff673651850bfa1ba` |
| `dashboard/src/app/api/agent/[deviceId]/config/route.ts` | `873c7291f749dcf58f0eb966bc3e77a2d2600215d1a4bf5c6055fc2054ac08ce` |
| `dashboard/src/app/api/agent/[deviceId]/records/route.ts` | `56df32a3452fe919675077b349b7b5a4bccbe5deb3d1a5878057fdd7dbada2c0` |
| `dashboard/src/app/api/agent/[deviceId]/deregister-ack/route.ts` | `d9375170026c836331928090889c5b833b2eda8ce88250bc4fb859ff6174e811` |

---

## 1. エンドポイントと URL

| 契約 | 値 | firmware | dashboard |
|---|---|---|---|
| config 取得 | `GET {base}/api/agent/{deviceId}/config` | `server_sync.cpp:221` | `app/api/agent/[deviceId]/config/route.ts:16` |
| 記録 POST | `POST {base}/api/agent/{deviceId}/records` | `server_sync.cpp:399` | `app/api/agent/[deviceId]/records/route.ts:16` |
| 初期化 ACK | `POST {base}/api/agent/{deviceId}/deregister-ack` | `server_sync.cpp:469` | `app/api/agent/[deviceId]/deregister-ack/route.ts` |
| **URL は素朴な文字列連結。正規化しない** | `String(_baseUrl) + "/api/agent/" + _deviceId + "/config"` | `server_sync.cpp:221` | — |
| **`baseUrl` はパス接頭辞を含む** | 実運用値 `https://akebi.dam-lab.work/hogo-meter` → 実パスは `/hogo-meter/api/agent/{id}/config` | — | — |
| **`baseUrl` の末尾スラッシュ禁止** | 付けると `//api/agent/...` になり 404 | — | — |

★Mac 側は `hogoValidateBaseUrl()` で **検出** はしますが、**黙って直しません**。
`geoframe_client` は末尾スラッシュを正規化しますが（`geoframe_client.cpp:411-413`）、
hogo 側で同じことをすると「Mac では通るが Tab5 では 404」という差が生まれます。
設定ミスは両方で同じように壊れるのが正しい。

## 2. 認証ヘッダ

| 契約 | 値 | firmware | dashboard |
|---|---|---|---|
| デバイス認証 | `X-Device-Secret: <secret>` | `server_sync.cpp:228, 405, 475` | `config/route.ts:18` / `records/route.ts:18` |
| **secret 不一致 → 401** | `{ok:false, status:401}` | `server_sync.cpp:248` (コメント) | `lib/device-auth.ts:89` |
| **deviceId 不明 → 404** | `{ok:false, status:404}` | 同上 | `lib/device-auth.ts:86` |
| **検証順は 404 → 401 → 400** | body 検証は認証の後 | — | `records/route.ts:14` |
| FW バージョン申告 | `X-FW-Version: <ver>`（非空のときだけ） | `server_sync.cpp:229-231` | `config/route.ts:21` |
| OTA 失敗申告 | `X-FW-Failed: <ver>#<attempt>` | `server_sync.cpp:235-237` | `config/route.ts:24` |
| Cloudflare Access | `CF-Access-Client-Id` / `CF-Access-Client-Secret` | `server_sync.cpp:241-244` | — |
| **★平文 http ではトークンを送らない** | 両方非空 **かつ** `https://` のときだけ | `server_sync.cpp:241` | — |

★**secret をログ・URL・ボディ・診断メッセージに出さないこと。**
Mac 側は `redactForLog()` を通す規律で、`test_hogo_client.cpp` が検査しています。

## 3. `GET /config` の応答

| 契約 | 値 | firmware | dashboard |
|---|---|---|---|
| 成功 | `200` + `{"items":[...]}` | `server_sync.cpp:246-253` | `config/route.ts:47-54` |
| item のキー | `slot` / `label` / `lastEpoch` / `status` / `shouldLight` | `server_sync.cpp:317-333` | `care-item-repository.ts:161-167` |
| **slot は 0..slotCount-1 を連続で返す** | 欠番なし・slot 昇順 | `server_sync.h:94-96` | `care-item-repository.ts:114-116`（`orderBy(slot)` + `filter(slot < slotCount)`） |
| **有効スロット数 = 見えた最大 slot + 1** | poll ごとに数え直す（6→3 の縮小に追従） | `server_sync.cpp:310, 336-338` | — |
| `lastEpoch` は未記録なら **0** | `it.lastRecordedAt ?? 0` | `server_sync.h:70` | `care-item-repository.ts:164` |
| **`status` は 0-3 の数値コード** | 0=None/1=Green/2=Yellow/3=Red | `app_types.h:53` | `care-item-repository.ts:28-35` (`computeStatusCode`) |
| **★`status` はサーバが計算する** | `computeScheduleStatus()` | — | `lib/schedule-status.ts:76-97` |
| **★`shouldLight` もサーバが計算する** | `shouldLightScreen()` | — | `lib/schedule-status.ts:148-164`、`care-item-repository.ts:166` |
| `status` が範囲外 → `None` へ倒す | クラッシュさせない | `server_sync.cpp:325-328` | — |
| `shouldLight` 欠落 → **`true`**（安全側） | 旧サーバ互換 | `server_sync.cpp:330-333`、`server_sync.h:169-171` | `care-item-repository.ts:166`（schedule が null なら true） |
| **`items` が無い応答は失敗扱い** | ラッチせず次回間隔で再試行 | `server_sync.cpp:300-304` | — |
| 応答サイズ上限 | `2048` バイト | `server_sync.cpp:268` | — |

### ★キー省略が正常系（省略 = false へリセットが契約）

| 契約 | firmware | dashboard |
|---|---|---|
| `firmware` は非対象ならキー自体を省略 | `server_sync.cpp:341-347` | `config/route.ts:49-50`（`...(firmware ? {firmware} : {})`） |
| `deregister` も非対象ならキー省略 | `server_sync.cpp:287-292`（`| false`） | `config/route.ts:51-53` |
| **キーが無い poll で必ず false へ戻る** | 古い要求／オファーを残留させない | `server_sync.cpp:292, 346-347` | 同上 |
| `firmware` は **version / sha256 / sig の 3 つすべて非空**のときだけ掴む | 未署名オファーを受け付けない | `server_sync.cpp:353-364` | — |
| `deregister` は `items` の検証より **前** に取り込む | items 欠落でも要求を取りこぼさない | `server_sync.cpp:287-292` | — |

★「最初から無ければ false」だけを試験しても**リセットは検証できません**。
`test_hogo_client.cpp` は「true の応答 → キーが無い応答」の 2 段で検査しています。

## 4. `POST /records` の応答

| 契約 | 値 | firmware | dashboard |
|---|---|---|---|
| **ボディは配列ではなく単発 1 件** | `{"slot":N,"recordedAt":<epoch秒>}` | `server_sync.cpp:415-416` | `lib/validation.ts:101-104` (`agentRecordSchema`) |
| `recordedAt` は **必須** | `epochSchema`（正の整数） | — | `lib/validation.ts:103, 34` |
| **成功は 204 No Content（空ボディ）** | firmware は念のため 2xx を成功とみなす | `server_sync.cpp:421-425` | `records/route.ts:55` |
| slot 越境は 400 | `slot >= slotCount` を拒否 | — | `records/route.ts:40-46` |
| `Content-Type: application/json` | — | `server_sync.cpp:412` | — |

## 5. 同期の規律（`SyncPolicy` が持つ純粋ロジック）

| 契約 | 値 | firmware | 実装 |
|---|---|---|---|
| config poll 間隔 | `POLL_INTERVAL_MS = 30000` | `server_sync.h:202` | `constants::kPollInterval` |
| **起動直後は即時 1 回** | 間隔を待たない | `server_sync.cpp:73` | `SyncPolicy::nextAction` |
| **readiness ゲートは即時 poll の判定より前** | 圏外／NTP 未同期でも即時 poll の権利を消費しない | `server_sync.cpp:56-58` | 同上 |
| HTTP タイムアウト | `HTTP_TIMEOUT_MS = 5000`（各フェーズ。最悪積算 ~15 秒） | `server_sync.h:207`, `:203-206` | `constants::kHttpTimeout` |
| キュー長 | `QUEUE_SIZE = 8` | `server_sync.h:222` | `constants::kRecordQueueSize` |
| **満杯なら epoch 最小（最も古い）を上書き** | 同値なら添字の小さい方（strict `<`） | `server_sync.cpp:97-112` | `SyncPolicy::enqueueRecord` |
| **総試行 5 回**（初回 1 回 + 再試行 4 回） | `RECORD_MAX_RETRIES = 5` | `server_sync.h:223`, `server_sync.cpp:432-442` | `constants::kRecordMaxRetries` |
| リトライ間隔 | `RETRY_INTERVAL_MS = 5000` | `server_sync.h:225`, `server_sync.cpp:441` | `constants::kRecordRetryInterval` |
| **1 回の update() で HTTP は最大 1 件** | 両方は絶対にやらない | `server_sync.h:52-54`, `server_sync.cpp:60-67` | `constants::kMaxHttpRequestsPerUpdate` |
| **未送信キューがあれば record を config poll より優先** | 記録が届く方が config の新しさより重要 | `server_sync.h:55-57`, `server_sync.cpp:60-67` | `SyncPolicy::nextAction` |
| **失敗の種類でリトライ規律を変えない** | 401 も 5xx も `code < 0` も同じ減算経路 | `server_sync.cpp:418-443` | `SyncPolicy::applyResult(bool)` |
| **間隔の起点はリクエスト完了時刻** | `millis()` を HTTP の **後** で取る | `server_sync.cpp:75, 441` | `applyResult(..., completedAt)` |
| **送信キューは RAM のみ。永続化しない** | ローカル NVS が正、キューは非同期な複製 | `server_sync.cpp:20-24` | — |
| 取り消し猶予 | `TOAST_TOTAL_SEC = 2` | `variants/tab5/ui.h:26`, `main.cpp:94-95, 515` | `constants::kToastTotal` |
| label 上限 | `LABEL_MAX_LEN = 18`（UTF-8 バイト数） | `server_sync.h:21-26` | `lib/types.ts:3` (`LABEL_MAX_BYTES`) |
| スロット数上限 | `CARE_ITEM_COUNT = 6` | `app_types.h:20` | `care-item-repository.ts:37` (`SLOTS`) |
| **3 / 6 バケット化は呼び出し側の責務** | `activeItemCount() >= 4 ? 6 : 3`（0 も 3 へ） | `server_sync.h:96-98`, `variants/tab5/main.cpp:140-142` | — |
| 経過時間の整形 | `formatElapsed()` | `ui_model.cpp:13-36` | — |
| 表示に使う最終記録時刻 | `max(ローカル, サーバ)` | `variants/tab5/main.cpp:156-157` | — |
| **同じカードの保留中の再タップは無視（重複記録防止）** | 2 件目を記録しない | `variants/tab5/main.cpp:305-307` | — |

★上流は再タップのガードを**タッチハンドラ**に置いていますが、Mac 側は
`CareModel::beginRecord()` に置きました。app 層は 1 つとは限らない
（タッチ / キーボード / テスト）ので、どの入力経路から来ても成立させるためです。
ガードが無いと `PendingCareRecord::begin()` が `leave(OtherCardTapped)` 経由で
1 件目を**確定**し、**1 タップのつもりが 2 件記録されます**（ダッシュボードを
見るまで気づけません）。保留自体は破棄せず、猶予の起点も動かしません。

---

## ★意図的に移植した「意外な」挙動 — 直さないこと

### record がバックオフ中は config poll も走らない

上流 `update()` は「キューに active な要素が 1 つでもあれば `drainOneRecord()` を呼んで
return」します（`server_sync.cpp:62-67`）。`drainOneRecord()` は送信期限が来ていなければ
何もせず false を返します（`server_sync.cpp:387-390`）。
つまり **5 秒のバックオフ中は config poll が飢えます**（最悪 4 回 × 5 秒 = 20 秒）。

直したくなりますが、直しません。

1. 「1 tick の停止時間を 1 リクエストぶんに抑える」規律の帰結であって、バグではない。
   バックオフ中に poll を挟むと、Tab5 と Mac で **HTTP の発生順序が変わり**、
   「Mac では再現しない」不具合の温床になる。
2. 記録が滞留している状況で config の新しさを優先する理由が無い
   （`server_sync.h:55-57` が「お世話の記録が届く方が config の新しさより重要」と明記）。

### 401 でも 5 回リトライする

上流は HTTP ステータスで分岐しません（`server_sync.cpp:418-443`）。
「401 は永続的な失敗だから即諦める」を足すと、secret ローテーション直後の記録が
黙って消えます。Mac 側は `SyncPolicy::applyResult` が `bool` しか受け取らないことで
構造的に保証しています。

---

## ★Mac 側が上流と挙動を変えた点（差分は 4 つだけ）

### 1. label の 18 バイト丸めが UTF-8 の境界を割らない

| | 挙動 |
|---|---|
| 上流 | `strlcpy(_labels[slot], label, 19)`（`server_sync.cpp:321`）。**バイト単位**で切るので、全角 7 文字（21 バイト）では 18 バイト目が 3 バイト文字の途中に当たり、**不正な UTF-8** になる |
| Mac | `truncateLabelUtf8()` がコードポイント境界まで戻して切る（結果 18 バイト以内） |

理由: SDL_ttf (`TTF_RenderUTF8_Blended`) も efont も不正な UTF-8 でグリフが化けます。
「18 バイト以内」という契約は満たしたまま、壊れた表示だけを避けられます。

★**dashboard は書き込み時点で 18 バイトを検証しています**
（`lib/validation.ts:29-31`、`lib/types.ts:3`）。したがって正常系ではこの分岐に
到達しません。**防御的な二段目**であり、API を直接叩かれた場合や上流仕様が
変わった場合にだけ効きます。

### 2. 応答サイズ上限の効き方

| | 挙動 |
|---|---|
| 上流 | `Content-Length` が明示されたときだけ事前に拒否。chunked では時間（5 秒）で間接的に抑える（`server_sync.cpp:255-274`） |
| Mac | `maxResponseBytes = 2048` が **本文全体に対する硬い上限**（`CurlHttpClient` が超過で `TooLarge`） |

**値（2048）は変えていません。** 上流コメントの実測「items 3 件 + firmware で 600-700
バイト」から、6 項目モードでも約 1100 バイトで、約 1.9 倍の余裕があります。

### 3. `SyncPolicy` が `IHttpClient` に依存しない

`docs/CORE_CONTRACTS.md` §8.1 は「ポリシーが `IHttpClient` に依存する以上、
依存される抽象も core になければならない」としています。実装はそれより **強い**形に
なりました: `SyncPolicy` は「この tick に何をすべきか」を 1 個返すだけで、
HTTP を投げるのは transport の `HogoSync` です。

結果として:

- **「1 回の update() で HTTP は最大 1 件」が構造的に成立する**（2 本投げる書き方が無い）
- `SyncPolicy` が `unit_tests`（`ambient_core` だけにリンク）で完結して試験できる
- Tab5 側は「action を実行する 30 行」だけを書き直せばよい

`IHttpClient` は `core/http.hpp` にあるまま（移動していません）。

### 4. 平文 `http://` を許可しない（`X-Device-Secret` を平文で流さない）

| | 挙動 |
|---|---|
| 上流 | `http://` を「開発・復旧用の平文フォールバック」として**許可**する（`net_client.h` の `makeClient()` の節、`server_sync.cpp:15`）。既定値も `SERVER_BASE_URL "http://192.168.1.10:3000"`（`app_config_defaults.h:16`）。`X-Device-Secret` は**スキームに関わらず無条件で送る**（`server_sync.cpp:227`）。https ゲートが掛かっているのは CF-Access トークンだけ（`server_sync.cpp:241-244`） |
| Mac | `hogoValidateBaseUrl()` が `http://` を**エラーとして返す**。加えて `addCommonHeaders()` が **https 以外では `X-Device-Secret` を積まない** |

理由:

- 上流が CF トークンにだけ https ゲートを掛けているのは、**LAN 直結では
  Cloudflare を経由しないためトークンに機能上の意味が無い**からです
  （上流のコメントにそう書かれています）。`X-Device-Secret` は逆に
  「無いと動かない」ため、上流は平文でも送るという判断をしています。
  これは意図した非対称であって、見落としではありません
- そのうえで Mac 側は許可しません。`X-Device-Secret` は**失効させる手段が
  デバイス再登録しか無い長期資格情報**で、漏れると記録の偽装（ダッシュボードへの
  書き込み）ができます。加えて `CurlHttpClient` は `CURLOPT_PROXY` を設定して
  いないため libcurl が `http_proxy` 環境変数を尊重し、平文だとプロキシにも見えます
- **二段構えにしてあります。** 検証（`hogoValidateBaseUrl`）は自由関数であって
  `HogoClient` のコンストラクタが呼ぶわけではないので、検証を忘れた配線でも
  secret が出ないよう送信直前にも gate を置いています。検証側は「黙って 401 に
  ならず、設定時点で理由が出る」ための loud な入口です

⚠️ **影響**: 平文 http のローカル dashboard へは接続できません（サーバは 401 を
返します）。**逃げ道（明示的な opt-in フラグ等）は意図的に作っていません。**
今は `.env` を読むコードも `HogoClient` の配線も無く、フラグを足しても誰も設定
しないため、実消費者と実テストが現れた時点で「https にする / loopback だけ許す /
opt-in を足す」のどれを採るか決めるほうが確実です。

---

## ★未配線の申し送り — CA バンドル

上流 `ca_certs.h` の `ROOT_CA_BUNDLE`（`ca_certs.h:28-104`）は **純 PEM で Arduino
非依存**です。これを Mac 側の検証にも使えば、**Tab5 での CA ピン切れを前倒しで
検出**できます（収録 CA は GTS Root R4 / R1 / ISRG Root X1）。

現状:

| | 状態 |
|---|---|
| `HOGO_CA_BUNDLE`（PEM ファイルへのパス）の読み込み | **実装済み** (`loadCaBundleFromEnv()`)。契約テストあり |
| `HogoClientConfig::caBundlePem` | **実装済み**（フィールドだけ） |
| `CURLOPT_CAINFO_BLOB` への配線 | ★**未実施** |

★**この値は現時点で誰も読みません。** 配線先は
`native-display/src/transport/curl_http_client.cpp` で、Wave 4 の所有ファイルでは
ありません。**配線する Wave は curl_http_client.cpp を直してください。**
未設定時（既定）はシステムのトラストストアに任せます。

`HOGO_CA_BUNDLE` は **パスとしてのみ**扱います（インライン PEM は受け付けません）。
PEM に見えない中身は拒否して理由を返します。黙って通すと TLS 時に
「証明書検証失敗」としか出ず、原因の切り分けができなくなるためです。

---

## ★実 API に対して検証できないこと

### 6 項目レイアウト

自己登録経路では `slotCount` が **3 に固定**されます
（`device-repository.ts:37-41` の `MODEL_SLOT_COUNT`、`tab5: 3`）。
登録後にダッシュボードで 6 項目へ変更できますが、
**自己登録した端末で 6 項目レイアウトの実 API 試験を行うことは原理的にできません。**
6 項目は fixture / 合成 snapshot で担保しています。

### 異常系

★**401 / 403 / 404 / 409 / 5xx / タイムアウトを実本番 API へ撃たないこと。**
Cloudflare の防御を誘発し、運用ログを汚染します。異常系はすべて
`FakeHttpClient`（メモリ上）で再現しています。実ネットワークへ出る経路は
`tests/contract/test_hogo_client.cpp` に 1 つもありません。

### 実本番への書き込み

実本番 API への最初の `POST /records` は **外部サービスへの書き込み**です。
実行前に明示的な指示を得ること（`native-display/PROGRESS.md` の申し送り）。

---

## 契約テストの対応表

`tests/contract/test_hogo_client.cpp`（`contract_tests`）と
`tests/unit/test_sync_policy.cpp` / `test_care_model.cpp`（`unit_tests`）。

| # | 契約 | テスト |
|---|---|---|
| 1 | キュー満杯時に epoch 最小が上書きされる | `hogo: ★キュー満杯で epoch 最小が上書きされ、その 1 件は送られない` / `sync_policy: キュー満杯で epoch 最小が追い出される` |
| 2 | 総試行 5 回・5 秒間隔で諦める | `hogo: ★総試行 5 回で諦める（HTTP の呼び出し回数で確認）` / `sync_policy: 総試行 5 回で諦める` |
| 3 | 1 回の update() で HTTP は最大 1 件 | `hogo: ★1 回の update() で HTTP は最大 1 件（callCount の差分で確認）` |
| 4 | record が config poll より優先 | `hogo: ★record が config poll より優先される` |
| 5 | 起動直後は即時 1 回 poll | `hogo: ★起動直後は即時 1 回 poll する` / `hogo: ★圏外 / NTP 未同期では…` |
| 6 | 異常系 401/403/404/409/5xx/タイムアウト | `hogo: ★異常系（401/403/404/409/5xx/タイムアウト）を表で網羅する` |
| 7 | firmware / deregister のキー省略は false へのリセット | `hogo: ★deregister はキー省略で false へ戻る（2 段階で確認する）` / `hogo: ★firmware オファーもキー省略で消える` |
| 8 | label の 18 バイト上限（マルチバイト安全） | `hogo: ★label は 18 バイトで丸められ、UTF-8 の途中で切れない` / `care_model: 丸めた結果は必ず有効な UTF-8 の境界で終わる` |
| 9 | ログに secret が出ない | `hogo: ★secret が URL / ボディ / 診断メッセージ / ログに出ない` |

### 上流に対応物が無い（Mac 側の追加）契約

| 契約 | テスト | 理由 |
|---|---|---|
| `RecordedAwaitingServer` は 90 秒でラッチが切れる | `care_model: サーバが追いつかなくても最強 claim はラッチしない` | 記録が総試行 5 回で破棄されるとサーバは永久に追いつかない。最強 claim が固着すると地震アラートまで抑止する |
| レイアウト外へ落ちた slot は待ちを出さない | `care_model: レイアウト外へ落ちた slot はサーバ待ちを出さない` | 6→3 の縮小で消えた slot は `present` になる機会が二度と来ない（同上のラッチ経路） |

★上流にはそもそも `RecordedAwaitingServer` という状態がありません
（`docs/CORE_CONTRACTS.md` §5.4 が「企画書に無いが必要と判明した状態」と明記）。
したがってこの 2 行は移植ではなく **Mac 側で新たに必要になった安全機構**です。

★これらのテストは **ミューテーション試験で有効性を確認済み**です。
実装に故意の欠陥（追い出しを添字 0 固定 / リトライ予算の off-by-one /
リトライ間隔 5 秒→3 秒 / 3-6 バケットの境界 / label のバイト切り /
1 update で 2 リクエスト）を入れると、対応するテストが必ず落ちます。
