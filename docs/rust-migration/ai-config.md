# AI設定（`GEOFRAME_AI_CONFIG`）v1

- 状態: T4.1bで実装。設計は[p4-design.md](p4-design.md) §3。接続先の固定・認証・実際の送信はT4.2以降で、ここは**設定の読み取りと検査**まで。
- `GEOFRAME_AI_CONFIG`（Rustだけの項目）は、用途別のrouting・クラウド許可・ベクトル空間をまとめた小さなJSONのパス。**未設定なら新しい外部AI呼び出しは無効**（写真の配信・管理・保存済み推薦の読み取りは動く）。相対パスは起動したカレントディレクトリ基準。

## 読み込みの決まり

- 通常のファイルだけ・**64KiBまで**・UTF-8のJSONオブジェクト。**全ての階層で未知のキーと重複したキーは不正**（秘密をうっかり書き足しても受けない）。
- 起動時にDNS・ネットワーク・認証ファイルの読み取りをしない。URL・パスは文字列として検査するだけ。
- **誤った設定はAIだけを無効にする**: JSONが不正、または旧環境変数の数値が不正でも、`serve`は起動して写真を配信する。ログには種類のコード（下の表）だけを出し、パス・中身・URL・モデル名・`serde`のエラー文は出さない。起動ログの`ai=`は`unset`／`invalid:<コード>`／`ready:tasks=<件数>`。STATUSへの`ai_config_invalid`の表示はT4.1c。
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
| `local_base_url` | LANの用途があるとき | `http`／`https`の`scheme://host[:port]`だけ（末尾の`/`は可）。userinfo・query・fragment・パス・空白・制御文字・非ASCII・`%`は不正。ホストはIPリテラルかDNS名の書式（解決・privateの判定はしない。接続時の固定はT4.2）。APIのパスは実装が固定で付ける |
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
| `GEOFRAME_AI_EMBED_PER_CYCLE` | `12` | `image_embedding`の1周期の上限 |
| `GEOFRAME_AI_JUDGE_PER_CYCLE` | `2` | `photo_judge`の1周期の上限 |
| `GEOFRAME_AI_SEARCH_PER_MIN` | `30` | `query_embedding`の毎分（60秒の窓）の上限 |
| `GEOFRAME_LOC_SUGGEST` | `1` | `1`以外は`location_clue`・`location_suggest`を無効 |
| `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE` | `60` | `location_clue`の1周期の上限 |
| `GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE` | `2` | `location_suggest`の1周期の上限 |

`GEOFRAME_DECISION_URL`・`GEOFRAME_LOC_SUGGEST_MODEL`・`GEOFRAME_LOCAL_EMBED_*`は**読まない**（routingとモデルは上のJSON。Decisionは`local_base_url`に固定のパスを付ける）。廃止するCPUモデル用の設定は後続のタスクで文書化する。

## ハッシュ・停止

設定全体のハッシュ・用途の送信ポリシーのハッシュ（SHA-256）は監査と送信可否の検査用で、**ジョブの冪等キーには入れない**（URL・許可・上限だけの変更で成功済みの結果を再送しないため）。Providerの一時停止（認証の失敗・403・モデル404・契約不一致。`ai_provider_pauses`）は、接続先・認証・モデルなど**その理由に関係する設定だけ**から識別を作り、再起動では解除されず、関係する設定が変わるか管理CLI（T4.1c）の明示の解除で効かなくなる。全ジョブを`failed`にはしない。
