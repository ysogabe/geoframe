# doctest（vendor 済み単一ヘッダ）

| 項目 | 値 |
|---|---|
| ライブラリ | [doctest](https://github.com/doctest/doctest) |
| バージョン | **2.4.11** |
| ファイル | `doctest.h`（単一ヘッダ、7108 行） |
| SHA-256 | `0be75cae36a24681f14b73df8509938db8e4366ffc05677ed79e6814d9ea756e` |
| ライセンス | **MIT**（Copyright (c) 2016-2023 Viktor Kirilov） |
| 取得元 | ローカルに既存の未改変コピー（`~/Documents/Arduino/libraries/FastLED/tests/doctest.h`）を複製 |

ライセンス本文はヘッダ冒頭のコメントに含まれています（MIT の全文は
<https://opensource.org/licenses/MIT>）。

## なぜ FetchContent ではなく vendor するのか

- **cmake の表面積がゼロになる。** `FetchContent_Declare` / `MakeAvailable` は
  外部プロジェクトの CMakeLists を実行するため、こちらの設定（警告フラグ、
  C++ 標準、ビルドタイプ）と干渉しうる。単一ヘッダなら `target_include_directories`
  1 行で済む。
- **ネットワークに依存しない。** configure のたびに GitHub を叩かない。
  オフラインでもビルドできる。
- **バージョンが git に固定される。** 「いつの間にか上流が更新されて
  テストの挙動が変わった」が起きない。更新は明示的なコミットになる。

## 更新するとき

1. <https://github.com/doctest/doctest/releases> から新しい `doctest.h` を取得
2. `doctest.h` を差し替え、この README のバージョン・行数・SHA-256 を更新
3. `ctest` が緑であることを確認

## 改変していないこと

このファイルは上流のまま**未改変**です。改変が必要になったら、
改変内容をこの README に明記すること（差分が追えなくなるため）。
