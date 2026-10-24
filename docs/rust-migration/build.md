# Rustバイナリのビルドと配布方式（P0 / T0.3）

- 状態: 検証済み（2026-10-24）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）
- 計画: [plan.md](plan.md)の「musl静的ビルドかglibcビルドか」への答え

## 1. 結論

**標準のビルドは`x86_64-unknown-linux-musl`の完全静的バイナリにする。** 現時点の依存（tokio・axum・rusqlite `bundled`・serde）は純Rustまたは同梱Cコードだけで、musl静的でビルドでき、検証環境（lxc-geoframe。Ubuntu 24.04、glibc 2.39）で動いた。

ただし次の依存が入るときは、その都度この結論を見直す（§6）。

- HEIC変換（libheif連携など）: P0の画像PoCの結果次第。純Rustで完結しなければ、muslで静的にリンクできるか、またはglibcビルドに切り替えるかを決める。
- 外部HTTP（Gemini）のTLSとDNS: `rustls`系ならmuslでも通る見込み。ただしmuslの名前解決はglibcと挙動が違う（§6）。

## 2. ビルド方法

### 開発機（n100: Debian 13、glibc 2.41）

```bash
# 初回だけ
sudo apt-get install -y musl-tools
rustup target add x86_64-unknown-linux-musl

cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test

# 配布用（完全静的）
cargo build --release --target x86_64-unknown-linux-musl
# 成果物: target/x86_64-unknown-linux-musl/release/geoframe
```

`musl-tools`は`rusqlite`の`bundled`がSQLite（C）をビルドするために要る（`musl-gcc`を使う）。

### CI

[`.github/workflows/test.yml`](../../.github/workflows/test.yml)の`rust`ジョブが、Pythonのジョブとは別に次を行う。

1. `dtolnay/rust-toolchain@stable`（rustfmt・clippy・muslターゲット付き）と`musl-tools`を入れる。
2. `Swatinem/rust-cache`でビルドをキャッシュする。
3. `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`。
4. `cargo build --release --target x86_64-unknown-linux-musl`。
5. 成果物が静的リンクであることを`file`（`statically linked`または`static-pie linked`）と`ldd`（共有ライブラリを列挙しない）で検査し、`--version`を実行する。動的リンクが混ざるとCIが落ちる。

## 3. musl静的ビルドの結果

`cargo build --release --target x86_64-unknown-linux-musl`（クリーンビルド25秒、依存込み）は通った。

| 項目 | musl静的 | glibc（既定ターゲット） |
| --- | --- | --- |
| サイズ（strip前） | 4,101,136 B（約3.9 MiB） | 4,067,704 B（約3.9 MiB） |
| サイズ（`strip`後） | 3,452,152 B（約3.3 MiB） | 未計測 |
| `file` | `ELF 64-bit LSB pie executable, x86-64, static-pie linked`、`statically linked` | `dynamically linked, interpreter /lib64/ld-linux-x86-64.so.2` |
| `ldd` | `statically linked` | `libgcc_s.so.1`、`libm.so.6`、`libc.so.6`、`ld-linux-x86-64.so.2` |
| 要求するglibc | なし | `GLIBC_2.34`まで（`objdump -T`） |

- サイズはほぼ同じ。リリースプロファイルは既定のまま（`strip`・LTOは未設定。必要になったら設定する）。
- glibcビルドの要求は2.34までで、検証環境（2.39）にもn100（2.41）にも載る。ただし、これはこの小さな依存セットでの値で、依存が増えると上がる。配布先のglibcより新しい機体でビルドすると動かなくなる（plan.mdの懸念）。

## 4. 検証環境での動作確認

n100でビルドしたバイナリを`scp`で検証環境（lxc-geoframe）の`/tmp`へ置き、確認後に削除した。本番DBのコピー（`/var/lib/geoframe/catalog.db`、約84 MiB）は`check-db`で読み取り専用に開いただけで、書き込みはしていない。

