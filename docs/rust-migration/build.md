# Rustバイナリのビルドと配布方式（P0 / T0.3）

- 状態: 検証済み（2026-10-24）
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）
- 計画: [plan.md](plan.md)の「musl静的ビルドかglibcビルドか」への答え

## 1. 結論

**標準のビルドは`x86_64-unknown-linux-musl`の完全静的バイナリにする。** 現時点の依存（tokio・axum・rusqlite `bundled`・`backup`・serde・tracing）は純Rustまたは同梱Cコードだけで、musl静的でビルドでき、検証環境（lxc-geoframe。Ubuntu 24.04、glibc 2.39）で動いた。

ただし次の依存が入るときは、その都度この結論を見直す（§6）。

- HEIC変換: 画像PoCの結果（[image-poc.md](image-poc.md)）、**純Rustの`zune-heic`ならmuslの静的ビルドのままHEICを含められる**（合成HEIC 50枚でlibheifと出力がバイト単位で一致）。**T2.2で本体に入れ、CIのmusl静的ビルドと`ldd`の検査がそのまま覆っている**（§8）。実機のHEICで問題が出て`libheif`が要る場合も、zig ccでmuslの静的ビルドができた（§6）。方針は変えない。
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

# 配布用（完全静的）。画像処理（HEIC・mimalloc）を含む本体
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
5. 成果物（`geoframe`。T2.2から画像処理＝純RustのHEICと、muslのときだけ`mimalloc`も入る）が静的リンクであることを`file`（`statically linked`または`static-pie linked`）と`ldd`（共有ライブラリを列挙しない）で検査し、`--version`を実行する。動的リンクが混ざるとCIが落ちる。さらに、静的バイナリの`geoframe img-probe`で合成のHEIC（ICC付きを含む）を処理して、HEICのデコードが静的のまま動くことを確かめる。T2.2で、PoC用の2つ目のmusl静的ビルドと、libheifへ動的リンクする`imgpoc-heic`ジョブは削除した。

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
- **T1.3（管理画面の静的ファイル約936 KiBの埋め込み、静的配信、Host検査）の後**: musl静的で strip前 5,767,424 B（約5.5 MiB）、strip後 4,886,392 B（約4.7 MiB）。T1.2の時点（strip前 4,565,432 B、strip後 3,772,152 B）から約1.1〜1.2 MB増え、そのうち約0.94 MBが埋め込んだファイル。`static-pie linked`・`ldd`は`statically linked`のまま。埋め込みは`crates/geoframe/build.rs`が`app/static`・`app/vendor`を`include_bytes!`の表にする（外部crateなし）ので、ビルドには**リポジトリの`app/`が要る**（[server.md §9.4](server.md)）。T3.5から旅行ブログの旅程JSON（`app/travel_blog/plans/*.json`、数KB）も同じ方法で埋め込む（[server.md §20.1](server.md)）。追加した依存は`httpdate`・`futures-util`だけで、どちらも純Rustで依存木に既にあった。
- **T1.4（管理画面の読み取りAPI）の後**: musl静的で strip前 6,170,752 B（約5.9 MiB）。`static-pie linked`・`ldd`は`statically linked`のまま。直接の依存に`libc`（`statvfs`・`localtime_r`のため。`tokio`が既に引いている純FFIのcrateで、Cのビルドは無い）を足した。`unsafe`は`sysinfo.rs`だけ（[server.md §10.4](server.md)）。musl版でも`TZ=Asia/Tokyo`の現地時刻が出ることを確かめた（合成DB・ループバックのみ）。
- **T2.2（画像処理）の後**: §8。musl静的で strip前 15,812,376 B（約15.1 MiB）、strip後 13,491,064 B（約12.9 MiB）（T2.2の最後のコミット時点。レビューの修正の前後で、strip後は同じ値だった）。直前（T2.1の終わり、`57714f2`）を同じ手順で作り直した 9,145,120 B／7,590,648 B から、strip前で +6.7 MB（+73%）。`static-pie linked`・`ldd`は`statically linked`のまま。
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
| `Cargo.toml` | workspace（resolver 3、メンバーは`crates/geoframe`だけ。T2.2で`crates/geoframe-imgpoc`を外した）。`Cargo.lock`はコミットする |
| `crates/geoframe/` | ライブラリ＋バイナリ`geoframe`。edition 2024（miharashiに合わせる） |
| `crates/geoframe/src/imaging/`・`datadir.rs` | T2.2。画像処理（derivativeの生成・EXIF・HEIC）と`DATA_DIR`の外へ出ないパスの検査。PoC（`crates/geoframe-imgpoc`）はここへ移して削除した（コミット`c41c4a9`に残る）。結果は[image-poc.md](image-poc.md)、実装の説明は[server.md §12](server.md) |
| `geoframe img-probe [--out <dir>] [--rotation <度>] [--jobs <n>] <画像>...` | T2.2。画像処理の確認・計測用。DB・データ置き場に触れず、連番・形式・寸法・向き・ICC・所要時間だけを出す（ファイル名は出さない）。HEICが読めないときは構造の分類が付く |
| `geoframe --version` | バージョンを出す |
| `geoframe check-db <path>` | `photos`の行数とSQLiteのバージョンを出す。読み取り専用 |
| `geoframe selfcheck-net` | T2.1。地理院の逆ジオコーダへ公開の代表点を1件問い合わせ、外向きHTTPS（名前解決・TLS・証明書）が通るかを確かめる。出力は状態とHTTPステータスだけ（§7） |
| `geoframe sync [--dry-run]` | T2.1。同期サイクルを1回だけ動かす。`--dry-run`は書かずに件数だけ（[server.md §11](server.md)） |
| `geoframe serve [--listen <addr>] [--data-dir <dir>]` | 読み取り専用のサーバー。P0では`GET /healthz`が`{"ok":true}`を返すだけだった。T1.1で、設定・DB読み取り層・ログ・DBを読めるかを返すヘルスを足した（[server.md](server.md)）。SIGTERMで停止 |
| `deploy/geoframe-rs.service`・`deploy/rust-deploy.sh` | T1.5。検証環境向けのsystemd unitと、ビルド→転送→入れ替え→確認→巻き戻しの配布スクリプト（[deploy/README.md §7](../../deploy/README.md)、[server.md §6](server.md)）。本番の切替はP5 |

