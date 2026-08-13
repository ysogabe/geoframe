# nlohmann/json（vendor）

| | |
|---|---|
| バージョン | **3.12.0** |
| ライセンス | **MIT**（SPDX-License-Identifier: MIT） |
| 取得元 | `https://github.com/nlohmann/json/releases/latest/download/json.hpp` |
| 取得日 | 2026-07-30 |
| SHA-256 | `aaf127c04cb31c406e5b04a63f1ae89369fccde6d8fa7cdda1ed4f32dfc5de63` |
| サイズ | 953,436 バイト |

## なぜ vendor するのか（FetchContent を使わない理由）

doctest と同じ扱いです。

- **cmake の表面積がゼロ** — `add_library(INTERFACE)` と include パスだけ
- **ネットワーク非依存** — オフラインでもビルドできる
- **バージョンが git に固定される** — 「手元では通るが CI では別バージョン」が起きない

## 使ってよい場所

**`ambient_transport` の実装（`.cpp`）だけです。**

- `ambient_core` には**絶対に入れません**（標準ライブラリのみに依存する層）
- `ambient_transport` の**公開ヘッダに JSON 型を漏らさないこと**。PRIVATE リンクなので、
  漏らすと利用側でコンパイルエラーになります（それが正しい状態です）
- Tab5 では ArduinoJson に差し替わります。JSON 型が公開ヘッダに出ていると差し替えられません

## セキュリティ上の前提

パース対象は**認証の無い LAN 上のサーバから来る信頼できない入力**です。攻撃者が応答を
偽装しうるので、次を守ってください。

- **例外を有効にしないパース経路を使うこと。** `nlohmann::json::parse(..., nullptr, false)`
  （3番目の引数 `allow_exceptions=false`）で `is_discarded()` を検査する。
  `ambient_core` の規律は「例外を制御フローに使わない」であり、transport でも同じ方針を採る
- **入力サイズに上限を掛けること**（`IHttpClient` 側に `maxResponseBytes` の口がある）
- **ネストの深さに上限を掛けること**（深い入れ子でスタックを枯らせる攻撃を防ぐ）
