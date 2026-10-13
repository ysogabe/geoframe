# ADR: 管理画面のキーマップだけをsettingsへ保存する

- 日付: 2026-09-17
- 状態: 廃止（2026-10-13。[ADR 0008](0008-photo-platform-and-miharashi-display.md)で表示をmiharashiへ移し、この決定の対象コードを削除した。元の状態: 採用）

## 決定

キー `0`〜`9` の閉じた割当（`sleep` / `rotation` / `reserved`）は、管理画面の
`POST /api/admin/setkeymap` だけで `settings` 表に保存する。表示機は
`GET /api/keymap` を読むだけで、自由なモード名や `agents` はこのWaveに追加しない。
保存は `base_revision` を用いるCASとし、競合は409、入力違反は400、同一内容は書込みなし
の200とする。変更時だけIP・Origin有無・rev遷移・変更キーをjournalへ記録する。

## 信頼境界と限定

サーバーは既定で `0.0.0.0`・無認証であり、IPリテラルHostは常時許可される。
`_csrf_ok` はOriginがある場合だけ同一Hostを検査し、Origin無しのcurlは通る。
このためレート制限は設けない（攻撃者の連打で正規管理者を429にする自爆DoSを避ける）。
被害範囲を限定するのは、閉じた語彙、`rotation`必須、Esc/同キーで復帰、表示側が
`textContent`のみを使うことの4点である。`base_revision`はlost-update防止であって防御ではない。
残余リスクはOrigin無しLAN curl、全キーsleepの悪戯、rev/updated_atの露出。管理画面の
「既定に戻す」で復旧する。

## 運用

設定行を削除すれば既定値に戻る（rollback）。前例は無い。根拠は、`admin_sethome` の初回公開が
`60fab6c`（2026-08-13）で、ADR 0002 は 2026-08-30 だからであり、前例を設計根拠にしない。
W3はAPIを信頼せず、キー集合・閉じた語彙・個数制約・revを再検証し、違反時は既定値へfail closedする。
この決定は [[0002-screen-power-override-via-ha]] のHA経由原則と、[[0005-agent-status-via-ha-bus]] の
エージェント状態データのHA唯一バスに影響せず、キーマップ管理画面のsettings CASにだけ適用する。
roadmapの「catalog.dbへ写真索引以外を足さない」には、閉じた語彙の小さな設定値だけを
settings表へ置く例外を明記する。