依存は`tokio`・`axum`・`rusqlite`（`bundled`・`backup`。dry-run の写し用。追加の依存は無い）・`serde`・`serde_json`、T1.1で`tracing`・`tracing-subscriber`（どちらも純Rust。`env-filter`・時刻・色の機能は入れない）。外部HTTPクライアントはT2.1で入れた（§7）。画像処理の依存はT2.2で入れた（§8）。T3.6で`serde_json`に`preserve_order`機能を入れた（下書きと書き出しのmanifestをPythonと同じキーの並びで書くため。`indexmap`が増える。純Rustで、musl静的ビルドは変わらない）。SQLiteの呼び出しは`db::Catalog::read`で`spawn_blocking`に出し、tokioのworkerを塞がない形にした（P1以降もこの形に揃える。[server.md](server.md) §2.2）。

## 6. 今後の注意と代替案

- **HEIC**: 画像PoC（[image-poc.md](image-poc.md)）の結果は次のとおり。
  - **純Rustの`zune-heic`（既定の候補）**: C依存なし。musl静的で通り、7.0 MBのバイナリ（`ldd`は動的実行ファイルではないと答える）で検証環境のHEIC 50枚を処理できた。
  - **`libheif-rs`＋システムのlibheif 1.17.6**: glibc動的リンク。HEVCのデコーダが別パッケージの`libheif-plugin-libde265`で、無いと読めない。
  - **`libheif-rs`の`embedded-libheif`**: glibcだと自己完結しない（`libde265`・`libx265`・`libaom`などへ動的リンク）。**zig ccでmuslの完全静的ビルドは通った**（静的libde265、`RUSTFLAGS=-C link-self-contained=no`、コンパイラのラッパーで`--target=`を落とす）。LGPLの再リンクの扱いに注意（配布するなら）。
  - 外部コマンド（`heif-convert`）への委譲は、JPEG再圧縮が増え、いちばん遅い。
  **T2.2の決定: `zune-heic`だけを本体に入れ、libheifへの逃げ道はコードに残さない**（optionalな依存として置くとCIで一度もコンパイルされず腐るため）。実機のHEIC（T2.4で確認）で`zune-heic`が読めない形式が出たら、PoCの`crates/geoframe-imgpoc/src/heic.rs`（コミット`c41c4a9`）を参照して`libheif-rs`（`default-features = false, features = ["v1_17"]`）を足す。そのときの配布は次のどちらか（[image-poc.md §4・§6](image-poc.md)）。
  libheifが必要でmuslで通らない場合は次のどちらかにする。
  - 配布先と同じglibcでビルドする（検証環境の中、または同じ世代のコンテナでビルドする）。HEIC部分だけ別プロセス（`heif-convert`など）にする案もある。
  - `cargo-zigbuild`でglibcの古い版（例: 2.31）を指定してビルドする。n100の新しいglibcを避けられる。
