# GEOFRAME 表示モード拡張 実装プラン

作成日: 2026-08-29
前提文書: [[0002-screen-power-override-via-ha]] [[0003-screen-control-state-machine]] [[0004-frontend-display-mode-arbitration]] [[0005-agent-status-via-ha-bus]] [[0006-implementation-sequencing]]（`docs/decisions/0002〜0006`）

このプランは、Codex・Claude Opus・Claude Sonnetの合議とFableの最終裁定を経て確定した方針を、**複数回のプランモードで1 Wave = 1回のプラン作成→実装として消化するための土台文書**である。将来のセッションはこの文書の該当Wave節と対応するADRだけを読めばプランを立てられることを目標にしている。

## 0. 全Wave共通の前提（プランモードに入る前に必ず確認すること）

- **作業ブランチ**: `main`/`master`/`develop`等の保護ブランチ上で直接作業しない。ブランチ名は`<type>/<issue番号>-<短い説明>`（例: `feat/12-screen-override`）。Issueが未起票のWaveは、着手前にGitHub Issueを起票してから番号を確定する（後述、Waveごとに起票の要否を明記）。
- **worktree**: 複数Waveを並行実装する場合は`git worktree`を使う。worktreeには`node_modules`・`.env`・ローカルDBが無い場合があるため、検証コマンドを回す前に依存インストールと環境ファイルのコピーを済ませる。
- **実装フェーズの運用契約**: プラン承認直後、最初のサブエージェント dispatch前に`orchestrator-qa` skillを起動して契約を読み込む。
- **`catalog.db`は再構築不可**。本プランのどのWaveも、写真索引以外の状態を`catalog.db`に追加しない（永続が必要な状態はHome Assistant側に置く、[[0003-screen-control-state-machine]]参照）。
- **命名の注意**: フロントのJSは`app/static/js/`配下（`static/js/`ではない）。テストは`app/tests/`（Python）と`app/tests/js/`（Node、package.json無し、`node <file>`で直接実行）。
- **PRごとの検証コマンド（現状確認済み、実在する）**:
  - Python: `cd app && .venv/bin/python -m unittest discover -s tests`
  - JS回帰テスト: `cd app && for t in tests/js/*.test.js; do node "$t" || exit 1; done`（`app/tests/js/`の全テスト）
  - N100実機での見た目確認は、各Waveの受け入れ条件に個別に記載する（自動テストでは検出できないレイアウト崩れ・焼き付き・DPMS実機挙動を含むため）。

## Wave 0a: 画面手動オーバーライド

**対応ADR**: [[0002-screen-power-override-via-ha]] [[0003-screen-control-state-machine]]
**Issue起票**: 要（このWave着手前に起票し、ブランチ名の`<issue番号>`を確定する）
**ブランチ例**: `feat/<issue番号>-screen-override`
**依存Wave**: なし。単独で着手可能。

> [!NOTE]
> **実装状況: 完了**（PR #13 マージ済み、Issue #12 クローズ済み）。以下の記述は着手前の
> プランであり、現行仕様の説明ではありません。`app/geoframe/screen_control.py` の新設と
> `GEOFRAME_HA_SCREEN_MODE_ENTITY`・`GEOFRAME_HA_SCREEN_RECONCILE_SEC` は実装済みで、
> 判断の根拠は [[0002-screen-power-override-via-ha]] と
> [[0003-screen-control-state-machine]]、現行の設定手順は
> [Home Assistant連携](../integrations/home-assistant.md) にあります。

### 目的
Home Assistant経由でGEOFRAMEキオスクの画面ON/OFFを外部（iPhone含む）から手動制御できるようにする。人感センサーによる自動スリープと矛盾なく共存させる。

### 触るファイル（実パス）
- `app/geoframe/ha.py` — 既存のスリープ判定ロジックを`screen_control.py`へ切り出す
- `app/geoframe/screen_control.py`（新規）— `decide(mode, ha_ok, idle_s, ...) -> desired_screen`の純関数、および`screen-ctl.sh status`との照合ロジック
- `app/geoframe/config.py` — 新規環境変数の追加
- `app/geoframe/handlers.py` — `/api/presence`のレスポンスに`mode`・`screen_confirmed`を追加（読み取り専用のまま、mutatingエンドポイントは追加しない）
- `deploy/kiosk/screen-ctl.sh` — 変更不要見込み（既存の`status`サブコマンドをそのまま利用）

