# AI設定（`GEOFRAME_AI_CONFIG`）v1

- 状態: T4.1bで実装し、T4.2〜T4.6で用途・接続を追加した（最終の状態を書く。当時の記録は節ごとに明示）。設計は[p4-design.md](p4-design.md) §3。ここは**設定の読み取りと検査**。接続先の固定・Discovery・Decisionの入出力・共有の一時待機は[ai-provider.md](ai-provider.md)（T4.2）。クラウドの認証・送信は[ai-cloud.md](ai-cloud.md)、日候補の本番接続は[ai-location-suggest.md](ai-location-suggest.md)を参照。
- `GEOFRAME_AI_CONFIG`（Rustだけの項目）は、用途別のrouting・クラウド許可・ベクトル空間をまとめた小さなJSONのパス。**未設定なら新しい外部AI呼び出しは無効**（写真の配信・管理・保存済み推薦の読み取りは動く）。相対パスは起動したカレントディレクトリ基準。

## 読み込みの決まり

- 通常のファイルだけ・**64KiBまで**・UTF-8のJSONオブジェクト。**全ての階層で未知のキーと重複したキーは不正**（秘密をうっかり書き足しても受けない）。
- 起動時にDNS・ネットワーク・認証ファイルの読み取りをしない。URL・パスは文字列として検査するだけ。
- **誤った設定はAIだけを無効にする**: JSONが不正、または旧環境変数の数値が不正でも、`serve`は起動して写真を配信する。ログには種類のコード（下の表）だけを出し、パス・中身・URL・モデル名・`serde`のエラー文は出さない。起動ログの`ai=`は`unset`／`invalid:<コード>`／`ready:tasks=<件数>`。STATUSの`ai_config_invalid`は下の「管理画面のSTATUS」。
- 設定の変更は再起動で反映する。

| コード | 意味 |
|---|---|
| `unreadable` / `not_regular_file` / `too_large` | 開けない／通常のファイルでない（ディレクトリ・FIFO）／64KiB超 |
| `syntax` | JSONとして読めない（構文・UTF-8・末尾のゴミ） |
| `duplicate_key` / `unknown_key` / `missing_key` | 重複したキー／未知のキー／必須のキーが無い |
| `bad_value` / `unsupported_version` | 型・範囲・文字が違う／`version`が1でない |
| `conflict` | キーの組み合わせが矛盾（LANの用途にクラウドの項目、`location_clue`のクラウドなど） |
| `legacy_env` / `path_invalid` | 旧`GEOFRAME_AI_*`の数値が不正／`GEOFRAME_AI_CONFIG`を絶対パスにできない |

## 形（例は合成の名前・予約アドレスだけ）

```json
{
  "version": 1,
  "local_base_url": "http://192.0.2.10:8100",
  "embedding_space": {
    "space_id": "example-space-1",
    "model_id": "example-embed",
    "revision": "r1",
    "image_preprocess": "img-v1",
    "text_preprocess": "txt-v1",
    "dimension": 768,
    "normalization": "l2"
  },
  "tasks": {
    "image_embedding": {"provider": "local", "model": "example-embed", "service_id": "example-embed-svc"},
    "query_embedding": {"provider": "local", "model": "example-embed", "service_id": "example-embed-svc"},
    "photo_judge": {
      "provider": "cloud", "model": "example-judge",
      "allow_cloud": true, "daily_request_limit": 20, "include_hidden": false,
      "credential_path": "/etc/example/credentials.json",
      "project": "example-project", "region": "example-region1"
    },
    "location_clue": {"provider": "local", "model": "example-decision"},
    "location_suggest": {
      "provider": "cloud", "model": "example-suggest",
      "credential_path": "/etc/example/credentials.json",
      "project": "example-project", "region": "example-region1"
    }
  }
}
```