- **名前解決**: muslの`getaddrinfo`はglibcと違い、`/etc/resolv.conf`の`search`・`options`の扱いが一部異なる。T2.1で検証環境から実際に名前解決できることは確かめた（§7）が、検証環境の`resolv.conf`は`nameserver`1行と`search .`だけの単純な形で、**この差は踏んでいない**。本番機の`resolv.conf`（`search`が複数ある、`ndots`の指定があるなど）での再確認はP5の切替の手順に入れる。
- **メモリアロケータ**: muslの標準アロケータは画像処理で**約15〜25%遅かった**（画像PoC: JPEG 273 vs 235 ms、`zune-heic` 1,086 vs 871 ms）。`mimalloc`を付けると、JPEGはglibcと同等以上（222 ms）になるが、HEIC（`zune-heic`）はglibcより約8%遅いまま（939 vs 871 ms）で、RSSもglibc並みに増える。`mimalloc`はCのソースを`cc`でビルドする（CIの`rust`ジョブは`musl-tools`を入れてある）。**T2.2で、muslのときだけ`mimalloc`を付けた**（`main.rs`の`#[global_allocator]`、依存は`[target.'cfg(target_env = "musl")'.dependencies]`）。glibcのビルドは標準のアロケータのまま。画像処理のない本体の最大RSS（1.5〜2.8 MiB）では差は出ていなかった。
- **glibcビルドを選ぶ場合**: CIとn100のglibcは、本番（P5で切り替える機体）より古くなければならない。バイナリの`objdump -T`で要求バージョンを確かめる。画像PoCでは、開発機（glibc 2.41）でビルドしたglibcバイナリが検証環境（2.39）でそのまま動いた。`objdump -T`が出す最大版（`GLIBC_2.39`）は、Rust 1.99の`std::process::Command`が参照する`pidfd_spawnp`・`pidfd_getpid`の**弱参照**（`readelf -V`で`WEAK`）で、必須の要求は2.34まで。最大版だけでなく`WEAK`の有無も見る。

## 7. 外向きHTTPSとJWT（P2 / T2.1）

取り込み（Drive・地理院・Nominatim）が外へ出るので、**musl静的のままHTTPSとサービスアカウントのJWTが動くか**を、P2の最初に確かめた（確かめられなければ通信の作りを変える、というのがtasks.mdの条件だった）。結果は**通った**。

### 7.1 選んだ依存

