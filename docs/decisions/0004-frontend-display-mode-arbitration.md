# ADR: 表示モードのアービトレーションはフロント新モジュールに一元化する

- 日付: 2026-08-29（決定）/ 2026-08-31（実装反映）
- 状態: 承認済み・実装済み（Codex・Claude Opus・Claude Sonnetの合議 → Fable裁定。modes.js と #modelayer は 3D PHOTO JOURNEY Lite 実装で着地）

## 文脈

今後、地図表示（既存）・夜間時計モード・エージェント稼働状況モード・Hermes稼働状況モード・センサー連動といった「大モード」が増える。「今どのモードを出すべきか」を判定する責務をどのレイヤに置くかが論点だった。

現状の`#mode`表示要素は、`app/static/js/events.js`（5箇所）・`app/static/js/panels.js`（4箇所）・`app/static/js/scheduler.js`（3箇所）の計12箇所（`git grep -n "getElementById('mode')"`で確認済み）から直接`textContent`を書き換えられており、単一の所有者が存在しない創発的な状態になっている。

Codex・Claude Sonnetは「server.py側に新モジュールを作り一元判定、フロントは指示に従って描画するだけ」を推奨。Claude Opusは「フロント側に新モジュールを作り一元化すべき」を推奨し、正面から対立した。Fableが検証の上で裁定した。

## 決定

**フロント側に新モジュール `app/static/js/modes.js` を新設し、表示モードの判定をここに一元化する。** 優先度ラダー

```
SLEEP（画面off中は何も描かない）
 > ALERT（地震・レア機体などの割り込み、既存の eventUntil を吸収）
 > PINNED（HA経由のユーザー指定モード）
 > AMBIENT（夜間時計・将来の照度連動）
 > ROTATION（地図・写真巡回。既存の scheduler.js のキュー）
```

を`modes.js`の`pickMode()`が評価する。**`modes.js`を`#mode`要素の唯一のwriterとし、既存12箇所の直接書き込みを撤去することを受け入れ条件とする**（検証コマンド: `git grep -n "getElementById('mode')" app/static/js/` の一致箇所が`modes.js`以外に無いこと）。

`server.py`側は判定を行わず、環境事実とユーザー意図（`night`、画面モード、将来の照度、エージェントスナップショット）を読み取り専用JSON（`/api/presence`拡張、および[[0005-agent-status-via-ha-bus]]で新設する`/api/agents`）として配信するだけに留める。新モードは既存の右カラムパネルに追加せず、**全画面テイクオーバー層`#modelayer`**として実装し、`#hud-root`が持つ焼き付き対策のdrift（240秒周期）を継承させる。

## 理由

裁定の決め手は、サーバー側アービタ案が現行の実装の上で成立しないという検証済みの事実にある。

1. `server.py`は`SimpleHTTPRequestHandler`系のスタックであり、SSEやWebSocketのようなpush手段が存在しない。フロントは`/api/presence`を15秒間隔でポーリングしている（`app/static/js/main.js`）一方、写真ローテーションは16秒間隔（`DWELL_MS`、`app/static/js/config.js`）で動いており、サーバー側でモードを決めても伝達がこの2つの周期に挟まれてガタつく。
2. 判定に必要な状態のうち、`playQueue`・`roundNo`・`interludeTurn`・`eventUntil`はクライアント側にしか存在しない。サーバー側で判定するには、これらをサーバーへ逆流させる新しい経路が要る。
3. 既存の`showInterlude()`（候補を順に試して描画できたものを採用する）は、クライアント側アービトレーションが既に動いている実績ある前例であり、`modes.js`はこのパターンの一般化にすぎない。
4. Codex・Sonnetが本来求めていた「単一の決定点」「12箇所に散った書き込みの解消」は、`modes.js`を唯一のwriterにすることで満たされる。サーバー側アービタにしてもフロント側に受信・遷移コントローラは結局必要になり、判定層が二重になるだけで解消しない。

サーバー側にSSEを新設してまでサーバーアービタ化する価値は現時点でない。将来、別の理由でSSE導入が決まった場合に限り、本ADRの再検討対象とする。

## 制約と次段階

- 既存の`isNightByClock`等、フロント側のフォールバック判定（HA未到達時に時刻ベースへ切り替える処理）は維持する。サーバーは「事実のpublisher」に徹し、事実が来ないときの自衛はフロント側の責務として残す。
- 全画面テイクオーバー方式により、新モードは右カラム（`#intel`/`#envpanel`/`#hzlegend`）のDOM構造に触れない（[[0006-implementation-sequencing]]の前提）。
- 実証モードとして「夜間時計（NIGHT CLOCK）」を最初に実装する。判定入力は既存の`night`事実をそのまま流用し、新規のデータ経路を追加しない。
- 受け入れ条件（必須・再掲）: `git grep -n "getElementById('mode')" app/static/js/` の一致が`modes.js`以外で0件になること。

## 実装状況（2026-08-31）

3D PHOTO JOURNEY Lite（[[3d-photo-journey-lite]]、`docs/architecture/3d-photo-journey-lite.md`）
の実装に合わせて、本ADRの決定事項を着地させた。

- `app/static/js/modes.js` を新設し、`setMode(label)` を `#mode` の唯一の writer とした。
  既存12箇所の直接書き込み（events.js 5・panels.js 4・scheduler.js 3）は `setMode()` へ
  置き換え、受け入れ条件（`modes.js` 以外で0件）を満たした。表示ラベル
  （TRANSIT / PHOTO JOURNEY / INTERCEPT / MEMORY LINK / SEISMIC WATCH / SLEEP /
  TRIP REPLAY / TRAVEL STATS / WEEKLY FORECAST / ON THIS DAY）は全て従来どおり。
- `pickMode()` は優先度ラダー（SLEEP > ALERT > PINNED > AMBIENT > ROTATION）の
  文書化された参照点として保持。現行の呼び出し順（eventUntil / sleeping ガード）で
  既にラダーが成立しており、新モード追加時に唯一の判定点へ昇格させる。
- `#modelayer`（全画面テイクオーバー層）を `app/index.html` に追加し、`#hud-root` の
  drift（240秒周期・焼き付き対策）を継承させた。右カラム（`#intel`/`#envpanel`/
  `#hzlegend` の `right:46px`）とは独立で、右カラム回帰テストの不変条件を壊さない。
- 実証モードは当初の「NIGHT CLOCK」ではなく「3D PHOTO JOURNEY Lite（`?mode=3d`）」を
  最初の消費者とした。いずれも `modes.js` を唯一の writer とし `#modelayer` を使う点で
  本ADRの決定を変えない。
- 純ロジック分離の副次効果として、`app/static/js/scheduler.js` の再生スケジューラ
  （キュー構築・永続化・差分同期）を `app/static/js/scheduler-core.js` へ切り出し、
  Node/vm で検証可能にした（`app/tests/js/scheduler-quality.test.js`）。
