# ADR: エージェント稼働状況は同一ホストのdropを読み取り専用で表示する

- 日付: 2026-08-29
- 状態: 承認（2026-09-17）

## 文脈

GeoFrameの「エージェント稼働状況」モードは、稼働状態・最終更新からの経過・処理種別・簡易メトリクスだけを非機密の要約として表示する。表示機は状態を書き戻さず、取得経路にもジョブ投入権限やタスク本文を持ち込まない必要がある。

現在実装・検証対象のHermesとGeoFrameは同一ホスト上で動作する。したがって、この経路にHome Assistantや、秘密のHTTPトークンを持つ別ホストpublisherを挟む必要はない。`miach-runtime` は実在を確認できておらず、専用adapterも未実装である。

## 決定

### 1. 同一ホストdrop方式を主経路とする（実装済み）

データ経路は次の一方通行とする。

```text
Hermesの心拍・状態
  → deploy/agents/hermes-status-exporter.py
  → /run/geoframe-agent-status/*.json（temp→fsync→os.replace）
  → app/geoframe/agents.py（読み取り専用）
  → GET /api/agents（Cache-Control: no-store）
  → app/static/js/agents-mode.js
  → #agentslayer
```

exporterは標準ライブラリだけで非機密の要約を作り、`published_at`は計測時刻（心拍ファイルのmtime）とする。発見（unpinned）では存在するregular fileの心拍ごとに1本のdropを書き、既定プロファイルは`hermes.json`、名前付きプロファイル（`<name>`）は`hermes-<name>.json`とする。不正なプロファイル名や存在しない候補にはdropを作らない。固定（`HERMES_PROFILE`・`HERMES_HEARTBEAT_FILE`）は単一カード運用とし、プロファイル名に関係なく`hermes.json`（id=`hermes`）へ、固定先が欠落していても常に1本のdropを書く。古い・壊れた・読めない心拍は`state=unknown`の要約にする。dropはsystem unitと専用groupでGeoFrameから読める権限にし、GeoFrameの`ProtectHome=true`は維持する。

心拍源は次の順で解決する。`HERMES_HEARTBEAT_FILE`（1ファイルへ固定）→`HERMES_PROFILE`（そのプロファイルの心拍。プロファイルが実在するときだけ）→`<HERMES_HOME>/state/gateway.heartbeat`（`HERMES_HOME`未設定時は`~/.hermes`）と`<HERMES_HOME>/profiles/<name>/state/gateway.heartbeat`を列挙して各dropへ反映する。既定で名前付きプロファイルを拾うため、稼働させるプロファイルを変えてもunitのdrop-inで心拍のパスを指定し直す必要はない。既定（発見）で複数の心拍が同時に新鮮なときは、exporterがfreshな集合単位でjournaldへ警告を出し（勝者が入れ替わっても再出力せず、freshな集合が変わったときだけ再出力する）。発見（unpinned）では1周期のpublish後、今回publishしなかった`hermes.json`・`hermes-<name>.json`を削除するため、過去に存在したプロファイルのディレクトリが削除・リネームされた場合もghost dropを残さない。固定したパス（`HERMES_HEARTBEAT_FILE`・`HERMES_PROFILE`）では`hermes.json`（id=`hermes`）だけを書き、`hermes-<name>.json`など他のexporter-owned dropを削除する。複数カードはunpinnedで運用し、`GEOFRAME_AGENT_LABELS`へ`hermes-<name>=ラベル`を登録する。未登録idのdropはカードに出ず、サーバーがjournaldへ1回警告する。`miach.json`など他のidのdropには触れない。固定したパスはregular fileでなくても`state=unknown`のdropを書き、発見した候補はregular fileのときだけ開く。いずれも開く際は非ブロッキング（`O_NONBLOCK`）にする（FIFOを掴んでも`open()`が戻らず無応答になることを避ける）。

サーバー側のカード定義は`GEOFRAME_AGENT_LABELS`（`id=ラベル`のカンマ区切り）から作り、設定にないidのdropは読まない。設定順を候補順としつつ、`live`を優先して`published_at`降順に並べ、最大2枚だけを公開する。ラベルはdrop内の値を使わず、設定値だけを使う。選抜後のカードに`live`がある場合だけ`rotation_allowed`をtrueにする。

`agents.py`はregular fileだけをno-followで読み、64KiB上限とstrict schemaを適用する。破損したdropではlast-goodの値を使うが、`published_at`を若返らせない。ファイル名とJSON内の`id`が一致しないdropは採用しない。

APIの各要素は`{id, label, state, age_s, task_kind, metrics[]}`だけとする。生ログ、パス、ホスト名、PID、ユーザー名、プロンプト、トークン、コスト、個人情報は含めない。フロントエンドは新設の`#agentslayer`へ`textContent`だけで描画し、`#modelayer`は使用しない（3D表示の所有者は[[0004-frontend-display-mode-arbitration]]）。

### 2. stalenessの境界を固定する（実装済み）

`published_at`と現在時刻の差により状態を次のように判定する。

- 経過時間が30秒以下: `live`
- 30秒超かつ120秒以下: `stale`
- 経過時間が120秒超: `unknown`
- `published_at`が現在より120秒超未来: 時計ずれとして`stale`

片系だけ取得できる場合は、その系だけを部分表示する。両系が`stale`または`unknown`で`rotation_allowed`がfalseになった場合、自動巡回の候補から外し、明示ピンは5分の猶予後に解除する。「無い」系を架空のカードで補わない。

### 3. Home AssistantやMac publisherは将来案（未実装）

ホスト境界を越える別ソースを将来追加する場合に限り、Mac上のpublisherがEcho等のlocalhost診断APIを読み、Home Assistantへ非機密要約をpublishする構成を候補とする。GeoFrameはその要約を読み取り専用で正規化する。この構成の疎通、認証、recorder除外、運用権限は未測定であり、現在の決定・実装には含めない。

特に、Mac publisher、Echo adapter、Hermes adapter、Home Assistant REST publish、`ha_get()`によるこの経路の取得は未実装である。`miach-runtime`の実在も未確認のため、承認済みの現行経路として扱わない。

## 理由

同一ホストでは、原子的なlocal dropと専用groupを使う方が、無認証の表示機へ書き込み口や強いHermes認証トークンを追加せずに済む。GeoFrameからの操作面を増やさず、exporterが非機密の閉じたschemaだけを公開できる。また、30秒・120秒の固定境界により、publish間隔に依存する曖昧な「N倍」判定を避け、時計ずれも明示的に扱える。

将来の別ホスト経路については、GeoFrameからHermesのDBや操作可能なゲートウェイを直接読む案、localhost限定の診断APIをLAN公開する案、GeoFrameへ無認証のpush書き込み口を追加する案を採用しない。実測と権限設計が完了するまでは、これらを承認済み構成として扱わない。

## 制約と次段階

- 表示機からサーバー、Home Assistant、exporterへの書き込みは行わない。
- タスクのタイトルや本文などの私的内容はdrop、API、表示、ログへ出さない。
- dropの配置、group、system unitはroot作業として配備し、GeoFrame側の`ProtectHome=true`を緩めない。
- 新しい別ホストsourceを追加するときは、まず実在確認、最小限の疎通、読み取り専用権限、staleness、非機密schemaを別途検証してからADRを更新する。
- キーマップの保存経路は管理画面からsettings表へ限定する（[[0007-admin-keymap-write-path]]）。
