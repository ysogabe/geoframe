# ライブラリの見直し タスク

- PRD: [prd.md](prd.md) / 設計: [design.md](design.md)
- ブランチ: t3/admin-cards-horizontal-photo-preview
- PR は 1 本。タスクはコミット。

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | サーバー: calendar・places の cover、places の bbox（応答）と from/to・bbox（絞り込み）、photos の q。結合試験 | AC2, AC6, AC7, AC8, AC9（サーバー側） | - | full | 完了 | 589b82c | ad76431, 777d0c2 | Sol: 必須なし。推奨 2 件採用（設計書の代表の選び方 e04efe2、優先順位の試験 777d0c2） |
| T2 | 日付の規則のパーサー（library_when.rs、暦の関数）と単体試験 | AC10 | - | full | 完了 | f90b535 | 667a322, 90a3fc4 | Sol: 必須 1（期間の年の上限）・推奨 2（右側の年なし部品・追加解釈の試験）を 90a3fc4 で修正 |
| T3 | LLM: Endpoint::VlChat、ai/when.rs（上限・待ち・停止・検証）、server.rs・cli.rs の配線、GET /api/admin/library/when（Sec-Fetch-Site）。試験 | AC11 | T2 | full | 完了 | da01a07 | 572ebc7, 9a8e048 | Sol: 必須 1（20 年の上限を暦で）・推奨 1（並行予約の試験）を 9a8e048 で修正。実機の llm-vl で指示文の応答を確認（JSON のみ、0.3 秒） |
| T4 | 画面の土台と旅・日付: admin.html・admin.css の部品と見た目、fakedom.mjs・fake_maplibre.mjs、static_files.rs の禁止語、library.js の状態・選択・下の詳細（旅・日付）・古い応答の捨て方・除外。試験の書き直しと回帰。場所・地図はこの段では今の動きのまま | AC1, AC3, AC4, AC5, AC15, AC16, AC16a | T1 | full | 完了 | a9bf10c | 5e8d8ee, 04fa5f3 | Sol: 必須 1（地図で見るで前の範囲の選択を持ち越す）・推奨 1（ページ送りと選択の交差の試験）を 04fa5f3 で修正。選択中のカードを再度押すと選択を外す（戻るの代わり）。HTML を変えると今の library.js の試験が壊れるため、旧 T4（土台）と旧 T5（旅・日付）を 1 段にまとめた |
| T5 | （T4 に統合） | - | - | - | 完了 | | | 旧 T5 は T4 に統合 |
| T6 | library.js: 場所と地図の統合（カード・チップ・fitBounds・地図の移動の理由・places?bbox・期間と解除） | AC6, AC7, AC8（画面側） | T5 | full | 完了 | c272636 | 4027d02, 1f7197f | Sol: 必須 2（回転・傾斜、resize の補正を利用者の移動と取る）を 1f7197f で修正: 利用者の移動は originalEvent / libUser で判定、回転・傾斜・Shift の範囲ズームを無効化。再確認は最後の全体レビューで |
| T7 | library.js: 旅の検索・日付の言葉（when）とピッカー（Shift の範囲）・場所のコンボボックス | AC9, AC12, AC13, AC14（画面側） | T3, T6 | full | 完了 | d158c80 | 20e8509, c60634b, 3ffe1b1 | 1 回目の子は API の上限で止まり、残した変更から再開。Sol: 必須 1（候補の再選択が保留中の選択を捨てない）・推奨 3（除外の切り替えで「読んでいます…」が残る・遅延の試験が遅らせていない・常に通る assert）を 3ffe1b1 で修正。読みの一致の例外と再選択の動きを設計書に追記（c60634b） |
| T8 | 全体レビューの指摘: 遅れたクラスタの展開ズームの応答が新しい選択を上書きしない（R14）、年をまたぐ月から月の範囲を操作で作れるように（R10） | AC6, AC7, AC13, AC14, AC16a | T7 | full | 完了 | 300f82a | 1c33051, 47b5686 | Sol の PRD 突き合わせ: 必須 1・推奨 1（両方採用）。安全面（security-review）は指摘なし |

AC17（fmt・clippy・test）は各タスクで通し、最後に全体で確かめる。
