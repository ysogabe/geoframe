# AI Provider基盤（T4.2）

- 状態: T4.2で実装。設計は[p4-design.md](p4-design.md) §4.3・§5、設定は[ai-config.md](ai-config.md)。
- 範囲: **AI専用のHTTP・接続先の固定・Discovery・Decisionの入出力・共有の一時待機**まで。用途別のhandler（Decisionの質問と閾値・clueの保存はT4.6、画像／文EmbeddingはT4.4）、クラウドのアダプター（Vertexの認証・送信）は含まない。`production_handlers()`は空のまま（未登録のjobはclaimも成功化もされない）。
- 実LAN・外部AI・クラウド・課金は使っていない（試験はループバックのスタブ・偽のDNS・合成の画像／DBだけ）。

## 参照した契約（固定した版）

| 対象 | 版 | 使った範囲 |
| --- | --- | --- |
| local-model-lab（Discovery） | `main@7f472a5c`（2026-10-26T11:41Z）。以前の`2c78c991`から契約の変化なし | `GET /llm/catalog`（`services[]`の`id`・`apis[]`・`available`）、`GET /llm/services/<id>`（`id`・`state`・`available`・`lease_count`）。`reason`・`url`・`local_url`・`endpoint`は使わない |
| Decision（d3-lite） | Hugging Face `vllm-sr/d3-lite`の`d3_server.py`（revision `a8d25a91`） | `GET /health`（`status`・`model`・`modalities`）、`POST /v1/systemone`（要求・応答の形と上限）。入口の接頭辞は`/llm-decision` |
| 画像／文Embedding | local-model-lab #93は**OPEN・採用された画像APIなし**（2026-10-26） | **何も実装していない**。仮のAPI・実providerの実行は追加しない（T4.4） |

契約が変わったときは、上の版を更新してから`tests_provider.rs`のスタブ（`new_lab`）を直す。

## HTTP（`ai::http`・`ai::dest`・`ai::lanes`）

取り込みの同期が使う`net::agent()`は変えない。AI専用のagentは次のとおり。

| 項目 | 振る舞い |
| --- | --- |
| 送信先 | 設定の`local_base_url`のhost:portだけ。呼べるのは`Endpoint`（固定のパス）で、任意のURL・パス・ヘッダーを受ける入口は無い。サービスIDは`ServiceId`で文字種を再検査してからパスに入る。旧`GEOFRAME_DECISION_URL`は読まない |
| 名前解決 | 接続のたびに全A/AAAAを検査し、private（RFC 1918・IPv6 ULA）とloopbackだけを許す。IPv4-mapped IPv6はIPv4へ正規化。公開・link-local・CGNAT・unspecified・multicast・空・17個以上が**1つでも**あれば全体を拒否（`DestinationDenied`）。検査済みのアドレスをそのまま接続に渡し、**URLをIPに書き換えない**ので`Host`とTLSのSNI・証明書の検証は元のhostのまま。検査後に別のDNS問い合わせはしない（接続の再利用でも要求ごとに解決し直して検査する） |
| DNS待ち | 要求の全体期限の残りまで。補助スレッドは同時に4本まで（超えたら時間切れ）。遅いDNSでスレッドが増え続けない |
| リダイレクト | 追わない（`max_redirects(0)`）。3xxは`Redirect(状態)`で、本文を読まず、Locationへ接続しない。認証・呼び出し側のヘッダーを別の宛先へ出す経路は無い |
| プロキシ | 使わない（`proxy(None)`）。`HTTP_PROXY`/`HTTPS_PROXY`は読まない。クラウドのプロキシは管理者が明示した場合だけ（T4.6以降の境界。設定JSONには足していない） |
| ヘッダー | `X-Client-Name: geoframe`・`Accept`・POSTの`Content-Type`・`User-Agent: geoframe/<版>`だけ。`Origin`は付けない |
| 期限 | Discovery全体2秒（catalog＋service＋health）、対話15秒、背景60〜180秒（Decision 150秒、Embedding 60秒、判定・位置候補180秒）。DNS・接続（5秒まで）・ヘッダー・本文の読み切りを含む |
| 要求の上限 | 完成したJSON（base64込み）8 MiB。**送る前**に検査し、超えたらDNSも接続もしない（`RequestTooLarge`）。上流のDecisionの本文16 MiBよりこちらを優先 |
| 応答の上限 | 展開後1 MiB（`net::read_body_limited`。圧縮前の上限も同じ関数が掛ける）。2xx以外の本文は読まない |
| 枠 | 背景1・対話1で独立。対話の枠が使用中なら待たず`Busy`（定型503に当たる）。対話が枠を待つ間は新しい背景を始めない |
| 呼出境界 | 同期の`ureq`をtokioのworkerやwriterの上では呼ばない。背景はAIワーカーの専用スレッド、対話は`AiHttp::run_interactive`（枠を先に取り`spawn_blocking`で実行） |
| 停止 | 停止の合図が立っていれば、枠も要求も始めない。送信済みの要求は期限まで待ち、有効な応答は確定できる（既存のworker/Writerの停止・token・LockHoldは変えていない） |
| エラー | `HttpError`は固定の列挙と状態コードだけ（URL・本文・例外文字列・認証・モデル応答を持たない）。`Debug`も本文を出さない |

