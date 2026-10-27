# Vertex クラウド通信基盤（T4.6a）

P4-AC2/5/10aの認証・通信境界を実装した。写真比較・位置候補のhandler、判定prompt・domain parse、画像／文Embedding APIは含まない。T4.6bで写真別LAN location_clueを登録済みだが、クラウドのhandlerは未登録。実クラウド・実写真・実鍵・実DBは使わず、合成入力とloopbackだけで試験する。

## 呼出しと共有

`ai::cloud::CloudClients::new(&config.ai)`を、後続のhandler構築時にプロセスで一度作り、cloneして共有する。構築時は設定文字列だけを検査し、鍵読取り・DNS・HTTPは行わない。同じ認証パス・project・regionの用途は同じProviderを参照し、agentと背景1／対話1の`Lanes`を共有する。token取得は既に取得した枠の内側で行い、二重に枠を取らない。LANとは枠を分ける。現時点の登録対象は`photo_judge`・`location_suggest`だけ。対話実処理とEmbeddingの登録・契約はT4.4（#93待ち）。別々に`CloudClients::new`して枠を増やさない。

公開入口は`CloudClients::client(TaskKind)`→`CloudClient::run(&RunCtx, &GenerateRequest)`。任意のURL・header・JSONを送れる公開APIは無い。同期HTTPはAI専用スレッドで呼び、writer・DB transaction・画像lockを保持しない。

`GenerateRequest`のconstructorはcrate内だけ。後続の固定prompt/schemaから次のように作る（例はcrate内での使い方。まだhandlerに登録しない）。

```rust,ignore
let requests = CloudClients::new(&config.ai)?; // 一度作って共有
let client = requests.client(TaskKind::PhotoJudge).unwrap();
// 既存imagingでデコード・向き適用・縮小を終えたRGBから新規JPEGを作る。
let image = PreparedImage::encode_jpeg_rgb(&rgb, w, h, 85, ImageProfile::Decision)?;
let request = GenerateRequest::photo_judge(FIXED_PROMPT, &fixed_schema, &[(1, image)])?;
let reply = client.run(ctx, &request)?;
// reply.json_text()をT4.5のdomain parserへ渡す。本文をログに出さない。
```

位置候補は`GenerateRequest::location_suggest`を使い、`TaskKind::LocationSuggest`のctx/clientと組み合わせる。異なる用途・設定の組合せは拒否する。constructorはtext・重複しない正の画像番号・`PreparedImage`・response schemaを受け、`contents`と`generationConfig`だけを内部構築する。圧縮bytes・dataURL・URL・path・任意inlineDataを受ける入口は無い。完成JSONをbase64込み8 MiBで検査し、超過は鍵／DNS／HTTPより前に断る。