| 確認 | musl静的 | glibc |
| --- | --- | --- |
| `--version` | `geoframe 0.1.0` | 同じ |
| `check-db`（読み取り専用） | `photos: 6535`、`sqlite: 3.53.2`、終了コード0 | 同じ値 |
| `check-db`の最大RSS | 2,848 KiB | 4,848 KiB |
| `serve --listen 127.0.0.1:18080`→`curl /healthz` | `HTTP/1.1 200 OK`、`{"ok":true}` | 同じ |
| `serve`の最大RSS | 1,544 KiB | 4,124 KiB |
| SIGTERMでの終了 | 正常終了（終了コード0） | 同じ |

- 検証環境にはRustもmuslも入っていない。musl版は検証環境側に何も入れずに動いた。
- `check-db`の件数（6,535件）は、検証環境にある2026-10-24 06:17のバックアップのコピーに対する値。
- `photos`テーブルの行数と`sqlite`のバージョン以外（位置情報・ファイル名など）は出力していない。

### 読み取り専用で開いたときの副作用

`catalog.db`はWALモードで、`SQLITE_OPEN_READ_ONLY`で開いても、DBと同じディレクトリに空の`catalog.db-wal`（0バイト）と`catalog.db-shm`（32 KiB、共有メモリ）が作られた。DB本体（`catalog.db`）のサイズと更新時刻は変わっていない。これはSQLiteのWALの仕様で、読み取り側も共有メモリを使うため。

- 検証環境のコピーでは問題にならない（作られた2つのファイルは残っている。次にRustかPythonがDBを開くときにそのまま使われる）。
- 本番DBを直接開かない（plan.md §2）理由の一つ。本番DBに`check-db`を向けると、本番のディレクトリにこれらのファイルを増やしうる。本番機での確認は、バックアップの`sqlite3 .backup`コピーに向ける。
- `immutable=1`で開けば副作用は避けられるが、書き込み中のDBを開くと不整合な読み取りになりうるため、常用しない。

## 5. 作ったもの

| ファイル | 内容 |
| --- | --- |
| `Cargo.toml` | workspace（resolver 3、メンバーは`crates/geoframe`のみ）。`Cargo.lock`はコミットする |
| `crates/geoframe/` | ライブラリ＋バイナリ`geoframe`。edition 2024（miharashiに合わせる） |
| `geoframe --version` | バージョンを出す |
| `geoframe check-db <path>` | `photos`の行数とSQLiteのバージョンを出す。読み取り専用 |
| `geoframe serve --listen <addr>` | `GET /healthz`が`{"ok":true}`を返す。SIGTERMで停止 |

依存は`tokio`・`axum`・`rusqlite`（`bundled`）・`serde`・`serde_json`。外部HTTPクライアントと`image`はまだ入れていない。SQLiteの呼び出しは`db::check_db_blocking`のように`spawn_blocking`で包み、tokioのworkerを塞がない形にした（P1以降もこの形に揃える）。

## 6. 今後の注意と代替案

- **HEIC**: libheifのような外部Cライブラリが必要になった場合、musl静的で通るか（libheif・libde265・x265のビルド）を、画像PoCで確かめる。通らなければ次のどちらかにする。
  - 配布先と同じglibcでビルドする（検証環境の中、または同じ世代のコンテナでビルドする）。HEIC部分だけ別プロセス（`heif-convert`など）にする案もある。
  - `cargo-zigbuild`でglibcの古い版（例: 2.31）を指定してビルドする。n100の新しいglibcを避けられる。
- **名前解決**: muslの`getaddrinfo`はglibcと違い、`/etc/resolv.conf`の`search`・`options`の扱いが一部異なる。外部HTTP（Gemini）を入れるP4で、検証環境から実際に名前解決できることを確かめる。
- **メモリアロケータ**: muslの標準アロケータはマルチスレッドの負荷で遅いことがある。画像処理を入れた段階（P2〜）で処理時間とRSSを計測し、問題なら`mimalloc`などを検討する。現状の最大RSS（1.5〜2.8 MiB）では差は出ていない。
- **glibcビルドを選ぶ場合**: CIとn100のglibcは、本番（P5で切り替える機体）より古くなければならない。バイナリの`objdump -T`で要求バージョンを確かめる。
