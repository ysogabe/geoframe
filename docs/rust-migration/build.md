# Rustバイナリのビルドと配布方式（P0 / T0.3）

- 状態: 検証済み（2026-10-24）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）
- 計画: [plan.md](plan.md)の「musl静的ビルドかglibcビルドか」への答え

## 1. 結論

**標準のビルドは`x86_64-unknown-linux-musl`の完全静的バイナリにする。** 現時点の依存（tokio・axum・rusqlite `bundled`・serde・tracing）は純Rustまたは同梱Cコードだけで、musl静的でビルドでき、検証環境（lxc-geoframe。Ubuntu 24.04、glibc 2.39）で動いた。

ただし次の依存が入るときは、その都度この結論を見直す（§6）。

- HEIC変換: 画像PoCの結果（[image-poc.md](image-poc.md)）、**純Rustの`zune-heic`ならmuslの静的ビルドのままHEICを含められる**（合成HEIC 50枚でlibheifと出力がバイト単位で一致）。実機のHEICで問題が出て`libheif`が要る場合も、zig ccでmuslの静的ビルドができた（§6）。方針は変えない。
- 外部HTTPのTLSとDNS: **P2のT2.1で確かめた**。`ureq`＋`rustls`（ring）＋`webpki-roots`の埋め込みで、musl静的のまま検証環境から地理院へHTTPSが通った（§7）。

## 2. ビルド方法

### 開発機（n100: Debian 13、glibc 2.41）

```bash
# 初回だけ
sudo apt-get install -y musl-tools
rustup target add x86_64-unknown-linux-musl

cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test

# 配布用（完全静的）。本体だけ（画像PoCは含めない）
cargo build --release --target x86_64-unknown-linux-musl -p geoframe
# 成果物: target/x86_64-unknown-linux-musl/release/geoframe
```

`musl-tools`は`rusqlite`の`bundled`がSQLite（C）をビルドするために要る（`musl-gcc`を使う）。

### CI

[`.github/workflows/test.yml`](../../.github/workflows/test.yml)の`rust`ジョブが、Pythonのジョブとは別に次を行う。

1. `dtolnay/rust-toolchain@stable`（rustfmt・clippy・muslターゲット付き）と`musl-tools`を入れる。
2. `Swatinem/rust-cache`でビルドをキャッシュする。
3. `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`。
4. `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`。
5. 成果物（本体の`geoframe`。`-p geoframe`で限る。画像PoCは別の手順で、libheifなしの`--features heic-zune,mimalloc`だけ同じ検査をする）が静的リンクであることを`file`（`statically linked`または`static-pie linked`）と`ldd`（共有ライブラリを列挙しない）で検査し、`--version`を実行する。動的リンクが混ざるとCIが落ちる。

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
- **T1.3（管理画面の静的ファイル約936 KiBの埋め込み、静的配信、Host検査）の後**: musl静的で strip前 5,767,424 B（約5.5 MiB）、strip後 4,886,392 B（約4.7 MiB）。T1.2の時点（strip前 4,565,432 B、strip後 3,772,152 B）から約1.1〜1.2 MB増え、そのうち約0.94 MBが埋め込んだファイル。`static-pie linked`・`ldd`は`statically linked`のまま。埋め込みは`crates/geoframe/build.rs`が`app/static`・`app/vendor`を`include_bytes!`の表にする（外部crateなし）ので、ビルドには**リポジトリの`app/`が要る**（[server.md §9.4](server.md)）。追加した依存は`httpdate`・`futures-util`だけで、どちらも純Rustで依存木に既にあった。
- **T1.4（管理画面の読み取りAPI）の後**: musl静的で strip前 6,170,752 B（約5.9 MiB）。`static-pie linked`・`ldd`は`statically linked`のまま。直接の依存に`libc`（`statvfs`・`localtime_r`のため。`tokio`が既に引いている純FFIのcrateで、Cのビルドは無い）を足した。`unsafe`は`sysinfo.rs`だけ（[server.md §10.4](server.md)）。musl版でも`TZ=Asia/Tokyo`の現地時刻が出ることを確かめた（合成DB・ループバックのみ）。
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
| `Cargo.toml` | workspace（resolver 3、メンバーは`crates/geoframe`と`crates/geoframe-imgpoc`）。`Cargo.lock`はコミットする |
| `crates/geoframe/` | ライブラリ＋バイナリ`geoframe`。edition 2024（miharashiに合わせる） |
| `crates/geoframe-imgpoc/` | 画像処理の技術PoC（`publish = false`）。本体の静的ビルドの検査には含めない。CIは別の手順で、`--features heic-zune,mimalloc`（libheifなし）のmusl静的ビルドと`file`／`ldd`の検査だけを行う。結果は[image-poc.md](image-poc.md) |
| `geoframe --version` | バージョンを出す |
| `geoframe check-db <path>` | `photos`の行数とSQLiteのバージョンを出す。読み取り専用 |
| `geoframe selfcheck-net` | T2.1。地理院の逆ジオコーダへ公開の代表点を1件問い合わせ、外向きHTTPS（名前解決・TLS・証明書）が通るかを確かめる。出力は状態とHTTPステータスだけ（§7） |
| `geoframe sync [--dry-run]` | T2.1。同期サイクルを1回だけ動かす。`--dry-run`は書かずに件数だけ（[server.md §11](server.md)） |
| `geoframe serve [--listen <addr>] [--data-dir <dir>]` | 読み取り専用のサーバー。P0では`GET /healthz`が`{"ok":true}`を返すだけだった。T1.1で、設定・DB読み取り層・ログ・DBを読めるかを返すヘルスを足した（[server.md](server.md)）。SIGTERMで停止 |
| `deploy/geoframe-rs.service`・`deploy/rust-deploy.sh` | T1.5。検証環境向けのsystemd unitと、ビルド→転送→入れ替え→確認→巻き戻しの配布スクリプト（[deploy/README.md §7](../../deploy/README.md)、[server.md §6](server.md)）。本番の切替はP5 |