### 追加する環境変数
- `GEOFRAME_HA_SCREEN_MODE_ENTITY`（例: `input_select.geoframe_screen_mode`）— 未設定時は本機能全体を無効化し、従来のauto専用ロジックにフォールバックする。
- `GEOFRAME_HA_SCREEN_RECONCILE_SEC`（実装済みの既定値は90秒）— `screen-ctl.sh status`との照合間隔。

### 受け入れ条件
1. Python単体テスト: `decide()`に対し、`(mode=auto, idle超過)→off`、`(mode=force_off, 人感反応)→off`（人感で解除されないこと）、`(mode=force_on, idle超過)→on`、`(ha_ok=False継続, mode=force_off)→一定時間後にautoへ減衰`の各ケースをカバーする。
2. N100実機確認: HA側で`input_select`を`force_off`に切り替え、10秒程度でモニターが消灯すること。人感センサーの前で手を振っても再点灯しないこと。`auto`へ戻すと、その場の人感状態に応じた表示に復帰すること。
3. `screen-ctl.sh status`とGEOFRAME内部状態が一致しない状態（例: ssh経由で`xset dpms force on`を直接叩く）を作り、次の照合サイクルで内部状態が実機に追従すること。
4. `cd app && .venv/bin/python -m unittest discover -s tests` が通ること。

### rollback手順
- `GEOFRAME_HA_SCREEN_MODE_ENTITY`を未設定に戻せば、この機能は無効化され既存のauto専用ロジックのみが動く（後方互換）。
- 緊急時はブランチをrevertしてデプロイし直す（`deploy/deploy-pull.sh`経由）。

### HA側セットアップ手順（GEOFRAMEリポジトリ外の作業、本Waveの成果物として必須）
1. Home Assistantで `input_select.geoframe_screen_mode` ヘルパーを作成し、選択肢を `auto` / `force_on` / `force_off` の3つにする。
2. iPhone側の導線を用意する（いずれか、または複数）:
   - HA公式アプリのダッシュボードに当該`input_select`のカードを配置
   - ロック画面ウィジェットとして追加
   - Siri Shortcutを作成し、「画面消して」等の発話で`input_select.select_option`を呼ぶ
3. （任意）HA automationで、`force_off`を24時間以上放置した場合や日の出時に`auto`へ自動的に戻す復帰ポリシーを設定する。このポリシーはGEOFRAME側のコードには一切実装しない。
4. `GEOFRAME_HA_TOKEN`が有効であることを確認済みの前提で、`.env`に`GEOFRAME_HA_SCREEN_MODE_ENTITY`を設定する。

---

## Wave 0b: Issue #11 レイアウトテストの限定堅牢化

**対応ADR**: [[0006-implementation-sequencing]]
**Issue起票**: 不要（既存Issue #11を継続利用）
**ブランチ例**: `fix/11-layout-test-hardening`
**依存Wave**: なし。Wave 0aとworktree並行可。

### 目的
`app/tests/js/right-column-layout.test.js`が、右カラムと無関係な新規`@media`追加だけで機械的に壊れる問題を、範囲を絞って堅牢化する。**#6（全面スケーリング未対応）や#10（重なりバグ）そのものの解消は本Waveのスコープ外**（Wave 4で扱う）。

### 触るファイル
- `app/tests/js/right-column-layout.test.js`

### 受け入れ条件
1. 右カラムと無関係なテスト用の`@media`ブロックを`app.css`に追加した状態で、`node app/tests/js/right-column-layout.test.js`が引き続き成功すること（回帰確認用に一時的にダミー`@media`を追加してテストし、確認後revertする）。
2. 既存の`node app/tests/js/right-column-layout.test.js`の成功が維持されること。

### rollback手順
- テストファイルのみの変更のため、revertのリスクは低い。失敗時は当該コミットをrevertする。

