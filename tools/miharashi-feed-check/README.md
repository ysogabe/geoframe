# miharashi-feed-check

miharashi（キオスクの表示アプリ）の**取得と読み込みのコード**を、画面（DRM/KMS）なしで Rust 版 geoframe に向けて動かす確認用のハーネス（T1.5、[受入記録](../../docs/rust-migration/p1-acceptance.md) §4）。

- 秘密を含まない。接続先は引数・設定の例のプレースホルダー（`192.0.2.10` は文書用のアドレス）で、実際の値は Git の外に置く。
- miharashi 本体は**読むだけ**で変更しない（ビルドの出力先も miharashi の外に逃がす）。
- このリポジトリのワークスペースには入れない（miharashi のソースに依存するため）。CI ではビルドしない。

## 中身

| ファイル | 役割 |
| --- | --- |
| `src/main.rs` | miharashi の`photos.rs`・`album.rs`を`#[path]`でそのまま取り込み、`Feed::spawn`を画面なしで動かす。`photos.rs`が参照する画面側の型だけ最小の代役 |
| `setup.sh` | `MIHARASHI_DIR`（miharashi のチェックアウト）へのリンク`vendor/miharashi`と、依存の版をそろえる`Cargo.lock`を用意する |
| `repeat-png.sh` | 実バイナリの`miharashi png --renderer cpu`を N 回くり返し、成功を数える |
| `miharashi-check.toml.example` | `png`用の最小の設定（`[geoframe] url`だけ） |

## 1. `Feed`の継続取得（定常と、停止・再開）

```sh
export MIHARASHI_DIR=~/work/miharashi          # miharashi のチェックアウト（読むだけ）
tools/miharashi-feed-check/setup.sh
cd tools/miharashi-feed-check
CARGO_TARGET_DIR=/tmp/miharashi-feed-check-target cargo build --release

BIN=/tmp/miharashi-feed-check-target/release/miharashi-feed-check
URL=http://<検証環境のホスト>:8080

# 定常: 200 枚（写真かカードが届くたびに next() を呼ぶ）。取得・デコードの失敗は標準エラーに "photo: …" で出て、ツールが数えて終了コードに反映する
$BIN --url $URL --count 200 2>feed.err; echo "rc=$?"; grep -c '^photo:' feed.err

# 停止と再開: 10 秒後に停止コマンド、その 8 秒後に再開コマンドを実行し、300 枚まで読む。
# 止めている間は Retrying（指数バックオフ）、戻すと Online が 1 回届いて自動で続きを読む。キー操作は要らない
$BIN --url $URL --count 300 --stop-after 10 --resume-after 8 \
  --stop-cmd  "ssh <検証環境> 'sudo systemctl stop geoframe-rs'" \
  --start-cmd "ssh <検証環境> 'sudo systemctl start geoframe-rs'" 2>feed.err
```

停止コマンドを実行したあとは、件数に届いても時間切れになっても、終了前に必ず再開コマンドを実行する（`--resume-after`より前に終わっても止めたままにしない）。コマンドの失敗は終了コード1になる。

出力は件数だけ（写真・場所・タイトルは出さない）: 読み込んだ写真とカードの枚数、`Retrying`・`Online`の回数、最初の`Retrying`から次の写真まで（再開コマンドの実行を含む）。

終了コード 0 の条件（満たさなければ 1。引数の誤りは 2）:

- 目標の枚数に届いた。
- 取得・デコードの失敗が 0 件。miharashi が標準エラーに出す`photo: …`の行（ツールが自分を子プロセスとして起こして数え、行はそのまま流す）と、停止から復帰までの区間の外で届いた`Retrying`（一覧や写真を3回続けて読めず再接続に入った）の合計。1件でもあれば失敗。miharashi が別の写真へ飛ばして先へ進む1〜2回の失敗は、イベントにも標準エラーにも出ないので数えられない。
- 停止・再開を指定したときは、さらに、停止と再開の両コマンドが実行されて成功し、`Retrying`と、再開後の`Online`が観測され、復帰後に写真を受信したこと。停止の前に件数へ届いた、復帰を観測する前に終わった場合は失敗（`--count`を増やす）。

`photos: 対象 N 枚 …`は、miharashi の品質フィルタを通った表示対象の数（`/api/v1/photos`の件数より少ない）。

## 2. `png --renderer cpu`のくり返し

実クライアントの`Catalog::open`（`GET /api/v1/photos`）と`current_photo`（写真1枚の取得・デコード・合成）を、GPU・DRM を開かずに動かして PNG を書く。

```sh
# miharashi のバイナリ（リポジトリは変更しない。出力先を外へ）
cargo build --release -p miharashi-app --manifest-path "$MIHARASHI_DIR/Cargo.toml" --target-dir /tmp/miharashi-target

# 設定を Git の外へ写し、url を検証環境の Rust 版に書き換える
cp tools/miharashi-feed-check/miharashi-check.toml.example /tmp/miharashi-check.toml
$EDITOR /tmp/miharashi-check.toml

tools/miharashi-feed-check/repeat-png.sh /tmp/miharashi-target/release/miharashi /tmp/miharashi-check.toml 60
```

`--config`を明示するので`/etc/miharashi`を拾わず、`STATE_DIRECTORY`も無いので表示履歴も書かない。**本番の`miharashi.service`・`/etc/miharashi`には触れない**。

## 注意

- 向ける先は検証環境の Rust 版だけ。本番機の geoframe に向けて長時間回さない。
- `--stop-cmd`は`sh -c`でそのまま実行する。検証環境のサービスを止めるコマンドだけを渡すこと。
