# 国・地域別の場所表示 タスク

- PRD: [prd.md](prd.md) / 設計: [design.md](design.md)
- ブランチ: 段①は `t3/f73af70e`（geoframe の PR #80 でマージ済み、2026-11-04）。段②は `t3/international-places-api`（geoframe の PR #81 でマージ済み、2026-11-04）。段③は `t3/international-places-ui`。
- PR は段ごとに 1 本（設計「実装の順番」）。

## 段①（PR 1）: 境界データと分類段

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | 境界データの変換ツール（`tools/regions/`）・同梱データ・manifest・出典・名称表（日本 47 都道府県の対応、国の日本語名を miharashi の `NAME_OVERRIDES` と揃える） | AC10 | - | full | 完了 | 7d0b42e | cbc3ffb..ffa7b57 | レビュー Sol 3 回（必須 4 件を修正）。RSS は読込だけで +83MB → T2 で削減。同梱 13.7MB、バイナリ +13.8MB |
| T2 | `regions` 分類器: 埋め込みデータの検証・遅延索引・判定手順 1〜4・合成ポリゴン試験・公開地点スモーク・サイズ/RSS/索引時間の計測 | AC6, AC10 | T1 | full | 完了 | 494f765 | 65870c7 | レビュー Sol 1 回、必須なし。推奨 2 件（外積の丸めで向き依存・流し込み復号の容量加算）は T3 の最初のコミットで直す。RSS +51MB・索引 130ms（musl+mimalloc） |
| T3 | `photo_regions` 表（`available`/`pending`/`ensure_tables`）と `regions` 同期段: 候補選択・CAS 適用・残り行の掃除・件数・dry-run・`STAGE_NAMES`・ログ・索引の解放 | AC7, AC8, AC9, AC12 | T2 | full | 完了 | 493529d | b26b66e..a3f8203 | レビュー Sol 2 回、必須なし。推奨 3 件を修正（dry-run 掃除の上限・failed の記録・候補走査の時間上限）。残る推奨は T8 で対応 |
| T8 | 仕上げ: 候補走査の打ち切りを件数 `scan_incomplete` に出す（全体レビューの推奨）、PRD・設計の状態更新 | AC8 | T3 | quick | 完了 | bd13f8c | 67ac4f7..1f439d4 | 全体レビュー: PRD 突き合わせ Sol（必須なし）、security-review Opus（指摘なし）。修正の差分はオーケストレータが確認 |

## 段②（PR 2）: 管理API

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T4 | 場所の区分の規則（共通 SQL 片）と `places` の拡張（`countries`・`regions`・`boundary_only`・`unknown`・旧DB分岐） | AC1, AC2, AC3, AC5, AC9 | T3 | full | 完了 | b99a7d5 | ce73db4..3c75c0b | レビュー Sol 2 回、必須なし。推奨 4 件を修正（cities の重複をやめ日本の地域は pref で旧 prefs を引く、性能、表紙順位の試験、経度 180 度超の試験）。性能目標を「1.5 倍以内または増加 5ms 以内」に改めた（欧州 bbox 1.43→2.92ms） |
| T5 | `photos` の旧形式/新形式の絞り込み・`q` 検索（日本の固定別名表）・trips・`location_region` | AC1, AC2, AC4, AC7 | T4 | full | 完了 | 9a23754 | 7c793cb | レビュー Sol 1 回、指摘なし。trips 1.45 倍、q +2.6ms。photos.js の location_region 正規化と座標変更時の消去は T7 |
| T9 | 仕上げ: 不正な座標から不正な `fit_bbox`/`bbox` を返さない（全体レビューの推奨） | AC11 | T5 | quick | 完了 | eb704c3 | 5739ef7 | 全体レビュー: PRD 突き合わせ Sol（必須なし）、security-review Opus（指摘なし）。修正の差分はオーケストレータが確認 |

## 段③（PR 3）: 画面

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T6 | 国・地域カード、地域不明・地域未判定・場所不明、日本の市区町村と「市区町村未取得」、地図 fit（日付変更線で省略） | AC1, AC3, AC11 | T5 | full | 未着手 | | | |
| T7 | 場所コンボボックス（国→地域→市区町村）、写真詳細の国・地域と注記、位置変更通知への追随 | AC4, AC7, AC11 | T6 | full | 未着手 | | | |