---

## Wave 1: 表示モード基盤

**対応ADR**: [[0004-frontend-display-mode-arbitration]]
**Issue起票**: 要
**ブランチ例**: `feat/<issue番号>-display-modes`
**依存Wave**: Wave 0b完了推奨（QAゲートを安定させた上で新規CSSを追加するため）

### 目的
`app/static/js/modes.js`を新設し、表示モードの判定を一元化する。実証モードとして夜間時計（NIGHT CLOCK）を実装する。

### 触るファイル
- `app/static/js/modes.js`（新規）— 優先度ラダー（SLEEP > ALERT > PINNED > AMBIENT > ROTATION）と`pickMode()`
- `app/static/js/events.js` / `app/static/js/panels.js` / `app/static/js/scheduler.js` — `document.getElementById('mode').textContent = ...`の直接書き込み12箇所を撤去し、`modes.js`経由に置き換える
- `app/index.html` — `#modelayer`要素の追加
- `app/static/app.css` — `#modelayer`のスタイル、`#hud-root`のdrift継承またはジッタ付与（焼き付き対策）
- `app/geoframe/handlers.py` — `/api/presence`のレスポンスに必要な事実（`night`等、既存分に加え新規は無い見込み）が揃っているか確認

### 受け入れ条件
1. **必須**: `git grep -n "getElementById('mode')" app/static/js/` の一致箇所が`modes.js`以外で0件であること。
2. NIGHT CLOCKモードが、既存の`night`判定（`sun.sun`ベース、HA未到達時は時刻フォールバック）に連動して全画面テイクオーバーで表示されること。
3. 地震アラート等の既存ALERT系イベントが、NIGHT CLOCK表示中でも優先度どおり割り込み表示されること。
4. N100実機で、NIGHT CLOCKを深夜〜早朝に数時間表示させ、翌日昼間の画面に焼き付き痕が出ないこと（`#hud-root`のdriftが継承されているかを実機で目視確認）。
5. `node app/tests/js/right-column-layout.test.js`と`node app/tests/js/scheduler-quality.test.js`が引き続き成功すること（右カラムに触れていないため回帰しない想定）。

### rollback手順
- `modes.js`の新設とeventUntil等の吸収はフィーチャーブランチ内で完結するため、問題があればブランチ全体をrevertしてデプロイし直す。

---

## Wave 2: AGENTSモード（50-echo-left-device連携）

**対応ADR**: [[0005-agent-status-via-ha-bus]]
**Issue起票**: 要（GEOFRAME側）。加えて50-echo-left-device側にもMac publisher追加の作業が発生するため、そちらのリポジトリでも別途Issue管理する。
**ブランチ例**: GEOFRAME側 `feat/<issue番号>-agents-mode`、50-echo-left-device側は別ブランチ
**依存Wave**: Wave 1

### 目的
50-echo-left-deviceの6枚タスクカードの状態を、Home Assistant経由でGEOFRAMEに取り込み、AGENTSモードとして表示する。

### 触るファイル
- **50-echo-left-device側（新規）**: Mac常駐publisher（言語・配置は実装時に決定。既存の`bridge/`や`mac/`とは独立したプロセスとして追加する想定）。診断API（`GET /api/state`）をポーリングし、Home AssistantのREST APIへpublishする。診断API自体・そのbind先（`127.0.0.1`）は変更しない。
- **GEOFRAME側**:
  - `app/geoframe/agents.py`（新規）— HAから`ha_get()`で取得し正規化、staleness判定
  - `app/geoframe/handlers.py` — `GET /api/agents`（読み取り専用）の追加
  - `app/geoframe/config.py` — 関連する環境変数
  - `app/static/js/modes.js` — AGENTSモードをROTATION or AMBIENTの候補に追加
  - `app/static/js/agents.js`（新規、または`panels.js`に追加）— カード描画

### 追加する環境変数
- GEOFRAME側: `GEOFRAME_HA_AGENTS_ENTITY`（Echo状態を持つHAエンティティID）
- 50-echo-left-device側: publisherが読むHAのURL・トークン（既存のHA運用と同じ変数命名規則に揃える）

