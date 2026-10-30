# 本番切替の手順書（P5 / T5.3）

本番の geoframe を Python 版（`geoframe-server`）から Rust 版（`geoframe-rs`）へ切り替え、問題が出たら戻す手順。この文書だけを見て、上から順に実行できるようにしてある。

- 要求・設計: [p5-prd.md](p5-prd.md)・[p5-design.md](p5-design.md)
- 使う道具: [`rust-deploy.sh`](../../deploy/rust-deploy.sh)・[`deploy-pull.sh`](https://github.com/ysogabe/geoframe-ambient-display/blob/a804623029ad67d415692d08ea3182fd765eb2a4/deploy/deploy-pull.sh)・[`geoframe-admin.sh`](../../deploy/geoframe-admin.sh)・[`geoframe-rs.service`](../../deploy/geoframe-rs.service)・[`geoframe-server.service`](https://github.com/ysogabe/geoframe-ambient-display/blob/a804623029ad67d415692d08ea3182fd765eb2a4/deploy/geoframe-server.service)・[`backup-catalog.sh`](../../deploy/backup-catalog.sh)・[deploy/README.md §7](../../deploy/README.md)
- 検証環境（lxc-geoframe）の演習（T5.4）でも、この手順書を使う。本番と違う所は「**演習では**」で注記する（[§0.6](#06-演習では本番との違い)にまとめた）。演習に固有の確認（省く節・置き換える節・シャドー比較・`Conflicts=` の試験・元に戻す手順）は [§15 演習（T5.4）のチェックリスト](#15-演習t54のチェックリスト)にまとめた。

**この手順書のコマンドは、本番機で、ユーザーの立ち会いのもと、段ごとの許可を得てからだけ実行する。** 手順書を書く・読む・レビューする作業では実行しない。

## 流れの早見表

| 章 | 内容 | 本番を変えるか |
| --- | --- | --- |
| 1 | 準備（前日まで） | 変える（`/opt/geoframe`の更新・設定のファイル・バイナリを置く）。Pythonは動き続ける（`/opt/geoframe`の更新で一度再起動する） |
| 2 | 切替の直前の検査と控え | 変えない（作業用の置き場に書くだけ） |
| 3 | Python を止める | **ここから配信が止まる** |
| 4 | 保全（DB のバックアップと一次データの tar） | 変えない（バックアップの置き場に書く） |
| 5 | Rust を入れる | 変える（`rust-deploy.sh --production`） |
| 6 | 確かめる | **配信が戻る**。タイマーを戻す |
| 7 | AI の書き込み系の操作 | サービスを一時的に止める |
| 8 | Python へ戻す | 戻す |
| 9 | 切り替え直す | 戻したあとにもう一度 Rust にする |
| 10 | 復元（最後の手段） | DB を置き換える |
| 11 | 切替のあとの数日 | 確認だけ |
| 12 | クラウド AI の有効化（数日後） | AI 設定を変えて再起動 |
| 13 | 戻しの猶予期間と片付け | 片付ける（ユーザーの許可で） |
| 14 | 記録 | `p5-acceptance.md` に書く |
| 15 | 演習（T5.4）のチェックリスト | 検証環境だけ |

## 0. 前提と役割

### 0.1 役割

- **ユーザー（管理者）**: 切替の日時を決め、各段の前に許可を出し、立ち会う。止める判断（[§0.4](#04-時間の目安と止める判断)）の最後の決定をする。切替のあと、miharashi の表示と管理画面を確かめる。
- **管理者（Claude）**: この手順書のコマンドを実行し、結果（件数・ハッシュ・時間・終了コード）をユーザーに報告する。**各段の前にユーザーの許可を取り、許可なしに次の段へ進まない。**
- 立ち会いは、本番の切替（§1の更新・§3〜§6）と、Python へ戻す（§8）・切り替え直す（§9）・復元（§10）・クラウド AI の有効化（§12）・片付け（§13）で必要。§11 の確認は読み取りだけなので、許可は包括的でよい。

### 0.2 段と許可

停止時間を延ばさないために、**§3 を始める前に §3〜§5 のコマンドを画面に出して**おき、許可は短い言葉（「3 を始める」など）で取る。ユーザーが事前にまとめて許可した範囲は、その旨を `p5-acceptance.md` に記録する。

| 段 | 許可を取るとき | 許可が要る理由 |
| --- | --- | --- |
| §1 の各項 | 項ごと（1.1・1.5・1.6・1.8 は本番を変える） | Python の再起動・設定のファイルの追加と権限・バイナリの配置 |
| §2 | 切替の日の始めに 1 回 | 読み取りと控えだけ |
| §3 | 始める前 | 配信が止まる |
| §4 | §3 の確認が済んだとき | 保全を取る |
| §5 | §4 の確認が済んだとき | Rust を入れる |
| §6 | 結果を報告したあと、続けるか戻すかの決定 | 止める判断 |
| §7〜§10・§12・§13 | 実行の前に毎回 | サービスの停止・設定の変更・削除を伴う |

### 0.3 この機体と、触るもの

**この機体は本番機である**（開発機と同じ機体）。切替の前は、本番の `catalog.db` に直接つながない（読むのは §4 のバックアップのファイルだけ）。切替のあとの確認で読むのは、サービスと同じユーザー（`geoframe`）での読み取りだけ。

| 触るもの | どう触るか | 戻し方 |
| --- | --- | --- |
| `/opt/geoframe`（checkout） | §1.1 で P5a のマージのコミットへ更新（`deploy-pull.sh`） | `deploy-pull.sh` が出す `git reset --hard <旧SHA>` |
| `/usr/local/bin/geoframe`（`.prev`・`.failed`） | §5 で `rust-deploy.sh` が入れる | `rust-deploy.sh` が自動で戻す |
| `/usr/local/lib/systemd/system/geoframe-rs.service`（`.prev`） | §5 で `rust-deploy.sh` が入れる | 同上 |
| `/usr/local/lib/systemd/system/geoframe-server.service` | §3 で Python 版の本体を `/etc/systemd/system/` から**移す** | §3 の「止まったとき」・§8 |
| `/etc/systemd/system/geoframe-server.service.d/p5-rollback.conf` | §3 で置く（drop-in。Python の AI を止める） | §13 まで残す |
| `/etc/systemd/system/geoframe-server.service`（mask のリンク） | §3 で mask、§8 で unmask | `systemctl unmask` |
| `/etc/default/geoframe`・`geoframe-rs`・`geoframe-py-rollback` | §1.5 で整える（`0600 root:root`） | §4 の tar |
| AI 設定の JSON・AI の鍵 | §1.6 で確かめる（`0640 root:geoframe`・鍵は `0600 geoframe`） | §4 の tar |
| `/etc/geoframe/checkout-pinned` | §3 で置く。**猶予期間が終わるまで外さない** | §13 |
| `/var/lib/geoframe`（DATA_DIR） | Python が止まったあと、Rust（`geoframe`ユーザー）が書く | §4 のバックアップ・§10 |
| `/var/backups/geoframe` | §4 で `p5-precutover-<時刻>.db.gz` を足す | 消さない（P5 の記録が終わるまで） |
| `geoframe-backup.timer` | §4.1 で止める。**止めたら、どの出口（成功・中断・Python へ戻す・復元）でも [§4.5](#45-バックアップのタイマーを戻す共通の出口) で戻す** | §4.5 |
| 作業用の置き場（`/root/p5-cutover`） | 控え・tar・バイナリ・補助スクリプト（`0700 root`。リポジトリの外） | 記録のあと、ユーザーの許可で消す |

### 0.4 時間の目安と止める判断

時計は **§3.1 で Python を止めた時刻（T0）** から始まり、**§6.1 で `/api/v1/photos` が 200 になった時刻（T1）** で止まる。目標は **5 分以内**（[PRD](p5-prd.md)）。下の目安は T5.4 の演習で測った値（[p5-acceptance.md](p5-acceptance.md) §2。コマンドを 1 つのスクリプトで続けて打った場合。人が 1 行ずつ打つときは、その分を足す）。

| 段 | 目安（T5.4 の演習で測った値） |
| --- | --- |
| §3 Python を止める | 約 1 秒（コマンドの時間だけ。許可を取る時間は別） |
| §4 保全 | 約 40 秒（DB 約 85 MiB。ほとんどが `.backup` と `gzip -9`） |
| §5 Rust を入れる | 約 2 秒（`rust-deploy.sh --production`。起動から `/healthz` 200 まで 0.2〜0.4 秒） |
| T0〜T1 | 42〜43 秒（2 回） |

止める判断（ここでいう「戻す」は **§8 の手順**）:

- **配信が止まっている**（`/healthz` か `/api/v1/photos` が 200 にならない）: T0 から **10 分**で直らなければ戻す。
- **配信は続いているが機能に問題がある**（写真の欠け・管理操作の失敗・取り込みの失敗）: T1 から **30 分**のうちに原因と直し方が見えなければ戻す。
- **`rust-deploy.sh --production` の終了コードが 0 でない**（Python を止めたあとの §5 では、何も変えずに断った 2・4 も含めて、規則は一つ）: 終了コード 1・3 は待たずに**すぐ §8**。終了コード 2・4 は、**原因がすぐ直せて、T0 から 10 分の配信停止の期限内に再実行して 0 になる見込みがあるときだけ**、1 回再実行する。再実行も 0 でないとき、原因が分からないとき、直すのに配布物や設定の入れ直しが要るとき、期限に間に合わないときは**すぐ §8**（理由は [§5](#5-rust-を入れる)）。
- どちらにも当たらない迷いは、ユーザーに聞く。時間が過ぎていても、原因が見えていて直る見込みが数分ならユーザーが続けてよい（その判断を記録する）。

### 0.5 共通の約束と補助スクリプト

- **コマンドは root のシェル（`sudo -i`）で打つ**前提で書く。`/var/lib/geoframe` は `0750 geoframe` なので、`sudo` を付けてばらばらに打つと、glob（`catalog.db*`）とリダイレクト（`>`）が一般ユーザーのシェルで展開されて失敗する。ばらばらに打つなら `sudo sh -c '…'` に包む。サービスのユーザーで動かす所だけ `sudo -u geoframe` と書いてある。
- **環境変数 `GEOFRAME_RS_*` を持ったシェルでは `rust-deploy.sh --production` を動かさない**（持っていれば終了コード 4 で断られる）。確かめ方は §1.8（**名前だけ**を出す。`env | grep` は値まで出すので使わない）。
- **ログは秘密を除いて出す処理ではない**。`journalctl … | grep` や `cut -c1-200` は、行を選ぶ・長さを切るだけで、値を隠さない。特に **Python 版のログには写真のファイル名・パスが出る**（`[fetch]` の行など）。Python 版の journal は、**件数（`grep -c`）か固定の語の有無だけ**を画面に出し、生の行を安全な出力として扱わない。行の中身が要るとき（起動しない原因を調べるなど）は `$WORK` に保存して読み（`journalctl … > "$WORK/py-journal.txt"`。`0600 root`）、報告・`p5-acceptance.md` へは例外の型・ステータスコード・件数だけを写す。Rust 版のログは定型文だけを出す設計（段の名前・種類・件数・写真の `id`）なので、画面に出してよい。
- **「DB を開いているプロセスが無い」の確認は `dbopen.sh`（§1.4）を使う**（§4.2・§8.1・§10.1）。`fuser` があれば `fuser`、無ければ `/proc` の走査で調べる。**終了コード 0（開いているプロセスが無いと確認できた）のときだけ「無い」と扱う。** 1（ある）はもちろん、2（`fuser` も `/proc` も使えない・root でない・対象のファイルが無い・`fuser` が致命的なエラー・**生きているプロセスの fd を列挙できない／行き先を読めない**（権限など。消えたプロセス・fd だけを読み飛ばす））は「確認できなかった」であり、先へ進まない。`fuser: command not found` や空の出力を「無い」と読まない。
- **秘密と実値を画面から文書へ写さない**: ホスト名・件数・SHA・ファイル名・座標・地名・設定の値は、`p5-acceptance.md` にも書かない（書くもの・書かないものは §14）。設定のファイルは、モード・所有者・**名前だけ**を確かめ、中身は読まない。
- 変数（値は自分で埋める。ここへ書き戻さない）:

```sh
WORK=/root/p5-cutover                 # 作業用の置き場（§1.4 で作る。リポジトリの外・0700 root）
HOST='<本番のホスト名>'                # hostname の出力
COMMIT='<P5aのマージのコミット（40桁）>'
SHA='<演習で記録したバイナリのsha256（64桁）>'
BIN="$WORK/geoframe"                  # 切替に使うバイナリ（§1.8 で置く）
PYAPP=/opt/geoframe/app               # Python 版のコードと venv（$PYAPP/.venv）。演習では /opt/geoframe-pyref/app
BASE=http://127.0.0.1:8080
```

補助スクリプトは §1.4 で `$WORK` に作る（Python は標準ライブラリだけで `python3 -I` で動かす。シェルは `dbopen.sh`・`dumpsha.sh`・`drafts_sha.sh`）。出すのは件数・固定のキー名だけで、写真の値（ID ごとの座標・下書きのパス・ファイル名）は画面に出さず、`$WORK` のファイルにだけ置く。

### 0.6 演習では（本番との違い）

演習の配置の詳細は [p5-design.md §演習](p5-design.md)。手順書では差だけを書く。

| 項目 | 本番 | 演習では |
| --- | --- | --- |
| ホスト名 | `<本番のホスト名>` | 検証環境の `hostname` |
| 実行する checkout | `/opt/geoframe`（P5a のマージのコミット） | `/opt/geoframe-pyref`（P5a のコミットにしたもの）。コマンドの `/opt/geoframe/deploy/…` はこの checkout の `deploy/…` に読み替える。`--expect-commit` もその HEAD |
| 検証環境の印 `/etc/geoframe/staging` | 無い | **演習のあいだだけ退避**する（`--production` は印があると断る）。終わったら戻す |
| `/etc/systemd/system/geoframe-rs.service` の古い本体 | 無い | 退避する（新しい置き場所より優先されるので `rust-deploy.sh` が断る） |
| Python 版 unit | 本番の本体を `/etc/systemd/system/` から移す | 比較用 Python（`/opt/geoframe-pyref` の checkout と venv）を、`geoframe-server.service` の名前・同じ DATA_DIR・8080 で `/usr/local/lib/systemd/system/` に一時的に置く（§3.2 の「移す」は省く） |
| `deploy-pull.sh`（§1.1） | 使う | 使わない（`/opt/geoframe` 固定の道具）。checkout は手で更新する |
| `fuser`（psmisc） | 無くてよい（`dbopen.sh` が `/proc` で代わる） | 同じ。有無と、`dbopen.sh` がどちらの経路で判定したかを記録する |
| 設定のファイルの値 | 本番の値 | `GEOFRAME_DRIVE_KEY` は存在しないパス（取り込みは Drive へ出ずに終わる）。AI 設定はローカルの用途だけ。Python 役は AI 無効・鍵なし |
| データ | 本番 | 本番のバックアップから作った新しいコピー（鍵は入れない） |
| §6 の miharashi・管理画面 | 実機 | 検証環境の方法（`tools/miharashi-feed-check`・`compare_with_python.py` など）。結果は `p5-acceptance.md` |
| 時間 | 本番の T0〜T1 を記録 | `--wait-secs` の値と、停止から配信の再開までを**測る** |
| 立ち会い・許可 | ユーザー | 演習の開始の許可だけでよい |
| 終わったあと | 猶予期間 | 一時的な Python 版 unit・drop-in・設定のファイル・印の退避を元に戻す（設計 §演習 9。[§15.6](#156-元に戻す流れ-9)） |

## 1. 準備（前日まで）

目的: 切替の当日に、止まっている時間を短くするためのものをすべて置き、検査を済ませる。

### 1.1 `/opt/geoframe` を P5a へ更新する

目的: 切替に使う道具（`rust-deploy.sh`・`geoframe-admin.sh`・unit・`deploy-pull.sh` の拒否）を、レビュー済みの P5a のマージのコミットから本番へ入れる。**この時点では drop-in を置かない**（`geoframe-server.service` は `Restart=always` なので、切替の前に再起動が起きると drop-in が効いて本番の AI が止まる）。

**前提: `origin/main` が P5a のマージのコミットであること**（P5b を含んでいないこと）。P5b は Python を消すので、含んだ `main` を `deploy-pull.sh` で取り込むと本番の Python が壊れる。

```sh
cd /opt/geoframe
git status --short                 # 追跡下の変更が無いこと（?? の未追跡は app/.venv などで正常）
git rev-parse HEAD                 # 更新前の HEAD を控える
sudo -u geoframe /opt/geoframe/deploy/backup-catalog.sh   # 更新の前に手動でバックアップ（deploy-pull.sh の注意）
./deploy/deploy-pull.sh --dry-run  # 取り込まれるコミットを見る。サービスには触れない
```

- 確かめること: 「origin/main :」の SHA が P5a のマージのコミット（`$COMMIT`）と**一致**する。取り込まれるコミットの一覧が想定どおり。
- 一致したら（ユーザーの許可を得て）:

```sh
./deploy/deploy-pull.sh
git rev-parse HEAD                              # = $COMMIT
systemctl is-active geoframe-server             # active
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/admin/status   # 200
ls /etc/systemd/system/geoframe-server.service.d 2>&1               # 無い（drop-in はまだ置かない）
```

- 失敗したら: `deploy-pull.sh` が「サービスは停止したままの可能性がある。元に戻すには: `git reset --hard <旧SHA>`・`systemctl start geoframe-server`」と出す。そのとおりにして Python を起動し、原因をユーザーに報告する。終了コード **3** は「checkout の固定の印か `geoframe-rs` の状態で断った」で、この段階では起きないはず（起きたら進まず調べる）。`origin/main` が `$COMMIT` と違う・P5b を含むときは `deploy-pull.sh` を使わず、ユーザーに報告して指示を受ける。
- 更新で Python が数十秒止まる。ユーザーと時刻を決める。

> 演習では: 省く（`/opt/geoframe-pyref` を手で P5a のコミットにする）。

### 1.2 演習のコミットと P5a のマージのコミットに差が無いことを確かめる

目的: 演習で確かめたものと違うコード・スクリプトを本番で使わない。バイナリは sha256 で縛るが、`rust-deploy.sh`・unit・`geoframe-admin.sh` は checkout のものがそのまま使われる。

```sh
DRILL='<演習のコミット（p5-acceptance.md に記録したもの）>'
git -C /opt/geoframe cat-file -e "$DRILL^{commit}" 2>&1 || git -C /opt/geoframe fetch origin "$DRILL"
git -C /opt/geoframe diff --stat "$DRILL" "$COMMIT" -- crates Cargo.toml Cargo.lock app deploy
```

- 確かめること: **出力が空**（`docs/` だけの差は対象外）。
- 差があるとき: 進まない。差の中身をユーザーに見せ、演習をやり直すか、差が無害と判断するか（記録して）決めてもらう。
- 本番の checkout に演習のコミットが取れない（`fetch` が断られる）ときは、開発機で同じ差分を取り、結果（差の有無）を記録する。

### 1.3 道具の有無

```sh
command -v sqlite3 curl python3 tar gzip sha256sum file xargs readlink
command -v fuser || echo "fuser なし（dbopen.sh が /proc の走査で代わる。導入しなくてよい）"
sqlite3 --version
test -x "$PYAPP/.venv/bin/python" && echo "Python 版の venv あり"
```

- 確かめること: `fuser` 以外はすべて出る。**`fuser`（psmisc）は無くてよい**: `dbopen.sh`（§1.4）が `/proc` の走査で代わる（本番の依存を足さない）。どちらの経路で判定したかは `dbopen.sh` の出力に出る。`jq` は使わない（補助スクリプトが `python3 -I`）。
- 失敗したら: `fuser` 以外の道具が無ければ、その段の前に導入の許可を取る（本番の依存を勝手に足さない）。`$PYAPP/.venv` が無いと §8.1 の `.cache.json` の確認ができない（Python 版は venv がなければ戻せない）ので、切替の前に直す。

### 1.4 作業用の置き場と補助スクリプト

```sh
install -d -m 0700 -o root -g root "$WORK"
cd "$WORK"

cat > ids.py <<'PY'
# 使い方: python3 -I ids.py <フィードのJSON>   写真の ID を 1 行に 1 つ出す
import json, sys
for p in json.load(open(sys.argv[1])):
    print(p["id"])
PY

cat > extra.py <<'PY'
# 使い方: python3 -I extra.py <Rust のフィードの JSON> <増えた ID の一覧> <展開した切替前のバックアップ> [<説明できない ID の出力先>]
# Rust のフィードにだけある写真（増えた分）が、既知の差（p4-acceptance.md §7.3）などで説明できるかを、
# 切替の前の DB（バックアップを展開したもの。§6.8）の事実と照らして調べる。出すのは件数だけ。
# 分け方（上から順に当てる）:
#   取り込み … 切替の前の DB に無い、または表示できる状態でなかった（display か位置が無い）。Rust の取り込みで増えた。
#              ただし切替の前に hidden=1 だった写真は、この分け方に入れない（下の「説明できない」）
#   既知の差 … 切替の前の DB で、旧グループ（photo_ai_groups: judged・同一場面）の推薦でない・お気に入りでない写真で、
#              そのグループの推薦写真（代表）がすべて「表示できる代表」でない（hidden・display・位置、特徴量の世代
#              （display・md5・mtime・SHA-256）、品質の表示条件（有効な photo_quality の行が exact の非推薦）のどれかで。
#              crates/geoframe/src/selection.rs の VALID_MEMBERS.shown と同じ条件）、
#              またはその写真自身の特徴量の世代が合わない（Rust は世代が合わない代表・メンバーを有効と見なさず、隠すのをやめる）
#   説明できない … 上のどちらでもない。切替の前に hidden=1 だった写真がフィードに出ているのもここ（切替の手順の中では
#              管理操作をしないので、解除は説明できない差。管理操作で解除したなら、その記録と照らす）。終了コード 1
#              （ID は出力先のファイルにだけ書く。画面には出さない）
import json, sqlite3, sys

feed = json.load(open(sys.argv[1]))
ids = {int(l) for l in open(sys.argv[2]) if l.strip()}
con = sqlite3.connect("file:%s?mode=ro&immutable=1" % sys.argv[3], uri=True)
con.row_factory = sqlite3.Row

def one(sql, *a):
    return con.execute(sql, a).fetchone()

def displayable(p):       # display と位置がある（hidden は見ない）
    return bool(p and p["display"] and p["lat"] is not None and p["lon"] is not None)

def shown(p):
    return displayable(p) and not p["hidden"]

def generation_ok(pid, p):
    af = one("SELECT display, source_md5, source_mtime, content_sha256 FROM photo_ai_features WHERE photo_id=?", pid)
    qf = one("SELECT display, source_md5, source_mtime, content_sha256, error FROM photo_quality_features WHERE photo_id=?", pid)
    if not af or not qf or qf["error"] != "":
        return False
    for f in (af, qf):
        if f["display"] != p["display"] or f["source_md5"] != p["md5"] or f["source_mtime"] != p["mtime"]:
            return False
    return af["content_sha256"] == qf["content_sha256"]

def quality_hidden(pid, p):   # 今の世代の photo_quality の行が exact の非推薦なら、フィードに出ない（selection.rs と同じ）
    q = one("SELECT group_kind, recommended FROM photo_quality WHERE photo_id=? AND display=? AND source_md5 IS ? AND source_mtime IS ?",
            pid, p["display"], p["md5"], p["mtime"])
    return bool(q and q["group_kind"] == "exact" and q["recommended"] != 1)

def usable_representative(pid):
    p = one("SELECT display, md5, mtime, lat, lon, hidden FROM photos WHERE id=?", pid)
    return shown(p) and generation_ok(pid, p) and not quality_hidden(pid, p)

rows = [p for p in feed if p["id"] in ids]
def groups(key):          # 空文字・None は群ではない
    return len({p.get(key) for p in rows if p.get(key) not in (None, "")})

fetched = known = 0
known_groups = set()
unexplained = []
for i in sorted(ids):
    p = one("SELECT display, md5, mtime, lat, lon, hidden, favorite FROM photos WHERE id=?", i)
    if p is not None and p["hidden"]:      # 切替の前に隠れていた写真の再出現は、取り込みでは説明できない
        unexplained.append(i)
        continue
    if not displayable(p):                 # DB に無い、または display か位置がまだ無かった
        fetched += 1
        continue
    explained = False
    if not p["favorite"]:
        for m in con.execute("""SELECT m.group_id g FROM photo_ai_group_members m JOIN photo_ai_groups g ON g.group_id=m.group_id
                                 WHERE m.photo_id=? AND m.recommended=0 AND g.status='judged' AND g.same_scene=1""", (i,)):
            reps = [r["photo_id"] for r in con.execute(
                "SELECT photo_id FROM photo_ai_group_members WHERE group_id=? AND recommended=1", (m["g"],))]
            if not generation_ok(i, p) or not any(usable_representative(r) for r in reps):
                explained = True
                known_groups.add(m["g"])
    if explained:
        known += 1
    else:
        unexplained.append(i)

print("増えた", len(ids), "件（フィードに", len(rows), "件）: 取り込み", fetched, "既知の差", known,
      "（旧グループ", len(known_groups), "群）説明できない", len(unexplained))
print("ai_group（空を除く）", groups("ai_group"), "群 quality_group", groups("quality_group"), "群")
if unexplained and len(sys.argv) > 4:
    with open(sys.argv[4], "w") as out:
        out.write("".join("%d\n" % i for i in unexplained))
sys.exit(1 if unexplained else 0)
PY

cat > status_counts.py <<'PY'
# 使い方: curl -fsS $BASE/api/admin/status | python3 -I status_counts.py
# db の整数の値（件数）だけを「パス=値」で出す。文字列は clue_pause だけ。AI の追加キーは有無だけ。
import json, sys
def walk(v, path):
    if isinstance(v, bool):
        return
    if isinstance(v, int):
        print(f"{path}={v}")
    elif isinstance(v, dict):
        for k in sorted(v):
            walk(v[k], f"{path}.{k}")
d = json.load(sys.stdin)
walk(d.get("db", {}), "db")
loc = d.get("db", {}).get("loc", {})
print("db.loc.clue_pause=" + (repr(loc["clue_pause"]) if "clue_pause" in loc else "(キー無し)"))
for k in ("ai_task_blocks", "ai_config_invalid", "ai_pauses", "ai_waits"):
    print(f"{k}={'あり' if k in d else 'なし'}")
PY

cat > check_files.py <<'PY'
# 使い方: python3 -I check_files.py <フィードのJSON> [<200 でない写真の ID の出力先>]
# フィードの file を全部 GET し、200 でない数だけを出す（ID は出力先のファイルにだけ書く）
import http.client, json, sys, urllib.parse
feed = json.load(open(sys.argv[1]))
conn = http.client.HTTPConnection("127.0.0.1", 8080, timeout=30)
bad = []
n = 0
for p in feed:
    f = p.get("file")
    if not isinstance(f, str):
        continue
    n += 1
    conn.request("GET", "/" + urllib.parse.quote(f.lstrip("/")))
    r = conn.getresponse()
    r.read()
    if r.status != 200:
        bad.append(p["id"])
print("確かめた", n, "200でない", len(bad))
if len(sys.argv) > 2:
    open(sys.argv[2], "w").write("".join("%s\n" % i for i in bad))
PY

cat > counts.sql <<'SQL'
SELECT 'photos', count(*) FROM photos;
SELECT 'manual', count(*) FROM photos WHERE loc_source='manual';
SELECT 'favorite', count(*) FROM photos WHERE favorite=1;
SELECT 'hidden', count(*) FROM photos WHERE hidden=1;
SELECT 'rotated', count(*) FROM photos WHERE rotation IS NOT NULL AND rotation<>0;
SELECT 'quality_rows', count(*) FROM photo_quality;
SELECT 'quality_groups', count(DISTINCT group_id) FROM photo_quality;
SELECT 'algorithm:' || algorithm, count(*) FROM photo_quality GROUP BY algorithm;
SELECT 'kind:' || group_kind, count(*) FROM photo_quality GROUP BY group_kind;
SQL

cat > state_snapshot.sql <<'SQL'
.headers off
.mode list
.separator "\t"
SELECT id, ifnull(loc_source,''), ifnull(lat,''), ifnull(lon,''), ifnull(favorite,0), ifnull(hidden,0), ifnull(rotation,0), ifnull(display,'')
  FROM photos
 WHERE loc_source='manual' OR favorite=1 OR hidden=1 OR (rotation IS NOT NULL AND rotation<>0)
 ORDER BY id;
SQL

cat > state_diff.py <<'PY'
# 使い方: python3 -I state_diff.py <前の .tsv> <後の .tsv>   ID ごとの行（手動の位置・お気に入り・除外・回転・display）を比べ、件数だけを出す。
# 値は画面に出さない（座標が入っている）。違いがあれば終了コード 1。
import sys
def load(path):
    d = {}
    for line in open(path, encoding="utf-8"):
        line = line.rstrip("\n")
        if line:
            d[line.split("\t", 1)[0]] = line
    return d
a, b = load(sys.argv[1]), load(sys.argv[2])
changed = sum(1 for k in a.keys() & b.keys() if a[k] != b[k])
only_a, only_b = len(a.keys() - b.keys()), len(b.keys() - a.keys())
print("前", len(a), "件 後", len(b), "件: 値が違う", changed, "前だけ", only_a, "後だけ", only_b)
sys.exit(1 if changed or only_a or only_b else 0)
PY

cat > quality_snapshot.sql <<'SQL'
.headers off
.mode list
.separator "\t"
-- P: 写真と、群の形成の入力（display・md5・mtime・taken_at）。F: 今の写真に合っている有効な特徴量（群の形成が使う SHA と pHash）。Q: 群の結果
SELECT 'P', id, ifnull(display,''), ifnull(md5,''), ifnull(mtime,''), ifnull(taken_at,'') FROM photos ORDER BY id;
SELECT 'F', f.photo_id, f.content_sha256, f.phash
  FROM photos p JOIN photo_quality_features f
    ON f.photo_id=p.id AND f.display=p.display AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
 WHERE p.display IS NOT NULL AND p.display!='' AND f.error=''
   AND f.content_sha256 IS NOT NULL AND f.phash IS NOT NULL
   AND f.sharpness IS NOT NULL AND f.luminance IS NOT NULL AND f.exposure_score IS NOT NULL
   AND f.black_clip IS NOT NULL AND f.white_clip IS NOT NULL AND f.contrast IS NOT NULL
 ORDER BY f.photo_id;
SELECT 'Q', photo_id, group_id, group_kind FROM photo_quality ORDER BY photo_id;
SQL

cat > quality_diff.py <<'PY'
# 使い方: python3 -I quality_diff.py <前の quality .tsv> <後の quality .tsv>
# 簡易品質の「グループの顔ぶれと kind」を、写真の変更・特徴量の変更と分けて比べる。出すのは件数だけ（値は画面に出さない）。
#   写真の変更 … photos の ID の増減（追加・削除）、同じ ID で display・md5・mtime・taken_at が変わったもの（更新）
#                taken_at は 3 分の窓の判定に使われるので、群の形成の入力として見る
#   特徴量の変更 … 今の写真に合っている有効な特徴量の行（SHA・pHash を使う行）の増減（追加・削除）と、同じ ID で SHA か pHash が変わったもの（更新）
#                  既存の写真の未解析分を Python が解析して有効な特徴量が増えるのは、写真が変わらなくても起こる正常な変化
#   群の比べ方 … group_id は振り直されうるので、顔ぶれ（写真の ID の集合）と kind で比べる。前後の群を、共通の写真でつないだ
#                まとまりに分け、前後で顔ぶれも kind も同じものを「変わらない」、そうでないものを「変わった」とする。
#                「変わった」まとまりが、写真か特徴量が変わった ID を含めば「説明できる」、含まなければ「説明できない」
# 写真の変更も特徴量の変更も 0 件なら、「変わらない」だけでなければならない（入力が同じなら顔ぶれ・kind は変わらない）。
# 終了コード 1 = 説明できない変化がある（入力が変わっていない群が変わった）。
import sys
def load(path):
    photos, feats, groups = {}, {}, {}
    for line in open(path, encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        if f[0] == "P":
            photos[f[1]] = tuple(f[2:6])
        elif f[0] == "F":
            feats[f[1]] = tuple(f[2:4])
        elif f[0] == "Q":
            groups.setdefault(f[2], [f[3], set()])[1].add(f[1])
    return photos, feats, [(k, frozenset(m)) for k, m in groups.values()]
(pa, fa, ga), (pb, fb, gb) = load(sys.argv[1]), load(sys.argv[2])
p_add, p_del = pb.keys() - pa.keys(), pa.keys() - pb.keys()
p_upd = {i for i in pa.keys() & pb.keys() if pa[i] != pb[i]}
f_add, f_del = fb.keys() - fa.keys(), fa.keys() - fb.keys()
f_upd = {i for i in fa.keys() & fb.keys() if fa[i] != fb[i]}
touched = p_add | p_del | p_upd | f_add | f_del | f_upd

parent = {}
def find(x):
    while parent.setdefault(x, x) != x:
        parent[x] = parent[parent[x]]
        x = parent[x]
    return x
for _, members in ga + gb:
    first = next(iter(members))
    for m in members:
        parent[find(m)] = find(first)
clusters = {}
for side, groups in (("a", ga), ("b", gb)):
    for kind, members in groups:
        clusters.setdefault(find(next(iter(members))), {"a": set(), "b": set(), "ids": set()})[side].add((kind, members))
        clusters[find(next(iter(members)))]["ids"] |= members
same = explained = unexplained = 0
for c in clusters.values():
    if c["a"] == c["b"]:
        same += 1
    elif c["ids"] & touched:
        explained += 1
    else:
        unexplained += 1
print("写真: 追加", len(p_add), "削除", len(p_del), "更新", len(p_upd),
      "/ 特徴量: 追加", len(f_add), "削除", len(f_del), "更新", len(f_upd),
      "/ quality_rows", sum(len(m) for _, m in ga), "→", sum(len(m) for _, m in gb),
      "/ グループ", len(ga), "→", len(gb),
      "/ 変わらない", same, "入力の変更に連なる変化", explained, "説明できない変化", unexplained)
sys.exit(1 if unexplained else 0)
PY

cat > cache_load.py <<'PY'
# 使い方: sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe GEOFRAME_AI_ENABLED=0 GEOFRAME_LOC_SUGGEST=0 \
#           $PYAPP/.venv/bin/python -I -B - $PYAPP < cache_load.py
# Python 版が起動時にやる読み方（store.load_cache → purge_cache_legacy）を、Python 版自身の関数で、メモリの中だけで行う。
# .cache.json には書かない。load_cache は読めないとき黙って {} にして、あとで空の内容で上書きするので、
# ファイルを別に JSON として読んだ件数と照らす。出すのは件数だけ。読めない・件数が合わないときは終了コード 1。
import json, sys
sys.path.insert(0, sys.argv[1])
from geoframe import config, store
try:
    raw = json.loads(config.CACHE_FILE.read_text(encoding="utf-8"))
except FileNotFoundError:
    print(".cache.json: 無い（Python は空で始める）"); sys.exit(0)
except Exception as e:
    print(".cache.json: JSON として読めない（", type(e).__name__, "）。Python は空で始めて上書きする"); sys.exit(1)
if not isinstance(raw, dict):
    print(".cache.json: 辞書でない。Python は使えない"); sys.exit(1)
store.load_cache()
loaded = len(store._cache)
legacy = store.purge_cache_legacy()      # メモリの上だけ。起動時に捨てられるキー（現役でない接頭辞）の数
print("file_keys=%d python_loaded=%d python_will_drop=%d python_keeps=%d" % (len(raw), loaded, legacy, loaded - legacy))
sys.exit(0 if loaded == len(raw) else 1)
PY

cat > dbopen.sh <<'SH'
#!/bin/bash
# 使い方: dbopen.sh   /var/lib/geoframe/catalog.db*（-wal・-shm を含む）を開いているプロセスの有無を調べる。root で動かす
# 終了コード: 0 開いているプロセスは無いと確認できた / 1 ある / 2 確認できなかった（「無い」と扱わない）
# fuser があれば fuser、無ければ /proc/<pid>/fd の走査（生きているプロセスの fd を列挙・読めなければ 2。消えたものだけ読み飛ばす）。
set -u
shopt -s nullglob
[ "$(id -u)" = 0 ] || { echo "確認できない: root で実行すること" >&2; exit 2; }
files=(/var/lib/geoframe/catalog.db*)
[ "${#files[@]}" -gt 0 ] || { echo "確認できない: 対象のファイルが無い" >&2; exit 2; }
if command -v fuser >/dev/null 2>&1; then
    out=$(fuser -v "${files[@]}" 2>&1); rc=$?
    if [ "$rc" = 0 ]; then echo "開いているプロセスがある（fuser）:"; echo "$out"; exit 1; fi
    # fuser は「プロセスが無い」も「致命的なエラー」も rc=1 を返す。出力が空のときだけ「無い」と読む
    if [ "$rc" = 1 ] && [ -z "$out" ]; then echo "開いているプロセスは無い（fuser）"; exit 0; fi
    echo "確認できない: fuser が rc=$rc でエラーを出した" >&2; exit 2
fi
# /proc の走査: 消えたプロセス・fd（ENOENT）だけを読み飛ばす。在るのに読めない（権限など）は「確認できなかった」
hits=(); unreadable=(); scanned=0
for d in /proc/[0-9]*; do
    pid=${d#/proc/}
    if ! list=$(ls -1A -- "$d/fd" 2>/dev/null); then
        [ -e "$d" ] && unreadable+=("$pid")      # プロセスは在るのに fd を列挙できない。消えたなら読み飛ばす
        continue
    fi
    scanned=$((scanned + 1))
    while IFS= read -r n; do
        [ -n "$n" ] || continue
        if t=$(readlink -- "$d/fd/$n" 2>/dev/null); then
            case "$t" in
                /var/lib/geoframe/catalog.db*) hits+=("$pid"); break ;;
            esac
        elif [ -L "$d/fd/$n" ]; then
            unreadable+=("$pid"); break           # fd は在るのに行き先を読めない。閉じた・消えたなら次へ
        fi
    done <<< "$list"
done
[ "$scanned" -gt 0 ] || { echo "確認できない: /proc を調べられない" >&2; exit 2; }
if [ "${#hits[@]}" -gt 0 ]; then echo "開いているプロセスがある（/proc）: pid ${hits[*]}"; exit 1; fi
if [ "${#unreadable[@]}" -gt 0 ]; then echo "確認できない: fd を読めないプロセスが ${#unreadable[@]} 個ある（権限など）。先頭の pid: ${unreadable[*]:0:5}" >&2; exit 2; fi
echo "開いているプロセスは無い（/proc の fd を走査）"
exit 0
SH

cat > dumpsha.sh <<'SH'
#!/bin/bash
# 使い方: dumpsha.sh <sqlite3 の URI> [実行ユーザー]   .dump の SHA-256 を 1 行で出す。
# 途中（sudo・sqlite3・sha256sum）のどれかが失敗したら何も出さずに非ゼロで終わる。出力が空のときのハッシュも断る。
set -euo pipefail
[ "$#" -ge 1 ] || { echo "使い方: dumpsha.sh <URI> [user]" >&2; exit 2; }
if [ -n "${2:-}" ]; then run=(sudo -u "$2" sqlite3); else run=(sqlite3); fi
sha=$("${run[@]}" "$1" .dump | sha256sum | cut -d' ' -f1)
if [ "${#sha}" != 64 ] || [ "$sha" = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 ]; then
    echo "NG: .dump が空" >&2; exit 1
fi
echo "$sha"
SH

cat > drafts_sha.sh <<'SH'
#!/bin/bash
# 使い方: drafts_sha.sh <出力ファイル>   旅行ブログの下書き（DATA_DIR/travel_blog/drafts）の全ファイルの sha256 を、パスの順に書く。
# 下書きが無ければ空のファイル。読めない・途中で失敗したら出力を作らずに非ゼロ。件数だけを画面に出す。
set -euo pipefail
trap 'rm -f -- "$1.part"' EXIT
d=/var/lib/geoframe/travel_blog/drafts
if [ -d "$d" ]; then
    ( cd "$d" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum ) > "$1.part"
else
    : > "$1.part"
fi
mv -- "$1.part" "$1"
echo "下書き: $(wc -l < "$1") ファイル"
SH

chmod 0600 *.py *.sql
chmod 0700 *.sh
```

- 確かめること: `ls -l "$WORK"` で `.py`・`.sql` が `-rw-------`、`.sh` が `-rwx------`（全部 root）。`counts.sql` と `state_snapshot.sql` は、`sudo -u geoframe sqlite3 '<URI>' < "$WORK/….sql"` の形で使う（リダイレクトは root のシェルが開くので、`geoframe` が `$WORK` を読めなくてよい）。`cache_load.py` も同じ理由で標準入力から渡す。
- 空打ち（読み取りだけ）: `sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe GEOFRAME_AI_ENABLED=0 GEOFRAME_LOC_SUGGEST=0 "$PYAPP/.venv/bin/python" -I -B - "$PYAPP" < "$WORK/cache_load.py"; echo "rc=$?"` が `rc=0`（本番の venv と `app/.env` の下で動くことを、当日の §8.2 より前に確かめる）。`"$WORK/dbopen.sh"` は Python が動いているあいだは rc=1（開いているプロセスがある）になるのが正常で、判定の経路の表示だけ見る。
- 書き換えない。`$WORK` のファイルを手で直したら、演習の記録に「何を直したか」を残す。

### 1.5 設定のファイルの分割とモード

目的: Rust の設定を Python と分け、`geoframe-rs.service` と `geoframe-admin.sh`（と、すべての CLI）が**同じファイルから同じ DATA_DIR**を読むようにする。値は文書に書かない（名前と意味だけ）。

| ファイル | モード・所有者 | 何を置くか |
| --- | --- | --- |
| `/etc/default/geoframe`（共通。今のもの） | `0600 root:root` | Python と Rust が同じ意味で読む値（`GEOFRAME_ALLOWED_HOSTS`・`GEOFRAME_AI_ENABLED` など）。今の中身のまま |
| `/etc/default/geoframe-rs`（Rust だけ。**無いと Rust は起動しない**） | `0600 root:root` | `GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`（Rust は解釈せず libc に渡す。無いと撮影時刻が 9 時間ずれる）・`GEOFRAME_INGEST=1`・`GEOFRAME_AI_CONFIG`（AI 設定の JSON のパス）・`GEOFRAME_TILE_UA`（連絡先入り。値はユーザーと確かめる）・`app/.env` から移す値 |
| `/etc/default/geoframe-py-rollback`（Python へ戻すときの AI の停止） | `0600 root:root` | `GEOFRAME_AI_ENABLED=0`・`GEOFRAME_LOC_SUGGEST=0`（PythonのAIをすべて止める） |

- `GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ` は、これまで `geoframe-server.service` の `Environment=` にあった。**同じ値**を `geoframe-rs` へ移す（`geoframe-rs.service` には `Environment=` が無い）。`geoframe-admin.sh` は `GEOFRAME_DATA_DIR` を**絶対パス（英数字と `_ . / -` だけ）**で読めないと動かない。
- **同じ名前を `geoframe` と `geoframe-rs` の両方に置かない**（`geoframe-rs` が後に読まれて勝つが、混乱のもと）。表の「`geoframe-rs` に置くもの」の名前（`GEOFRAME_TILE_UA` など）が、共通の `/etc/default/geoframe` に**すでにある**ときは、共通のまま置き、`geoframe-rs` には足さない（Rust も共通のファイルを読む。T5.4 の演習はこの形）。**例外: `GEOFRAME_DATA_DIR` は必ず `geoframe-rs` に置く**（`geoframe-admin.sh` は `geoframe-rs` の中のこの名前だけを読み、無ければ断る）。共通のファイルにもあるときは、共通の側から消して `geoframe-rs` へ移す（Python は `geoframe-server.service` の `Environment=` で読むので影響しない。消す前に、Python の unit の `Environment=` に同じ値があることを確かめる）。準備の最後に、§7 のラッパーで `ai-jobs status` が動くこと（DATA_DIR のパスが表示されること）を確かめる。
- Rust は `app/.env` を読まない。`app/.env` の値のうち Rust が読むもの（`docs/reference/configuration.md` の `GEOFRAME_*` で、Rust が対応するもの）を `geoframe-rs` へ移す。**名前だけ**を出して探す（値は出さない）:

```sh
sed -nE 's/^[[:space:]]*(export[[:space:]]+)?([A-Za-z_][A-Za-z0-9_]*)=.*/\2/p' /opt/geoframe/app/.env
```

  出た名前を、ユーザーと一つずつ「`geoframe-rs` へ移す／共通に既にある／Rust は読まない」に分ける。
- `geoframe-py-rollback` は、§3 の drop-in の中身。**このファイルを置いても、drop-in を置くまで Python は変わらない**。

置いたあとの確認（モード・所有者・名前だけ。中身は読まない）:

```sh
stat -c '%a %U:%G %n' /etc/default/geoframe /etc/default/geoframe-rs /etc/default/geoframe-py-rollback
sed -nE 's/^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*)=.*/\1/p' /etc/default/geoframe-rs     # 名前の一覧
grep -c '^GEOFRAME_INGEST=1$' /etc/default/geoframe-rs                                 # 1
comm -12 <(sed -nE 's/^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*)=.*/\1/p' /etc/default/geoframe | sort -u) \
         <(sed -nE 's/^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*)=.*/\1/p' /etc/default/geoframe-rs | sort -u)   # 空（重複なし）
cat /etc/default/geoframe-py-rollback                                                  # 2 行とも =0（値は秘密ではない）
```

- 確かめること: 3 つとも `600 root:root`。`geoframe-rs` に `GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`・`GEOFRAME_INGEST`・`GEOFRAME_AI_CONFIG` の名前がある。重複が無い。
- 失敗したら: モードが違うと `rust-deploy.sh --production` が断る（§1.10 で分かる）。直す: `chown root:root`・`chmod 0600`。

### 1.6 AI 設定と鍵の権限（ローカルの用途だけ）

目的: 切替の時点ではローカルの用途（画像 Embedding・検索語 Embedding・位置の手がかりの Decision）だけを有効にする。クラウドの用途は [§12](#12-クラウド-ai-の有効化数日後) まで入れない。

```sh
AI_CFG=$(sed -n 's/^GEOFRAME_AI_CONFIG=//p' /etc/default/geoframe-rs | tail -n 1 | tr -d "\"'")
stat -c '%a %U:%G' "$AI_CFG"                       # 640 root:geoframe
python3 -I - "$AI_CFG" <<'PY'
import json, sys
c = json.load(open(sys.argv[1]))
for name, t in sorted(c.get("tasks", {}).items()):
    print(name, t.get("provider"), "allow_cloud=" + str(t.get("allow_cloud")))
PY
```

- 確かめること: モード `640`・所有者 `root:geoframe`。用途の行がすべて `local`（`photo_judge`・`location_suggest` が**無い**か、`cloud` でない）。`embedding_space`／`embedding_contract` などの中身は [ai-config.md](ai-config.md)・[ai-embedding.md](ai-embedding.md) のとおりで、ここでは読まない。
- 鍵（Drive の鍵。クラウド AI を有効にするとき同じ鍵を使う）の所有者とモード: `KEY=$(sed -n 's/^GEOFRAME_DRIVE_KEY=//p' /etc/default/geoframe-rs | tail -n 1 | tr -d "\"'"); stat -c '%a %U:%G' "$KEY"` が **`600 geoframe:geoframe`**（実行ユーザーの所有で、他から読めない）。**root 所有やグループ可読は、クラウドで拒否される**（[ai-cloud.md](ai-cloud.md)。拒否は `Unauthorized` 停止になり、直しても自動では戻らず `release-pause` が要る）。中身は読まない。
- 失敗したら: モード・所有者を直すのは設定の変更なので、ユーザーの許可を得てから。**AI 設定の JSON と鍵は要件が違う**ので、同じ値で直さない:

| 対象 | 要件 | 直し方 | 直したあとの確認 |
| --- | --- | --- | --- |
| AI 設定の JSON（`$AI_CFG`） | `0640 root:geoframe`（サービスが読むだけ。書き換えられない） | `chown root:geoframe "$AI_CFG" && chmod 0640 "$AI_CFG"` | `stat -c '%a %U:%G' "$AI_CFG"` が `640 root:geoframe` |
| 鍵（`$KEY`。Drive の鍵・クラウド AI の鍵） | `0600 geoframe:geoframe`（実行ユーザーの所有で、他から読めない。**root 所有・グループ可読は不可**） | `chown geoframe:geoframe "$KEY" && chmod 0600 "$KEY"` | `stat -c '%a %U:%G' "$KEY"` が `600 geoframe:geoframe`。親のディレクトリを `geoframe` が辿れること（`namei -m "$KEY"`。出力はパスを含むので画面だけで見て、報告に写さない） |

  鍵を `root:geoframe`・`0640` にしてはいけない（クラウドで `Unauthorized` 停止になる）。鍵の中身は読まない・`cp` しない（所有者を変えるだけ）。Python 版が動いているあいだに鍵の所有者を変えるときは、Python も `geoframe` ユーザーで動いているので影響しない（`geoframe-server.service` の `User=geoframe`）が、直した直後に `systemctl is-active geoframe-server` と `/api/admin/status` が 200 であることを確かめる。

### 1.7 Decision が常駐していること

目的: 位置の手がかりの全件再判定（約 4 時間、本番機の CPU を使う）が、切替のあと止まらずに進むようにする。

```sh
curl -fsS -H 'X-Client-Name: geoframe' "<local_base_urlの値>/llm-decision/health"
```

- 確かめること: `"status":"ready"`（`image` を含む `modalities`・設定の `location_clue` と同じ `model`。契約は [ai-provider.md](ai-provider.md)）。同じ機体の入口の既定は `http://127.0.0.1:8100/llm-decision/health`。
- 毎朝のタイマー（`geoframe-loc-decision.timer`。Decision を動かすユーザーの**ユーザー unit**）は、Decision が常駐しているあいだは「人が起動済み」として何もしない。常駐をやめる運用に戻したときは [§11](#11-切替のあとの数日) の `clue_runnable` で動く。
- 失敗したら（`ready` でない）: 切替の前に Decision を直す（このリポジトリの外）。切替は延期か、Decision なしで進める（手がかりの再判定が進まないだけで、配信は変わらない）ことをユーザーが決める。

### 1.8 切替に使うバイナリを置く

```sh
compgen -e | grep '^GEOFRAME_RS_' || echo "GEOFRAME_RS_* なし（正常）"     # 名前だけを調べる（env | grep は値まで出す）
install -m 0755 -o root -g root '<受け取ったバイナリ>' "$BIN"
sha256sum "$BIN"                      # = $SHA（演習で記録した値）
file "$BIN"                           # statically linked / static-pie linked
ls -l /usr/local/bin/geoframe* 2>&1   # 無い（初回の配布）
```

- 確かめること: `sha256sum` が `$SHA` と**完全に一致**。静的リンク。`GEOFRAME_RS_*` が環境に無い（名前だけを調べる。値は読まない・写さない）。`/usr/local/bin/geoframe` が**無い**（あれば初回の配布ではない。理由をユーザーに聞く）。
- 失敗したら: バイナリが違う（sha256 が合わない）ときは進まない。演習で使ったものを受け取り直す。`rust-deploy.sh` も同じ検査をして断るが、ここで先に分かっていれば当日に止まらない。

### 1.9 本番の状態の確認

```sh
hostname                                                  # = $HOST
test ! -e /etc/geoframe/staging && echo "印なし（正常）"     # 検証環境の印が無い
test ! -e /etc/geoframe/checkout-pinned && echo "固定の印なし（まだ）"
test ! -e /etc/systemd/system/geoframe-rs.service && echo "古い本体なし"
systemctl is-enabled geoframe-rs 2>&1                     # not-found（または「No such file」。有効でない）
systemctl show -p FragmentPath geoframe-server            # /etc/systemd/system/geoframe-server.service
test -f /etc/systemd/system/geoframe-server.service -a ! -L /etc/systemd/system/geoframe-server.service && echo "通常のファイル"
systemctl is-active geoframe-server                       # active
ls -ld /var/backups/geoframe                              # geoframe の所有・0750
df -h /var/backups/geoframe /var/lib/geoframe             # DB の約 3 倍以上の空き
```

- 確かめること: 上のとおり。Python 版 unit が `/etc/systemd/system/` の**通常のファイル**（§3.2 で移す対象）。別の場所・リンクなら、移し方を当日の前に調整する（ユーザーに報告）。
- 失敗したら: バックアップ先が無い・所有者が違えば `install -d -o geoframe -g geoframe -m 0750 /var/backups/geoframe`（許可を得て）。

### 1.10 `--check-only` を一度通す

目的: 設定のファイル・mask・古い置き場所・バイナリ・commit の誤りを、Python を止める**前に**見つける。変更はしない。

```sh
COUNT='<仮の件数。当日は §2 の控えた件数>'     # 前日は 1 でもよい（--expect-min-photos は --check-only でも必須）
/opt/geoframe/deploy/rust-deploy.sh --production="$HOST" --bin="$BIN" \
    --expect-sha256="$SHA" --expect-commit="$COMMIT" --expect-min-photos="$COUNT" --check-only; echo "rc=$?"
```

- 確かめること: 終了コード **0**、「`--check-only: 検査はすべて通った。変更はしていない`」。ホスト名・検証環境の印が無いこと・commit・作業ツリー・バイナリの sha256・設定のファイル（`/etc/default/geoframe-rs` が通常のファイルで `0600 root`）・mask・古い置き場所の unit の本体、を見ている。**Python 版の状態は見ない**（まだ動いているため）。
- 失敗したら: 終了コード **4**（拒否。何も変えていない。理由が出る）と **2**（引数の誤り）を直して通す。**通るまで当日に進まない。**

### 1.11 前日の最後に

ユーザーに報告して決める: 切替の日時（Python の停止が許される時間帯。`geoframe-backup.timer` の発火（毎日 00・06・12・18 時台の :17）と、毎朝 5:00 の Decision のタイマーを避ける）、止める判断の基準（§0.4）の確認、立ち会い、miharashi を見る人。

## 2. 切替の直前

目的: 当日の最初に、変えずに検査し、戻したあとに比べる控えを取る。Python はまだ動いている。

```sh
cd "$WORK"
date '+%F %T'                                              # 開始時刻を記録
curl -fsS $BASE/api/v1/photos -o py-feed.json
python3 -I ids.py py-feed.json | LC_ALL=C sort > py-ids.txt
COUNT=$(wc -l < py-ids.txt); echo "Python の写真: $COUNT 件"
curl -fsS $BASE/api/admin/status | python3 -I status_counts.py > py-status.txt
"$WORK/drafts_sha.sh" "$WORK/drafts-before.sha"; echo "rc=$?"          # 下書きの全ファイルの sha256。rc=0
MIN=$((COUNT * 98 / 100)); echo "--expect-min-photos=$MIN"     # 控えた件数より少し下
WAIT=30                                                    # 演習で測った値から（起動から /healthz 200 まで 0.2〜0.4 秒。p5-acceptance.md §2.3）
/opt/geoframe/deploy/rust-deploy.sh --production="$HOST" --bin="$BIN" \
    --expect-sha256="$SHA" --expect-commit="$COMMIT" --expect-min-photos="$MIN" --wait-secs="$WAIT" --check-only; echo "rc=$?"
```

- 確かめること:
  - 一覧の件数が §1 以降で大きく変わっていない。`py-status.txt` に `db.loc.clue_pending` がある。`ai_task_blocks` などの AI の追加キーは Python では `なし`。
  - `--check-only` が終了コード 0。**これは §5 のコマンドから `--check-only` を除いただけ**にしておく（同じ引数を通す）。
  - 控えたもの（`py-feed.json`・`py-ids.txt`・`py-status.txt`・`drafts-before.sha`。下書きのパスが入るので画面に出さない）は `$WORK` にあり、リポジトリには入れない（リポジトリには**件数だけ**）。
- 失敗したら: `--check-only` が 0 でなければ切替を始めない。理由を直して§2 をやり直す。

## 3. Python を止める

目的: Python を止めて、二度と勝手に起動しない状態にする。**ここから配信が止まる。** 許可を得てから始める。

### 3.1 止める

```sh
T0=$(date +%s); date '+%F %T'                      # T0 を記録
systemctl stop geoframe-server
systemctl disable geoframe-server
systemctl is-active geoframe-server                # inactive
```

- 確かめること: `is-active` が `inactive`。
- 失敗したら: `stop` が失敗（止まらない）ときは配信を止めていない。原因（`systemctl status`）をユーザーに報告して延期。

### 3.2 unit の本体を移す

mask は `/etc/systemd/system/<unit>` に `/dev/null` へのリンクを作る。そこに本体のファイルがあると失敗するので、Python 版の本体を `/usr/local/lib/systemd/system/` へ移す。

```sh
install -d -m 0755 /usr/local/lib/systemd/system
mv /etc/systemd/system/geoframe-server.service /usr/local/lib/systemd/system/geoframe-server.service
ls -l /usr/local/lib/systemd/system/geoframe-server.service
test ! -e /etc/systemd/system/geoframe-server.service && echo "/etc 側に本体なし"
```

> 演習では: 一時的に置いた Python 版 unit が最初から `/usr/local/lib/systemd/system/` にあるので、この移動は省く（`/etc/systemd/system/` に本体が無いことだけ確かめる）。

### 3.3 drop-in を置く（Python の AI を止める）

```sh
install -d -m 0755 /etc/systemd/system/geoframe-server.service.d
cat > /etc/systemd/system/geoframe-server.service.d/p5-rollback.conf <<'EOF'
[Service]
EnvironmentFile=/etc/default/geoframe-py-rollback
EOF
chmod 0644 /etc/systemd/system/geoframe-server.service.d/p5-rollback.conf
```

`-` を付けない（ファイルが無ければ Python は起動しない＝AI が動いたまま戻ることを防ぐ）。`EnvironmentFile=` は追記で、共通の `/etc/default/geoframe` の**後**に読まれて勝つ。

### 3.4 読み込みの確認

```sh
systemctl daemon-reload
systemctl show -p FragmentPath -p DropInPaths -p EnvironmentFiles geoframe-server
```

- 確かめること: `FragmentPath` が `/usr/local/lib/systemd/system/geoframe-server.service`、`DropInPaths` に `p5-rollback.conf`、`EnvironmentFiles` に `/etc/default/geoframe`（`ignore_errors=yes`）と `/etc/default/geoframe-py-rollback`（`ignore_errors=no`）の**2つが並ぶ**。**mask のあとは `show` で読めなくなる**ので、この確認は mask の前に行う。

### 3.5 mask

```sh
systemctl mask geoframe-server
systemctl is-enabled geoframe-server               # masked
systemctl is-active geoframe-server                # inactive
```

- 確かめること: `masked`・`inactive`。drop-in は mask のあいだも残り、unmask で効く。
- **mask が失敗したらここで止まる**（先へ進まない）。典型は「`/etc/systemd/system/geoframe-server.service` が既にある」。`ls -l` で見て、`/usr/local/lib/…` の本体と同じ内容（`cmp`）の古い本体が `/etc` に残っているだけなら、`/etc` 側を消して mask をやり直してよい。それ以外は「**§3 で止まったとき**」で元に戻す。

### 3.6 checkout の固定の印

```sh
install -d -m 0755 /etc/geoframe
printf 'P5 cutover %s\n' "$(date +%F)" > /etc/geoframe/checkout-pinned
chmod 0644 /etc/geoframe/checkout-pinned
```

これがあると `deploy-pull.sh` は何もせずに終了コード 3 で断る（Python へ戻したあとも）。**猶予期間が終わるまで外さない**（§13）。

### §3 で止まったとき（切替の前の状態へ戻す）

Python を元の状態で起動し直す（AI も元どおり）。drop-in を置いたままにしない。

```sh
systemctl unmask geoframe-server 2>/dev/null
rm -f /etc/systemd/system/geoframe-server.service.d/p5-rollback.conf
rmdir /etc/systemd/system/geoframe-server.service.d 2>/dev/null
[ -e /usr/local/lib/systemd/system/geoframe-server.service ] && \
    mv /usr/local/lib/systemd/system/geoframe-server.service /etc/systemd/system/geoframe-server.service
rm -f /etc/geoframe/checkout-pinned
systemctl daemon-reload
systemctl enable --now geoframe-server
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/admin/status    # 200
```

- Python が 200 を返したら、**§4.1 でバックアップのタイマーを止めたあと（§4 の途中）に来たときは、続けて [§4.5](#45-バックアップのタイマーを戻す共通の出口) を実行する**（§3 の途中で来たときは、タイマーを止めていないので `systemctl is-active geoframe-backup.timer` が `active` のことを確かめるだけでよい）。
- 原因をユーザーに報告して延期する。

## 4. 保全

目的: Rust を起動する前に、元に戻れる材料（DB のバックアップと、バックアップに入っていない一次データ）を取る。書き手がいない状態で取る。`backup-catalog.sh` は名前が `catalog-*` で `.dump` のハッシュも取らないので、ここでは**手で**取る。

### 4.1 バックアップのタイマーを止める

```sh
systemctl stop geoframe-backup.timer
systemctl is-active geoframe-backup.timer geoframe-backup.service    # どちらも inactive
```

- 確かめること: 両方 `inactive`。`geoframe-backup.service`（oneshot）が走っていたら終わるのを待つ。
- **ここから先、タイマーは止まったまま**になる。切替が終わったら（§6.6）戻す。**中断・切り戻し・復元のどの出口でも、サービスを復旧したあとに [§4.5](#45-バックアップのタイマーを戻す共通の出口) で戻す**（§4 の失敗→§3 の「止まったとき」、§5 の失敗→§8、§6 の判断→§8、§9 の途中、§10）。出口ごとの戻す場所は §4.5 の表。

### 4.2 書き手がいないこと

```sh
"$WORK/dbopen.sh"; echo "dbopen rc=$?"          # rc=0（開いているプロセスが無いと確認できた）
pgrep -u geoframe -a; echo "pgrep rc=$?"         # 出力が空・rc=1
```

- `dbopen.sh` は `fuser` があれば `fuser`、無ければ `/proc` の走査で調べる（§0.5）。**rc=0 のときだけ「無い」**。rc=1（ある）も rc=2（確認できなかった）も先へ進まない。`pgrep` は **rc=1 だけが「無い」**（rc=2・3 は確認できなかった）。
- 確かめること: DB を開いているプロセスも、`geoframe` ユーザーのプロセスも**無い**（unit の外で動いている Python なども無い）。
- 失敗したら: **何かが出たら・確認できなかったら先へ進まない**。`ps -o pid,user,comm -p <pid>` で何かを確かめ、ユーザーに報告する（勝手に kill しない。`ps -f` は引数を出すので使わない）。確認できなかったとき（rc=2）は、原因（root でない・`/proc` が読めない・`fuser` のエラー・fd を読めないプロセスがある）を直して `dbopen.sh` をやり直す（出た pid は `ps -o pid,user,stat,comm -p <pid>` で何かを確かめる。読めない理由が分からないまま「無い」と扱わない）。

### 4.3 DB のバックアップ

```sh
TS=$(date +%Y%m%d-%H%M%S)
SNAP=/var/backups/geoframe/.tmp-p5-precutover-$TS.db
OUT=/var/backups/geoframe/p5-precutover-$TS.db.gz
sudo -u geoframe sqlite3 'file:/var/lib/geoframe/catalog.db?mode=ro' ".backup '$SNAP'"; echo "rc=$?"
```

- **`mode=ro` が `-shm` の無さで失敗したとき**（`unable to open database file`。停止直後に `-shm` が消えている）の代わり: 本番の DB には書かず、`catalog.db` と（あれば）`-wal` を**コピーして**、コピーから取る。`-shm` はコピーしない。

```sh
CP=$(mktemp -d /var/backups/geoframe/.p5-copy.XXXXXX)
cp -p /var/lib/geoframe/catalog.db "$CP/catalog.db"
[ -f /var/lib/geoframe/catalog.db-wal ] && cp -p /var/lib/geoframe/catalog.db-wal "$CP/catalog.db-wal"
chown -R geoframe:geoframe "$CP"
sudo -u geoframe sqlite3 "$CP/catalog.db" ".backup '$SNAP'"; echo "rc=$?"
rm -rf "$CP"
```

続けて、検査・ハッシュ・圧縮:

```sh
sudo -u geoframe sqlite3 "file:$SNAP?immutable=1" 'PRAGMA integrity_check'          # ok の 1 行
gzip -9 -c -- "$SNAP" > "$OUT.part" \
  && chown geoframe:geoframe "$OUT.part" && chmod 0640 "$OUT.part" \
  && mv "$OUT.part" "$OUT" && gzip -t "$OUT" && rm -f -- "$SNAP"
ls -l "$OUT"                                       # -rw-r----- geoframe geoframe
ls -A /var/backups/geoframe | grep '^\.' || echo "一時ファイルなし"   # .tmp-・.p5-copy. が残っていない
```

- 確かめること: `integrity_check` が **`ok` の 1 行だけ**（`.dump` の SHA-256 と件数は、時間のかかる読み取りなので停止時間の外の §6.8 で取る。この段で要るのは `integrity_check` と `gzip -t` だけ）。バックアップは `p5-precutover-<時刻>.db.gz`・`0640`・`geoframe` 所有（`catalog-*` に当たらない名前なので、7 日の世代削除の対象外）。**P5 の記録が終わるまで消さない。**
- 失敗したら: `.backup` が失敗・`integrity_check` が `ok` でないときは先へ進まない（配信は止まったまま）。**ここで原因が分からなければ、§3 の「止まったとき」で Python を戻し、続けて [§4.5](#45-バックアップのタイマーを戻す共通の出口) でタイマーを戻す**（DB は Python が止まって以降、誰も書いていない）。

### 4.4 一次データの tar

```sh
AI_CFG=$(sed -n 's/^GEOFRAME_AI_CONFIG=//p' /etc/default/geoframe-rs | tail -n 1 | tr -d "\"'")
FILES=()
for p in /var/lib/geoframe/travel_blog/drafts /var/lib/geoframe/.cache.json /opt/geoframe/app/.env "$AI_CFG"; do
    if [ -e "$p" ]; then FILES+=("$p"); else echo "無い: $p"; fi
done
FILES+=(/etc/default/geoframe*)
( umask 077; tar -cf "$WORK/p5-primary-$TS.tar" "${FILES[@]}" )      # 「Removing leading `/'」は正常
chmod 0600 "$WORK/p5-primary-$TS.tar"
tar -tf "$WORK/p5-primary-$TS.tar" | wc -l
sha256sum "$WORK/p5-primary-$TS.tar"
```

- 確かめること: tar が `0600 root`・`/var/lib/geoframe` の外（`$WORK`）にある。「無い:」と出たものは、本当に無いか（下書きを書いたことがない等）をユーザーに確かめる。**鍵（Drive の鍵・AI の鍵）は動かさないので tar に入れない**。tar には秘密（`.env`・設定のファイル）が入るので、持ち出さない・文書に中身を書かない。
- 失敗したら: tar が作れなければ先へ進まない（`app/.env` や設定のファイルは Python 側の設定の唯一の控え）。原因が直せなければ §3 の「止まったとき」→ §4.5。

### 4.5 バックアップのタイマーを戻す（共通の出口）

§4.1 で止めたタイマーを、**サービス（Python か Rust）が配信に戻ったあと**に、どの出口でも戻す。何度実行してもよい（すでに `active` なら何も変わらない）。

```sh
systemctl start geoframe-backup.timer
if [ "$(systemctl is-active geoframe-backup.timer)" = active ] && [ "$(systemctl is-enabled geoframe-backup.timer)" = enabled ]; then
    echo "タイマー OK（active / enabled）"; else echo "NG: タイマーが active / enabled に戻っていない"; fi
systemctl is-active geoframe-backup.service       # inactive（走っていない）か activating（走行中）。failed なら journal の種類を報告
systemctl list-timers geoframe-backup.timer       # NEXT が先の時刻
```

- 確かめること: `タイマー OK`。`NG` が出たら先へ進まず、ユーザーに報告する。
- 戻す場所（出口ごと）:

| 出口 | サービスの復旧 | タイマーを戻す |
| --- | --- | --- |
| §3 の途中で止まった | §3 の「止まったとき」 | 止めていない。`is-active` が `active` なのを確かめるだけ |
| §4 の途中で止まった（4.2〜4.4 の失敗） | §3 の「止まったとき」 | その直後に §4.5 |
| §5 の終了コードが 0 でない・§6 の判断で戻す | §8（§8.3 で Python が 200 になったあと） | §8.3 の最後に §4.5（§6.6 を済ませていても、もう一度確かめる） |
| §9 の切り替え直しの途中で戻す | 同じく §8 | 同じく §8.3 の最後に §4.5 |
| §10 の復元 | §10.5 | §10.6（= §4.5。途中で止めたときも戻す） |
| 切替が成功した | — | §6.6（= §4.5） |

## 5. Rust を入れる

目的: 演習で使ったバイナリそのものを、`rust-deploy.sh --production` で入れる（unit の導入・enable・起動・確認まで）。許可を得てから。

```sh
compgen -e | grep '^GEOFRAME_RS_' || echo "GEOFRAME_RS_* なし（正常）"     # 名前だけ。出たらこのシェルでは実行しない
/opt/geoframe/deploy/rust-deploy.sh --production="$HOST" --bin="$BIN" \
    --expect-sha256="$SHA" --expect-commit="$COMMIT" \
    --expect-min-photos="$MIN" --wait-secs="$WAIT"; echo "rc=$?"
```

| 引数 | 値 |
| --- | --- |
| `--production=` | `hostname` と一致するホスト名（英数字と `. _ -`） |
| `--bin=` | 配るバイナリの**絶対パス**。root 所有の一時ディレクトリへ写してから sha256 を取り、写しを入れる |
| `--expect-sha256=` | 小文字の 16 進 64 桁 |
| `--expect-commit=` | `/opt/geoframe` の `HEAD`（小文字の 16 進 **40 桁**。短縮形は 2 で断られる） |
| `--expect-min-photos=` | 配布のあと `/api/v1/photos` の件数がこれ未満なら失敗として戻す（1〜999999999）。§2 の `$MIN`（控えた件数の 98%）。DATA_DIR の誤りで空の DB を作っても「成功」にしない |
| `--wait-secs=` | 再起動のあと `/healthz` を待つ秒数（0〜3600。既定 30）。演習で測った値 |

終了コード（`rust-deploy.sh --production`）:

| 終了コード | 意味 | すること |
| --- | --- | --- |
| **0** | 成功 | §6 へ |
| **1** | 失敗したが配布前の状態へ戻した。初回の配布なので、止めて `disable` し、unit を撤去し、バイナリは `geoframe.failed` へ退避してある | **すぐ §8**（Rust は無い状態。Python を起動する） |
| **3** | 失敗し、戻す途中でも失敗した。手作業の復旧手順が出る（本番では「本番機で root として手で戻すこと」と出て、Python 版へ戻すのは §8 と案内する） | まず `systemctl stop geoframe-rs` で止めてから、**§8**。出た手順と状態（`ls -l /usr/local/bin/geoframe*`・`systemctl status geoframe-rs`）を報告 |
| **4** | 誤配布の防止で拒否した（何も変えていない。理由が出る。ホスト名・印・mask・設定のファイル・Python が稼働中・古い置き場所など） | 下の**一つの規則**（再実行は条件つき。それ以外は **§8**） |
| **2** | 引数の誤り（何も変えていない） | 同じ規則 |

- **非ゼロのときの規則は一つ**（[§0.4](#04-時間の目安と止める判断)）: 終了コード 1・3 は待たずに**すぐ §8**。何も変えずに断った 2・4 は、**原因がすぐ直せて、T0 から 10 分の配信停止の期限内に再実行して 0 になる見込みがあるときだけ**、1 回だけ再実行する（引数の綴りの誤り・環境変数 `GEOFRAME_RS_*` を持ったシェルで打った、など）。再実行も 0 でない・原因が分からない・直すのに設定やバイナリの入れ直しが要る・期限に間に合わないときは**すぐ §8**。Python は止まったままなので、迷っている時間がそのまま停止時間になる。
- **`--rollback --production` は、初回の切替の戻しには使えない**。初回の配布は `.prev`（直前のバイナリ）を作らないので「`.prev` が無い」で終了コード 1 になる。`--rollback --production` は、**Rust を一度入れたあとで更新に失敗した**ときに、Rust の直前の版へ戻す道具（§11 以降）。
- 確かめること（0 のとき）: 「確認 OK（/healthz 200、/api/v1/photos は配列で N 件 >= 下限 M）」「完了: geoframe <版>」が出る。

```sh
systemctl is-enabled geoframe-rs; systemctl is-active geoframe-rs      # enabled / active
systemctl is-active geoframe-server                                    # inactive
ls -l /usr/local/bin/geoframe*                                         # geoframe（初回なので .prev は無い。§9 の切り替え直しでは、同じバイナリの .prev ができる）
journalctl -u geoframe-rs -n 30 --no-pager                             # [guard] の info が 1 行。エラーが無い
```

## 6. 確かめる

目的: 配信が戻ったこと、Python の配信にあった写真がすべて Rust にあること、取り込み・AI・管理画面・miharashi が動くことを確かめる。

**実行する順番**: 6.1 → 6.8（バックアップの展開と控え。6.2 の検査に使う）→ 6.2 → 6.3 → 6.4 → 6.5 → 6.6 → 6.7。6.6（タイマー）は他の確認より先に済ませてもよい。

### 6.1 配信の再開（T1）

```sh
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/healthz            # 200
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/v1/photos      # 200
T1=$(date +%s); date '+%F %T'; echo "停止から再開まで $((T1 - T0)) 秒"
```

- 確かめること: 両方 200。**時間を記録**する（目標 5 分以内、10 分で戻す）。
- 失敗したら: 200 にならないまま T0 から 10 分で §8。

### 6.2 写真の集合の比べ方

Python の配信にあった写真が**すべて**Rust の配信にあり、増えた分は既知の差の範囲であること。増えた分の検査には、切替の前の DB（§4.3 のバックアップを展開したもの）を使うので、**先に [§6.8](#68-保全の控え停止時間の外で) を実行する**（`$WORK/precutover.db` ができる。T1 のあとなので停止時間に入らない）。

```sh
cd "$WORK"
curl -fsS $BASE/api/v1/photos -o rs-feed.json
python3 -I ids.py rs-feed.json | LC_ALL=C sort > rs-ids.txt
wc -l < py-ids.txt; wc -l < rs-ids.txt
comm -23 py-ids.txt rs-ids.txt | wc -l                    # Python だけにあった写真: 0 でなければ失敗
comm -13 py-ids.txt rs-ids.txt > rs-only.txt; wc -l < rs-only.txt    # Rust だけの写真（増えた分）
python3 -I extra.py rs-feed.json rs-only.txt "$WORK/precutover.db" "$WORK/unexplained-ids.txt"; echo "extra rc=$?"
python3 -I check_files.py rs-feed.json                    # 画像: 200でない 0
rm -f "$WORK/precutover.db"                               # 検査が済んだら消す（§6.8 の控えは残る）
```

- 確かめること:
  - **Python だけにあった写真が 0**（`comm -23` が 0）。1 件でもあれば失敗。
  - **増えた分（`rs-only.txt`）のすべてが説明できること**（`extra.py` が終了コード 0・「説明できない 0」）。`extra.py` は、切替の前の DB の旧グループ（`photo_ai_groups`）・代表・特徴量の世代と、増えた ID を ID ごとに照らし、次に分けて件数だけを出す:
    - **取り込み**: 切替の前の DB に無い・まだ表示できなかった（`display` か位置が無い）写真が、Rust の取り込みで表示可能になった。**切替の前に `hidden=1` だった写真はここに入れない**（取り込みでは `hidden` は変わらない）。
    - **既知の差**（[p4-acceptance.md §7.3](p4-acceptance.md)）: 旧グループの**推薦写真（代表）が「表示できる代表」でなくなった**（回転して特徴量の世代が合わなくなった・非表示・位置や `display` が無い・品質で `exact` の非推薦になった）ために、Rust は代表を有効と見なさず、残りの写真を隠すのをやめる（P4 の受入では回転による 18 の旧グループで 45 件）。「表示できる代表」の条件は `selection.rs` と同じ（`hidden`・`display`・位置・特徴量の世代・今の世代の `photo_quality` の行が `exact` の非推薦でないこと）。
    - **説明できない**: 上のどちらでもない。**切替の前に `hidden=1` だった写真がフィードに出ているのもここ**（切替の手順の中では管理操作をしないので、`hidden` の解除は説明できない差。ユーザーが管理画面で解除したことが分かっているなら、その記録と照らして別に扱う）。**0 でなければ失敗として扱う**（ID は `$WORK/unexplained-ids.txt` にだけ書かれ、画面には出ない）。§0.4 の 30 分で原因を見る。
  - 群の数は `ai_group`（空文字は群に数えない）と `quality_group` を別々に数えて記録する。**0 件でも異常ではない**（受入の前は 3,065 件で同数）。
  - 画像（フィードの `file`）が全部 200（`200でない 0`）。
- 失敗したら: Python だけの写真が出た・説明できない増加が出た・画像が欠けたときは、件数だけをユーザーに報告し、30 分のうちに原因が見えなければ §8。

### 6.3 STATUS

```sh
curl -fsS $BASE/api/admin/status | python3 -I "$WORK/status_counts.py" | tee "$WORK/rs-status-1.txt"
diff "$WORK/py-status.txt" "$WORK/rs-status-1.txt"
```

- 確かめること:
  - **`ai_task_blocks=なし`**（Provider や用途の停止にかかっている用途が無い）、`ai_config_invalid=なし`、`ai_pauses=なし`。
  - `db.loc.clue_pending` が**0でない**（Rust は旧 Python の判定を信用せず全件を判定し直す。総待ちが数百〜千件台）、`db.loc.clue_runnable` が 0 でなく（Decision が動けば進む数。[ai-location-clue.md](ai-location-clue.md#statusのclue_pendingclue_runnableclue_pause)）、`db.loc.clue_pause` が空（`''`）。`clue_runnable` が 0 で `clue_pause` が非空なら、用途が停止している（§7 の `release-pause`）。
  - その他の `db.*` の件数が Python の控えと大きく変わらない（`diff` の差は、Rust の追加キー `ai_jobs`・`ai_scenes` などと、AI の件数）。
- 失敗したら: `ai_config_invalid` があれば AI 設定の誤り（配信は続く）。コードは `ai-jobs status` で見る（§7）。直すのは設定の変更なのでユーザーの許可を得て、再起動する。

### 6.4 管理画面・miharashi

ユーザーが確かめる（管理者は画面を共有してもらうか、結果を聞く）。

- 管理画面が開き、STATUS・写真一覧・地図（OSM のタイルが出る）・地名検索が動く。
- **miharashi** の表示が続いている（写真が切り替わり続け、取得が成功している。取得先は `http://127.0.0.1:8080` のまま）。
- 書き込みの確認（ユーザーが選んだ 1 枚で、お気に入りを付けてすぐ外す。許可を得て）: 操作が成功し、元に戻る。書き込み（INGEST=1 の writer）が動いていることの確認になる。

### 6.5 取り込みの 1 サイクル

```sh
journalctl -u geoframe-rs --since '<T0の時刻>' --no-pager | grep -E 'サイクル完了|表示可能'
journalctl -q -u geoframe-rs --since '<T0の時刻>' -p warning --no-pager | cut -c1-200     # -q: 無いとき「-- No entries --」を出さない
```

- 確かめること: `[sync] サイクル完了: 表示可能 N / 取得待ち N / 全N` が出る（起動直後に 1 回。以降 `GEOFRAME_DRIVE_POLL_MIN` ごと）。失敗した段の警告が無い（あっても段の名前と種類だけの定型文。写真の値は出ない）。
- 失敗したら: 失敗した段があれば、その種類をユーザーに報告し、30 分のルールで判断する。Drive の鍵が読めない等の外部要因は配信に影響しないので、原因が見えれば続ける。

### 6.6 バックアップのタイマーを戻す

[§4.5](#45-バックアップのタイマーを戻す共通の出口) のコマンドを実行する（`タイマー OK`・`NEXT` が先の時刻）。

### 6.7 そのほかの確認

```sh
/opt/geoframe/deploy/deploy-pull.sh --dry-run; echo "rc=$?"      # 拒否（rc=3）。サービスにも git にも触れない
/opt/geoframe/deploy/geoframe-admin.sh ai-jobs status             # 読み取りだけ。対象の DATA_DIR（パス）が最初に出る
systemctl show geoframe-rs -p NRestarts -p Result                 # NRestarts=0
```

- 確かめること: `deploy-pull.sh` が固定の印で断る（rc=3）。`ai-jobs status` が動く。`NRestarts=0`。

### 6.8 保全の控え（停止時間の外で）

バックアップ（§4.3）の `.dump` の SHA-256・件数・ID ごとの状態を、**配信が戻ったあとで**、バックアップのファイルから取る（本番の DB には触れない。root の作業用の置き場で展開する）。展開が途中で失敗したまま先へ進まないよう、**`&&` でつないで、どれか 1 つでも失敗したら全体を失敗とする**（`gzip -dc` の終了コード・展開後のサイズが 0 でないこと・`integrity_check` が `ok`・`.dump` のハッシュが取れること。`.dump | sha256sum` の上流の失敗は `dumpsha.sh` が `pipefail` で検出する）。`set -e` に頼らない（`||` や `if` の中に貼ると効かなくなるため）。`$OUT` は §4.3 のバックアップのパス（シェルを替えたなら `OUT=/var/backups/geoframe/p5-precutover-<時刻>.db.gz` と置き直す）。

```sh
PRE="file:$WORK/precutover.db?immutable=1"
rm -f -- "$WORK/precutover.db.part"
if gzip -dc -- "$OUT" > "$WORK/precutover.db.part" \
   && [ -s "$WORK/precutover.db.part" ] \
   && mv -- "$WORK/precutover.db.part" "$WORK/precutover.db" \
   && [ "$(sqlite3 "$PRE" 'PRAGMA integrity_check')" = ok ] \
   && "$WORK/dumpsha.sh" "$PRE" > "$WORK/precutover.dumpsha" \
   && sqlite3 "$PRE" < "$WORK/counts.sql" > "$WORK/precutover-counts.txt" \
   && sqlite3 "$PRE" < "$WORK/state_snapshot.sql" > "$WORK/precutover-state.tsv"; then
    echo "展開と控え OK"; echo "dump sha256: $(cat "$WORK/precutover.dumpsha")"; cat "$WORK/precutover-counts.txt"
else
    rm -f -- "$WORK/precutover.db" "$WORK/precutover.db.part" "$WORK/precutover.dumpsha" "$WORK/precutover-counts.txt" "$WORK/precutover-state.tsv"
    echo "NG: 展開か控えに失敗した。先へ進まず、バックアップの gzip -t と容量を調べる"
fi
```

- 確かめること: `展開と控え OK`。`DUMPSHA`（`$WORK/precutover.dumpsha` の値）を控える（§10 の復元で照合し、`p5-acceptance.md` に書く）。`precutover-counts.txt` に photos・manual・favorite・hidden・rotated・quality_rows・quality_groups・algorithm 別の件数が出る（§10 の復元の確認に使う）。`precutover-state.tsv` は ID ごとの手動の位置・お気に入り・除外・回転・表示ファイル（座標が入るので画面に出さない）。`precutover.db` は §6.2 の検査（`extra.py`）が済むまで残す（済んだら消す）。
- 失敗したら（`NG`）: 一時ファイルと途中の控えは消えている。バックアップのファイル自体の `gzip -t` が失敗するなら、**そのバックアップは使えない**ので、ユーザーに報告する（§10 の復元の前提が崩れる。`backup-catalog.sh` の最新の世代など別のバックアップを検討する）。
- 戻す（§8）が先になったときは、戻したあとでこの 6.8 を行ってよい（バックアップのファイルは変わらない）。

ここまでで切替は完了。T1 から **30 分**は様子を見る（§0.4）。時間・件数を記録する（§14）。

## 7. AI の書き込み系の操作

`ai-jobs retry`・`release-pause`・`deactivate-space`、`sync`・`organize`・`quality-scan` は DB に書く。稼働中の `serve` が `.ingest.lock` を持っているので、**サービスを止めて**から動かす。**root で直接 `geoframe` を動かさない**: バイナリは root での書き込みを終了コード 2 で断る（作られるファイルが root の所有になり、サービスが書けなくなるため）。代わりに `geoframe-admin.sh` を使う。サービスと同じユーザー（`geoframe`）・同じ設定のファイル（`/etc/default/geoframe` → `/etc/default/geoframe-rs`）・`UMask=0027`・`photos/` 読み取り専用で `systemd-run` する。動かす前に**対象の DATA_DIR（パスだけ）**を出すので、サービスと同じ場所であることを見る。

```sh
# 書き込み系: 止める → CLI → （成功しても失敗しても）起動し直す
(
  trap 'systemctl start geoframe-rs; echo "[admin] geoframe-rs を起動し直した rc=$?"' EXIT
  trap 'exit 130' HUP INT TERM
  systemctl stop geoframe-rs \
    && /opt/geoframe/deploy/geoframe-admin.sh ai-jobs retry --kind photo_judge
)
systemctl is-active geoframe-rs                                      # active
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/healthz             # 200
```

- 最後の行（CLI）だけを差し替える。主なもの:

| 目的 | コマンド（`geoframe-admin.sh` の引数） |
| --- | --- |
| 失敗したジョブを再検査 | `ai-jobs retry --kind <image_embedding｜location_clue｜location_suggest｜location_geocode｜photo_judge>` または `--all`（外部へは送らない） |
| 認証・契約の不一致で止まった用途を解除 | `ai-jobs release-pause --task <image_embedding｜query_embedding｜photo_judge｜location_clue｜location_suggest>` または `--all` |
| 有効な検索空間を外す | `ai-jobs deactivate-space` |
| 取り込みを手で 1 回 | `sync`（Drive の大量削除の保留を意図して外すときだけ `sync --allow-mass-delete`） |

- `ai-jobs status` も**同じラッパー**で動かす（読み取りだけなので**サービスを止めなくてよい**）: `/opt/geoframe/deploy/geoframe-admin.sh ai-jobs status`。ラッパーを使うのは、AI 設定（`GEOFRAME_AI_CONFIG`）と DATA_DIR をサービスと同じ設定のファイルから取るため（root で直接打つと、設定が無く、AI の用途が出ない）。
- 確かめること: CLI の終了コードが 0（1 失敗／2 引数・設定の誤り、または書き込みを断った）。`trap` により、CLI が失敗しても Rust が**起動し直される**。起動し直しのあと `is-active`・`/healthz` を確かめる。
- 失敗したら: 起動し直しが `start-limit` で断られたら `systemctl reset-failed geoframe-rs` のあとで `systemctl start geoframe-rs`。ラッパーが「root で実行すること」「設定のファイルを読めない」と断ったら、`sudo` と `/etc/default/geoframe-rs` を確かめる。
- サービスを止めるあいだは配信が止まる（通常は数十秒）。ユーザーに伝えてから行う。

## 8. Python へ戻す

目的: 問題が出たとき（§0.4 の基準・§5 の終了コード）に Rust を止め、Python に戻す。**AI は無効**で戻す（drop-in が効く）。既定は **DB を保ったまま**戻す（Python は Rust が足した表・列を無視して読める。[p4-acceptance.md §7](p4-acceptance.md)）。DB が壊れたときだけ §10。許可を得て（迷う時間が停止時間なので、§5 の失敗では即実行してよい）。**どの経路で戻しても、Python が 200 を返したら [§4.5](#45-バックアップのタイマーを戻す共通の出口) でバックアップのタイマーを戻す**（§8.3 の最後）。

### 8.1 Rust を止めて mask する

```sh
TR=$(date +%s); date '+%F %T'                      # 戻し始めた時刻
curl -fsS --max-time 20 $BASE/api/v1/photos -o "$WORK/rs-end-feed.json" || echo "Rust の一覧を取れなかった（応答しない）"   # 止める前の一覧（§8.4 の件数の基準）
systemctl stop geoframe-rs
systemctl disable geoframe-rs || true
systemctl mask geoframe-rs
systemctl is-active geoframe-rs                    # inactive
systemctl is-enabled geoframe-rs                   # masked
"$WORK/dbopen.sh"; echo "dbopen rc=$?"             # rc=0（Rust が DB を開いていない）。rc=1・2 は先へ進まない（§0.5）
```

- 初回の配布の失敗で unit が外れているとき（`not-found`）も同じ手順で通る（mask は unit が無くても効く）。このとき Rust は書いていないので、`rs-end-feed.json` は無くてよい。
- **mask が失敗したらここで止まる**。`/etc/systemd/system/geoframe-rs.service` に本体が無いか（`ls -l`）を見る。

### 8.2 戻す直前の控え（Rust を止めたあと・Python を起動する前）

Python が起動すると、取り込みの 1 サイクルが始まって DB が動く。**Rust の期間に付いた手動の位置・お気に入り・除外・回転・下書き**が戻したあとも保たれているかを比べるために、Python を起動する**前**の値を取る（数秒）。実値（座標・パス）は `$WORK` のファイルにだけ置き、画面と報告には**件数だけ**を出す。

```sh
DB_RO='file:/var/lib/geoframe/catalog.db?mode=ro'
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/counts.sql" > "$WORK/rs-end-counts.txt"; echo "counts rc=$?"
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/state_snapshot.sql" > "$WORK/rs-end-state.tsv"; echo "state rc=$?"; wc -l < "$WORK/rs-end-state.tsv"
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/quality_snapshot.sql" > "$WORK/rs-end-quality.tsv"; echo "quality rc=$?"     # 品質の群の顔ぶれと、群の形成の入力（写真の display・md5・mtime・taken_at と、有効な特徴量の SHA・pHash）。座標は入らない。display・taken_at は画面に出さない
"$WORK/drafts_sha.sh" "$WORK/drafts-rs-end.sha"; echo "drafts rc=$?"
comm -23 <(sed 's/^[0-9a-f]\{64\}  //' "$WORK/drafts-before.sha" | LC_ALL=C sort) \
         <(sed 's/^[0-9a-f]\{64\}  //' "$WORK/drafts-rs-end.sha" | LC_ALL=C sort) | wc -l     # 0（切替の前にあった下書きが、Rust の期間に消えていない）
[ -e /var/lib/geoframe/.cache.json ] && cp -p -- /var/lib/geoframe/.cache.json "$WORK/cache-rs-end.json"
sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe GEOFRAME_AI_ENABLED=0 GEOFRAME_LOC_SUGGEST=0 \
    "$PYAPP/.venv/bin/python" -I -B - "$PYAPP" < "$WORK/cache_load.py"; echo "cache rc=$?"
```

- 確かめること:
  - `counts rc=0`・`state rc=0`・`quality rc=0`・`drafts rc=0`。**切替の前にあった下書き（§2 の `drafts-before.sha`。§9 の切り替え直しなら、その回の §2）のパスが、Rust の期間に消えていない**（`comm` の行数が 0。Rust の期間に保存して内容が変わった・増えたものは可）。0 でなければ、消えた件数をユーザーに報告する。`rs-end-counts.txt` と `rs-end-quality.tsv` が**品質の切り戻しの基準**（§8.4。切替の前の値ではなく、Rust から戻す直前の値）。
  - `cache_load.py`（Python 版自身の `store.load_cache` と `purge_cache_legacy` を、メモリの上だけで動かす）が `rc=0` で、`python_loaded` と `file_keys` が**同じ**（`load_cache` は読めないとき黙って空にするため、別に JSON として読んだ件数と照らす）。`python_will_drop` は起動時に Python が捨てる現役でないキーの数（件数だけ記録する）。
- 失敗したら:
  - `counts`・`state`・`quality` が `unable to open database file`（停止直後に `-shm` が無い）なら、§4.3 の「コピーして取る」と同じ要領でコピーから取る。DB が開けない・`malformed` なら **DB を疑う**（§10 の復元。失うものを先に読む）。
  - `cache_load.py` が `rc≠0`（JSON として読めない・辞書でない・件数が合わない）でも、**配信の回復を優先して Python を起動してよい**（Python は黙って空のキャッシュで始め、最初の保存で置き換える。キャッシュは地名の再問い合わせで作り直される）。`cache-rs-end.json` に退避してあるので、記録して続ける。
  - 控えが取れないまま Python を起動するとき（`rs-end-*` が無い）は、§8.4 の比べる基準を `precutover-counts.txt`・`precutover-state.tsv`（§6.8）に替え、「Rust の期間の操作の分だけの差」を許す（品質の群の `quality_diff.py` は基準が無いので、`counts diff` の件数だけで見る）。ユーザーに伝える。

### 8.3 Python を起動する

```sh
systemctl unmask geoframe-server
systemctl daemon-reload
systemctl show -p FragmentPath -p DropInPaths -p EnvironmentFiles geoframe-server    # drop-in と 2 つの EnvironmentFile
systemctl enable --now geoframe-server
systemctl is-active geoframe-server                # active
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/admin/status    # 200（Python の確認は deploy-pull.sh と同じ /api/admin/status）
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/v1/photos       # 200
TB=$(date +%s); T_PY=$(date '+%F %T'); echo "$T_PY"; echo "戻すのにかかった時間 $((TB - TR)) 秒"
```

- `show` の確認が**起動の前**であることが重要（drop-in が読まれていなければ、AI が有効な Python が起動して課金・送信が始まる）。`EnvironmentFiles` に `geoframe-py-rollback`（`ignore_errors=no`）が出なければ、`enable --now` をしない。
- 失敗したら（Python が起動しない）: `journalctl -u geoframe-server -n 50 --no-pager > "$WORK/py-journal.txt"` に保存してから読む（Python 版のログは写真のファイル名・パスを含みうる。§0.5）。典型: ①`/etc/default/geoframe-py-rollback` が無い（`-` 無しなので起動しない）、②`/opt/geoframe/app` の権限（`status=200/CHDIR`）、③8080 を Rust が持っている（`ss -ltnp 'sport = :8080'`。Rust が止まっていない）。報告には終了コードと `status=` の種類だけを写す。
- **Python が 200 を返したら、ここで [§4.5](#45-バックアップのタイマーを戻す共通の出口) を実行する**（§4.1 でタイマーを止めた経路は、ここで戻さないと止まったままになる。§5 の失敗・§6 の判断・§9 の途中のすべて）。

### 8.4 確かめる

**ユーザーが管理画面で操作を始める前に**行う（操作が入ると比べる基準がずれる）。

```sh
SINCE="$T_PY"
PID=$(systemctl show -p MainPID --value geoframe-server)
tr '\0' '\n' < /proc/$PID/environ | grep -E '^GEOFRAME_(AI_ENABLED|LOC_SUGGEST)='    # 両方 =0
journalctl -q -u geoframe-server --since "$SINCE" -p err --no-pager | wc -l                   # 例外の行数: 0（件数だけ。行は画面に出さない。-q が無いと、無いときの「-- No entries --」を 1 行と数える）
journalctl -u geoframe-server --since "$SINCE" --no-pager | grep -ciE 'gemini|vertex|aiplatform|embed|decision|loc_suggest'   # AI を示す語の行数: 0（件数だけ）
journalctl -u geoframe-server --since "$SINCE" --no-pager | grep -F 'レガシーキー削除'          # 件数だけの行（「N件 / 現役M件を維持」）。出なくてもよい
sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe GEOFRAME_AI_ENABLED=0 GEOFRAME_LOC_SUGGEST=0 \
    "$PYAPP/.venv/bin/python" -I -B - "$PYAPP" < "$WORK/cache_load.py"; echo "cache rc=$?"     # rc=0・python_will_drop=0
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/counts.sql" > "$WORK/py-after-counts.txt"
diff "$WORK/rs-end-counts.txt" "$WORK/py-after-counts.txt"; echo "counts diff rc=$?"
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/state_snapshot.sql" > "$WORK/py-after-state.tsv"
python3 -I "$WORK/state_diff.py" "$WORK/rs-end-state.tsv" "$WORK/py-after-state.tsv"; echo "state rc=$?"
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/quality_snapshot.sql" > "$WORK/py-after-quality.tsv"
python3 -I "$WORK/quality_diff.py" "$WORK/rs-end-quality.tsv" "$WORK/py-after-quality.tsv"; echo "quality rc=$?"     # 件数だけ。品質の群の変化を、写真の変更・特徴量の変更と分ける
"$WORK/drafts_sha.sh" "$WORK/drafts-py-after.sha"; diff "$WORK/drafts-rs-end.sha" "$WORK/drafts-py-after.sha" | grep -c '^[<>]'     # 0
curl -fsS $BASE/api/v1/photos -o "$WORK/py-feed-2.json"; python3 -I "$WORK/check_files.py" "$WORK/py-feed-2.json"
```

- 確かめること:
  - **Python の AI が無効**: プロセスの環境の `GEOFRAME_AI_ENABLED`・`GEOFRAME_LOC_SUGGEST` が `0`（共通の `/etc/default/geoframe` が `1` でも、drop-in が勝つ）。journal の例外の行数が 0、AI を示す語の行数が 0。**0 でないときは、行を画面に出さず**、`$WORK/py-journal.txt` に保存してユーザーと読み、送信か定型の「無効」の行かを区別する（Python 版のログの文言は演習で出たものを見本にする）。
  - **`.cache.json` を Python の読み込みの経路で確かめた**: `cache_load.py` が `rc=0`、`python_will_drop=0`（起動時の掃除が済んでいる）、`file_keys` が §8.2 の `python_keeps` 以上。
  - **手動の位置・お気に入り・除外・回転が、ID ごとに保たれている**: `state_diff.py` が「値が違う 0・前だけ 0・後だけ 0」（`rs-end-state.tsv` との比較。Rust の期間にユーザーが付けた分も含めて、戻す直前と同じ）。件数（`counts diff`）は `manual`・`favorite`・`hidden`・`rotated` の行が同じ。`photos` の行だけが増えているのは Python の取り込みが動いた（新しい写真が入ったので、下の品質の 2 が後で走る）。
  - **旅行ブログの下書きの全ファイルが同じ**: `diff | grep -c` が **0**（ハッシュも件数も、消えたものも増えたものも無い）。
  - 画像の欠けが 0。管理画面が動く（ユーザーが確かめる）。
  - 報告は**不一致の件数だけ**（座標・パス・ファイル名を書かない）。
- 品質（簡易品質）の切り戻し — [server.md §16.10](server.md#1610-rustで作った結果をpythonへ切り戻すp5のロールバック)。**比較の基準は Rust から戻す直前の値**（`rs-end-counts.txt`・`rs-end-quality.tsv`・止める前の一覧 `rs-end-feed.json`。切替の前の値ではない）。Python が再構築したかは、**基準の `algorithm` が `rust-phash-v1:…` のとき（Rust の期間に品質の再構築が走ったとき）だけ** `algorithm` で見分けられる（全行が `rust-phash-v1:…` のままなら再構築していない、全行が `poc-phash-v1:…` に変わっていれば再構築した）。基準が元から `poc-phash-v1:…` のときは見分けられないので、`photo_quality_dirty` が `0` に戻ったことと `photo_quality` の `max(analyzed_at)` の変化で確かめる（T5.4 の演習はこの場合）。次の**2つの場合を分けて**記録する:
  1. **そのまま戻した直後**（Python がまだ再構築していない）: Python は前回の `photo_quality` を保つので、`counts diff` の `quality_rows`・`quality_groups`・`kind:` の行は基準と**同じ**（取り込みで `photos` が増えていても同じ。新しい写真は、解析が終わって再構築されるまで品質の行に入らない）。`algorithm` は基準（Rust から戻す直前）と同じ（Rust の期間に再構築が走っていれば `rust-phash-v1:…`、走らなければ切替の前の `poc-phash-v1:…` のまま。T5.4 の演習は後者）。`/api/v1/photos` の件数が、止める前の件数（`rs-end-feed.json`）から、§6.2 で記録した「既知の差」の件数（Python が再び隠す分）を引き、Python の最初のサイクルで表示できるようになった写真を足した数と一致する（既知の差も、表示できるようになった写真も 0 件なら同じ）。表示できるようになった写真は、`py-feed-2.json` と `rs-end-feed.json` を `ids.py` と `comm -13` で突き合わせて数える。**取り込みで増えた写真だけでなく、位置の推定（`infer`）で位置が付いた既存の写真も入る**（Rust の期間に付けた手動の位置が錨になり、Rust の次のサイクルより先に Python のサイクルが近くの写真に位置を付ける。`photos` の件数は増えない。演習では 3 件で、すべて `loc_source='inferred'`）。
  2. **再構築が走った後**（新しい写真が入る・何かで `dirty` になる）: Python は**今の特徴量の全体から作り直す**ので、写真が増減・更新していれば、件数・グループ数・`kind` が基準と違うのは正常。**不変を求めるのは、写真と特徴量が固定されているときだけ**。`quality_diff.py` の出力で次のとおり分けて確かめ、記録する:
     - **写真の変更も特徴量の変更も 0 件**（`写真: 追加 0 削除 0 更新 0 / 特徴量: 追加 0 削除 0 更新 0`。演習が `dirty` だけを立てる場合、または本番で取り込み・削除・更新も、未解析分の解析も無かった場合）: `quality_rows`・`quality_groups`・`kind:` の件数は基準と**同じ**で、`quality_diff.py` が「**変わらない**」だけ（`入力の変更に連なる変化 0`・`説明できない変化 0`。顔ぶれ・`kind` は変わらない）。変わりうるのは順位・推奨・スコア・`group_id` の振り直しと、完全重複のグループで表に出る写しの入れ替わりだけ（`quality_diff.py` は顔ぶれと `kind` だけを見るので、これらは差として出ない）。
     - **写真か特徴量の変更があった**（本番で取り込みを伴った場合。**取り込みが無くても、Rust の期間に解析し切れなかった既存の写真を Python が解析すれば、特徴量の追加が起きて群ができる**。これは正常）: 件数が基準と違ってよい。差を**入力の変更の分と、それ以外に分ける**: ①`写真: 追加 a 削除 d 更新 u / 特徴量: 追加 fa 削除 fd 更新 fu` を、**別々に**記録する（写真の更新は、同じ ID の `display`・`md5`・`mtime`・`taken_at` の変化で、回転も入る。`taken_at` は 3 分の窓の判定に使われるので群を変えうる。特徴量は、今の写真に合っている有効な行〈SHA・pHash〉の増減と、同じ ID での SHA・pHash の変化）。②`説明できない変化` が **0**（顔ぶれか `kind` が変わった群は、すべて、写真か特徴量が変わった ID を含む群だけ。新しい写真や、未解析だった写真が既存の群に入る・新しい群を作る・削除や更新で群が縮む／消える／分かれる、は説明できる）。③`quality_rows`・グループ数・`kind:` の「前→後」を記録する。`説明できない変化` が 0 でなければ失敗として扱う（入力が変わっていない群が変わっている。ユーザーに件数だけ報告し、`quality_diff.py` の出力を記録する）。
  - **本番の確認と演習の違い**: 本番では 1 を必ず記録し、2 は**自然に走ったときだけ**記録する（走らなければ「走らなかった」と記録する。本番の DB を `dirty` にして走らせない）。**T5.4 の演習では両方を実施・記録する**（2 は演習の複製の DB だけで `photo_quality_dirty` を立てて再構築させる。このときは写真と特徴量が固定なので「写真の変更も特徴量の変更も 0 件」の側で確かめる。手順は [§15](#15-演習t54のチェックリスト)）。
  - Python だけで解析した結果に戻したいとき（Rust の結果を保つ方法ではない）は、**Python を起動する前に**、server.md §16.10 の手順 2（`photo_quality_features` の Rust が解析した行の削除）を、ユーザーの許可を得て行う。件数・グループが変わりうる。
- 失敗したら: 件数が減った・手動の列が消えた（`state_diff.py` が 0 でない）・下書きが消えたときは、**DB を疑う**（§10 の復元。失うものを先に読む）。`cache_load.py` が `rc≠0` なら `cache-rs-end.json` と見比べて記録する（キャッシュは作り直せる）。配信が動いているなら、まず原因を調べる。

### 8.5 戻したあとの注意

- **checkout の固定の印（`/etc/geoframe/checkout-pinned`）は外さない**（Python へ戻したあとも `deploy-pull.sh` は P5b 以降を取り込まない）。Python の更新が要るときは、`/opt/geoframe` を P5a のマージのコミットへ**手で**戻す: `git -C /opt/geoframe reset --hard "$COMMIT" && systemctl restart geoframe-server`。
- Rust のバイナリ・unit・`/etc/default/geoframe-rs` は残す（mask されているので起動しない）。Python のあいだは `GEOFRAME_INGEST=1` の書き込みは動かない。
- `geoframe-backup.timer` が動いていること: §8.3 の最後に [§4.5](#45-バックアップのタイマーを戻す共通の出口) を実行した（`タイマー OK`）。
- 原因と戻した時刻をユーザーに報告し、`p5-acceptance.md` に記録する。

## 9. 切り替え直す

目的: Python へ戻したあと、原因を直してもう一度 Rust にする。許可を得て。**Python が動いているうちに控えを取り直す**（止めたあとはフィードを取れない）。

1. **`geoframe-rs` を unmask する**（`--check-only` も `--production` も、mask を理由に断る）。Rust は `disabled`（または unit が無ければ `not-found`）になり、起動しない:

```sh
systemctl unmask geoframe-rs
systemctl is-enabled geoframe-rs                   # disabled（unit が残っているとき）／ not-found（失敗で外れたとき）
systemctl is-active geoframe-rs                    # inactive
```

2. **§2 をやり直す**: Python の `/api/v1/photos` の ID の一覧・件数・STATUS・下書きのハッシュを控え直す（Python のあいだに写真が増減している）。`--check-only` を通す（`--expect-min-photos` も新しい件数から）。
3. **Python を止めて mask する**（drop-in は残したまま。ここで T0 を取り直す）:

```sh
T0=$(date +%s); date '+%F %T'
systemctl stop geoframe-server
systemctl disable geoframe-server || true
systemctl mask geoframe-server
systemctl is-active geoframe-server; systemctl is-enabled geoframe-server    # inactive / masked
```

4. **§4 の保全を取り直す**（`$TS` を新しくする。名前は `p5-precutover-…` のままでよい。前回のものは消さない）。`geoframe-backup.timer` を止めてから（§4.1。**止めたタイマーは、どの出口でも §4.5 で戻す**）。
5. **§5 → §6** を実行する（T1 と時間も記録し直す）。バイナリは同じものでよい（`--expect-sha256` は同じ）。

**3 以降で止まって Python へ戻すとき**: §3 の「止まったとき」は使わない（それは drop-in を外して AI が有効の切替の前の状態へ戻す手順で、ここでは AI が有効な Python を起動してしまう）。**§8（8.1 は Rust が動いていなければ省く）の 8.3 で、drop-in を残したまま Python を起動**し、Python が 200 になったら §4.5 でタイマーを戻す。

`p5-rollback.conf` は Python だけに効くので、Rust の AI の設定は変わらない。checkout の固定の印はそのまま。

## 10. 復元（最後の手段）

DB が壊れた・開けないときの、バックアップからの復元。**既定の戻しは DB を保ったまま**（§8）。復元は、戻したあとの DB が壊れていると分かったときだけ。許可を得て。

**復元で失うもの**: 復元に使うバックアップの時点より**後の**変更。手動の位置・お気に入り・除外・回転、取り込み（写真の追加・削除・位置の推定・地名）、AI の結果（Embedding・手がかり。ローカルの再計算で戻る）、品質の解析。旅行ブログの下書きと `.cache.json` は DB の外なので失われない。最新の `p5-precutover-*.db.gz`（切替の直前）なら、切替の前までの状態。

### 10.1 止める

```sh
CUR=geoframe-rs                                    # いま動いている方（Python なら geoframe-server）
systemctl stop "$CUR"
systemctl stop geoframe-backup.timer
systemctl is-active geoframe-backup.service        # inactive
"$WORK/dbopen.sh"; echo "dbopen rc=$?"             # rc=0（DB を開いているプロセスが無いと確認できた）。rc=1・2 は先へ進まない（§0.5）
```

- **ここで中断したときも、サービスを起動し直したあとにタイマーを戻す**（[§4.5](#45-バックアップのタイマーを戻す共通の出口)）。

### 10.2 いまの DB を 3 つ一緒に退避する

同じファイルシステムの中で `mv` する（コピーしない）。`-wal`・`-shm` を残すと、新しい DB に古い WAL が当たって壊れる。

```sh
TS=$(date +%Y%m%d-%H%M%S)
ASIDE=/var/lib/geoframe/.p5-aside-$TS
install -d -m 0750 -o geoframe -g geoframe "$ASIDE"
cd /var/lib/geoframe
for f in catalog.db catalog.db-wal catalog.db-shm; do [ -e "$f" ] && mv "$f" "$ASIDE/"; done
ls catalog.db* 2>&1                                # 残っていない（No such file）
ls -l "$ASIDE"
```

### 10.3 一時名へ展開して検査する

展開が途中で失敗したファイルを DB として使わないよう、**展開の終了コードと「空でないこと」を条件にし、失敗したら一時ファイルを消して止める**。

```sh
BK=/var/backups/geoframe/p5-precutover-<時刻>.db.gz          # 復元に使うバックアップ
TMP=/var/lib/geoframe/.catalog.db.restore-$TS                  # 同じディレクトリの一時名
DUMPSHA=$(cat "$WORK/precutover.dumpsha")                      # 取ったときに控えた値（§6.8）。他のバックアップなら、$WORK に別に展開して dumpsha.sh で取った値
if gzip -t -- "$BK" && gzip -dc -- "$BK" > "$TMP" && [ -s "$TMP" ] \
   && chown geoframe:geoframe "$TMP" \
   && { chmod --reference="$ASIDE/catalog.db" "$TMP" 2>/dev/null || chmod 0640 "$TMP"; }; then
    echo "展開 OK"
else
    rm -f -- "$TMP"; echo "NG: 展開に失敗した。一時ファイルを消した。先へ進まない（いまの DB は $ASIDE にある）"
fi
```

`展開 OK` のときだけ、検査する（どちらも失敗したら、その一時ファイルを置かない）:

```sh
RESTORED=$(sudo -u geoframe sqlite3 "file:$TMP?immutable=1" 'PRAGMA integrity_check'); echo "integrity_check: $RESTORED"     # ok の 1 行
DUMPNOW=$("$WORK/dumpsha.sh" "file:$TMP?immutable=1" geoframe); echo "dump sha256: $DUMPNOW (rc=$?)"                    # 上流の失敗・空の出力は rc≠0
if [ "$RESTORED" = ok ] && [ -n "$DUMPNOW" ] && [ "$DUMPNOW" = "$DUMPSHA" ]; then echo "検査 OK（integrity_check ok・.dump が一致）"
else rm -f -- "$TMP"; echo "NG: 検査に失敗した。一時ファイルを消した。別のバックアップを試すか、\$ASIDE へ戻す"; fi
```

- 確かめること: `integrity_check` が `ok`。`.dump` の SHA-256 が、**取ったときに控えた値**（`p5-precutover-*` なら §6.8 の `$WORK/precutover.dumpsha`）と一致する。他のバックアップ（`catalog-*.db.gz`）は控えが無いので、`$WORK` に別に展開して同じ方法（`dumpsha.sh`。root の `sqlite3`）でハッシュを取り、照合の基準にする（`/var/lib/geoframe` の外で）。
- 失敗したら: 展開に失敗した・一致しなければ、その一時ファイルを置かない（上のコマンドが消す）。別のバックアップを試す。**いまの DB は `$ASIDE` にあるので、戻せる**（`mv "$ASIDE"/catalog.db* /var/lib/geoframe/`）。

### 10.4 置き換えて、所有者を確かめる

**§10.3 が `検査 OK` のときだけ**行う。

```sh
mv -f "$TMP" /var/lib/geoframe/catalog.db
ls -l /var/lib/geoframe/catalog.db*                                  # すべて geoframe:geoframe
find /var/lib/geoframe -maxdepth 1 ! -user geoframe -print           # 空（root 所有が混ざっていない）
```

**ここまでが起動の前の確認**（`integrity_check`・`.dump` の一致・所有者）。

### 10.5 起動して確かめる

DB を別の inode に置き換えたので、**必ず（再）起動する**（古いファイルを開いたままにしない）。

```sh
systemctl start "$CUR"
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/v1/photos     # 200
curl -fsS $BASE/api/v1/photos -o "$WORK/restored-feed.json"
python3 -I "$WORK/check_files.py" "$WORK/restored-feed.json" "$WORK/missing-ids.txt"        # 画像の欠け（200でない）の数。ID は missing-ids.txt にだけ
sudo -u geoframe sqlite3 'file:/var/lib/geoframe/catalog.db?mode=ro' < "$WORK/counts.sql"
```

- 確かめること: 配信が 200。表示の画像の欠けが 0（`200でない 0`）。0 でなければ、下の「画像の欠けの直し方」へ進む。
- **画像の欠けが起きる理由**: 回転は新しい画像を作って古い画像を消す。DB だけを古い時点に戻すと、`display` が消えた画像を指す行ができる。`fetch_derivatives` は `source='drive' AND display IS NULL` の行だけを取り直すので、**取り込みのサイクルでは自動では直らない**。管理画面の回転は、今の画像が無いと 400 で、回転では直らない（演習で確かめた）。件数（`check_files.py` の「200でない」と、ID を書いた `$WORK/missing-ids.txt` の行数）を記録する。

#### 画像の欠けの直し方（T5.4 の演習で確定）

DB への書き込みなので、**ユーザーの許可を得て**、サービスを止めて `geoframe` ユーザーで行う。手順の 1〜4 は T5.4 の演習で、バックアップのあとに回転した 1 件を欠けさせて通した（[p5-acceptance.md](p5-acceptance.md) §7）。**Drive から取り直す部分（4 の「本番」）は、鍵を置かない演習では確かめられなかった**ので、本番では**対象を 1 件に限って先に取り直し**、200 になり向きが合うことを確かめてから残りへ広げる。

1. **対象の限定と想定との照合**（読み取りだけ）:

```sh
DB_RO='file:/var/lib/geoframe/catalog.db?mode=ro'
IDS=$(paste -sd, "$WORK/missing-ids.txt")
sudo -u geoframe sqlite3 "$DB_RO" "SELECT id FROM photos WHERE source='drive' AND id IN ($IDS)" | LC_ALL=C sort > "$WORK/fix-targets.txt"
sudo -u geoframe sqlite3 "$DB_RO" "SELECT count(*) FROM photos WHERE source<>'drive' AND id IN ($IDS)"     # 0（drive でない欠けが無い）
sudo -u geoframe sqlite3 "$DB_RO" "ATTACH 'file:$ASIDE/catalog.db?mode=ro' AS a; SELECT p.id FROM photos p JOIN a.photos q ON q.id=p.id WHERE p.display IS NOT q.display" \
    | LC_ALL=C sort > "$WORK/fix-expected.txt"      # 想定: 退避した DB（§10.2）と display が違う行＝バックアップのあとに回転・取り直しをした写真
wc -l < "$WORK/missing-ids.txt"; wc -l < "$WORK/fix-targets.txt"; wc -l < "$WORK/fix-expected.txt"
comm -23 "$WORK/fix-targets.txt" "$WORK/fix-expected.txt" | wc -l                                           # 0（想定の外の欠けが無い）
```

- 確かめること: 欠け＝対象（`drive` の行）の件数、`drive` でない欠けが 0、**想定の外の欠けが 0**（欠けた写真はすべて、バックアップのあとに `display` が変わった写真）。ID は `$WORK` のファイルにだけ置き、画面には件数だけを出す。
- どれかが外れたら進まない（復元で巻き戻った回転では説明できない欠けで、derivatives が別の理由で消えている）。ユーザーに件数を報告し、`$ASIDE` の DB に戻すかを決めてもらう。`local` の行の欠けは、この手順の対象外（本番の写真はすべて `drive`。演習の本番コピーでも `local` は 0 件だった）。

2. **取り直しの印を付ける**（`display` だけを NULL にする。**`rotation` は変えない**。`fetch_derivatives` は行の `rotation` を当てて、回転の API と同じ名前・同じ向き（`derivatives/<drive_id>[.r<回転>].jpg`）で作り直す。`fetched_at` は取り直しで書き直されるので触らない）:

```sh
TGT=$(head -n 1 "$WORK/fix-targets.txt")        # 本番は 1 件から。4 で確かめたら TGT=$(paste -sd, "$WORK/fix-targets.txt") で残りを同じ手順で
fixrows() { sudo -u geoframe sqlite3 -separator "$(printf '\t')" "$DB_RO" \
  "SELECT id, source, ifnull(rotation,0), ifnull(lat,''), ifnull(lon,''), ifnull(loc_source,''), favorite, hidden, ifnull(md5,''), ifnull(drive_id,'') FROM photos WHERE id IN ($TGT) ORDER BY id"; }
fixrows > "$WORK/fix-before.tsv"
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/state_snapshot.sql" > "$WORK/fix-state-before.tsv"
curl -fsS $BASE/api/admin/status | python3 -I "$WORK/status_counts.py" | grep '^db.no_display='     # 印を付ける前の値を控える
date '+%F %T'                                    # 4 の journal の起点
(
  trap 'systemctl start "$CUR"; echo "[fix] $CUR を起動し直した rc=$?"' EXIT
  trap 'exit 130' HUP INT TERM
  systemctl stop "$CUR" && "$WORK/dbopen.sh" \
    && sudo -u geoframe sqlite3 /var/lib/geoframe/catalog.db "UPDATE photos SET display=NULL WHERE source='drive' AND id IN ($TGT); SELECT changes();"
)
systemctl is-active "$CUR"; curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/healthz      # active / 200
```

- 確かめること: `changes()` の値が `TGT` の件数と同じ。`dbopen.sh` が rc=0 でなければ（開いているものがある・確認できない）、`UPDATE` は動かずにサービスが起動し直される。

3. **回転と人の列が保たれたこと**:

```sh
fixrows > "$WORK/fix-after.tsv"
cmp -s "$WORK/fix-before.tsv" "$WORK/fix-after.tsv" && echo "対象の行: display 以外は同じ"
sudo -u geoframe sqlite3 "$DB_RO" < "$WORK/state_snapshot.sql" > "$WORK/fix-state-after.tsv"
notgt() { awk -F'\t' 'NR==FNR{t[$1];next} !($1 in t)' <(tr ',' '\n' <<< "$TGT") "$1"; }
python3 -I "$WORK/state_diff.py" <(notgt "$WORK/fix-state-before.tsv") <(notgt "$WORK/fix-state-after.tsv"); echo "state rc=$?"   # 対象以外: 値が違う 0・前だけ 0・後だけ 0
```

- 確かめること: 対象の行の `rotation`・手動の位置（`lat`・`lon`・`loc_source`）・お気に入り・除外・`md5`・`drive_id` が変わらない（`cmp` が同じ）。対象以外の写真の手動の位置・お気に入り・除外・回転・`display` が変わらない（`state rc=0`）。

4. **取り直しと再確認**:

```sh
journalctl -u "$CUR" --since '<2 の date の時刻>' --no-pager | grep -E 'fetch_derivatives|サイクル完了'
curl -fsS $BASE/api/admin/status | python3 -I "$WORK/status_counts.py" | grep '^db.no_display='
curl -fsS $BASE/api/v1/photos -o "$WORK/fix-feed.json"; python3 -I "$WORK/check_files.py" "$WORK/fix-feed.json"
python3 -I "$WORK/ids.py" "$WORK/fix-feed.json" | LC_ALL=C sort | comm -12 - <(tr ',' '\n' <<< "$TGT" | LC_ALL=C sort) | wc -l    # フィードにある対象の数
sudo -u geoframe sqlite3 "$DB_RO" "SELECT count(*) FROM photos WHERE id IN ($TGT) AND display = 'derivatives/' || drive_id || CASE WHEN ifnull(rotation,0)=0 THEN '' ELSE '.r' || rotation END || '.jpg'"   # 名前が rotation と合う数
```

- **演習（鍵なし）で確かめたこと**: 起動直後のサイクルが `fetch_derivatives: 省略（Drive の鍵が無いので飛ばします）` で、`サイクル完了` の「取得待ち」が対象の件数だけ増え、`db.no_display` も同じだけ増えた。対象は `display` が無いのでフィードから外れ（フィードにある対象 0）、`check_files.py` の「200でない」は 0 になった（miharashi に壊れた画像を渡さない）。取り直しの処理本体が、回転した写真を同じ名前・同じ向きで取り直すことは `cargo test` の `a_rotated_photo_is_fetched_again_with_the_same_name_and_orientation`（`crates/geoframe/src/drive/tests.rs`）で確かめた。
- **本番（鍵あり）で確かめること**: 起動直後のサイクル（以降は `GEOFRAME_DRIVE_POLL_MIN` ごと。1 サイクル `GEOFRAME_FETCH_PER_CYCLE` 枚まで）で `fetch_derivatives` が `fetched` に対象の件数を数え、`db.no_display` が 2 の前の値に戻る。フィードにある対象の数と、名前が `rotation` と合う数が、どちらも `TGT` の件数。`check_files.py` の「200でない」が 0。**1 件目は、管理画面でその写真の向きが復元した DB の回転と合うことをユーザーに見てもらう**（向きが合わない・取り直されない・`fetch_derivatives` の警告が出たら、残りへ広げずに報告する）。1 件目が通ったら、`TGT` を残り全部にして 2〜4 を繰り返す。
- 取り直しが Drive の不調で進まないときは、配信は続く（対象はフィードに出ないだけ）。対象が残っているあいだは `db.no_display` が戻らないので、数日のうちに戻ることを §11 の確認に足す。

- 失敗したら: 起動しない・画像の欠けが多い・件数が想定と違うときは、`$ASIDE` の DB に戻せる（止める→3 つを入れ替える→起動）。原因をユーザーに報告する。

### 10.6 タイマーを戻す

[§4.5](#45-バックアップのタイマーを戻す共通の出口) のコマンドを実行する（`タイマー OK`）。

**途中で止めたときも、サービスを起動し直したあとに戻す**（§10.1 で止めたまま終わらない）。`$ASIDE` は P5 の記録が終わるまで消さない（ユーザーの許可で消す）。

## 11. 切替のあとの数日

目的: 切替の直後には終わらない処理が終わったことと、定期の仕組みが動いていることを確かめる。読み取りだけ（ユーザーの包括的な許可でよい）。

### 11.1 Decision の全件再判定（約 4 時間）

Rust は旧 Python の手がかりを信用せず全件を判定し直す。入口の Decision は CPU で動き、1 件約 12 秒・約 4 コアで、約 1,200 件で約 4 時間、本番機の CPU を使い続ける（承認済み）。

```sh
/opt/geoframe/deploy/geoframe-admin.sh ai-jobs status | grep -E '位置の手がかり|location_clue|待ち'
curl -fsS $BASE/api/admin/status | python3 -I "$WORK/status_counts.py" | grep 'db.loc.clue'
```

- 確かめること: `db.loc.clue_pending` が減っていき、**0 になる**（`clue_runnable` も 0）。`clue_pause` が空のまま。`ai-jobs status` の位置の手がかりの待ちが 0。
- 止まっているとき: `clue_runnable` が 0 で `clue_pause` が非空なら用途の停止（§7 の `release-pause --task location_clue`）。`clue_pending` が減らず `clue_runnable` が 0 でないなら Decision（§1.7 の `health`）。品質の解析待ちだけが残るときは `clue_runnable` が 0 になる（品質の解析が進めば減る）。

### 11.2 索引の公開（約 48 分の意味検索の 503）

新しい空間の Embedding が、構築開始時の対象の待ちが 0 になるまで**公開されない**。公開までは意味検索（E6 の非空の検索語）が 503 になる（承認済み。約 6,500 件で約 48 分）。

```sh
curl -fsS $BASE/api/ai/status | python3 -I -c 'import json,sys; p=json.load(sys.stdin).get("pipeline",{}); print({k:p.get(k) for k in ("index_state","indexed","pending")}, "building=", p.get("building"))'
/opt/geoframe/deploy/geoframe-admin.sh ai-jobs status | grep -E '公開前の空間|公開の条件'
```

- 確かめること: E5（`/api/ai/status`）の `pipeline.building` が**消える**（`index_state` が `active`）。それまでは `building` に `eligible`・`indexed`・`pending`・`coverage`・`publish`（`waiting`／`ready` など）が出る。公開されたら、管理画面の意味検索が結果を返す。
- 長引くとき: `failed_excluded` の理由の固定コードと `publish`（`below_threshold` は成功率が 0.95 未満）を [ai-index.md](ai-index.md) で見る。

### 11.3 バックアップのタイマー

```sh
systemctl list-timers geoframe-backup.timer
systemctl show geoframe-backup.service -p Result -p ExecMainStatus      # Result=success
journalctl -u geoframe-backup.service -n 20 --no-pager | cut -c1-160
ls -l /var/backups/geoframe | tail -5
gzip -t /var/backups/geoframe/latest.db.gz && echo "latest: gzip OK"
```

- 確かめること: Rust の稼働中の最初のバックアップが成功した（`Result=success`・`integrity_check: ok`・`latest.db.gz` が更新）。

### 11.4 journal の警告

```sh
journalctl -q -u geoframe-rs --since '<T0の日>' -p warning --no-pager | cut -c1-160 | sort | uniq -c | sort -rn | head
systemctl show geoframe-rs -p NRestarts -p Result
```

- 確かめること: 警告が定型文（段の名前・種類・件数）で、同じ警告が増え続けていない。`NRestarts` が 0 か、説明できる。

### 11.5 `clue_runnable` と毎朝のタイマー

- Decision が常駐しているあいだ、`geoframe-loc-decision.timer`（Decision を動かすユーザーのユーザー unit）は「人が起動済み」として何もしない。**確認のために `systemctl --user start geoframe-loc-decision.service` を叩かない**（Decision を起動しうる）。
- 常駐をやめる運用に戻したときは、タイマーが STATUS の `db.loc.clue_runnable`（無ければ `clue_pending`）で起動を判断し、起動後は総待ちの減り方・`clue_pause`・15 分の停滞で止める（[ai-location-clue.md](ai-location-clue.md#timerの読み方loc_decision_morningpy)）。品質の待ちだけが残ると約 16 分で終了コード 1（既知）。

## 12. クラウド AI の有効化（数日後）

目的: §11 の確認が済んだあと、ユーザーが決めた日に、写真比較（`photo_judge`）と撮影地の候補（`location_suggest`）のクラウド AI を、**日の上限を小さくして**有効にする。費用と画像の送信を伴うので、ユーザーの許可・日の上限・開始日を決めてから。

**前提**: §11.1〜§11.4 が済んでいる。鍵の所有者とモードが `600 geoframe`（§1.6。違えば `Unauthorized` 停止）。鍵に使う Vertex の権限を確かめてある（403 の詳しい理由はログに出ない）。

### 12.1 AI 設定の JSON を直す

```sh
cp -p "$AI_CFG" "$WORK/ai-config.before"          # 直す前の控え（中身は読まない・文書に写さない）
${EDITOR:-vi} "$AI_CFG"
stat -c '%a %U:%G' "$AI_CFG"                       # 640 root:geoframe のまま
python3 -I -c 'import json,sys; json.load(open(sys.argv[1])); print("JSON OK")' "$AI_CFG"
```

`tasks` に次の形を足す（キーの意味は [ai-config.md](ai-config.md)。値はユーザーと決める）。**未知のキー・重複したキーがあると AI 全体が無効になる**（配信は続く）。

```json
"photo_judge": {
  "provider": "cloud", "model": "<モデル>",
  "allow_cloud": true, "daily_request_limit": <日の上限（小さく）>, "include_hidden": false,
  "credential_path": "<鍵の絶対パス>", "project": "<project>", "region": "<region>"
},
"location_suggest": {
  "provider": "cloud", "model": "<モデル>",
  "allow_cloud": true, "daily_request_limit": <日の上限（小さく）>,
  "credential_path": "<鍵の絶対パス>", "project": "<project>", "region": "<region>"
}
```

- `daily_request_limit` が未設定か 0 だと**送らない**。`allow_cloud` が無いと送らない。位置の 2 用途は共通の `/etc/default/geoframe` の `GEOFRAME_AI_ENABLED`・`GEOFRAME_LOC_SUGGEST` が `1` のときだけ動く。クラウドへ送るのは**画像（最大 6 枚）と撮影日**だけ（座標・地名・ファイル名・写真 ID は送らない。[ai-config.md](ai-config.md)）。

### 12.2 再起動して確かめる

設定の変更は再起動で反映する。

```sh
systemctl restart geoframe-rs
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/healthz           # 200
/opt/geoframe/deploy/geoframe-admin.sh ai-jobs status               # photo_judge・location_suggest が「使える」
curl -fsS $BASE/api/admin/status | python3 -I "$WORK/status_counts.py" | grep -E 'ai_'
```

- 確かめること: `ai_config_invalid=なし`、`ai_task_blocks=なし`、`ai_pauses=なし`。2 つの用途が使えると出る。
- 失敗したら: `ai_config_invalid` があれば JSON の誤り（固定コード。[ai-config.md](ai-config.md)）。直前の控え（`$WORK/ai-config.before`）へ戻して再起動する。

### 12.3 見ていく

- 最初の日は、`ai-jobs status` の送信数が日の上限を超えないこと、journal の警告、費用の見込みを確認する。上限を上げるのはユーザーの判断（一度に上げすぎない）。
- **停止が出たとき**（`ai_task_blocks` が出る・`ai_pauses`・`ai-jobs status` に「停止中」）: 理由コード（`unauthorized`・`forbidden`・`model_not_found`・`contract_mismatch`）の原因を直す（鍵の所有者とモード、Vertex の権限、モデル名）。**直しても自動では戻らない**ので、§7 の形で `ai-jobs release-pause --task <photo_judge｜location_suggest>` を行う。
- やめるときは、`allow_cloud` を `false` にするか該当の用途を消して再起動する。

## 13. 戻しの猶予期間と、そのあとの片付け

**戻しの猶予期間**は、本番の Python の venv・Python 版の unit・checkout を残しておく期間。このあいだは Python へ数分で戻せる（§8）。**既定は「クラウド AI を有効にしてから 1 週間」**（ユーザーが変えてよい）。期間中、`/opt/geoframe` は P5b（Python を消すコミット）以降を取り込まない（固定の印が `deploy-pull.sh` を断る）。

### 13.1 期間が終わったら（片付け）

すべて**ユーザーの許可を得て**、一つずつ。先に最後の保全を取る（§4.3・§4.4 と同じ方法で。`geoframe-backup.timer` は止めない場合は `mode=ro` で足りる）。

1. **印を外す**: `rm /etc/geoframe/checkout-pinned`。
2. **`/opt/geoframe` を手で新しい main へ**（`deploy-pull.sh` の Python 版の部分は P5b で消えている）:

```sh
git -C /opt/geoframe fetch origin
git -C /opt/geoframe reset --hard origin/main
git -C /opt/geoframe rev-parse HEAD
test -x /opt/geoframe/deploy/backup-catalog.sh && echo "backup-catalog.sh あり"
```

3. **venv を消す**: `ls -ld /opt/geoframe/app/.venv` で対象を確かめてから `rm -rf /opt/geoframe/app/.venv`。`app/.env` は、`geoframe-rs` へ移した値が足りていることをユーザーと確かめるまで残す（消すかはユーザーが決める）。
4. **Python 版の unit・drop-in・設定を片付ける**:

```sh
systemctl unmask geoframe-server                   # /etc の mask のリンクが消える
rm -f /usr/local/lib/systemd/system/geoframe-server.service
rm -f /etc/systemd/system/geoframe-server.service.d/p5-rollback.conf
rmdir /etc/systemd/system/geoframe-server.service.d
rm -f /etc/default/geoframe-py-rollback
systemctl daemon-reload
systemctl status geoframe-server 2>&1 | head -3    # Unit ... could not be found
```

5. **確認**: `systemctl is-enabled geoframe-rs`（enabled）・`is-active`（active）、`/healthz` が 200。`Conflicts=geoframe-server.service` は unit が無くても害が無い。
6. **保全物の整理**（P5 の記録が終わってから）: `p5-precutover-*.db.gz`・`$WORK` の tar（秘密を含む）・控え・バイナリを、ユーザーの許可を得て消す。

### 13.2 猶予期間のあとに Python へ戻す（演習しない）

片付けのあと（venv・unit が無い）は、**数分では戻せない**。次が要り、ネットワークとビルドに時間がかかる（Pillow などの導入）。猶予期間の中で戻すかを判断するのはこのため。

1. `/opt/geoframe` を P5a のマージのコミットへ: `git -C /opt/geoframe fetch origin && git -C /opt/geoframe reset --hard "$COMMIT"`（Python の実装はそのコミットにある）。
2. venv を作り直す: [P5a のコミットの deploy/README.md §1](https://github.com/ysogabe/geoframe-ambient-display/blob/a804623029ad67d415692d08ea3182fd765eb2a4/deploy/README.md) の手順（`python3 -m venv app/.venv`・`app/.venv/bin/pip install -r app/requirements.txt`）。
3. Python 版の unit（そのコミットの `deploy/geoframe-server.service`）を `/usr/local/lib/systemd/system/` に置き、§3.3 の drop-in と `/etc/default/geoframe-py-rollback` を作り直す（AI は無効）。固定の印も置き直す。
4. §8（Rust を止めて mask → Python を unmask して起動 → 確かめる）。

## 14. 記録

`p5-acceptance.md`（T5.4 の演習と T5.5 の本番）に書く。**リポジトリ・文書・PR には、件数・ハッシュ・時間・commit・sha256 だけを書く。**

**書くもの**

- 日付、立ち会い、許可を得た段（事前にまとめて許可した範囲）。
- `COMMIT`（P5a のマージのコミット）と演習のコミット、その差が無いことの確認。バイナリの sha256。
- 件数: Python の写真の件数、Rust の件数、Python だけ（0）・Rust だけの件数と、その内訳（`extra.py` の取り込み・既知の差・**説明できない（0）**）と群の数（`ai_group` は空を除く）、`--expect-min-photos`、`db.loc.clue_pending` の初期値。保全の `.dump` の SHA-256、`photo_quality` の行数・グループ数（`algorithm` 別）、手動・お気に入り・除外・回転の件数。
- 戻した場合の確認: 切替の前にあった下書きが Rust の期間に消えた件数（§8.2 の `comm`）、`state_diff.py` の不一致の件数（値が違う・前だけ・後だけ）、下書きの全ハッシュの不一致の件数、`.cache.json` の読み込み（`file_keys`・`python_keeps`・`python_will_drop`）、品質の 2 つの場合（そのまま／`dirty` の再構築）の件数。基準は Rust から戻す直前の値。
- バックアップのタイマー: 止めた時刻と、戻した出口（§4.5 の表のどれか）、`タイマー OK` の確認。`dbopen.sh` が使った経路（`fuser`／`/proc`）。
- 時間: T0〜T1（停止から配信の再開まで）、`--wait-secs`、各段の時間。戻した場合は戻すのにかかった時間。
- 終了コード（`rust-deploy.sh`）、止める判断を使ったか、その理由。
- 取り込みの 1 サイクルの結果（表示可能／取得待ち／全の件数）、`clue_pending` が 0 になった時刻、索引の公開の時刻、最初のバックアップの成功。
- 見つかったこと（手順書の直し）と、直し方。

**書かないもの**

- ホスト名・LAN のアドレス・ユーザー名、写真のファイル名・座標・地名、設定の値（`GEOFRAME_*` の値・AI 設定の JSON の中身・URL・モデル名・project・region・鍵のパス）、鍵・トークン、差分の中身。これらは作業用の置き場（`$WORK`）に残し、記録が終わったらユーザーの許可で消す。

## 15. 演習（T5.4）のチェックリスト

検証環境（lxc-geoframe）で、本番の切替の前に、この手順書を通しで演習する。設計は [p5-design.md §演習](p5-design.md)。**本番の機体では実行しない**（この節は検証環境だけ）。本番と違う所は [§0.6](#06-演習では本番との違い)、各節の「演習では」の注記と、ここで足す。記録は `p5-acceptance.md`（書くもの・書かないものは §14）。

### 15.1 設計の流れ 1〜9 と、この手順書の対応

| 設計の流れ | 内容 | 手順書の節 | 演習で確かめるもの（合格の条件） |
| --- | --- | --- | --- |
| 1 | 復元の演習（回転を 1 件してから古いバックアップへ戻す） | §10 | 起動前の `integrity_check`・`.dump` の一致・所有者、起動後の画像の欠け。§10.5 の画像の欠けの直し方（T5.4 で確定。Drive からの取り直しは本番で 1 件ずつ先に試す） |
| 2 | Python を本番の役で動かし、Rust へ切り替える | §1.5〜§1.10・§2〜§6（§7 は 1 つ実施） | `--check-only`・保全・`--production` の配布。T0〜T1（5 分以内）。`--wait-secs` の測定。`ai-jobs status` と書き込み系の操作を 1 つ（`ai-jobs retry`） |
| 3 | シャドー比較 | §15.3 | `compare_with_python.py` の `DIFF` がすべて §15.3 のキー単位の許容に当てはまる（`shadow_judge.py` が終了コード 0。許容外 0。§15.3 の例外（条件 3）に当たるときだけ終了コード 1 でも合格）と、STATUS の大小関係の人の確認（条件 2） |
| 4 | Rust の期間の操作 | §15.4 | 手動の位置・お気に入り・除外・回転・下書きの保存を、それぞれ 1 件以上 |
| 5 | 戻す（AI 無効） | §8 | ID ごとの状態・下書きの全ハッシュ・`.cache.json` が保たれる。AI の通信が出ない。品質の 2 つの場合（そのまま／`dirty` の再構築）の両方 |
| 6 | 切り替え直す | §9 | 再び Rust で配信が戻る |
| 7 | 旧 Python の判定済みの行がある DB での `clue_pending` | §6.3・§11.1 | `db.loc.clue_pending` が 0 でなく、Decision が動けば進む |
| 8 | `Conflicts=` と `After=` の試験 | §15.5 | 両方の向きで、旧プロセスの終了が新プロセスの起動・最初の書き込みより前 |
| 9 | 元に戻す | §15.6 | 演習の前の状態と一致 |

**実行する順番**: 2 → 3 → 4 → 5 → 6 → 7 → 1 → 8 → 9（T5.4 の順）。3 は 4 の操作の前（同じ時点の DB を 2 つの複製に分けるため）。1（復元）は**切り替え直したあとの Rust の期間**に行い、§9 で取った `p5-precutover-*`（§6.8 で `precutover.dumpsha` を取り直したもの）へ戻す。そのバックアップのあとに回転を 1 件してから戻すので、欠けの想定は「その 1 件」になる（§10.5 の 1 の照合）。7（Decision の進み）は 1 の前に見る（復元で Rust の期間の結果が巻き戻るため）。

### 15.2 演習の前の準備と、各節の省略・置き換え

**演習の前に控える**（元に戻すときの基準。値は文書に書かず、`$WORK` に置く）: `systemctl list-unit-files 'geoframe*'`・各 unit の `is-enabled`／`is-active`（`masked`・`not-found` もそのまま。`geoframe-backup.timer` の有無も）・`ls -l /etc/systemd/system/geoframe* /usr/local/lib/systemd/system/geoframe* /usr/local/bin/geoframe* /etc/default/geoframe* /etc/geoframe`・`/usr/local/bin/geoframe` の sha256・`/var/lib/geoframe` の退避先の名前。T5.4 では、これに `/etc/default/geoframe` などのモード・所有者・内容の sha256（中身は出さない）、`/opt/geoframe-pyref` のファイルの名前と内容のハッシュ（`app/.venv` は名前だけ）、`/var/lib/geoframe` の直下の名前と derivatives の件数、`/var/backups/geoframe` の名前、`systemctl list-units 'geoframe*'`・`list-timers` の geoframe の行を足して 1 つのスクリプトで取り、§15.6 の 8 で同じスクリプトの出力と `diff` した（差 0）。

**演習の前の退避**（検証環境で、この順に。値は読まない）:

1. **検証環境の Rust を止めてから unit を退避する**: `systemctl stop geoframe-rs` → `systemctl disable geoframe-rs`（**移す前に disable する**。先に移すと `multi-user.target.wants/` のリンクが残り、`is-enabled` が `not-found` にならない）→ `/etc/systemd/system/geoframe-rs.service`（と `.prev`）と、空の `geoframe-rs.service.d/` があれば記録して消す（§15.6 で同じモードで作り直す）→ `/usr/local/bin/geoframe*` を root の退避先（`0700`）へ `mv` → `systemctl daemon-reload; systemctl reset-failed`。`systemctl is-enabled geoframe-rs` が `not-found`。
2. **共通の `/etc/default/geoframe`** は `cp -p` で退避先へ写す（検証環境では `0644` のことがある。演習のあいだは本番と同じ `0600 root:root` に書き直し、§15.6 で `cp -p` で戻す）。検証環境の印 `/etc/geoframe/staging` は `mv` で退避する。
3. **`/var/lib/geoframe` はディレクトリごと `mv` で退避する**（同じファイルシステムの中。`dbopen.sh` が rc=0 のときだけ）。検証環境の `/var/lib/geoframe` には AI の鍵のファイルがありうる（T5.4 では 2 つ）。**中身を読まず、名前も文書に写さず、新しい DATA_DIR には写さない**（ディレクトリごと動かすだけ）。退避の前に元の DB の `.dump` の SHA-256 を `dumpsha.sh` で控え、§15.6 で戻したあとに一致を確かめる。
4. **新しい DATA_DIR**: `install -d -m 0750 -o geoframe -g geoframe /var/lib/geoframe` を作り、退避した DATA_DIR から `derivatives`・`blog_exports`・`travel_blog`・`photos`・`tmp` **だけ**を `cp -a` で写す（`catalog.db*`・`.ingest.lock`・鍵のファイル・`.cache.json` は写さない。derivatives はハードリンクにしない: 回転が新しい名前で書くので、退避した側を変えないため）。本番のバックアップのコピー（`.db.gz`）は `geoframe` 所有の `0600` で DATA_DIR の `tmp/` に置き、`sudo -u geoframe` で `catalog.db.part` へ展開して `mv`、`integrity_check` が `ok`、`.dump` の SHA-256 を控える（§6.8 の `precutover.dumpsha` が最初の切替でこれと一致する）。置いた `.db.gz` は消す。
5. **比較用 Python を P5a のコミットの checkout にする**（§1.1 の置き換え。`/opt/geoframe-pyref` は git の checkout ではないので、`rust-deploy.sh --production` の「HEAD が `--expect-commit` と同じ・追跡下の変更が無い」を満たさない）: `mv /opt/geoframe-pyref /opt/geoframe-pyref.t54-orig`（`geoframe-pyref.service`（8090）は止めない。動いているプロセスの作業ディレクトリは移したディレクトリについていく）→ 同じ所有者で空の `/opt/geoframe-pyref` を作り、`git init` → 開発機で作った `git bundle create <file> <コミット>` の bundle（検証環境から GitHub へ出なくてよい）から `git fetch <bundle> <40 桁のコミット>` → `git checkout --detach FETCH_HEAD` → 退避した `app/.venv` を `cp -a` で同じパスへ写す（venv は絶対パスを持つので、同じ `/opt/geoframe-pyref/app/.venv` に置けば使える）。`git rev-parse HEAD` が演習のコミット、`git status --short` が空（`app/.venv/` は `.gitignore` で無視される）、`git diff --quiet HEAD --` が真。§15.6 では、演習の checkout を退避先へ `mv` し、`/opt/geoframe-pyref.t54-orig` を `/opt/geoframe-pyref` へ `mv` で戻す。`requirements.txt` が変わっていないことを先に確かめる（変わっていれば venv を作り直す必要がある）。

| 節 | 演習では |
| --- | --- |
| §0.5 変数 | `WORK=/root/p5-cutover`（検証環境の root）・`HOST` は検証環境の `hostname`・`COMMIT` は演習の checkout（`/opt/geoframe-pyref`）の `HEAD`・`PYAPP=/opt/geoframe-pyref/app`。コマンドの `/opt/geoframe/…` は `/opt/geoframe-pyref/…` に読み替える |
| §1.1 | 省く。`/opt/geoframe-pyref` を手で P5a のコミットにする（`git -C /opt/geoframe-pyref status --short` が空であること） |
| §1.2 | 省く（演習のコミットが基準）。**本番の §1.2 で、演習のコミットと P5a のマージのコミットの差が無いことを確かめる**ので、演習のコミットを `p5-acceptance.md` に記録する |
| §1.3 | 同じ。`fuser` は無くてよい（`dbopen.sh` がどちらの経路で判定したかを記録する）。**`fuser` の経路は、psmisc のある状態で一度確かめる**（`dbopen.sh` を、DB を開いているとき rc=1・開いていないとき rc=0・root でないとき rc=2 で。`fuser` の無い状態の `/proc` の経路も、同じ rc=1・rc=0 になること。検証環境に psmisc が無ければ導入して確かめ、演習のあと元に戻す）。**psmisc があるときの `/proc` の経路**: psmisc は消さず、`PATH` から `fuser` だけを外して動かす。Ubuntu などは `/bin` が `/usr/bin` へのリンク（merged-usr）なので、`PATH` から `/usr/bin` を外すだけでは `fuser` が残る。`install -d -m 0700 "$WORK/nofuser"; for f in /usr/bin/*; do [ "${f##*/}" = fuser ] \|\| ln -sf "$f" "$WORK/nofuser/"; done` で `fuser` の無い道具の置き場を作り、`PATH="$WORK/nofuser:/usr/sbin:/sbin" /bin/bash "$WORK/dbopen.sh"` で動かす（出力が「（/proc）」になる）。root でない場合は、`dbopen.sh` を一時的に別の場所へ写し、`sudo -u <一般ユーザー>` で動かす |
| §1.4 | 同じ（`$WORK` を作り、補助スクリプトを置く。ここで作ったスクリプトを、そのまま本番で使う。直したら記録する） |
| §1.5 | 同じ 3 つの設定のファイル。`GEOFRAME_DRIVE_KEY` は**存在しないパス**。`GEOFRAME_AI_CONFIG` はローカルの用途だけ。`TZ` は本番と同じ。検証環境の共通の設定に既にある名前（`GEOFRAME_TILE_UA` など）は共通に置いたままにする（§1.5 の「両方に置かない」）。**AI 設定の JSON は検証環境に無いことがある**（T5.4 では無かった）: `/etc/geoframe/` に `0640 root:geoframe` で作る。中身は [ai-embedding.md](ai-embedding.md) の固定の契約（`embedding_contract`・`embedding_space`）と、用途 `image_embedding`・`query_embedding`（`service_id` は `embed`）・`location_clue`（`model` は §1.7 の `health` が返すもの）、`local_base_url` は Git 管理外のメモの LAN の入口。クラウドの用途は書かない。**Decision と Embedding の入口は本番機の上にある**ので、演習のあいだだけ `geoframe-rs` に `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE=1`・`GEOFRAME_AI_BACKLOG_POLL_S=60` を足して、本番機の CPU を使い続けないようにする（本番では足さない。流れ 7 の進みは 1 分に 1 件ほどで見える） |
| §1.6 | AI 設定の JSON の確認は同じ。**鍵は置かないので、鍵の所有者・モードの確認（表の鍵の行）は省く**（クラウドの `Unauthorized` 拒否は `ai-cloud.md` の試験で済み） |
| §1.7 | 検証環境の Decision の `health` を確かめる。無ければ §15.1 の流れ 7 は「待ちが 0 でなく、`clue_runnable` の表示が出る」ところまで |
| §1.8 | 演習で使うバイナリ（`rust-deploy.sh` の検証環境の経路が作る musl の成果物と同じ手順でビルドしたもの）を `$BIN` に置き、**その sha256 を記録して `$SHA` とする**（本番の `--expect-sha256` に渡す）。**演習が済んだら、このバイナリそのものを検証環境の外へ保管し、sha256 を確かめて記録する**（[§15.6](#156-元に戻す流れ-9) の 2。`$WORK` を消す前）。**初回の配布を再現するため**、検証環境にいまある `/usr/local/bin/geoframe*` と `geoframe-rs.service`（`/usr/local/lib/systemd/system/` と `/etc/systemd/system/`）を、演習の前に退避して「無い」状態にする（§15.6 で戻す） |
| §1.9 | `hostname` は検証環境のもの。検証環境の印 `/etc/geoframe/staging` は**退避**する（§15.6 で戻す）。Python 版 unit は最初から `/usr/local/lib/systemd/system/geoframe-server.service`（比較用 Python を `geoframe-server.service` の名前・`/var/lib/geoframe`・8080 で一時的に置く。`deploy/geoframe-server.service` の `WorkingDirectory=`・`ExecStart=` を `/opt/geoframe-pyref` の checkout と venv に向けたもの）。`FragmentPath` の期待値はその場所で、「通常のファイル」の判定は省く |
| §1.10 | 同じ |
| §2 | 同じ。Python 役は AI 無効・鍵なし（共通の `/etc/default/geoframe` は `GEOFRAME_AI_ENABLED=0`）。**`1` へ書き換えるのは §3.5 の mask の直後**（Python は mask と drop-in で起動できない。T5.4 の形。こうすると最初の Rust の期間も本番と同じく AI が有効で、§6.3 の `clue_pending`・`clue_runnable`・`ai_task_blocks` の確認と、ローカルの AI が動くことが最初の切替で確かめられる。流れ 5 で「共通が `1` のまま drop-in が勝つ」こともそのまま確かめられる）。**演習だけの確認として、§2 で Python のフィードに `check_files.py` を当てて画像の欠けの基準を取る**（検証環境の derivatives は本番より古いことがあり、欠けが元からあると §6.2・§10.5 の「200でない」を切替や復元のせいと区別できない。T5.4 では 0） |
| §3.1〜§3.6 | 同じ。§3.2（unit の本体を移す）は省き、`/etc/systemd/system/` に本体が無いことだけ確かめる。drop-in・mask・固定の印は同じ |
| §4 | 同じ。バックアップ先 `/var/backups/geoframe` が無ければ作る（`geoframe` 所有・`0750`）。**検証環境に `geoframe-backup.timer` が無いときは、本番と同じ配置のダミーを置く**（本番の §4.5 のコマンドは変えない）: `/etc/systemd/system/` に、`geoframe-backup.service`（`[Service]` に `Type=oneshot`・`ExecStart=/bin/true`）と `geoframe-backup.timer`（`[Timer]` に `OnCalendar=daily`、`[Install]` に `WantedBy=timers.target`）を置き、`systemctl daemon-reload` のあと `systemctl enable --now geoframe-backup.timer` する。**`/run/systemd/system/` や `enable --runtime` は使わない**（`is-enabled` が `enabled-runtime` になり、§4.5 の `enabled` との完全一致が必ず NG になる。`[Install]` が無いと `static`）。`OnCalendar=daily` があるので、timer が `active` のとき `systemctl list-timers geoframe-backup.timer` の `NEXT` に翌日の 0 時など**先の時刻**が出る（発火条件が無い timer は `active` でも `NEXT` が `n/a`）。この状態で §4.1・§4.5 のコマンドを**そのまま**打ち、`is-enabled` が `enabled`、`start` のあと `is-active` が `active`・`NEXT` が先の時刻になることを確かめる。§15.6 で `disable` して消す |
| §5 | 同じ。`--wait-secs` は**測る**（起動から `/healthz` 200 までの秒数を数回測り、余裕を足した値を本番の `$WAIT` に使う） |
| §6 | §6.4 の miharashi・管理画面は検証環境の方法（`tools/miharashi-feed-check`・`compare_with_python.py`）。§6.5 の取り込みは Drive へ出ずに終わる（鍵が無いため）ことを確かめる。§6.2 の `extra.py` は演習の複製 DB で実行する（説明できない 0） |
| §7 | `ai-jobs status` と、サービスを止めて行う書き込み系を 1 つ（`ai-jobs retry --kind location_clue` など。外へ送らないもの） |
| §8 | 同じ。`$PYAPP` を演習の venv に向ける。共通の `/etc/default/geoframe` が `GEOFRAME_AI_ENABLED=1` のまま戻し（§2 の行のとおり §3.5 の直後に書き換えてある）、drop-in が勝つこと（プロセスの環境が `0`）を確かめる。**品質は 2 つの場合の両方**（§15.4 の最後） |
| §9 | 同じ |
| §10 | 流れ 1。**§10.5 の画像の欠けの直し方を確定する**（対象の限定・取り直し・回転の保持・再確認。鍵が無いので Drive からの取り直しは確かめられない。代わりの確認は §10.5） |
| §11 | Decision の全件再判定・索引の公開は、検証環境で動く範囲で進み方を確かめる（本番機の CPU を使う確認は本番で）。バックアップの timer は本番で |
| §12 | 省く（鍵が無い）。設定の書き方は `ai-config.md` の試験で済み |
| §13 | 省く。代わりに §15.6 |

### 15.3 シャドー比較（流れ 3）

- **実行する段**: 流れ 2（§2〜§6）が済み、配信が安定したあと、**流れ 4 の操作の前**（同じ時点の DB を 2 つの複製に分けるため）。
- **別のコピーの置き方**: Rust と比較用 Python は、**同じ内容の別の複製**を使う（Python は書き込むので、同じ DB を Rust と共有しない）。①Rust を §7 の形で止める。②`sqlite3 .backup` で DB を 2 つの DATA_DIR（例: `/var/lib/geoframe-shadow-rs`・`/var/lib/geoframe-shadow-py`。`geoframe` 所有・`0750`）に取り、`derivatives/` はハードリンクで複製する（`cp -al`）。2 つの `.dump` の SHA-256 が一致することを確かめる。③Rust を Rust 用の複製で `GEOFRAME_INGEST` 無し（読み取り専用）・`127.0.0.1:8081`、比較用 Python を Python 用の複製で `GEOFRAME_AI_ENABLED=0`・`GEOFRAME_LOC_SUGGEST=0`・Drive の鍵なし・`127.0.0.1:8092` に、`systemd-run --uid=geoframe` の一時 unit で起動する（**8090 は使わない**: 検証環境の `geoframe-pyref.service` が 8090 で動き続けている）。④流れ 2 で止めた本番役の Rust を起動し直す（①〜②を §7 と同じ `trap` の形で包めば、複製が済んだところで自動で起動し直る）。
- **一時 unit の起動**（環境は `-E` で明示する。`EnvironmentFile=` の `/etc/default/geoframe-rs` は本番役の DATA_DIR を指すので渡さない。共通の `/etc/default/geoframe` も渡さない: §2 の行のとおり `GEOFRAME_AI_ENABLED=1` にしてあり、`EnvironmentFile=` は `-E` より勝つので、Python の AI を無効にできない）:

```sh
ss -ltn | awk '{print $4}' | grep -E ':(8081|8092)$' && echo "ポートが使われている（先へ進まない）"
systemd-run --uid=geoframe --unit=t54-shadow-rs -p UMask=0027 \
  -E GEOFRAME_DATA_DIR=/var/lib/geoframe-shadow-rs -E GEOFRAME_PORT=8081 -E GEOFRAME_HOST=127.0.0.1 \
  -E GEOFRAME_DRIVE_KEY=/nonexistent/drive_key.json -E TZ=Asia/Tokyo -E GEOFRAME_EMBED_BACKEND=local \
  /usr/local/bin/geoframe serve
systemd-run --uid=geoframe --unit=t54-shadow-py -p WorkingDirectory=/opt/geoframe-pyref/app \
  -E GEOFRAME_DATA_DIR=/var/lib/geoframe-shadow-py -E GEOFRAME_PORT=8092 -E GEOFRAME_HOST=127.0.0.1 \
  -E GEOFRAME_AI_ENABLED=0 -E GEOFRAME_LOC_SUGGEST=0 -E GEOFRAME_DRIVE_KEY=/nonexistent/drive_key.json \
  -E TZ=Asia/Tokyo -E PYTHONUNBUFFERED=1 -E GEOFRAME_EMBED_BACKEND=local \
  /opt/geoframe-pyref/app/.venv/bin/python server.py
curl -fsS -o /dev/null -w 'rs %{http_code}\n' http://127.0.0.1:8081/healthz; curl -fsS -o /dev/null -w 'py %{http_code}\n' http://127.0.0.1:8092/api/admin/status
```

  `GEOFRAME_EMBED_BACKEND=local` は `geoframe-pyref.service` の drop-in と同じ（共通の設定の値と同じであることを `grep -c '^GEOFRAME_EMBED_BACKEND=local$' /etc/default/geoframe` が 1 で確かめる）。Rust の起動の行に「取り込みは無効です」が出ること。
- **判定の補助スクリプト**（演習だけで使う。本番の §1.4 には置かない。先に作る）:

```sh
cd "$WORK"
cat > shadow_judge.py <<'PY'
# 使い方: python3 -I shadow_judge.py <compare_with_python.py の出力> <extra.py の既知の差の件数>
# 出力の DIFF の行とキーを、§15.3 の許容と照らす。出すのは件数とキー名だけ。許容外があれば終了コード 1。
import re, sys
text = open(sys.argv[1], encoding="utf-8").read().splitlines()
known = int(sys.argv[2])
STATUS_KEYS = {"ai_jobs(片方に無い)", "ai_scenes(片方に無い)", "db.ai.groups", "db.ai.suppressed",
               "db.loc.clue_runnable(片方に無い)", "db.loc.clue_pause(片方に無い)"}
sec, diffs, keys, result = 0, [], {}, None
for line in text:
    m = re.match(r"== (\d+)\. ", line)
    if m:
        sec = int(m.group(1)); continue
    m = re.match(r"== 結果: 想定外の差 (\d+) 項目", line)
    if m:
        result = int(m.group(1)); continue
    if line.startswith("  DIFF "):
        diffs.append((sec, line[7:]))
    m = re.match(r"\s+差のあるキー: (.+?)(?: … (\d+) 件)?$", line)
    if m:
        keys.setdefault(sec, []).append(m.group(1))
bad, allowed, rust_only = [], 0, 0
for sec, body in diffs:
    if sec == 1:
        m = re.fullmatch(r"JSON が一致しない: Rust のみ (\d+) 件、Python のみ 0 件、共通 \d+ 件のうち値の差 0 件、共通分の並びは同じ", body)
        if m and not keys.get(1):
            rust_only = int(m.group(1)); allowed += 1; continue
    elif sec == 2:
        m = re.fullmatch(r"差のあるキー (\d+) 個", body)
        if m and int(m.group(1)) == len(keys.get(2, [])) and set(keys.get(2, [])) <= STATUS_KEYS:
            allowed += 1; continue
    elif sec == 3:
        if re.fullmatch(r"状態コードと JSON がともに一致 \d+/\d+", body) and set(keys.get(3, [])) <= {"items[].scene(片方に無い)"}:
            allowed += 1; continue
    bad.append("節%d: %s" % (sec, body))
for sec, ks in sorted(keys.items()):
    for k in ks:
        if sec == 1 or (sec == 2 and k not in STATUS_KEYS) or (sec == 3 and k != "items[].scene(片方に無い)") or sec not in (1, 2, 3):
            bad.append("節%d の許容外のキー: %s" % (sec, k))
if rust_only != known:
    bad.append("Rust のみ %d 件が extra.py の既知の差 %d 件と合わない" % (rust_only, known))
if result is None:
    bad.append("結果の行が無い（出力が途中で切れている）")
elif result != len(diffs):
    bad.append("結果の行の項目数 %d と DIFF の行 %d が合わない" % (result, len(diffs)))
print("DIFF の行", len(diffs), "許容", allowed, "/ Rust だけの写真", rust_only,
      "/ STATUS の差のキー", len(keys.get(2, [])), "/ 管理写真の差のキー", len(keys.get(3, [])), "/ 許容外", len(bad))
for b in bad:
    print("  許容外 " + b)
sys.exit(1 if bad else 0)
PY
chmod 0600 shadow_judge.py
```

- **実行と判定**:

```sh
cd "$WORK"
python3 /opt/geoframe-pyref/tools/compare_with_python.py --rust http://127.0.0.1:8081 --python http://127.0.0.1:8092 | tee shadow-out.txt; echo "compare rc=${PIPESTATUS[0]}"
# フィードの Rust だけの写真を、§6.2 と同じ extra.py で調べる（複製の DB は読み取り専用で開く。取り込みは 0 のはず）
curl -fsS http://127.0.0.1:8081/api/v1/photos -o shadow-rs-feed.json && curl -fsS http://127.0.0.1:8092/api/v1/photos -o shadow-py-feed.json
python3 -I ids.py shadow-rs-feed.json | LC_ALL=C sort > shadow-rs-ids.txt; python3 -I ids.py shadow-py-feed.json | LC_ALL=C sort > shadow-py-ids.txt
comm -23 shadow-py-ids.txt shadow-rs-ids.txt | wc -l                       # Python だけ: 0
comm -13 shadow-py-ids.txt shadow-rs-ids.txt > shadow-rs-only.txt
python3 -I extra.py shadow-rs-feed.json shadow-rs-only.txt /var/lib/geoframe-shadow-rs/catalog.db shadow-unexplained.txt; echo "extra rc=$?"
# STATUS の整数の値の差（許容は下の表）
diff <(curl -fsS http://127.0.0.1:8081/api/admin/status | python3 -I status_counts.py) <(curl -fsS http://127.0.0.1:8092/api/admin/status | python3 -I status_counts.py)
KNOWN=$(python3 -I extra.py shadow-rs-feed.json shadow-rs-only.txt /var/lib/geoframe-shadow-rs/catalog.db | sed -nE 's/.*既知の差 ([0-9]+) .*/\1/p'); echo "KNOWN=$KNOWN"   # extra.py の「既知の差」の件数
python3 -I shadow_judge.py shadow-out.txt "$KNOWN"; echo "judge rc=$?"
```

- **想定外の差 0 の判定**: `compare_with_python.py` は、意図した差があるかぎり終了コード 1 になる（出力の最後は `想定外の差 N 項目`）。合格にするのは、**通常は 1 と 2 の両方**を満たすとき。**例外の場合は、1 の「終了コード 0」の要件だけを 3 で置き換え、2 は必ず満たす**。表に無い `DIFF`・キーは 1 つでも想定外（件数だけの増減でほかの差を免除しない）。
  1. `DIFF` の行がすべて下の表のキー単位の許容に当てはまり、`shadow_judge.py` が**終了コード 0**（`許容外 0`）。
  2. **STATUS の `db.ai.groups`・`db.ai.suppressed` の大小関係を人が確かめた**: `status_counts.py` の `diff` で **Rust の値 ≤ Python の値**の向きだけ（`shadow_judge.py` はキー名だけを見て大小は見ない）。ほかの `db.*` の行の差が出ていない。
  3. **例外（終了コード 1 でも合格にできる唯一の場合）**: 下の表の節 5 の `Python と状態コード・本文が一致` の `DIFF` が残って `shadow_judge.py` が終了コード 1 になるときは、`shadow_judge.py` の `許容外` の行が**すべてその節 5 の `DIFF`（とその `差のあるキー`）に由来**し、ほかの節・項目の `許容外` が **0** で、人が表の条件（`Rust の埋め込みファイルがリポジトリの実ファイルとバイト一致` が `OK`、差が比較用 Python の `app/` の版違いだけ）を確かめて `p5-acceptance.md` に記録したときだけ合格にする（P4 の 12/14 相当）。このときも 2 は必須。条件を確かめられない・記録していない場合は、終了コード 1 のまま不合格として扱う。
- **キー単位の許容**（`compare_with_python.py` の出力の形で書く。キーの表記は `key_paths` のもの）:

| 節 | 許容する差（出力に出る形） | 条件と、件数の確かめ方 |
| --- | --- | --- |
| 1 フィード | `JSON が一致しない: Rust のみ K 件、Python のみ 0 件、共通 N 件のうち値の差 0 件、共通分の並びは同じ` の **1 行だけ**。`差のあるキー:` の行は **1 つも無い** | K は `extra.py` の「既知の差」の件数と**一致**し、「取り込み 0」「説明できない 0」（複製どうしの比較では取り込みは起きない。`hidden` の写真の再出現もここで拾う）。`Python のみ 0`・`値の差 0`・並びが同じが外れたら想定外 |
| 2 STATUS | `ai_jobs(片方に無い)`・`ai_scenes(片方に無い)`（Rust だけの追加キー。[ai-config.md](ai-config.md)・[ai-scenes.md](ai-scenes.md)。AI の表があるときだけ出る） | `status_counts.py` は `db` の整数だけを見るので `diff` には出ない。判定は `shadow_judge.py` が出力のキー名で行う |
| 2 STATUS | `db.ai.groups`・`db.ai.suppressed`（値の差。[p4-acceptance.md §7.3](p4-acceptance.md)。旧選別の世代切れ・代表が表示できない分を Rust は数えない） | `status_counts.py` の `diff` で、**Rust の値 ≤ Python の値**の向きだけ。ほかの `db.*` の行の差が出たら想定外。出なくてもよい |
| 2 STATUS | `db.loc.clue_runnable(片方に無い)`・`db.loc.clue_pause(片方に無い)`（T5.2 の追加。Rust だけ） | 比較用の Rust は取り込みなし（`GEOFRAME_INGEST` 無し）なので**通常は出ない**。出たら取り込みを渡していないかを疑う。この 2 つの「片方に無い」だけを許し、`db.loc.clue_pending` の値の差は許さない（`diff` に出たら想定外） |
| 3 管理写真 | `items[].scene(片方に無い)`（Rust だけの追加キー。場面の注記。[ai-scenes.md](ai-scenes.md)） | `状態コードが一致` の行が `OK`（状態コードの差が 0）。`差のあるキー:` の行が `items[].scene(片方に無い)` だけ（`… N 件` はそのキーのあるクエリの数。0 件で行が無くてもよい） |
| 4 表示用画像 | なし | `OK` の行だけ |
| 5 ページ・拒否・メソッド・Host | なし | `OK` の行だけ。例外は 1 つ: `Python と状態コード・本文が一致` の `DIFF` は、`Rust の埋め込みファイルがリポジトリの実ファイルとバイト一致` が `OK` で、差が比較用 Python の `app/` の版違いだけ（[p4-acceptance.md §7.2](p4-acceptance.md) の 12/14 が前例）と確かめたときだけ（`shadow_judge.py` は想定外として数えるので、人が確かめて記録する） |

- 内訳（Rust だけの写真の件数 K・取り込み／既知の差／説明できない、STATUS の差のキー、管理写真の差の件数）を `p5-acceptance.md` に記録する。

- 終わったら一時 unit を止め、複製を消す（§15.6）。

### 15.4 Rust の期間の操作（流れ 4）と、品質の 2 つの場合（流れ 5）

- 流れ 3 のあと、管理画面（検証環境の `127.0.0.1:8080`）で: 手動の位置を 1 件以上、お気に入りを 1 件以上、除外を 1 件以上、回転を 1 件以上、旅行ブログの下書きの保存を 1 件以上。ID・値は `$WORK` に残さなくてよい（§8.2 の `rs-end-*` が控える）。
- 戻す（§8）。§8.4 の「そのまま戻した直後」を記録する（品質の場合 1）。
- **品質の場合 2（`dirty` の再構築）**は、演習の複製の DB だけで起こす（**本番の DB では行わない**）: ①Python を止める（§8.1 と同じ要領で、`geoframe-server` を `stop`）。②`geoframe` ユーザーで `UPDATE settings SET value='1' WHERE key='photo_quality_dirty'` を実行する（`sudo -u geoframe sqlite3 /var/lib/geoframe/catalog.db`）。③Python を起動し（§8.3 の `systemctl start geoframe-server`）、同期サイクルが 1 回回って未解析が 0 のとき再構築が走るまで待つ。**再構築の終わりは `settings` の `photo_quality_dirty` が `0` に戻り、`photo_quality` の `max(analyzed_at)` が ② の前の値から変わったことで見る**（`algorithm` が全行 `poc-phash-v1:…` になることでも分かるが、Rust の期間に再構築が走らず、もともと `poc-phash-v1:…` のときは見分けられない。T5.4 の演習はこの場合で、起動から約 5 秒で終わった）。④`counts.sql` と `quality_snapshot.sql` を取り直し、§8.4 の 2 の基準（`rs-end-counts.txt`・`rs-end-quality.tsv`＝Rust から戻す直前の値）と比べる。写真と特徴量は固定なので、`quality_diff.py` が `写真: 追加 0 削除 0 更新 0 / 特徴量: 追加 0 削除 0 更新 0`・`説明できない変化 0`（`入力の変更に連なる変化` も 0）、`counts diff` の `quality_rows`・`quality_groups`・`kind:` が同じ（変わったものがあれば順位・推奨・スコア・`group_id`・完全重複の写しの入れ替わりに限られ、`quality_diff.py` には出ない）。演習のあいだに写真の増減・更新や特徴量の追加があって「写真の変更」か「特徴量の変更」が 0 でないときは、§8.4 の「あった」側で分けて記録する。
- `p5-acceptance.md` には、場合 1・2 それぞれの `quality_rows`・`quality_groups`・`kind:` 別・`algorithm` 別の件数と、`/api/v1/photos` の件数を記録する。

### 15.5 `Conflicts=` と `After=` の試験（流れ 8。手順書の切替とは別）

目的: `geoframe-rs.service` の `Conflicts=geoframe-server.service` と `After=geoframe-server.service` で、**どちらの向きでも旧プロセスが終わってから新しいプロセスが書き始める**ことを確かめる。

1. 両方を unmask する（Rust は `GEOFRAME_INGEST=1`、Python は drop-in 付き・AI 無効）。`systemctl show geoframe-rs -p Conflicts -p After` に `geoframe-server.service` が出ること。
2. **向き A（Python が動いているときに Rust を起動）**: Python を起動して `active` にし、`systemctl start geoframe-rs`。時刻は次の 2 つで比べる: systemd の行（`journalctl -t systemd -o short-precise --since '<開始時刻>' --no-pager | grep -E 'geoframe-(rs|server)'`。「Stopped」「Started」など）と、Rust の定型文（`journalctl -u geoframe-rs -o short-precise --since '<開始時刻>' --no-pager | grep -E '\[guard\]|サイクル完了'`）。**Python の停止の完了の時刻 < Rust の起動・最初の書き込み（サイクル）の時刻**。Python 版自身のログの行は画面に出さない（§0.5）。
3. **向き B（Rust が動いているときに Python を起動）**: Rust を `active`（取り込みが動いている）にして、`systemctl start geoframe-server`。**Rust の停止の完了の時刻 < Python の起動の時刻**を、同じ systemd の行で比べる。Rust の停止は `TimeoutStopSec=90s` までかかりうる。
4. 終わったら通常の状態（Rust のみ active、Python は `disable` と mask）に戻し、`systemctl daemon-reload` する（mask のあと reload しないと、次の `systemctl stop` などが「unit file … changed on disk」と警告する）。`systemd-analyze security geoframe-rs --no-pager | tail -1` の総合の評価（数値と段階だけ）を記録する。
5. 記録するもの: 向きごとの「旧プロセスの停止の完了」と「新プロセスの起動」の時刻の差（秒）。どちらも旧が先であること。

### 15.6 元に戻す（流れ 9）

演習の前に控えたもの（§15.2）と一致させる。**順番が大事**: Rust が `active` のまま、バイナリ・unit・DATA_DIR を戻さない。先に演習で動かしているものを止め、DB を使うものが無いことを確かめ、本番へ入れるバイナリを保管してから、ファイルを戻す。**戻し終わるまで、何も起動しない**: すべてを止めたまま、設定・バイナリ・unit・`/var/lib/geoframe` を戻し、DB の使用者が無いことをもう一度確かめて演習のデータを片付けたあと、サービスを起動し、**最後に**タイマーを戻す（演習の DB を開いたまま `/var/lib/geoframe` を入れ替えない）。**一つずつ、戻したことを確かめる**:

1. **演習のサービス・タイマーを止める**。バックアップのタイマーを先に止め、走っているバックアップ（`geoframe-backup.service`）は**終わるのを待つ**（止めない。途中で止めると一時ファイルが残る）:

```sh
systemctl stop geoframe-backup.timer
while [ "$(systemctl is-active geoframe-backup.service)" = activating ]; do sleep 5; done      # 走っていれば終わるまで待つ
systemctl stop geoframe-rs geoframe-server                                                      # 演習で動かした Rust と一時の Python
systemctl stop '<§15.3 の一時 unit の名前>' ...                                                  # シャドー比較の Rust・Python（systemd-run のもの）
systemctl is-active geoframe-backup.timer geoframe-backup.service geoframe-rs geoframe-server   # すべて inactive
"$WORK/dbopen.sh"; echo "dbopen rc=$?"                                                          # rc=0（/var/lib/geoframe を開いているものが無い）
pgrep -x sqlite3; echo "sqlite3 rc=$?"                                                          # 出力が空・rc=1（バックアップや控えの sqlite3 が残っていない）
ls -A /var/backups/geoframe | grep '^\.' || echo "一時ファイルなし"                              # .tmp-・.p5-copy. が残っていない
```

   - `geoframe-pyref.service`（8090）など、演習の前から動いている通常のサービスは止めない（`/var/lib/geoframe-pyref` など**別の DATA_DIR** なので、`dbopen.sh`（`/var/lib/geoframe` だけを見る）には出ない。`pgrep -u geoframe -a` に出るのはその分だけ）。シャドー比較の複製（`/var/lib/geoframe-shadow-*`）は、一時 unit が `inactive` になったことで開いているものが無いと見る。**どれかが残っていれば先へ進まない**。

2. **本番へ入れる演習済みのバイナリそのものを保管する**（`$WORK` を消す前。ビルドし直したものは別のバイト列になりうるので、**これを保管する**）:

```sh
sha256sum "$BIN" /usr/local/bin/geoframe      # どちらも $SHA と一致（§1.8 と §5 で入れたもの）
```

   - 検証環境の**外**（検証環境を動かしているホスト側の保管先。検証環境のコンテナ・スナップショットを消しても残る場所。LXC なら `lxc file pull` で取り出す）へ `$BIN` を**コピー**し、保管先で `sha256sum` を取り直して **`$SHA` と完全に一致**することを確かめる。**バイナリを検証環境の外（開発機）でビルドして、検証環境へ写して使った場合**（T5.4 の形）は、写す元の保管物（`0444`）がそのまま保管先になる: 保管物の `sha256sum` が `$SHA` と一致し、検証環境の `$BIN` と `/usr/local/bin/geoframe` も `$SHA` と一致することを確かめる（取り出し直さなくてよい）。保管先のファイルは書き込み不可（`chmod 0444`）にし、名前に sha256 の先頭 12 桁を入れる。本番の §1.8 の `<受け取ったバイナリ>` はこのファイル。
   - 記録: `$SHA`（64 桁）、保管先の場所の名前、演習の commit を `p5-acceptance.md` に書く。**保管先の `sha256sum` が一致するまで、下の 3 以降（ファイルと `/var/lib/geoframe` を戻す・`$WORK` を消す）に進まない。**

3. ファイルを戻す（演習の前に控えた形へ。**すべて止めたまま。ここでは何も起動しない**）:
   1. 一時的な `geoframe-server.service`（`/usr/local/lib/systemd/system/`）と drop-in `p5-rollback.conf`（とそのディレクトリ）、`/etc/default/geoframe-py-rollback` を消す。共通の `/etc/default/geoframe` は §15.2 の退避から `cp -p` で戻す（モードも演習の前のもの）。
   2. 演習で作った `/etc/default/geoframe-rs`・AI 設定の JSON（§15.2 の §1.5 の行で作ったもの）ほかの設定のファイルを、演習の前の状態に戻す（無かったものは消す）。`/etc/geoframe/checkout-pinned` を消す。
   3. 検証環境の印 `/etc/geoframe/staging` を戻す（退避を `mv` で戻す）。
   4. `/usr/local/bin/geoframe*` と `geoframe-rs.service`（両方の置き場所）・`geoframe-pyref.service`（8090）を、演習の前の形に戻す（退避から戻す。演習で入れたバイナリ・`.prev`・`.failed` を消す。`rust-deploy.sh` が入れた `/usr/local/lib/systemd/system/geoframe-rs.service` と、§9 で作られる `.prev` も消す）。`systemctl disable geoframe-rs` を**退避から戻す前に**行う（演習の unit の `wants` のリンクを外す。4 の `enable` で元の unit のリンクを作り直す）。`/usr/local/lib/systemd/system/` が演習で作られたもの（`stat -c %w` の生成時刻が演習のあいだ）で空になったら `rmdir` する。§15.2 で消した空の `geoframe-rs.service.d/` は同じモード（`install -d -m 0755`）で作り直す。
      - **比較用 Python を戻す**: 演習の checkout（`/opt/geoframe-pyref`）を root の退避先へ `mv` し、`/opt/geoframe-pyref.t54-orig` を `/opt/geoframe-pyref` へ `mv` で戻す（`geoframe-pyref.service` は止めずに済む。8 の控えの `diff` で、ファイルの名前と内容のハッシュが演習の前と一致すること）。
   5. §15.2 で置いたダミーの `geoframe-backup.service`・`geoframe-backup.timer`（`/etc/systemd/system/`）は `systemctl disable geoframe-backup.timer` のあとで消す。演習の前から実物があったなら、その状態に戻す。

4. **systemd に反映し、演習の前の enabled／masked に戻す（起動しない）**: ファイルを戻したら `systemctl daemon-reload` のあと `systemctl reset-failed`。§15.2 で控えた各 unit について、控えが `enabled` なら `enable`、`disabled` なら `disable`、`masked` なら `mask`、`not-found`（unit が無かった）なら `unmask` で mask の残りを消す（`/etc/systemd/system/geoframe-*.service` が `/dev/null` への symlink で残っていないこと）。**`enable --now` は使わない**（起動は 6・7）。この時点で `geoframe-rs`・`geoframe-server`・`geoframe-backup.timer`・`geoframe-backup.service` が**すべて `inactive` のまま**であること（`systemctl is-active`）。

5. **`/var/lib/geoframe` を戻し、演習のデータを片付ける（止めたまま）**:
   1. もう一度 `"$WORK/dbopen.sh"; echo "dbopen rc=$?"` が **rc=0**、`pgrep -x sqlite3` が空（rc=1）。1 のあと、ファイルを戻すあいだに DB を開いたものが無い。**rc=0 でなければ先へ進まない**。
   2. **2 の保管先の確認が済んでいること**を確かめる（バイナリと `sha256` の記録は `p5-acceptance.md` にだけ残す）。
   3. `/var/lib/geoframe` を退避から戻す。戻したあと `"$WORK/dbopen.sh"` が rc=0（戻した DB を開いているものが無い）。
   4. 本番のコピー（演習の DATA_DIR を退避した名前へ `mv` してから消す。検証環境へ写した `.db.gz` の置き場も）・`/var/lib/geoframe-shadow-*`・一時 unit（`systemd-run` のもの）・`/var/backups/geoframe` の演習用バックアップ（`p5-precutover-*`）を消す（ユーザーの許可で）。戻した `/var/lib/geoframe` の DB の `.dump` の SHA-256 が §15.2 の 3 で控えた値と一致すること。`$WORK` は 8 の確認で控えを使うので、まだ消さない。

6. **サービスを起動する（演習の前に `active` だったものだけ）**: 控えが `active` だった unit だけを `start` する（`inactive` だったものは止めたまま。`geoframe-pyref.service`（8090）ほか通常のサービスは、止めなかったならそのまま、止めたなら控えの状態にする）。起動したものが `active` で、Python が `active` だったなら `/healthz` に答えること。まだタイマーは戻さない。

7. **最後にタイマーを戻す**: 控えで `geoframe-backup.timer` が `active` だったときだけ `start`（`enabled`／`disabled` は 4 で戻した）。演習の前から無かったなら無いまま。

8. 確認: §15.2 で控えたコマンドをもう一度実行し、**差が 0**（`diff`。`is-enabled`／`is-active` の行を含む）。検証環境の通常のサービス（`geoframe-pyref.service` 8090 など）が演習の前と同じ状態で動いていること。`systemctl list-timers` と `systemctl list-units 'geoframe*'` に、演習のダミーや一時 unit が残っていない。確認が済んだら `$WORK` を消す。