| キー | 必須 | 既定・規則 |
|---|---|---|
| `version` | 必須 | `1` |
| `local_base_url` | LANの用途があるとき | `http`／`https`の`scheme://host[:port]`だけ（末尾の`/`は可）。userinfo・query・fragment・パス・空白・制御文字・非ASCII・`%`は不正。ホストはIPリテラルかDNS名の書式（ここでは解決・privateの判定をしない。接続時にprivate/loopbackだけへ固定する: [ai-provider.md](ai-provider.md)）。APIのパスは実装が固定で付ける |
| `embedding_space` | 画像・検索語のEmbeddingを使うとき | 画像と検索語に共通のベクトル空間の識別。`space_id`・`model_id`・`revision`・`image_preprocess`・`text_preprocess`は英数字で始まる64字以内（英数字と`.` `_` `-` `:`）、`dimension`は1〜4096、`normalization`は`l2`か`none`。**無い間はEmbeddingの用途は無効**（版が分からない接続を有効にしない）。後続のタスクがProviderの応答と照合する検査済みのメタデータで、ここでAPIは呼ばない |
| `tasks.<用途>` | 任意 | 用途は`image_embedding`・`query_embedding`・`photo_judge`・`location_clue`・`location_suggest`の5つ（geocodeはLLMのroutingが無い）。書かない用途は無効 |
| `provider` | 必須 | `local`（LANの入口）か`cloud`（実装が組み立てる固定のHTTPS宛先） |
| `model` | 必須 | 英数字で始まる64字以内（英数字と`.` `_` `-`） |
| `service_id` | `local`のとき（`location_clue`を除く） | LANのサービスID（同じ文字種）。`location_clue`（Decision。パスは固定）と`cloud`では置けない |
| `allow_cloud` | `cloud`のみ | **既定`false`**。`false`なら`provider=cloud`でもその用途は送らない（用途ごとに独立） |
| `daily_request_limit` | `cloud`のみ | 1UTC日の送信数（〜1,000,000）。**未設定・0なら送らない** |
| `include_hidden` | `cloud`のみ | 既定`false`。クラウドの画像送信に`hidden`の写真を含めるか |
| `credential_path`・`project`・`region` | `cloud`で全て必須 | 認証ファイルの**絶対パス**（中身は起動時に読まない。秘密はJSONに埋めない）。`project`は小文字・数字・`-`の63字以内、`region`は同32字以内 |

`local`の用途にクラウドのキー（`allow_cloud`・`daily_request_limit`・`include_hidden`・`credential_path`・`project`・`region`）を書くと`conflict`。`location_clue`は**LANのDecisionに限る**（`cloud`は`conflict`）。

## 送信できる条件（用途ごと・送る前に毎回）

`AiConfig::gate`が理由つきで判定する。次のどれかなら送らない: 設定が未設定・不正／`GEOFRAME_AI_ENABLED`が`1`でない／`GEOFRAME_LOC_SUGGEST`が`1`でない（位置の2用途）／用途が未設定／**1周期の上限が0**／Embeddingで`embedding_space`が無い／クラウドで`allow_cloud`が無い／クラウドで`daily_request_limit`が未設定か0／**writerが無い（`GEOFRAME_INGEST=0`）**（クラウドは予算を予約できないので迂回して送らない。ジョブの用途は`ai_jobs`に書くので同じ。LANの検索語Embeddingと適合するメモリキャッシュだけは使える）。

クラウドの送信は、送る直前に`ai_request_usage`へ**UTC日と周期窓の両方の枠を原子的に予約**し、予約できたときだけ送る（`ai::budget`）。UTC日は固定窓、周期窓は**直近`cycle_secs`秒のローリング窓**（予約した時刻ごとに数え、窓の長さを履歴のキーに含めないので、周期を変えても使った分は復活しない。以前の固定窓の行は最後に足した時刻の予約として数え、移行は不要）。履歴の最新時刻（高水位）より`now`が前の予約は、枠の復活を防ぐため固定の理由（`Reservation::ClockRewound`）で拒否する（古い周期窓の掃除や再起動をまたいでも）。失敗・時間切れ・中断・再試行をしても消費は戻さず、再起動・設定変更でも履歴は消えない。対話的な予約（検索語）はwriterの順番待ちを含め200msで戻り、間に合わなければ送らない。

## 旧環境変数との対応

旧`GEOFRAME_AI_*`は周期と1周期の上限として読む（`ai::settings::LegacyAi`）。**旧設定だけではRustは推論を始めない**（`GEOFRAME_EMBED_BACKEND`は旧データの表示用の識別のまま）。数値が不正（数でない・負・有限でない。`GEOFRAME_AI_POLL_S`も負は不正で、0や小さな正値だけが30に切り上がる）なら`legacy_env`でAIだけ無効（JSONは読まない）。フラグは現行と同じく`1`だけが有効。