依存は`tokio`・`axum`・`rusqlite`（`bundled`）・`serde`・`serde_json`、T1.1で`tracing`・`tracing-subscriber`（どちらも純Rust。`env-filter`・時刻・色の機能は入れない）。外部HTTPクライアントはT2.1で入れた（§7）。`image`はまだ入れていない。SQLiteの呼び出しは`db::Catalog::read`で`spawn_blocking`に出し、tokioのworkerを塞がない形にした（P1以降もこの形に揃える。[server.md](server.md) §2.2）。

## 6. 今後の注意と代替案

- **HEIC**: 画像PoC（[image-poc.md](image-poc.md)）の結果は次のとおり。
  - **純Rustの`zune-heic`（既定の候補）**: C依存なし。musl静的で通り、7.0 MBのバイナリ（`ldd`は動的実行ファイルではないと答える）で検証環境のHEIC 50枚を処理できた。
  - **`libheif-rs`＋システムのlibheif 1.17.6**: glibc動的リンク。HEVCのデコーダが別パッケージの`libheif-plugin-libde265`で、無いと読めない。
  - **`libheif-rs`の`embedded-libheif`**: glibcだと自己完結しない（`libde265`・`libx265`・`libaom`などへ動的リンク）。**zig ccでmuslの完全静的ビルドは通った**（静的libde265、`RUSTFLAGS=-C link-self-contained=no`、コンパイラのラッパーで`--target=`を落とす）。LGPLの再リンクの扱いに注意（配布するなら）。
  - 外部コマンド（`heif-convert`）への委譲は、JPEG再圧縮が増え、いちばん遅い。
  libheifが必要でmuslで通らない場合は次のどちらかにする。
  - 配布先と同じglibcでビルドする（検証環境の中、または同じ世代のコンテナでビルドする）。HEIC部分だけ別プロセス（`heif-convert`など）にする案もある。
  - `cargo-zigbuild`でglibcの古い版（例: 2.31）を指定してビルドする。n100の新しいglibcを避けられる。
- **名前解決**: muslの`getaddrinfo`はglibcと違い、`/etc/resolv.conf`の`search`・`options`の扱いが一部異なる。T2.1で検証環境から実際に名前解決できることは確かめた（§7）が、検証環境の`resolv.conf`は`nameserver`1行と`search .`だけの単純な形で、**この差は踏んでいない**。本番機の`resolv.conf`（`search`が複数ある、`ndots`の指定があるなど）での再確認はP5の切替の手順に入れる。
- **メモリアロケータ**: muslの標準アロケータは画像処理で**約15〜25%遅かった**（画像PoC: JPEG 273 vs 235 ms、`zune-heic` 1,086 vs 871 ms）。`mimalloc`を付けると、JPEGはglibcと同等以上（222 ms）になるが、HEIC（`zune-heic`）はglibcより約8%遅いまま（939 vs 871 ms）で、RSSもglibc並みに増える。`mimalloc`はCのソースを`cc`でビルドする（CIの`rust`ジョブは`musl-tools`を入れてある）。P2の画像処理から`mimalloc`を付ける。現状の本体（画像処理なし）の最大RSS（1.5〜2.8 MiB）では差は出ていない。
- **glibcビルドを選ぶ場合**: CIとn100のglibcは、本番（P5で切り替える機体）より古くなければならない。バイナリの`objdump -T`で要求バージョンを確かめる。画像PoCでは、開発機（glibc 2.41）でビルドしたglibcバイナリが検証環境（2.39）でそのまま動いた。`objdump -T`が出す最大版（`GLIBC_2.39`）は、Rust 1.99の`std::process::Command`が参照する`pidfd_spawnp`・`pidfd_getpid`の**弱参照**（`readelf -V`で`WEAK`）で、必須の要求は2.34まで。最大版だけでなく`WEAK`の有無も見る。

