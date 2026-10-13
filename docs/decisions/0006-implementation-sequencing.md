# ADR: 実装順序はレイアウト債務を非ブロッカー化し、Wave単位で進める

- 日付: 2026-08-29
- 状態: 廃止（2026-10-13。[ADR 0008](0008-photo-platform-and-miharashi-display.md)で表示をmiharashiへ移し、この決定の対象コードを削除した。元の状態: 提案（Codex・Claude Opus・Claude Sonnetの合議 → Fable裁定。ユーザー承認待ち））

## 文脈

キオスク画面の右カラムには、Issue #6（AREA INTEL以外のスケーリング未対応）・#10（地震時`#hzlegend`と`#envpanel`の重なり）・#11（`right-column-layout.test.js`が新規`@media`追加で機械的に壊れる構造的な脆さ）という未解決のレイアウト債務がある。新しい表示モードをこれらの解決前に追加してよいかが論点だった。

## 決定

**[[0004-frontend-display-mode-arbitration]]で新モードを右カラムのパネルではなく全画面テイクオーバー（`#modelayer`）として実装すると決めたことにより、#6と#10は新モード追加のブロッカーではなくなる。** `right-column-layout.test.js`は`#intel`/`#envpanel`/`#hzlegend`のCSS静的解析による不変条件検査であり、`#modelayer`は右カラムのDOM/CSSに一切触れないため、両者は独立に進行できる。

ただし**#11のみ限定的に先行して堅牢化する**（Wave 0b）。理由は、#11が「`app.css`への`@media`追加」というあらゆるCSS変更全般で機械的に壊れうる問題であり、放置するとWave 1以降のあらゆる変更でQAゲートが不必要に詰まるため。#6・#10は独立した後続Waveに送る。

全体は以下のWave構成で進める（詳細は`docs/roadmap/display-mode-expansion-plan.md`）:

| Wave | 内容 | 依存 |
|---|---|---|
| 0a | 画面手動オーバーライド（[[0002-screen-power-override-via-ha]] / [[0003-screen-control-state-machine]]） | なし |
| 0b | Issue #11のテスト堅牢化（限定範囲） | なし（0aとworktree並行可） |
| 1 | 表示モード基盤（[[0004-frontend-display-mode-arbitration]]） | 0b完了推奨 |
| 2 | AGENTSモード（[[0005-agent-status-via-ha-bus]]のEchoアダプタ側） | Wave 1 |
| 3 | HERMESモード（[[0005-agent-status-via-ha-bus]]のHermesアダプタ側） | Wave 2 |
| 4 | レイアウト債務（#6・#10）とセンサー連動の拡張 | Wave 1以降いつでも着手可 |

## 理由

- 新モードを右カラムパネルとして追加する前提であれば、既存のレイアウト債務を先に解消しておく必要性は高かった。しかし全画面テイクオーバー方式に設計側で切り替えたことで、この前提そのものが無くなった。
- #11だけを先行させるのは、それが「新規のコード」ではなく「既存の壊れやすいテスト」であり、以後のあらゆるWaveの足を引っ張り得るため。#6（工数が読みにくい全面スケーリング作業）や#10（新モードと無関係な既存バグ）を先行条件にすると、ユーザーが最も早く欲しい機能（画面ON/OFFの手動制御）が数週間単位で遅れる。

## 制約と次段階

- 万一、後続Waveで右カラムのDOM/CSSに触れる必要が生じた場合は、その時点で#6・#10を当該Waveのスコープに繰り入れる。なし崩しの並行実装はしない。
- 粒度の原則: 1 Wave = 1つの新しい抽象、または1つの新しいホスト間経路。複数の抽象・経路を1つのWaveに混ぜない。
- 各Waveは独立した作業ブランチ（`<type>/<issue番号>-<説明>`）で進め、mainへの直接コミットは行わない。並行実装が必要な場合は`git worktree`を用いる。