生成設定は既存Pythonの`ai_pipeline.py::_judge_group`・`loc_suggest.py::ask_gemini`に合わせ、`temperature: 0.1`、`responseMimeType: application/json`、`responseJsonSchema: <渡されたJSON Schema>`とする。小文字の`object/array/integer/string/boolean`や`properties/required/enum/minimum/maximum`等はそのまま渡し、`responseSchema`へ変換・併記しない。[Google GenerationConfig Java資料](https://docs.cloud.google.com/java/docs/reference/google-cloud-vertexai/latest/com.google.cloud.vertexai.api.GenerationConfig)はJSON Schemaを受ける代替fieldと排他条件を記載し、[現行REST資料](https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/rest/Shared.Types/GeminiExample)にも両fieldがある（2026-10-27確認。REST資料では`responseJsonSchema`はdeprecated表記だが、今回は既存Python互換を維持する）。実APIで受理・拒否されたという検証ではない。

応答は展開後1 MiB（署名メタデータ込みの総本文）、圧縮前は`net::wire_limit`（2 MiB + 64 KiB）で制限。2xx以外の本文は読まない。単一candidate、`finishReason=STOP`、各Partが必須`text: string`と任意`thoughtSignature: string`だけである場合に、textを順に連結してJSON object/arrayを取り出す。最後の空text＋署名Partを含む分割JSONも許す。[Googleのthought signatures資料](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/thinking/thought-signatures)は非functionCallの最後のtext Partにも署名が付くと記載している（2026-10-27確認）。単発要求なので署名は破棄し、`JsonReply`・Debug・ログ・後続の保存へ渡さず、会話履歴として再送もしない。型不正の署名、text以外のpayload（functionCall/fileData/inlineData/URL/未知field等）、切れた応答、不正JSONは固定`BadResponse`。未知payloadを一括無視しない。モデル応答のURLをfetchしない。写真判定／地名／座標などの意味とschema適合の確認、有償回答の保存はT4.5/T4.6cが行う。

## 固定宛先と通信

設定を再検査してから以下を組み立てる（親の設計判断は[p4-design.md §5](p4-design.md)）。未知のモデル・regionの実在は推測しない。

| region | APIのhost |
| --- | --- |
| `us` / `eu` | `aiplatform.us.rep.googleapis.com` / `aiplatform.eu.rep.googleapis.com` |
| `global` | `aiplatform.googleapis.com` |
| 通常region | `{region}-aiplatform.googleapis.com` |

推論pathは`/v1/projects/{project}/locations/{region}/publishers/google/models/{model}:generateContent`。project≤63・region≤32は小文字ASCII英数字とhyphen、model≤64は英数字と`._-`を許す単一segment。先頭末尾のhyphen、`..`、区切り、userinfo、query、percent、control等を拒否する。認証は`https://oauth2.googleapis.com/token`だけ。すべてHTTPS・443固定。

名前解決の全A/AAAAが通常の公開ユニキャストであるときだけ検査済みアドレスを接続へpinする。private、loopback、link-local、CGNAT、unspecified、multicast、文書用・予約・変換用・mapped IPv6などが1つでもあれば全体拒否。空・17件以上・期限超過も拒否する。Host/SNIと証明書検証は元のGoogleホストのまま。redirect追跡0、環境proxy不使用、`Origin`・LANの`X-Client-Name`なし。認証headerは推論APIだけ。Private Service Connectや任意proxyの設定入口は無い。loopback/DNS/接続hookの差替えは`cfg(test)`だけ。

`http::build_agent`・`read_reply`・`Deadline`、`dest::PinResolver`、`gate::GateConnector`をLANと共有する。`ureq` unversioned API更新時は[ai-provider.md](ai-provider.md)の回帰に加えて`ai::cloud`も試験する。

## 認証とcache

AI JSONの明示`credential_path`だけを遅延読取りする。Drive鍵、ADC、環境のGoogle認証を探索しない。通常ファイル・64 KiBまで、リンクは辿る。FIFOを開く前のmetadata検査と`O_NONBLOCK`で差替え時の待ちも防ぐ。開いた後のmetadataと読取り後の上限も検査する。共有部品`service_account`は`client_email`・`private_key`だけを読む。鍵JSONの`token_uri`は無視する。

既存`jwt::Rs256Signer`/`GoogleAssertion`でRS256署名する。scopeは`https://www.googleapis.com/auth/cloud-platform`、audienceは固定token URL、JWTはiatから3600秒。鍵JSON/PEM/署名失敗は固定`Unauthorized`。権限警告はAI用固定文だけ。path/email/private key/token/assertion/本文/モデル/URLは新しいDebug・ログ・errorへ出さない。

AI専用cacheはcredential path + scope + audienceのキーを持ち、DriveSharedと混在しない。期限の60秒前でrefreshし、注入時計で期限・時計巻戻りを試験する。空／CRLF／非ASCII／16 KiB超token、不正expiry（60秒以下・3600秒超・負・非整数）を拒否する。cache mutexはメモリ操作中だけで、ファイル／署名／HTTP中に保持しない。並行交換の重複は許容。401による削除は、実際に拒否されたtokenのArcが現在cacheと同一の場合だけ行い、古い401が新tokenを消さない。同じパスの鍵差替えは次回のtoken更新時に読む。

Driveは同じ小さな読取り部品を使うが、scope/cache/agent/HTTP/待機付きretryは従来のまま。`net::agent`は変更していない。

## 予算・期限・失敗分類

順序は要求完成／上限→`before_probe`（許可・停止・pause、予算なし）→共有Provider枠→token取得→`before_probe`/`before_send`→推論POST。未許可、設定limit0、停止、不正／超過要求では鍵・DNS・HTTPが0。writerなしのcloudは既存gateが`policy_disabled`相当で拒否する。token取得は予算外・job attempt外。

推論は1送信ごとに1予約。401だけ一度cacheを無効化／更新し、再度probe/send gateを通して送る。再401はProviderの`Unauthorized`停止。2枠目が取れなければ再送しない。予約後にDNS/接続/停止で送れなくても消費は戻さない。

`Deadline::for_task`はphoto_judge 120秒、location_suggest 180秒。token交換は最大10秒かつ全体残時間内。Provider枠待ち・token・refresh・送信予約のwriter待ち・再送で全体期限を延ばさない。cloudは内部の`before_send_until`でwriterの待ち上限を既定5秒と全体残時間の短い方にする。期限内に始まらなかった予約は、writerが空いても実行しない。始まった予約は従来どおり取り消さず、HTTPのwrite前に期限切れならProviderWaitのまま。内部のsleep付きretryは無い。

| 失敗 | 分類 |
| --- | --- |
| 推論の最初のHTTP write試行前のDNS/接続/TLS/timeout | ProviderWait(Unreachable)、attempt 0（予約は残る） |
| 推論のwrite開始後のtimeout/切断/5xx | Attempt、worker既存上限5回 |
| 401（更新後も）/403 | ProviderPause(Unauthorized/Forbidden) |
| 推論model404 | TaskPause(ModelNotFound) |
| 3xx/宛先拒否/契約不一致 | ProviderPause(ContractMismatch) |
| 429 | ProviderWait、Retry-Afterをworkerが60..3600秒に制限 |
| 推論400/413/422 | そのjobだけPermanent(InputInvalid/InputTooLarge) |
| 推論の不正／過大2xx応答 | Attempt(BadResponse) |
| tokenの認証4xx/不正token | ProviderPause、推論0・予算0・attempt 0 |
| token429/5xx/timeout/通信障害 | ProviderWait、予算0・attempt 0 |

要求ごと・transportごとの最初のwriteでstop gateを確認する（DNS/TCP/TLS待ちの後）。最初のwrite**試行**を送信開始と記録し、そのwriteが失敗して実際の到達bytesが不明でも送信後として保守的に分類する。書き始めた後の本文・応答はstopで中断せず、有効な有償結果を返す。LAN/Decisionの既存分類・再probeは変更しない。

## 検証と後続

合成未登録RSA fixtureでJWT署名/scope/aud/期限、明示鍵・token_uri無視、巨大/FIFO/不正鍵、cache分離・期限・競合・I/O中mutex非保持を確認する。loopbackと偽DNSでHost/SNI、proxy/redirect、public DNS混在、要求/応答/圧縮前上限、stopの位置、401の送信数と予算、未送信／送信後の分類、writer利用と枠の独立、全体期限、Debug/log秘匿を確認する。

schema・既存有償結果・一次データの変更は無い。設定不正時のfeed継続、旧DB読み取り、Drive/Decision回帰はworkspace全体で確認する。実クラウド疎通、Google側でのモデル実在・権限・費用、実写真、正常なTLS証明書を伴う本番接続は未確認。実利用は用途・画像・費用の別途許可後。T4.6bはDecision画像準備/clue/写真別handler、T4.6cは日単位候補・有償回答保存・座標化、T4.5は固定promptとdomain parse、T4.4は#93と対話APIの契約を扱う。

T4.6aの実施結果: クラウド25試験、AI全体227試験が成功。`cargo test --workspace`は全28 suite成功、1023 pass・2 ignore。`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、musl release buildが成功し、`file`でstatic-pie、`--version`の起動を確認した。loopbackソケット試験はsandboxの`Operation not permitted`を確認後、権限付きで実行した。

LXCでの初回受入後の検証修正（2026-10-27）: 期限試験はRSA署名速度により2回目の推論が始まらないことがあり、実装の正しい未送信分類を誤って失敗としていた。試験専用の制御時計とtransportを使い、実際のwrite gateを通る前／後、token／401更新の期限切れを固定する。HTTPに渡す残時間を1000→800→600→400 msと厳密に検査し、予約数も0／1／2で固定した。認証CPU時間をwall clockで推測するsleepは廃止した。別途、送信予約が全体期限より長い固定5秒を待てる実装上の隙間を修正し、cacheを事前準備した実writerの待ちを200 msで切る回帰試験を追加した。

wire上限試験の失敗も試験入力の計算量が原因だった。既存gzip helperを拡張し、[RFC 1952 §2.3.1.1](https://www.rfc-editor.org/rfc/rfc1952#section-2.3.1.1)に従う大きなFEXTRAを持つ34 memberで圧縮前上限を超える入力を作る。展開後は空のまま。展開後上限+1の圧縮入力とは分け、合法headerの復号と展開後上限ちょうどの成功も確認する。両超過は正確な`ResponseTooLarge`かつ`started=true`を要求する。

否定対照は、wire上限の無効化で空の正常応答になって試験が失敗、各HTTPで期限を再開始すると残時間の比較が失敗、writerの全体期限制限を外すと待ち時間の検査が失敗することを確認し、すべて復元した。修正後はfmt／workspace全targets clippy（`-D warnings`）／workspace全28 suite（1024 pass・2 ignore）／musl releaseが成功。ローカルmusl debugのcloud全26件（元の失敗2件と新writer回帰を含む）も成功した。LXCでの再実行は親タスクが担当する。通常レビューはOpusのレート制限により未実施のまま。

Sol簡易レビュー指摘の修正（2026-10-27、修正前HEAD `6c4f509`、原タスク開始 `dd5b4a3`）: 正常text、text＋合成署名、末尾の空text＋署名を持つ分割JSON（object/array）が成功し、他payload・型不正署名・不正JSON・STOP以外・複数candidate・本文超過が拒否される回帰試験を追加した。署名付き200を返すloopback＋合成worker/DBの統合で、daily limit 1でもジョブ成功・推論1要求／予算1・token交換1回、追加cycleでも再送なし、`JsonReply`／Debug／ログ／合成保存値に署名なしを確認した。本番の保存handlerは追加していない。

両用途のschema fixtureは`crates/geoframe/tests/fixtures/ai-cloud-schemas-python.json`。出所は`app/geoframe/ai_pipeline.py:JUDGE_SCHEMA`と`app/geoframe/loc_suggest.py:SCHEMA`で、`tools/gen_ai_cloud_schema_fixture.py`が必要な定数参照とliteral/listだけをASTで展開する。アプリのimport・実行や環境／秘密の探索はしない。`python3 tools/gen_ai_cloud_schema_fixture.py --check`で現行定数との完全一致を確認する。Rustの両用途試験は`include_str!`でfixture全体をコンパイル時に含め、そのままのJSON Schema・`responseSchema`不在・temperature 0.1・JSON MIMEを検査し、LXC上のソース絶対パスからの実行時読取りに依存しない。固定prompt・domain parser・handler登録は後続のまま。

今回の旧版否定対照: `Part.object.len() == 1`の旧制限へ戻すと署名許可parser回帰と署名付き200統合が失敗した。旧`responseSchema`＋temperature省略へ戻すと両用途のschema fixture回帰がともに失敗した。いずれも一度だけ実行し、修正版へ復元した。

修正後の結果: cloud全31件成功。`cargo test --workspace`は全28 suite成功、**1029 pass・0 fail・2 ignore**（今回5件追加、既存Drive回帰を含む）。`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo build --workspace --release --target x86_64-unknown-linux-musl`が成功し、musl binaryのstatic-pieと`--version`起動も確認した。ソケット制限による`Operation not permitted`の後、loopbackを使う試験は権限付きで実行した。実クラウド・実鍵・実写真・実DB・SSH・本番は未使用。LXCでの再実行と別Solによる修正後レビューは親タスクが担当する。親所有の`tasks.md`・`p4-foundation-acceptance.md`は編集・stageしない。
