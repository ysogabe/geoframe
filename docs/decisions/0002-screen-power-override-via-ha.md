# ADR: モニターON/OFF手動オーバーライドはHome Assistant経由のpullで実現する

- 日付: 2026-08-29
- 状態: 採用（Codex・Claude Opus・Claude Sonnetの合議 → Fable裁定。2026-08-29 ユーザー承認）

## 文脈

外部（iPhone、外出先を含む）からキオスクモニターのON/OFFを強制したい。検討した経路は3つ:

- (A) Home Assistantの論理エンティティを既存`ha_loop()`にpullで読ませる
- (B) GEOFRAME `server.py`にmutatingな`/api/*`エンドポイントを新設する
- (C) 50-echo-left-device（Echo Show 5 + Macブリッジ）を拡張する

Codex・Claude Opus・Claude Sonnetの3者が独立に見解を出し、(A)で完全に一致した。

## 決定

**(A)を採用する。** Home Assistantに`input_select.geoframe_screen_mode`（`auto` / `force_on` / `force_off`の3値）を作成し、既存の`ha_loop()`ポーリング（人感センサー設定時10秒毎、`app/geoframe/ha.py`）で読み取る。iPhone側の操作導線はHA公式アプリのダッシュボード／ロック画面ウィジェット、および同じヘルパーを設定するSiri Shortcutとする。GEOFRAME・50-echo-left-deviceどちらの側にも新規の受信面・新規の認証設計を追加しない。

## 理由

- `ha_loop()`は既にBearerトークン付きで定期的にHAをポーリングしており、エンティティを1つ追加するだけで実現できる。遅延は最大10秒程度で、画面電源操作の用途には十分。
- **(B)を却下する理由**: `GEOFRAME_HOST`の既定値は`0.0.0.0`（`app/geoframe/config.py`）であり、「LAN限定」は運用上の前提であってソケットレベルで強制されていない。この状態の無認証サーバーにmutatingなルートを新設することは、`SECURITY.md`の設計方針およびIssue #2（deployスクリプトのsecurity-reviewフォローアップ）の方向性に反する。
- **(C)を却下する理由**: 50-echo-left-deviceの診断HTTP APIは`SECURITY.md`で「127.0.0.1限定・無認証・非暗号化を前提とする」と明記されている。かつoutbound HTTPクライアントが現状のコードに存在しない。これをクロスホスト制御のゲートウェイにするのは3案中最も実装コストが高く、かつ「家の外からの操作」という要件をLAN内完結の(C)では満たせない。

## 制約と次段階

- HAへの疎通が一定時間（目安5分）失敗し続けた場合、モードは`auto`へ自動的に減衰させる（[[0003-screen-control-state-machine]]参照）。現行ロジックはHA疎通断時に画面ON側へ倒れるため、減衰の結果は「点きっぱなし」であり、無人キオスクとして安全側に倒れる。
- 次段階: 将来の照度センサー導入時も、「センサー → HA → 既存のpull経路」という形を崩さずに同じ配線へ合流させる。
- HA側の設定作業（`input_select`ヘルパー作成、iPhoneウィジェット/Siri Shortcut設定）はGEOFRAMEリポジトリの外側の作業であり、実装プラン文書（`docs/roadmap/display-mode-expansion-plan.md`）に手順を明記する。