テストだけが偽のDNS（`AiHttp::with_lookup`、`#[cfg(test)]`）を差し込める。本番の設定・環境変数からは選べない。

### `ureq`の更新時の注意

`ureq`のunversioned API（`Resolver`・`DefaultConnector`・`Agent::with_parts`）はsemver外（minorで変わりうる）。使う箇所は`ai/dest.rs`の`PinResolver`と`ai/http.rs`の`AiHttp::build`の**2か所だけ**。更新時はコンパイルと`tests_http.rs`（接続先の固定・rebinding・SNI・リダイレクト・プロキシ）を必ず確認する。Cargo.lockの`ureq`は3.5.0。

## Discovery（`ai::discovery`）

- 送ってよいのは、catalogの`available`とserviceの`available`が**両方true**で、かつ`state=running`・`lease_count=0`のときだけ。`available`は上流の鮮度判定を含むので、`state`や`lease_count`からtrueを算出し直さない。矛盾する応答（`available=true`だが`state`が`running`でない、など）は送らない。
- `state`・`lease_count`は**待ちの範囲**の判定にだけ使う。`lease_count`が0でない・nullなら入口全体、`state`が`running`でない・`available=false`ならサービス単体。
- 契約エラー（固定コード）: catalogの形・必須項目・型の違い、catalogに無いサービス、必要なAPI（method＋path）の欠落、serviceの`id`不一致、未知の`state`、`lease_count`キーの欠落。healthの形・能力（`image`）・モデル名の不一致。
- `reason`は読まない。`url`・`local_url`・`endpoint`は接続にも比較にも使わない。

## Decision（`ai::decision`・`ai::image_input`）

- 送る前に`GET /llm-decision/health`: `status=ready`かつ`modalities`に`image`があり、`model`が設定の`location_clue`のモデルと同じ。`loading`・`modalities`がnullは一時待ち、`image`が無い・モデル違いは契約エラー。
- `POST /llm-decision/v1/systemone`: 要求は`{model, state, questions:{名前:{type: choice|noul, instructions, criteria}}, images:[dataURL]}`、応答は`{model, answers, usage}`。応答の形を要求の質問と突き合わせて検査する（`noul`は有限の0〜1、`choice`は要求の選択肢のいずれか、`error`を持つ質問は`Unanswerable`）。質問の文面・閾値・clueの保存はT4.6。
- 画像は`PreparedImage`（JPEG/PNG/WebPのdataURLだけ）。1枚8,000,000バイト以下・1.6メガピクセル以下・長辺はDecision 1536px（埋め込み用は768px）、EXIF/XMP/IPTC/コメント/PNGテキスト/WebPのEXIF・XMPを含むものは**断る**（黙って削らない）。外部URL・ファイルのパス・元画像は受けない。縮小・向きの適用・再エンコードは用途（T4.6/T4.4）が既存のderivativeから行い、この版は検査の境界と`PreparedImage::encode_jpeg_rgb`（EXIFを出さないエンコード）までを持つ。
- 入口は`decision::run(ctx, lan, request)`: 組み立て（8 MiB検査）→`before_probe`→背景の枠→Discovery・health→`before_send`→POST。POSTが通信失敗・5xxなら**Discoveryをやり直して分類**する（使えなければ待ち、使えるならそのjobだけの反復失敗）。