| crate | 用途 | 理由 |
| --- | --- | --- |
| `ureq` 3.5（`default-features = false`、`rustls`・`gzip`） | HTTPクライアント（同期） | 同期サイクルは専用のstdスレッドで動かす（[server.md §11](server.md)）ので、同期APIで足りる。miharashiの取得処理も`ureq`。`reqwest`は非同期でhyperのクライアント側まで引くため見送った。`native-tls`・`openssl`は使わない |
| `rustls` 0.23（`ureq`が引く）＋`ring` 0.17 | TLS | 暗号は`ring`。`aws-lc-rs`はcmakeと追加のCビルドが要るので避けた。`ring`のC・アセンブリは`cc`（`musl-gcc`）でビルドされ、rusqliteの`bundled`と同じ前提（CIの`musl-tools`で足りる） |
| `webpki-roots` 1.0（`ureq`の`rustls`が引く） | ルート証明書 | **バイナリに埋め込む**。OSの証明書（`/etc/ssl/certs`）に依存しないので、unitや機体に証明書の条件は要らない。更新はcrateの更新で行う |
| `ring`（直接） | JWT（RS256）の署名・検証 | `rsa` crateは使わない（暗号の依存を増やさずに済む。`rsa`にはRUSTSEC-2023-0071の経緯もある）。`ring`は既に依存木にある。PKCS#8（Googleの鍵JSONの`private_key`）とPKCS#1の両方を読める |
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
- Google（`oauth2.googleapis.com`・`www.googleapis.com`）への認証は、T2.4で入れた（合成の鍵を実際のGoogleに向けるとmusl静的バイナリのまま署名・TLS・トークンの交換が動き、400で断られる。[server.md §14.10](server.md#1410-検証環境での確認鍵が無い状態2026-10-25)）。本物の鍵での確認は管理者（[§14.11](server.md#1411-鍵が置かれたあとの管理者の確認手順)）。タイムアウトは`net::agent()`が接続10秒・1要求30秒で、Driveの原本のダウンロードだけ要求ごとに15分へ延ばす（[§14.3](server.md#143-通信)）。
- プロキシは`ureq`の既定どおり環境変数（`HTTPS_PROXY`など）を見る。unitは与えない。

## 8. 画像処理の依存（P2 / T2.2）

画像処理（derivativeの生成・EXIF・HEIC）を[image-poc.md](image-poc.md)の推奨構成で`crates/geoframe`に入れた。**すべて純Rust**（C依存が増えたのは`mimalloc`だけで、muslのときだけ）。実装の説明は[server.md §12](server.md)。

### 8.1 追加した依存

| crate | 用途 | メモ |
| --- | --- | --- |
| `zune-jpeg` 0.5.15 | JPEGのデコード | `image`が内部で使うのと同じ版。**厳格モードで読み、途中で切れたJPEGを失敗にする**（`image`の`JpegDecoder`は寛容で、部分的な画像で成功する。マーカーの並びだけの失敗は読み直し、EOIの無いものは失敗にする。[server.md §12.8](server.md)）。幅・高さの上限（既定16384）は外し、画素数で見る |
| `image` 0.25.10（`default-features = false`、`png`・`tiff`） | PNG・TIFFのデコード | PoCは`jpeg`のみだった。本番コピーに拡張子が`.png`の行が5件あるので`png`、PythonのEXTSに`.tif`/`.tiff`があるので`tiff`を入れた（本番コピーには0件）。miharashiと同じ版 |
| `fast_image_resize` 6.1.0 | 縮小（CatmullRom） | `image`の`resize`より約14倍速い。**バイナリの約3.4 MiBを占める**（§8.3） |
| `jpeg-encoder` 0.7.1 | JPEGのエンコード（4:2:0、品質85） | ハフマン表の最適化は付けない（`zune-jpeg`が誤読する） |
| `moxcms` 0.8.1 | ICCからsRGBへの変換 | `image`が引くのと同じ版（`cargo tree -d`に二重で出ない） |
| `zune-heic` 0.5.0 | HEICのデコード | 純Rust。libheifと出力がバイト単位で一致（PoC）。`zune-core`は`zune_heic::zune_core`で足りるので直接依存にしない |
| `mimalloc` 0.1.48（`[target.'cfg(target_env = "musl")'.dependencies]`） | muslのときだけグローバルアロケータ | Cのソースを`cc`でビルドする（`musl-tools`が要る）。glibcのビルドには入らない |

PoCの`kamadak-exif`は**使わない**: PillowのEXIFの読み方（ASCIIの末尾のNULを1つだけ取る、同じタグは後勝ち、`36867 or 36868 or 306`の連鎖、`S`/`W`だけ負）を境界まで合わせるため、必要なタグだけを自前で読む（約250行、unsafeなし、範囲外は黙って捨てる。[server.md §12.3](server.md)）。`cargo tree -d`に出る二重の版は`miniz_oxide`（0.8.9と0.9.1。`flate2`と`png`が別々に引く）だけで、`getrandom`・`syn`は以前から。

### 8.2 サイズの変化

| | strip前 | strip後 |
| --- | --- | --- |
| T2.1の終わり（基準。`57714f2`、同じ手順で作り直した） | 9,145,120 B（約8.7 MiB） | 7,590,648 B（約7.2 MiB） |
| T2.2の画像処理を入れた後（最後のコミット） | 15,812,376 B（約15.1 MiB） | 13,491,064 B（約12.9 MiB） |
| 差 | +6,667,256 B（約+6.4 MiB、+73%） | +5,900,416 B（約+5.6 MiB、+78%） |

`static-pie linked`・`ldd`は`statically linked`のまま。リリースプロファイルは既定のまま（LTO・`opt-level="s"`・`strip`は未設定）。

### 8.3 増えた分の内訳

strip前のバイナリのシンボルを`nm -C -S`でcrate別に合計した目安（シンボルの合計は約11.1 MiB）。画像処理の依存の分だけ抜き出す。

| crate | 目安 |
| --- | --- |
| `fast_image_resize` | 約3.3 MiB（SIMD実装を画素の型・フィルタごとに持つ。使うのはU8とU8x3だけだが、単独のfeatureで絞れるのはU8x4だけ） |
| `zune_heic` | 約0.5 MiB |
| `zune_jpeg` | 約0.2 MiB |
| `moxcms` | 約0.2 MiB（`pxfm`約0.1 MiBを含まない） |
| `jpeg_encoder` | 約0.2 MiB |
| `tiff` | 約0.1 MiB |

`fast_image_resize`が全体の約3割で突出している。置き場所が家庭のサーバー1台でバイナリのサイズが問題にならないので、**今は何もしない**。必要になったら、`[profile.release.package.fast_image_resize] opt-level = "s"`（縮小は約34 ms／枚で、全体の15%）や、`lto = "thin"`・`strip = true`（stripだけで約1.7 MiB減る）を試す。

## 9. ローカル写真の索引の依存（P2 / T2.3）

`local_derivatives`のファイル名`local_<md5(path)[:16]>.jpg`（現行の`hashlib.md5`）を作るため、**`md-5` 0.11.0を足した**（RustCryptoの純Rust実装。`digest`・`crypto-common`・`block-buffer`・`hybrid-array`・`typenum`・`const-oid`が一緒に入る。いずれもCのビルドは無い）。暗号の用途ではなく、既存の名前との互換のためだけ（`ring`にMD5は無い）。

| | strip前 | strip後 |
| --- | --- | --- |
| T2.2の画像処理を入れた後（§8.2。基準） | 15,812,376 B（約15.1 MiB） | 13,491,064 B（約12.9 MiB） |
| T2.3のローカル索引・`organize`・`md-5`を入れた後 | 16,003,320 B（約15.3 MiB） | 13,638,584 B（約13.0 MiB） |
| 差 | +190,944 B（約+0.2 MiB） | +147,520 B（約+0.1 MiB） |

`static-pie linked`・`ldd`は`statically linked`のまま。実装の説明は[server.md §13](server.md)。