## 7. 外向きHTTPSとJWT（P2 / T2.1）

取り込み（Drive・地理院・Nominatim）が外へ出るので、**musl静的のままHTTPSとサービスアカウントのJWTが動くか**を、P2の最初に確かめた（確かめられなければ通信の作りを変える、というのがtasks.mdの条件だった）。結果は**通った**。

### 7.1 選んだ依存

| crate | 用途 | 理由 |
| --- | --- | --- |
| `ureq` 3.5（`default-features = false`、`rustls`・`gzip`） | HTTPクライアント（同期） | 同期サイクルは専用のstdスレッドで動かす（[server.md §11](server.md)）ので、同期APIで足りる。miharashiの取得処理も`ureq`。`reqwest`は非同期でhyperのクライアント側まで引くため見送った。`native-tls`・`openssl`は使わない |
| `rustls` 0.23（`ureq`が引く）＋`ring` 0.17 | TLS | 暗号は`ring`。`aws-lc-rs`はcmakeと追加のCビルドが要るので避けた。`ring`のC・アセンブリは`cc`（`musl-gcc`）でビルドされ、rusqliteの`bundled`と同じ前提（CIの`musl-tools`で足りる） |
| `webpki-roots` 1.0（`ureq`の`rustls`が引く） | ルート証明書 | **バイナリに埋め込む**。OSの証明書（`/etc/ssl/certs`）に依存しないので、unitや機体に証明書の条件は要らない。更新はcrateの更新で行う |
| `ring`（直接） | JWT（RS256）の署名・検証 | `rsa` crateは使わない（純Rust実装の署名にタイミング攻撃の既知の問題があり、修正版が無い）。`ring`は既に依存木にある。PKCS#8（Googleの鍵JSONの`private_key`）とPKCS#1の両方を読める |
| `base64` 0.22 | JWTのBase64url、PEMの本文 | 純Rustで小さい |

`jsonwebtoken`は使わない（暗号バックエンドに`rsa`か`aws-lc-rs`を選ばされるため）。JWTは`crates/geoframe/src/jwt.rs`が、ヘッダ・claimsのBase64urlと`ring`の署名で組み立てる。

### 7.2 確かめたこと

- 検証環境（Ubuntu 24.04）で、n100でビルドしたmusl静的バイナリの`geoframe selfcheck-net`（地理院の逆ジオコーダへ公開の代表点を1件問い合わせる確認用のサブコマンド）が、名前解決・TLSの証明書検証・HTTP 200・JSONの読み取りまで**成功**した。出力は状態とHTTPステータスだけ（`status=ok http=200 json=ok failure=-`）で、URL・座標・応答の中身は出さない。
- `strace`で開くファイルを見ると、名前解決のために`/etc/hosts`と`/etc/resolv.conf`だけを読み、**`/etc/ssl`は一切開かない**（証明書が埋め込みであることの直接の証拠）。
- JWTは、テスト専用の使い捨てRSA鍵（2048ビット。どこにも登録していない。ヘッダ行を付けずにBase64だけを`crates/geoframe/tests/fixtures/`に置く）で、署名→検証、PKCS#1/#8、改ざん・不正な形式の拒否、鍵の中身をエラーやDebugに出さないことをユニットテストした。Googleへの実際の認証はT2.4。

### 7.3 サイズの変化

| | strip前 | strip後 |
| --- | --- | --- |
| P1の終わり（基準。`fdde8e1`） | 6,170,664 B（約5.9 MiB） | 5,206,040 B（約5.0 MiB） |
| T2.1の依存追加後（HTTPS・JWTのみ） | 8,813,736 B（約8.4 MiB） | 7,336,056 B（約7.0 MiB） |
| 差 | +2,643,072 B（約+2.5 MiB） | +2,130,016 B（約+2.0 MiB） |
| T2.1の終わり（writer・スキーマ・同期サイクル・キャッシュも含む。`eda1377`） | 9,051,008 B（約8.6 MiB） | 7,512,760 B（約7.2 MiB） |

`static-pie linked`・`ldd`は`statically linked`のまま。増加の大半は`rustls`・`webpki-roots`（証明書の表）・`ring`。LTO・`opt-level="s"`・`strip`は未設定のままで、必要になったら設定する。クリーンビルドの時間は増えたが、`ring`の`cc`ビルドを含めても1分未満。

### 7.4 残した課題

- 本番機の`resolv.conf`（`search`が複数、`options ndots`など）でのmuslの名前解決の再確認（P5）。
- Google（`oauth2.googleapis.com`・`www.googleapis.com`）への実際の認証と、Driveの大きな原本のダウンロードに合わせたタイムアウトの調整はT2.4。今の`net::agent()`は接続10秒・1要求30秒。
- プロキシは`ureq`の既定どおり環境変数（`HTTPS_PROXY`など）を見る。unitは与えない。