| 環境変数 | 既定 | 対応 |
|---|---|---|
| `GEOFRAME_AI_ENABLED` | `1` | `1`以外は全用途を無効（AI検索E6は現行どおり503） |
| `GEOFRAME_AI_POLL_S` | `60` | AIワーカーの周期（秒。30未満は30。上限は86400） |
| `GEOFRAME_AI_BACKLOG_POLL_S` | `5` | **Rustだけ**。仕事が残っているときの次の周期までの待ち（秒。1〜`GEOFRAME_AI_POLL_S`に収める。数でない・負・有限でない値は既定の`5`へ戻し、AIは無効にしない。[背景ワーカー](#背景ワーカー)の「1周期」） |
| `GEOFRAME_AI_EMBED_PER_CYCLE` | `12` | `image_embedding`の1周期の上限 |
| `GEOFRAME_AI_JUDGE_PER_CYCLE` | `2` | `photo_judge`の1周期の上限 |
| `GEOFRAME_AI_SEARCH_PER_MIN` | `30` | `query_embedding`の毎分（60秒の窓）の上限 |
| `GEOFRAME_LOC_SUGGEST` | `1` | `1`以外は`location_clue`・`location_suggest`を無効 |
| `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE` | `10` | `location_clue`の1周期の上限。**Rustの既定は`config.py`の`60`と違う**（下の注） |
| `GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE` | `2` | `location_suggest`の1周期の上限 |

**Rustだけ既定が違う項目**: `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE`はRustでは`10`（`config.py`は`60`のまま変えない）。Rustのワーカーは専用の1スレッドが用途を順に処理するので、1件約12秒の手がかり判定が60件続く（約12分）と、その間ほかの用途（画像Embedding・位置候補）が進まない。10件なら約2分で次の用途へ回る。`GEOFRAME_AI_BACKLOG_POLL_S`はPythonに無いRustだけの項目なので`docs/reference/configuration.md`（`config.py`から生成）には載せない。クラウドの日の上限・周期の窓（`GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE`など。窓の長さは`GEOFRAME_AI_POLL_S`のまま）は変えないので、周期が速くなっても日の上限は守られる。

`GEOFRAME_DECISION_URL`・`GEOFRAME_LOC_SUGGEST_MODEL`・`GEOFRAME_LOCAL_EMBED_*`は**読まない**（routingとモデルは上のJSON。Decisionは`local_base_url`に固定のパスを付ける）。廃止するCPUモデル用の設定は後続のタスクで文書化する。

## ハッシュ・停止

設定全体のハッシュ・用途の送信ポリシーのハッシュ（SHA-256）は監査と送信可否の検査用で、**ジョブの冪等キーには入れない**（URL・許可・上限だけの変更で成功済みの結果を再送しないため）。Providerの一時停止（認証の失敗・403・モデル404・契約不一致。`ai_provider_pauses`）は、接続先・認証・モデルなど**その理由に関係する設定だけ**から識別を作り、再起動では解除されず、関係する設定が変わるか管理CLI（`geoframe ai-jobs release-pause`。下の「管理操作」）の明示の解除で効かなくなる。全ジョブを`failed`にはしない。到達不能・GPU lease・429などの**一時待機**（`ai_provider_waits`）は別物で、`until`（60〜3600秒先）が過ぎれば自然に消え、停止とは混同しない（[ai-provider.md](ai-provider.md)）。

## 背景ワーカー

専用スレッド1本が、既存のwriter（`Ingest.writer`）と`.ingest.lock`（`LockHold`）を共有して動く（`ai::worker`。T4.1cで導入）。本番で登録するhandlerは、画像Embedding（`embedding_contract`が揃うとき。[ai-embedding.md](ai-embedding.md)）・写真別[location_clue](ai-location-clue.md)・日候補の[location_suggest／location_geocode](ai-location-suggest.md)・任意のGemini比較[photo_judge](ai-scene-judge.md)（用途のクラウド許可とクラウド接続が揃うとき）で、設定が無効・接続や契約が揃わない用途は登録されない（fakeのProvider・スタブの設定は置かない）。登録が無い種類のジョブは、claimも成功化もされず`pending`/`deferred`のまま残る。

- **起動の条件**: `GEOFRAME_INGEST=1`かつ`GEOFRAME_AI_ENABLED=1`かつ有効な背景用途が1つ以上。`query_embedding`は利用者を待たせる対話の用途で、永続キューの背景ジョブではない。`INGEST=0`では何も起動せず、AIの表を作らず、ジョブの回収もProvider停止の整理もしない。
- **起動の準備**（`INGEST=1`なら、ワーカーが無効でも。ロックを取ったあと）: ①前の起動の`running`を回収する。停止開始が記録された起動のものは数えず`deferred/shutdown`、記録の無いもの（`SIGKILL`・クラッシュ・電源断）は`interruption_count`を加算して3回で`failed`。②設定が**有効なときだけ**、関係する設定が変わって効かなくなったProvider停止を整理する（`release_unmatched`）。設定が未指定・不正のときは、現在の識別が分からないので既存の停止を1件も消さない。
- **種類と用途**: `image_embedding`→`image_embedding`、`location_clue`→`location_clue`、`location_suggest`・`location_geocode`→`location_suggest`（geocodeはLLMのroutingを持たず、位置候補の後続として同じ用途の有効・無効・停止・上限に従う）、`photo_judge`→`photo_judge`。
- **1周期**（`GEOFRAME_AI_POLL_S`ごと、起動直後にも。仕事が残っているときだけ`GEOFRAME_AI_BACKLOG_POLL_S`、既定5秒ごと。下の「次の周期までの待ち」）: ①reconcile: 使える用途のhandlerが現在の写真・結果から導いたキーを積む（冪等。別世代は`obsolete`）。設定で許可されない用途は積まない。②登録済みで使える種類だけでclaim→writerの外で`Handler::run`→writerで確定（token・入力世代の確認つき）、を空になるまで繰り返す。使えない種類は**claimの対象から外すだけ**で、他の種類は進む: 未登録、設定で無効（`Disabled`）、Provider・用途の停止中、Provider側の待ち（利用不可・429・予算切れ）、1周期の上限（上の表の`*_PER_CYCLE`。LANの用途にも効く）に達した用途。
- **次の周期までの待ち**: 1周期を終えたら、通常は`GEOFRAME_AI_POLL_S`だけ待つ。ただし**1周期の上限に達して止めた用途に、いま`claim`できるジョブ（`pending`／期限の来た`deferred`）が残っているとき**は、`GEOFRAME_AI_BACKLOG_POLL_S`（既定5秒、1〜`POLL_S`）だけ待って次の周期を始める。残りが無くなる（上限に達しない周期で終わる）と通常の待ちに戻る。次のものは「残り」に数えず、通常の待ちのまま（`claim`できないジョブだけが残る状態で空回りしない）: 停止中のProvider・用途、Provider側の待ち（利用不可・429・予算切れ）と共有の一時待機、期限前の`deferred`、設定で使えない用途、停止の合図が立った後。**クラウドの送信枠（日・周期窓）のある用途では、送信枠を予約して送る種類（`photo_judge`・`location_suggest`の推定）の残り**も数えず、通常の周期で進める（送信枠に当たるたびの予約の失敗と`deferred`を繰り返さないため。クラウドへの送信は速めない）。同じ用途でも送信枠を使わない`location_geocode`（座標化はNominatim）の残りは、`location_suggest`の上限に達していれば数える。待ちは停止の合図（SIGTERM）で即座に抜け、ワーカーのjoin期限（10秒）・全体の期限（60秒）は変わらない。同一場面の局所計算のスレッド（`ai-scenes`）は通常の周期のまま。
- **送る直前の確認**: handlerは外部へ送る**直前に毎回**`RunCtx::before_send`を呼ぶ。停止の合図→今の設定の送信可否→Provider・用途の停止→（クラウドの用途は）送信予算の予約、の順に再評価し、全部通ったときだけ接続情報（`Gate`）を返す。送る直前に、DBの行・入力世代・hiddenを読み直して古ければ`RunError::InputChanged`（ジョブは`obsolete`で送らない）。writerやトランザクションを外部I/Oの間は持たない（`RunCtx::read`は読み取り専用の接続）。
- **実行エラーの分類**（`RunError`。固定のenum。例外の本文・URL・応答は持たない）:

| `RunError` | ジョブ | その他 |
|---|---|---|
| `Stopping` | `deferred/shutdown`（失敗に数えない） | 送っていない |
| `InputChanged` | `obsolete` | |
| `PolicyDisabled` | `deferred/policy_disabled`（失敗に数えない） | 各handlerは`Disabled`をこれに対応付ける |
| `Paused` | `deferred/provider_paused` | |
| `Budget(予約の結果)` | `deferred`。1日の枠切れ・周期の枠切れは`budget_exhausted`（待ちは3600秒・周期窓の長さ）、時計の巻き戻りは`clock_rewound`（600秒）、順番待ちの超過などは`internal` | その用途を待ちの間は送らない。**予算をリセットする救済は無い**（時計が履歴に追いつくまで送らない） |
| `Unavailable` | `deferred/unavailable`（60秒後以降） | その用途を60秒送らない（この起動の中の用途単位。Provider全体へ広げる待ちは次の`ProviderWait`） |
| `ProviderWait(scope, reason, retry_after)` | `deferred/unavailable`または`rate_limited`（60〜3600秒） | 共有の一時待機を`ai_provider_waits`に記録し、`scope=Entrance`なら同じProviderの他の用途もclaimの前に飛ばし、`Service`なら同じサービスを使う用途だけ。別のProvider・別のサービスは進む。再起動後も`until`まで効く。詳細は[ai-provider.md](ai-provider.md) |
| `RateLimited` | `deferred/rate_limited`（Retry-Afterを60〜3600秒に収める） | その用途をその間送らない |
| `Attempt` / `Repeated` / `Permanent` | 失敗1回（5回で`failed`）／反復失敗（3回目から失敗に数える）／即`failed` | `jobs::Failure`と同じ |
| `ProviderPause` / `TaskPause` | 失敗に数えず後回し | 停止をDBに記録（再起動では解除されない） |

handlerの**panic**は当該ジョブの失敗1回（`panic`。5回で`failed`）に数え、同じジョブが全AIを塞ぐ無限ループにならない。

### 停止と回収

- **SIGTERMを受けた直後に**共有の`StopFlag`が立つ（HTTPのgraceful shutdownが終わるのを待たない）。ワーカーはclaimの前と送信の直前に確かめ、止まっていればclaimも送信もしない。claim済みで送る前に止まったジョブは`deferred/shutdown`に戻る（失敗にも中断にも数えない）。
- 起動IDと**停止開始**は`ai_worker_runs`（起動ID・開始・停止開始の時刻）に残る。停止開始はSIGTERMの直後に専用の小さなスレッドが書く。
- ワーカーのjoinの間（最大10秒）はtokenが有効で、待っていた**有効な応答は確定される**（受信済みの有償結果を一律に捨てない）。joinの期限までに終わらないhandlerは、スレッドごと残り（`LockHold`を持ったまま。別の書き手は始まらない）。期限つきの停止記録・最後の戻しのSQLがwriterで待っている場合も、DBの書き込みスレッド自身がロックを持つので、ワーカーや`Ingest`が先に終わってもSQLと接続の終了までロックは外れない。最後の戻し（`deferred/shutdown`・token失効）のあとに届いた応答は保存されない。
- 停止開始の記録も最後の戻しも**共通期限つき**（`Writer::call_within`）で、writerが忙しくても停止の期限を越えて待たない。最後の戻しが書けなかった場合も、次の起動の回収が停止開始の記録で区別するので、予期しない中断として加算しない。

### 管理操作（`geoframe ai-jobs`）

| コマンド | 内容 |
|---|---|
| `geoframe ai-jobs status` | **読み取りだけ**（ガード・ロック不要。稼働中でも使える。AIの表が無い旧DBは0件）。設定の状態、検索空間の有無・登録数と索引の件数（対象・保存済み・待ち・失敗で除外・壊れ。[ai-index.md](ai-index.md)）、用途ごとの使える／使えない（固定の理由コード。設定では使えても**Provider・用途の停止にかかっている用途**は`使えない（停止中: Provider全体|用途・<理由コード>）`と、その用途の待ち（`pending`＋`deferred`。`location_geocode`は`location_suggest`に数える）の件数`用途 <用途> の待ち: N 件（停止中のため送っていません）`。停止の確認ができないときは`使えない（停止の確認ができません）`）、種類×状態の件数、後回し・失敗・待機の理由コードごとの件数、停止中のProvider・用途（停止を起こした用途だけ。同じ接続先・認証・project・regionの用途は上の用途の行に出る）、有効な共有の一時待機（用途・範囲・固定の理由・UTCの期限） |
| `geoframe ai-jobs retry (--kind <種類>｜--all)` | `failed`だけを、登録済みhandlerが現在の入力・既存の結果に照らして再検査し、`Requeue`（カウンタ・履歴を0に戻して`pending`）／`Obsolete`／`Satisfied`（成功済み扱い。再計算しない）へ振り分ける。**外部へは送らない**。成功済み・待機中は変えない。検査できないものは戻さず`failed`のまま件数だけ報告する: handler未登録の種類（設定が無効、または固定契約・クラウド接続が揃わず登録されない用途）、設定で今は使えない用途 |
| `geoframe ai-jobs release-pause (--task <用途>｜--all)` | Provider・用途の停止の明示解除。`--task`は今の設定でその用途に当たる停止だけ（設定が未指定・不正なら何もせず終了コード2）、`--all`は全部 |
| `geoframe ai-jobs deactivate-space` | 有効な検索空間（`settings.ai_active_space`）を外して旧結果参照へ戻す（P4 / T4.3。[ai-index.md](ai-index.md)）。旧結果・空間・ベクトル・ジョブは消さない。稼働中は拒否 |

`retry`・`release-pause`・`deactivate-space`は**書く**ので、`GEOFRAME_INGEST=1`、検証環境のガード（`sync`と同じ印・Pythonの確認）、`.ingest.lock`、writerを通す。稼働中の`serve`はロックを持つので、**サービスを止めてから**実行する（稼働中は終了コード1で断る）。出力は件数・用途・固定の理由コードだけで、対象のキー・入力のhash・token・起動ID・モデル・URL・パス・写真の値は出さず、生の対象IDも引数に取らない（範囲は種類・用途単位）。

### 管理画面のSTATUS（`GET /api/admin/status`の追加キー）

Rustだけが返す追加フィールドで、**あるときだけ**付く（AIの表も設定の誤りも停止も無いDB、Python版の応答には1つも付かず、既存のキーの意味・値は変えない）。`tests/admin_api.rs`が、追加キーを除いたPython版のフィクスチャとの完全一致と、追加キーの形を固定している。

| キー | 値 |
|---|---|
| `ai_config_invalid` | `{"code": <設定の誤りの固定コード>, "disabled_tasks": 5}`（設定が不正なときだけ。値・パスは出さない。写真の配信・管理は続く） |
| `ai_jobs` | `{"total", "by_state", "by_kind", "reasons"}`（ジョブが1件でもあるとき。`reasons`は状態ごとの理由コードの件数で、`clock_rewound`・`budget_exhausted`・`provider_paused`・`interrupted`など。固定のコード以外は`other`にまとめる） |
| `ai_pauses` | `[{"task", "scope", "reason"}]`（停止中のProvider・用途。解除はCLI） |
| `ai_task_blocks` | `[{"task", "scope": "provider"\|"task", "reason", "waiting"}]`（設定で使える用途のうち、Provider・用途の停止にかかって送られていないもの。`ai_pauses`が停止を起こした用途だけなのに対し、同じ停止にかかる**全用途**。`waiting`はその用途の待ち＝`pending`＋`deferred`（`location_geocode`は`location_suggest`）の件数。ジョブの件数を集計できなかったときは`null`（件数不明。0件とは区別する。ジョブの表が無い旧DBは集計できているので0）で、停止の情報（`task`・`scope`・`reason`）は残す。停止にかかる用途が無ければキーを足さない。停止の確認ができない用途は載せない。用途名・範囲・固定の理由コード・件数だけで、鍵のパス・project・region・URL・モデル・識別のhashは出さない。送信側の判断は変えない） |
| `ai_waits` | `{"items": [{"task", "scope": "entrance"\|"service", "reason", "until": "YYYY-MM-DDTHH:MM:SSZ"}], "unidentified": n}`（まだ有効な共有の一時待機を今の設定の用途に結び直したもの。`unidentified`は設定が使えず結び直せない件数で0なら出さない。待ちが無ければキーを出さない。期限はUTC。接続先・モデル・ハッシュは出さない。[ai-provider.md](ai-provider.md)の「共有の一時待機」） |

STATUSタブ（`admin.js`）は、これらを赤・黄のセルで出し（`ai_task_blocks`は「AI送信が止まっている用途」に`<用途>: Provider全体の停止（権限エラー（403））で N 件が待ち`の形。`waiting`が`null`のときは`で 待ちは件数不明`。`geoframe ai-jobs status`は`用途 X の待ち: 件数を読めません（停止中のため送っていません）`）、解除・再試行のコマンドを文字で示す（画面からは操作できない）。

### 限界・実装状況

- 未登録の種類の`failed`は`ai-jobs retry`でも保持する（`unsupported`）。登録済みの種類（画像Embedding・location_clue・location_suggest・photo_judge）は、各handlerの`recheck`が現在の入力・処理版・既存結果を検査して、`pending`へ戻す・`obsolete`にする・成功済み扱いにするのいずれかへ振り分ける（通信なし）。location_clueは同世代のfailed投影も無効にする。
- `Disabled`から`RunError::PolicyDisabled`への対応付け、401/403/404/契約不一致から`ProviderPause`／`TaskPause`への対応付け、429のRetry-After、予算エラー（`Budget`）の扱いは、各handlerが`before_send`の結果をそのまま返すことで揃う（ワーカーがジョブの状態へ対応付ける）。
- HTTP通信・Discovery・Decisionの入出力・共有の一時待機はT4.2で実装済み（[ai-provider.md](ai-provider.md)）。空間別のベクトル保存・検索空間の切替・旧選別の安全化はT4.3で実装済み（[ai-index.md](ai-index.md)。切替の成功率の閾値`activation_min_coverage`は、今は`try_activate`の引数と既定0.95で、このJSONには置かない。後続が任意の独立キーとして足す場合は、有限の0〜1だけを許し、不正ならAI設定不正としてAIだけを無効にし、閾値は設定ハッシュ・結果の同一性に入れない）。画像／文Embeddingの固定契約アダプター・画像job・自動切替は[T4.4a](ai-embedding.md)、対話の検索語Embedding（`query_embedding`）の予約・キャッシュ・E5/E6接続は[T4.4b](ai-search.md)、Decisionの質問・clue保存・クラウドは[T4.6](ai-location-clue.md)で実装済み。


Vertexの明示認証・固定宛先・token/予算/失敗分類は[ai-cloud.md](ai-cloud.md)（T4.6a）。handlerの登録（photo_judge・location_suggest）は用途のクラウド許可とクラウド接続が揃うときだけで、実クラウド利用の許可はその用途ごとの設定（`allow_cloud`）が決める。

## T4.6c2 日候補の運用境界

`location_suggest` と `location_geocode` を本番factoryへ登録した。新規Cloud画像送信には有効なlocation_clue設定と、その時点のDecision画像readyも必要。保存済み有償回答の座標化にはCloudClientや認証ファイルが不要で、location_suggestの有効/無効・pause・周期上限を尊重する。実画像や費用の追加許可は導入しない。

`location_suggest` がクラウドへ送る内容は、**画像（最大6枚）と撮影日（YYYY-MM-DD）**。座標・地名・ファイル名・写真IDは送らない（撮影日はプロンプトの`{date}`に入る）。

有効suggest設定だけが `ai_location_suggest_results` / `ai_location_candidates` / `ai_location_suggest_holds` をmigration/dry-run対象にする。c1済みDBへの不足増分は後者2表。INGEST=0や無効/不正設定の配信継続と、既存CLIのGuard/ingest lockは維持する。statusに固定の保留理由と件数を追加し、写真ID・地名・日付・モデル応答・認証情報は出さない。

## T4.4a: 既知Embedding契約の明示

`embedding_contract`（任意）を追加した。対応値は `local-model-lab-eg2-bf16-c9b05b59-v1` のみ。未知値・未知キー・重複はAIだけ無効にする。未指定や宣言不一致では画像handlerを登録せず、query clientも作れない。BF16の固定revision/前処理/共通空間の照合、合成URLだけの設定例、T4.4bの呼出境界は [ai-embedding.md](ai-embedding.md) を参照。既存の一般 `gate` の成功だけでこのアダプターを使わない。空間の宣言とcontractは用途pauseの識別にも含め、修正後の再開を可能にする。新規schema/dry-run増分は0。coverageは既存APIの既定0.95を使い、設定キーは追加しない。
