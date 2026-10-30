# コントリビューションガイド

GEOFRAMEへの改善提案を歓迎します。変更を送る前に、次の点を確認してください。

## 開発環境

Rustの安定版ツールチェーンを使います（Pythonは不要です）。

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

環境変数を追加・変更した場合は、[設定リファレンス](docs/reference/configuration.md)を手で更新してください。
コードが読む`GEOFRAME_*`の名前が載っていないと、`cargo test`（`crates/geoframe/tests/env_reference.rs`）が失敗します。

文書を追加・移動した場合は、リポジトリルートで
`python3 tools/check_markdown_links.py`（標準ライブラリだけのシステムの`python3`で動きます）を実行し、[ドキュメント索引](docs/README.md)も
同じ変更で更新してください。

## 変更時の注意

- 1つの変更では、できるだけ1つの目的だけを扱ってください。
- 挙動を変更する場合はテストを追加または更新してください。
- 文書の本文と見出しは日本語を基本とし、API名や製品名は原表記を維持してください。
- `.env`、認証鍵、写真、データベース、実ホスト名、個人パス、Home Assistantの実エンティティ、
  位置情報をコミットしないでください。
- 外部APIを追加する場合は、認証情報をブラウザへ渡さず、利用規約・レート制限・失敗時の縮退を
  文書化してください。

## プルリクエスト

Pull Requestには、変更理由、主な変更点、確認したテスト、画面変更がある場合は画像を記載します。
大きな設計変更では、先にIssueで目的と範囲を共有してください。

脆弱性に関する報告は公開Issueへ書かず、[`SECURITY.md`](SECURITY.md)に従ってください。