## 分類（`ai::provider`）

| 状況 | 扱い |
| --- | --- |
| Discoveryの到達不能（DNS・接続・TLS・時間切れ・切断・5xx） | 入口全体の待ち（`unreachable`） |
| lease中・lease変更中 | 入口全体の待ち（`leased`） |
| 429 | 入口全体の待ち（`rate_limited`、`Retry-After`を60〜3600秒に収める） |
| 503（lockの競合） | 入口全体の待ち（`busy`） |
| サービス停止・起動中・health loading・`available=false` | サービス単体の待ち（`not_ready`） |
| 401・403 | Provider全体の**永続停止**（`unauthorized`・`forbidden`）。401のtoken更新はT4.6 |
| 404（service・model） | 用途の永続停止（`model_not_found`）。catalogの404は契約不一致 |
| 契約違反・3xx・解決先が許可外 | 永続停止（`contract_mismatch`。catalog自体はProvider全体、サービス固有は用途） |
| POST後の通信失敗・5xx | Discoveryをやり直す。使えなければ待ち、使えるなら`Repeated`（クラウドは`Attempt`で課金上限へ。`into_run_error(billable)`） |
| 400・413・422（POST） | そのjobだけの`Permanent`（他のjobは進む） |
| 契約に合わない2xxの本文（POST） | `Attempt(bad_response)`（5回で`failed`） |

永続停止（`ai_provider_pauses`）と一時待機（`ai_provider_waits`）は混同しない。永続停止は管理操作か関係する設定の変更まで続き、一時待機は`until`（60〜3600秒先）が過ぎれば消える。

## 共有の一時待機（`ai::waits`・`worker`）

- `RunError::ProviderWait(Wait{scope, reason, retry_after_secs})`。`scope`は`Entrance`（同じProviderの全用途）か`Service`（そのサービスを使う用途だけ）。
- 識別は`AiSettings::provider_fingerprint`（接続先・認証のハッシュ）と`service_key`（サービスIDのハッシュ。`location_clue`は固定の`decision`、クラウドはモデル単位）。別のProvider・別のサービスは巻き込まない。
- ワーカーは待ちを`ai_provider_waits`に記録し（同じWriterの1回の依頼でjobの`next_attempt_at`も遅らせる）、周期の最初にDBから読み直す。待っている用途は**claimの前に**飛ばす。再起動後も`until`まで連打しない。既存の`blocked`（`Unavailable`・`RateLimited`・予算切れ）はこの起動の中の用途単位で、そのまま残る。
- 待ちは成功済みの結果にも他のjobにも触れず、設定の変更や待ちだけで過去の成功結果を再送しない。
- `RunCtx::before_probe`（新設）はDiscoveryなどの読み取りの前に使い、`before_send`の確認のうちクラウドの送信枠の予約だけをしない（Discoveryでクラウド枠を消費しない）。推論のPOSTの直前は従来どおり`before_send`。

## 新しいスキーマ（互換・dry-run）

`ai_provider_waits(provider, service, reason, until, updated_at)`（`WITHOUT ROWID`、主キー`(provider, service)`、`service=''`は入口全体）を追加した。`INGEST=1`の起動の`ai::migrate`だけが作り、`REQUIRED_SCHEMA`・Pythonのスキーマには入れない。旧表・既存列は変えない。`INGEST=0`の読み取り専用起動は表が無くても動く（待ちは空として扱う）。`--dry-run`の「まだ無いAIの表」の一覧に`ai_provider_waits`が加わる（既存の4表＋索引は従来どおり）。

## 未実装・後続

- 用途別handlerの登録（Decisionのclue→T4.6、画像／文Embedding→T4.4）。それまで配布バイナリはAI処理をしない。
- クラウド（Vertex）のアダプター・token読取・401の一度のrefresh・送信後の課金計数（T4.6）。このタスクでクラウドへの実送信はしていない。
- 対話（検索語Embedding）の予約キャッシュとE5/E6への接続（T4.4）。`AiHttp::run_interactive`は枠と境界だけ。
- `ai-jobs status`/STATUSへの待ちの表示は未実装（jobの`last_error_code`・`next_attempt_at`で追える）。
- 実LAN・実Decision・実画像での確認はT4.7（LXC）。
