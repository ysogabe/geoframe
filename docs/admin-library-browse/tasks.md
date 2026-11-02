# ライブラリの見直し タスク

- PRD: [prd.md](prd.md) / 設計: [design.md](design.md)
- ブランチ: t3/admin-cards-horizontal-photo-preview
- PR は 1 本。タスクはコミット。

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | サーバー: calendar・places の cover、places の bbox（応答）と from/to・bbox（絞り込み）、photos の q。結合試験 | AC2, AC6, AC7, AC8, AC9（サーバー側） | - | full | 完了 | 589b82c | ad76431, 777d0c2 | Sol: 必須なし。推奨 2 件採用（設計書の代表の選び方 e04efe2、優先順位の試験 777d0c2） |
| T2 | 日付の規則のパーサー（library_when.rs、暦の関数）と単体試験 | AC10 | - | full | 完了 | f90b535 | 667a322, 90a3fc4 | Sol: 必須 1（期間の年の上限）・推奨 2（右側の年なし部品・追加解釈の試験）を 90a3fc4 で修正 |
| T3 | LLM: Endpoint::VlChat、ai/when.rs（上限・待ち・停止・検証）、server.rs・cli.rs の配線、GET /api/admin/library/when（Sec-Fetch-Site）。試験 | AC11 | T2 | full | 実装中 | da01a07 | | |
| T4 | 画面の土台: admin.html・admin.css の部品と見た目、fakedom.mjs・fake_maplibre.mjs（fitBounds・eventData・getBounds）、static_files.rs の禁止語 | AC1, AC16 | T1 | full | 未着手 | | | |
| T5 | library.js: 状態・選択・下の詳細（旅・日付）、古い応答の捨て方、除外。試験の書き直しと回帰 | AC3, AC4, AC5, AC15, AC16a | T4 | full | 未着手 | | | |
| T6 | library.js: 場所と地図の統合（カード・チップ・fitBounds・地図の移動の理由・places?bbox・期間と解除） | AC6, AC7, AC8（画面側） | T5 | full | 未着手 | | | |
| T7 | library.js: 旅の検索・日付の言葉（when）とピッカー（Shift の範囲）・場所のコンボボックス | AC9, AC12, AC13, AC14（画面側） | T3, T6 | full | 未着手 | | | |

AC17（fmt・clippy・test）は各タスクで通し、最後に全体で確かめる。