### 受け入れ条件
1. Macのpublisherが起動している状態で、50-echo-left-deviceのタスクカードの状態変化（例: providerのstatus変更）が、目安30〜60秒以内にGEOFRAMEのAGENTSモード表示に反映されること。
2. Macのpublisherを停止した状態で、一定時間（publish間隔の3倍程度）経過後、AGENTSモードの表示がSTALEバッジ＋減光になること。
3. タスクのタイトル等プライベートな内容がHome AssistantのRecorderに保持され続けないこと（`recorder: exclude`設定がHA側でされていること、GEOFRAME側のコード自体はプライベート情報を要求しない設計になっていること）。
4. `cd app && .venv/bin/python -m unittest discover -s tests` が通ること。

### rollback手順
- `GEOFRAME_HA_AGENTS_ENTITY`を未設定にすればAGENTSモードは`modes.js`の候補から自動的に外れる（`canRender()`がfalseを返す設計とする）。

---

## Wave 3: HERMESモード

**対応ADR**: [[0005-agent-status-via-ha-bus]]
**Issue起票**: 要（GEOFRAME側・50-echo-left-device側のpublisher拡張）
**ブランチ例**: `feat/<issue番号>-hermes-status`
**依存Wave**: Wave 2

### 目的
Hermes Coding Runtimeの稼働状況（ジョブ数・ワーカー統計等）を、Wave 2で作ったMac publisherにHermesアダプタを追加する形で取り込み、HERMESモードとして表示する。

### 着手前に必須のスパイク
Mac上の非対話的なHTTPクライアント（Pythonの標準ライブラリ等）から、Hermesゲートウェイへの認証付きリクエストが実際に成功するかを最初に確認する。**このスパイクが失敗した場合、本Waveの設計をやり直す**（フォールバックはMac上のファイルドロップ方式。Hermesゲートウェイ自体の仕様変更やトークンの移設は行わない）。

### 触るファイル
- 50-echo-left-device側: publisherにHermesアダプタを追加（Wave 2のEchoアダプタと同じpublisher内)
- GEOFRAME側: `app/geoframe/agents.py`にHermes分の正規化を追加、`app/static/js/modes.js`にHERMESモードを追加

### 受け入れ条件
1. スパイクが成功し、Hermesのジョブ数・ワーカー稼働状況がJSON化できること。
2. GEOFRAMEのHERMESモードに、Hermesの要約統計がSTALE表示込みで正しく描画されること。
3. AGENTSモードとHERMESモードが独立して部分表示できること（片方だけSTALEでももう片方は正常表示）。

### rollback手順
- Wave 2と同様、対応するHAエンティティを未設定にすればHERMESモードが候補から外れる。

---

## Wave 4: レイアウト債務とセンサー拡張

**対応ADR**: [[0006-implementation-sequencing]]
**Issue起票**: 既存Issue #6・#10を継続利用
**ブランチ例**: `fix/6-kiosk-scaling`、`fix/10-hzlegend-overlap`（別々のブランチに分割）
**依存Wave**: Wave 1以降いつでも着手可（優先度最下位）

### 目的
- Issue #6: AREA INTEL以外のキオスク要素（photocard/ticker/legend/airpanel/statspanel等）の全面スケーリング対応
- Issue #10: 地震時の`#hzlegend`と`#envpanel`の重なり解消
- 照度センサー導入時、[[0004-frontend-display-mode-arbitration]]の`modes.js`にセンサー入力を1つ追加する形で連動させる（センサー本体はHome Assistant登録済みであることが前提、[[0002-screen-power-override-via-ha]]と同じ配線パターン）

このWaveは他Waveの完了を待たず、必要になった時点で独立に着手してよい。

---

## 全体像（依存関係図）

```
Wave 0a (screen override) ─┐
Wave 0b (#11 hardening)   ─┼─→ Wave 1 (display-mode arbiter) ─→ Wave 2 (AGENTS) ─→ Wave 3 (HERMES)
                            │
Wave 4 (#6/#10/センサー)  ──┘ (いつでも独立着手可)
```
