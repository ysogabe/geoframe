# 本番切替の手順書（P5 / T5.3）

本番の geoframe を Python 版（`geoframe-server`）から Rust 版（`geoframe-rs`）へ切り替え、問題が出たら戻す手順。この文書だけを見て、上から順に実行できるようにしてある。

- 要求・設計: [p5-prd.md](p5-prd.md)・[p5-design.md](p5-design.md)
- 使う道具: [`rust-deploy.sh`](../../deploy/rust-deploy.sh)・[`deploy-pull.sh`](../../deploy/deploy-pull.sh)・[`geoframe-admin.sh`](../../deploy/geoframe-admin.sh)・[`geoframe-rs.service`](../../deploy/geoframe-rs.service)・[`geoframe-server.service`](../../deploy/geoframe-server.service)・[`backup-catalog.sh`](../../deploy/backup-catalog.sh)・[deploy/README.md §7](../../deploy/README.md)
- 検証環境（lxc-geoframe）の演習（T5.4）でも、この手順書を使う。本番と違う所は「**演習では**」で注記する（[§0.6](#06-演習では本番との違い)にまとめた）。

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
| `geoframe-backup.timer` | §4 で止め、§6 で戻す | §6・§10 |
| 作業用の置き場（`/root/p5-cutover`） | 控え・tar・バイナリ・補助スクリプト（`0700 root`。リポジトリの外） | 記録のあと、ユーザーの許可で消す |

### 0.4 時間の目安と止める判断

時計は **§3.1 で Python を止めた時刻（T0）** から始まり、**§6.1 で `/api/v1/photos` が 200 になった時刻（T1）** で止まる。目標は **5 分以内**（[PRD](p5-prd.md)）。演習で測った値で下の目安を置き換える。

| 段 | 目安（演習で置き換える） |
| --- | --- |
| §3 Python を止める | 約 1〜2 分 |
| §4 保全 | 約 1〜2 分（DB が約 85 MiB で数秒） |
| §5 Rust を入れる | 約 1 分 + 起動待ち（`--wait-secs`） |

止める判断（ここでいう「戻す」は **§8 の手順**）:

- **配信が止まっている**（`/healthz` か `/api/v1/photos` が 200 にならない）: T0 から **10 分**で直らなければ戻す。
- **配信は続いているが機能に問題がある**（写真の欠け・管理操作の失敗・取り込みの失敗）: T1 から **30 分**のうちに原因と直し方が見えなければ戻す。
- **`rust-deploy.sh --production` の終了コードが 0 でない**: 待たずに**すぐ §8**（理由は [§5](#5-rust-を入れる)）。
- どちらにも当たらない迷いは、ユーザーに聞く。時間が過ぎていても、原因が見えていて直る見込みが数分ならユーザーが続けてよい（その判断を記録する）。

### 0.5 共通の約束と補助スクリプト

- **コマンドは root のシェル（`sudo -i`）で打つ**前提で書く。`/var/lib/geoframe` は `0750 geoframe` なので、`sudo` を付けてばらばらに打つと、glob（`catalog.db*`）とリダイレクト（`>`）が一般ユーザーのシェルで展開されて失敗する。ばらばらに打つなら `sudo sh -c '…'` に包む。サービスのユーザーで動かす所だけ `sudo -u geoframe` と書いてある。
- **環境変数 `GEOFRAME_RS_*` を持ったシェルでは `rust-deploy.sh --production` を動かさない**（持っていれば終了コード 4 で断られる）。確かめ方は §1.8。
- **秘密と実値を画面から文書へ写さない**: ホスト名・件数・SHA・ファイル名・座標・地名・設定の値は、`p5-acceptance.md` にも書かない（書くもの・書かないものは §14）。設定のファイルは、モード・所有者・**名前だけ**を確かめ、中身は読まない。
- 変数（値は自分で埋める。ここへ書き戻さない）:

```sh
WORK=/root/p5-cutover                 # 作業用の置き場（§1.4 で作る。リポジトリの外・0700 root）
HOST='<本番のホスト名>'                # hostname の出力
COMMIT='<P5aのマージのコミット（40桁）>'
SHA='<演習で記録したバイナリのsha256（64桁）>'
BIN="$WORK/geoframe"                  # 切替に使うバイナリ（§1.8 で置く）
BASE=http://127.0.0.1:8080
```

補助スクリプトは §1.4 で `$WORK` に作る（標準ライブラリだけ。`python3 -I` で動かす）。出すのは件数・ID・固定のキー名だけで、写真の値は出さない。

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
| `fuser`（psmisc） | 無ければユーザーの許可で導入 | 有無を確かめ、無ければ導入 |
| 設定のファイルの値 | 本番の値 | `GEOFRAME_DRIVE_KEY` は存在しないパス（取り込みは Drive へ出ずに終わる）。AI 設定はローカルの用途だけ。Python 役は AI 無効・鍵なし |
| データ | 本番 | 本番のバックアップから作った新しいコピー（鍵は入れない） |
| §6 の miharashi・管理画面 | 実機 | 検証環境の方法（`tools/miharashi-feed-check`・`compare_with_python.py` など）。結果は `p5-acceptance.md` |
| 時間 | 本番の T0〜T1 を記録 | `--wait-secs` の値と、停止から配信の再開までを**測る** |
| 立ち会い・許可 | ユーザー | 演習の開始の許可だけでよい |
| 終わったあと | 猶予期間 | 一時的な Python 版 unit・drop-in・設定のファイル・印の退避を元に戻す（設計 §演習 9） |

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
command -v fuser sqlite3 curl python3 tar gzip sha256sum file
sqlite3 --version
```

- 確かめること: すべて出る。**`fuser`（psmisc）が無ければ**、導入（`apt install psmisc`）をユーザーに提案する。許可が出なければ §4.2 の代わりの確認（`find /proc`）を使う。`jq` は使わない（補助スクリプトが `python3 -I`）。
- 失敗したら: 無い道具があれば、その段の前に導入の許可を取る（本番の依存を勝手に足さない）。

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
# 使い方: python3 -I extra.py <フィードのJSON> <ID の一覧>   一覧の写真の件数と、属する群の数だけを出す
import json, sys
feed = json.load(open(sys.argv[1]))
ids = {l.strip() for l in open(sys.argv[2]) if l.strip()}
rows = [p for p in feed if str(p["id"]) in ids]
def groups(key):
    return len({p.get(key) for p in rows if p.get(key) is not None})
print("件数", len(rows), "ai_group", groups("ai_group"), "quality_group", groups("quality_group"))
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
# 使い方: python3 -I check_files.py <フィードのJSON>   フィードの file を全部 GET し、200 でない数だけを出す
import http.client, json, sys, urllib.parse
feed = json.load(open(sys.argv[1]))
conn = http.client.HTTPConnection("127.0.0.1", 8080, timeout=30)
bad = n = 0
for p in feed:
    f = p.get("file")
    if not isinstance(f, str):
        continue
    n += 1
    conn.request("GET", "/" + urllib.parse.quote(f.lstrip("/")))
    r = conn.getresponse()
    r.read()
    if r.status != 200:
        bad += 1
print("確かめた", n, "200でない", bad)
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
SQL
chmod 0600 *.py counts.sql
```

- 確かめること: `ls -l "$WORK"` で全部 `-rw-------` root。`counts.sql` は §4・§8・§10 で、`sudo -u geoframe sqlite3 '<URI>' < "$WORK/counts.sql"` の形で使う（リダイレクトは root のシェルが開くので、`geoframe` が `$WORK` を読めなくてよい）。

### 1.5 設定のファイルの分割とモード

目的: Rust の設定を Python と分け、`geoframe-rs.service` と `geoframe-admin.sh`（と、すべての CLI）が**同じファイルから同じ DATA_DIR**を読むようにする。値は文書に書かない（名前と意味だけ）。

| ファイル | モード・所有者 | 何を置くか |
| --- | --- | --- |
| `/etc/default/geoframe`（共通。今のもの） | `0600 root:root` | Python と Rust が同じ意味で読む値（`GEOFRAME_ALLOWED_HOSTS`・`GEOFRAME_AI_ENABLED` など）。今の中身のまま |
| `/etc/default/geoframe-rs`（Rust だけ。**無いと Rust は起動しない**） | `0600 root:root` | `GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`（Rust は解釈せず libc に渡す。無いと撮影時刻が 9 時間ずれる）・`GEOFRAME_INGEST=1`・`GEOFRAME_AI_CONFIG`（AI 設定の JSON のパス）・`GEOFRAME_TILE_UA`（連絡先入り。値はユーザーと確かめる）・`app/.env` から移す値 |
| `/etc/default/geoframe-py-rollback`（Python へ戻すときの AI の停止） | `0600 root:root` | `GEOFRAME_AI_ENABLED=0`・`GEOFRAME_LOC_SUGGEST=0`（PythonのAIをすべて止める） |

- `GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ` は、これまで `geoframe-server.service` の `Environment=` にあった。**同じ値**を `geoframe-rs` へ移す（`geoframe-rs.service` には `Environment=` が無い）。`geoframe-admin.sh` は `GEOFRAME_DATA_DIR` を**絶対パス（英数字と `_ . / -` だけ）**で読めないと動かない。
- **同じ名前を `geoframe` と `geoframe-rs` の両方に置かない**（`geoframe-rs` が後に読まれて勝つが、混乱のもと）。
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
- 鍵（Drive の鍵。クラウド AI を有効にするとき同じ鍵を使う）の所有者とモード: `stat -c '%a %U:%G' "${GEOFRAME_DRIVE_KEY のパス}"` が **`600 geoframe:`**（実行ユーザーの所有で、他から読めない）。**root 所有やグループ可読は、クラウドで拒否される**（[ai-cloud.md](ai-cloud.md)。拒否は `Unauthorized` 停止になり、直しても自動では戻らず `release-pause` が要る）。中身は読まない。
- 失敗したら: モード・所有者を直すのは設定の変更なので、ユーザーの許可を得てから（`chown root:geoframe`・`chmod 0640`）。

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
env | grep '^GEOFRAME_RS_' || echo "GEOFRAME_RS_* なし（正常）"
install -m 0755 -o root -g root '<受け取ったバイナリ>' "$BIN"
sha256sum "$BIN"                      # = $SHA（演習で記録した値）
file "$BIN"                           # statically linked / static-pie linked
ls -l /usr/local/bin/geoframe* 2>&1   # 無い（初回の配布）
```

- 確かめること: `sha256sum` が `$SHA` と**完全に一致**。静的リンク。`GEOFRAME_RS_*` が環境に無い。`/usr/local/bin/geoframe` が**無い**（あれば初回の配布ではない。理由をユーザーに聞く）。
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
( cd /var/lib/geoframe/travel_blog/drafts 2>/dev/null && find . -type f -exec sha256sum {} + | LC_ALL=C sort -k2 ) > drafts-before.sha
wc -l < drafts-before.sha
MIN=$((COUNT * 98 / 100)); echo "--expect-min-photos=$MIN"     # 控えた件数より少し下
WAIT='<演習で測った --wait-secs の値>'
/opt/geoframe/deploy/rust-deploy.sh --production="$HOST" --bin="$BIN" \
    --expect-sha256="$SHA" --expect-commit="$COMMIT" --expect-min-photos="$MIN" --wait-secs="$WAIT" --check-only; echo "rc=$?"
```

- 確かめること:
  - 一覧の件数が §1 以降で大きく変わっていない。`py-status.txt` に `db.loc.clue_pending` がある。`ai_task_blocks` などの AI の追加キーは Python では `なし`。
  - `--check-only` が終了コード 0。**これは §5 のコマンドから `--check-only` を除いただけ**にしておく（同じ引数を通す）。
  - 控えたもの（`py-feed.json`・`py-ids.txt`・`py-status.txt`・`drafts-before.sha`）は `$WORK` にあり、リポジトリには入れない（リポジトリには**件数だけ**）。
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

原因をユーザーに報告して延期する。

## 4. 保全

目的: Rust を起動する前に、元に戻れる材料（DB のバックアップと、バックアップに入っていない一次データ）を取る。書き手がいない状態で取る。`backup-catalog.sh` は名前が `catalog-*` で `.dump` のハッシュも取らないので、ここでは**手で**取る。

### 4.1 バックアップのタイマーを止める

```sh
systemctl stop geoframe-backup.timer
systemctl is-active geoframe-backup.timer geoframe-backup.service    # どちらも inactive
```

- 確かめること: 両方 `inactive`。`geoframe-backup.service`（oneshot）が走っていたら終わるのを待つ。
- 切替が終わったら戻す（§6.6）。**途中で止めたときも戻す**。

### 4.2 書き手がいないこと

```sh
fuser -v /var/lib/geoframe/catalog.db*; echo "fuser rc=$?"      # 出力が空・rc=1
pgrep -u geoframe -a; echo "pgrep rc=$?"                         # 出力が空・rc=1
```

- `fuser`（psmisc）が無いときの代わり: `find /proc/[0-9]*/fd -lname '/var/lib/geoframe/catalog.db*' -print 2>/dev/null`（出力が空）。
- 確かめること: DB を開いているプロセスも、`geoframe` ユーザーのプロセスも**無い**（unit の外で動いている Python なども無い）。
- 失敗したら: **何かが出たら先へ進まない**。`ps -fp <pid>` で何かを確かめ、ユーザーに報告する（勝手に kill しない）。

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
DUMPSHA=$(sudo -u geoframe sqlite3 "file:$SNAP?immutable=1" .dump | sha256sum | cut -d' ' -f1); echo "dump sha256: $DUMPSHA"
sudo -u geoframe sqlite3 "file:$SNAP?immutable=1" < "$WORK/counts.sql" | tee "$WORK/precutover-counts.txt"
gzip -9 -c -- "$SNAP" > "$OUT.part" \
  && chown geoframe:geoframe "$OUT.part" && chmod 0640 "$OUT.part" \
  && mv "$OUT.part" "$OUT" && gzip -t "$OUT" && rm -f -- "$SNAP"
ls -l "$OUT"                                       # -rw-r----- geoframe geoframe
ls -A /var/backups/geoframe | grep '^\.' || echo "一時ファイルなし"   # .tmp-・.p5-copy. が残っていない
```

- 確かめること: `integrity_check` が **`ok` の 1 行だけ**。`DUMPSHA` を控える（§10 の復元で照合する）。`precutover-counts.txt` に photos・manual・favorite・hidden・rotated・quality_rows・quality_groups・algorithm 別の件数が出る（§8 で比べる）。バックアップは `p5-precutover-<時刻>.db.gz`・`0640`・`geoframe` 所有（`catalog-*` に当たらない名前なので、7 日の世代削除の対象外）。**P5 の記録が終わるまで消さない。**
- 失敗したら: `.backup` が失敗・`integrity_check` が `ok` でないときは先へ進まない（配信は止まったまま）。**ここで原因が分からなければ、§3 の「止まったとき」で Python を戻す**（DB は Python が止まって以降、誰も書いていない）。

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
- 失敗したら: tar が作れなければ先へ進まない（`app/.env` や設定のファイルは Python 側の設定の唯一の控え）。

## 5. Rust を入れる

目的: 演習で使ったバイナリそのものを、`rust-deploy.sh --production` で入れる（unit の導入・enable・起動・確認まで）。許可を得てから。

```sh
env | grep '^GEOFRAME_RS_' || echo "GEOFRAME_RS_* なし（正常）"
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
| **3** | 失敗し、戻す途中でも失敗した。手作業の復旧手順が出る（文言は「検証環境で」とあるが本番でも同じ操作） | まず `systemctl stop geoframe-rs` で止めてから、**§8**。出た手順と状態（`ls -l /usr/local/bin/geoframe*`・`systemctl status geoframe-rs`）を報告 |
| **4** | 誤配布の防止で拒否した（何も変えていない。理由が出る。ホスト名・印・mask・設定のファイル・Python が稼働中・古い置き場所など） | 理由が数分で直せるなら直して再実行。直せなければ **§8** |
| **2** | 引数の誤り（何も変えていない） | 引数を直して再実行 |

- **0 でなければ待たずに §8**。Python は止まったままなので、迷っている時間がそのまま停止時間になる。
- **`--rollback --production` は、初回の切替の戻しには使えない**。初回の配布は `.prev`（直前のバイナリ）を作らないので「`.prev` が無い」で終了コード 1 になる。`--rollback --production` は、**Rust を一度入れたあとで更新に失敗した**ときに、Rust の直前の版へ戻す道具（§11 以降）。
- 確かめること（0 のとき）: 「確認 OK（/healthz 200、/api/v1/photos は配列で N 件 >= 下限 M）」「完了: geoframe <版>」が出る。

```sh
systemctl is-enabled geoframe-rs; systemctl is-active geoframe-rs      # enabled / active
systemctl is-active geoframe-server                                    # inactive
ls -l /usr/local/bin/geoframe*                                         # geoframe（初回なので .prev は無い）
journalctl -u geoframe-rs -n 30 --no-pager                             # [guard] の info が 1 行。エラーが無い
```

## 6. 確かめる

目的: 配信が戻ったこと、Python の配信にあった写真がすべて Rust にあること、取り込み・AI・管理画面・miharashi が動くことを確かめる。

### 6.1 配信の再開（T1）

```sh
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/healthz            # 200
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/v1/photos      # 200
T1=$(date +%s); date '+%F %T'; echo "停止から再開まで $((T1 - T0)) 秒"
```

- 確かめること: 両方 200。**時間を記録**する（目標 5 分以内、10 分で戻す）。
- 失敗したら: 200 にならないまま T0 から 10 分で §8。

### 6.2 写真の集合の比べ方

Python の配信にあった写真が**すべて**Rust の配信にあり、増えた分は既知の差の範囲であること。

```sh
cd "$WORK"
curl -fsS $BASE/api/v1/photos -o rs-feed.json
python3 -I ids.py rs-feed.json | LC_ALL=C sort > rs-ids.txt
wc -l < py-ids.txt; wc -l < rs-ids.txt
comm -23 py-ids.txt rs-ids.txt | wc -l                    # Python だけにあった写真: 0 でなければ失敗
comm -13 py-ids.txt rs-ids.txt > rs-only.txt; wc -l < rs-only.txt    # Rust だけの写真（増えた分）
python3 -I extra.py rs-feed.json rs-only.txt
python3 -I check_files.py rs-feed.json                    # 画像: 200でない 0
```

- 確かめること:
  - **Python だけにあった写真が 0**（`comm -23` が 0）。1 件でもあれば失敗。
  - 増えた分（`rs-only.txt`）は、[p4-acceptance.md §7.3](p4-acceptance.md) の既知の差で説明できること: 旧グループの**推薦写真（代表）を回転した**ために旧特徴量の世代が合わなくなり、Rust は世代が合わない代表を有効と見なさず、残りの写真を隠すのをやめる（P4 の受入では 18 の旧グループで 45 件）。増えた写真が、回転済みの代表のいる群（`extra.py` の群の数）に限られること。**0 件でも異常ではない**（受入の前は 3,065 件で同数）。説明できない増加は、§0.4 の 30 分で原因を見る。
  - 画像（フィードの `file`）が全部 200（`200でない 0`）。
- 失敗したら: Python だけの写真が出た・画像が欠けたときは、件数（と群の数）だけをユーザーに報告し、30 分のうちに原因が見えなければ §8。

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
journalctl -u geoframe-rs --since '<T0の時刻>' -p warning --no-pager | cut -c1-200
```

- 確かめること: `[sync] サイクル完了: 表示可能 N / 取得待ち N / 全N` が出る（起動直後に 1 回。以降 `GEOFRAME_DRIVE_POLL_MIN` ごと）。失敗した段の警告が無い（あっても段の名前と種類だけの定型文。写真の値は出ない）。
- 失敗したら: 失敗した段があれば、その種類をユーザーに報告し、30 分のルールで判断する。Drive の鍵が読めない等の外部要因は配信に影響しないので、原因が見えれば続ける。

### 6.6 バックアップのタイマーを戻す

```sh
systemctl start geoframe-backup.timer
systemctl is-active geoframe-backup.timer; systemctl is-enabled geoframe-backup.timer   # active / enabled
systemctl list-timers geoframe-backup.timer
```

### 6.7 そのほかの確認

```sh
/opt/geoframe/deploy/deploy-pull.sh --dry-run; echo "rc=$?"      # 拒否（rc=3）。サービスにも git にも触れない
/opt/geoframe/deploy/geoframe-admin.sh ai-jobs status             # 読み取りだけ。対象の DATA_DIR（パス）が最初に出る
systemctl show geoframe-rs -p NRestarts -p Result                 # NRestarts=0
```

- 確かめること: `deploy-pull.sh` が固定の印で断る（rc=3）。`ai-jobs status` が動く。`NRestarts=0`。

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

目的: 問題が出たとき（§0.4 の基準・§5 の終了コード）に Rust を止め、Python に戻す。**AI は無効**で戻す（drop-in が効く）。既定は **DB を保ったまま**戻す（Python は Rust が足した表・列を無視して読める。[p4-acceptance.md §7](p4-acceptance.md)）。DB が壊れたときだけ §10。許可を得て（迷う時間が停止時間なので、§5 の失敗では即実行してよい）。

### 8.1 Rust を止めて mask する

```sh
TR=$(date +%s); date '+%F %T'                      # 戻し始めた時刻
systemctl stop geoframe-rs
systemctl disable geoframe-rs || true
systemctl mask geoframe-rs
systemctl is-active geoframe-rs                    # inactive
systemctl is-enabled geoframe-rs                   # masked
fuser -v /var/lib/geoframe/catalog.db*; echo "fuser rc=$?"      # 空・rc=1（Rust が DB を開いていない）
```

- 初回の配布の失敗で unit が外れているとき（`not-found`）も同じ手順で通る（mask は unit が無くても効く）。
- **mask が失敗したらここで止まる**。`/etc/systemd/system/geoframe-rs.service` に本体が無いか（`ls -l`）を見る。

### 8.2 Python を起動する

```sh
systemctl unmask geoframe-server
systemctl daemon-reload
systemctl show -p FragmentPath -p DropInPaths -p EnvironmentFiles geoframe-server    # drop-in と 2 つの EnvironmentFile
systemctl enable --now geoframe-server
systemctl is-active geoframe-server                # active
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/admin/status    # 200（Python の確認。Python に /healthz は無い）
curl -fsS -o /dev/null -w '%{http_code}\n' $BASE/api/v1/photos       # 200
TB=$(date +%s); date '+%F %T'; echo "戻すのにかかった時間 $((TB - TR)) 秒"
```

- `show` の確認が**起動の前**であることが重要（drop-in が読まれていなければ、AI が有効な Python が起動して課金・送信が始まる）。`EnvironmentFiles` に `geoframe-py-rollback`（`ignore_errors=no`）が出なければ、`enable --now` をしない。
- 失敗したら（Python が起動しない）: `journalctl -u geoframe-server -n 50 --no-pager`。典型: ①`/etc/default/geoframe-py-rollback` が無い（`-` 無しなので起動しない）、②`/opt/geoframe/app` の権限（`status=200/CHDIR`）、③8080 を Rust が持っている（`ss -ltnp 'sport = :8080'`。Rust が止まっていない）。

### 8.3 確かめる

```sh
PID=$(systemctl show -p MainPID --value geoframe-server)
tr '\0' '\n' < /proc/$PID/environ | grep -E '^GEOFRAME_(AI_ENABLED|LOC_SUGGEST)='    # 両方 =0
journalctl -u geoframe-server --since '<8.2の時刻>' -p err --no-pager | cut -c1-200  # 例外が無い
journalctl -u geoframe-server --since '<8.2の時刻>' --no-pager | grep -iE 'gemini|vertex|aiplatform|embed|decision|loc_suggest'   # AI の送信を示す行が無い（「無効」の行は可）
python3 -I -c 'import json; json.load(open("/var/lib/geoframe/.cache.json")); print(".cache.json: 読める")'
sudo -u geoframe sqlite3 'file:/var/lib/geoframe/catalog.db?mode=ro' < "$WORK/counts.sql" > "$WORK/after-return-counts.txt"
diff "$WORK/precutover-counts.txt" "$WORK/after-return-counts.txt"
( cd /var/lib/geoframe/travel_blog/drafts && sha256sum -c --quiet "$WORK/drafts-before.sha" )   # 出力なし＝下書きが変わっていない／消えていない
curl -fsS $BASE/api/v1/photos -o "$WORK/py-feed-2.json"; python3 -I "$WORK/check_files.py" "$WORK/py-feed-2.json"
```

- 確かめること:
  - **Python の AI が無効**: プロセスの環境の `GEOFRAME_AI_ENABLED`・`GEOFRAME_LOC_SUGGEST` が `0`（共通の `/etc/default/geoframe` が `1` でも、drop-in が勝つ）。journal に AI の送信を示す行が無い（Python 版のログの文言は演習で出たものを見本にする）。
  - 件数: `photos`・手動の位置（`manual`）・お気に入り・除外（`hidden`）・回転（`rotated`）が、Rust の期間にした操作を含めて保たれている。切替の直後に戻したなら §4 の控えと同じ。Rust の期間にユーザーが操作したなら、その分の差だけ。下書き: 消えていない（Rust の期間に保存した分は `sha256sum -c` に「変更」と出るが、消えたものは出てはいけない）。
  - `.cache.json` を Python が読める。画像の欠けが 0。
  - 管理画面が動く（ユーザーが確かめる）。
- 品質（簡易品質）の切り戻し — [server.md §16.10](server.md#1610-rustで作った結果をpythonへ切り戻すp5のロールバック)。次の**2つの場合**のどちらかを記録する:
  1. **そのまま戻した直後**（Python の変化なしのサイクル）: `photo_quality` の行数（`quality_rows`）・グループ数（`quality_groups`）・`/api/v1/photos` の件数が戻す前と**同じ**。`algorithm` は `rust-phash-v1:…` のまま。
  2. **`dirty` の再構築が走った後**（新しい写真が入る・何かで `dirty` になった）: 件数・グループの顔ぶれ・`kind` は変わらない。変わりうるのは順位・推奨・スコア・`group_id` の振り直しと、完全重複のグループで表に出る写しの入れ替わり。`algorithm` は全行 `poc-phash-v1:…`。行数・グループ数を記録する。
  - Python だけで解析した結果に戻したいとき（Rust の結果を保つ方法ではない）は、**Python を起動する前に**、server.md §16.10 の手順 2（`photo_quality_features` の Rust が解析した行の削除）を、ユーザーの許可を得て行う。件数・グループが変わりうる。
- 失敗したら: 件数が減った・手動の列が消えた・下書きが消えたときは、**DB を疑う**（§10 の復元。失うものを先に読む）。配信が動いているなら、まず原因を調べる。

### 8.4 戻したあとの注意

- **checkout の固定の印（`/etc/geoframe/checkout-pinned`）は外さない**（Python へ戻したあとも `deploy-pull.sh` は P5b 以降を取り込まない）。Python の更新が要るときは、`/opt/geoframe` を P5a のマージのコミットへ**手で**戻す: `git -C /opt/geoframe reset --hard "$COMMIT" && systemctl restart geoframe-server`。
- Rust のバイナリ・unit・`/etc/default/geoframe-rs` は残す（mask されているので起動しない）。Python のあいだは `GEOFRAME_INGEST=1` の書き込みは動かない。
- `geoframe-backup.timer` が動いていること（`systemctl is-active geoframe-backup.timer`）。
- 原因と戻した時刻をユーザーに報告し、`p5-acceptance.md` に記録する。

## 9. 切り替え直す

目的: Python へ戻したあと、原因を直してもう一度 Rust にする。許可を得て。

1. **Python を止めて mask する**（drop-in は残したまま）:

```sh
systemctl stop geoframe-server
systemctl disable geoframe-server || true
systemctl mask geoframe-server
systemctl is-active geoframe-server; systemctl is-enabled geoframe-server    # inactive / masked
```

2. **`geoframe-rs` を unmask する**（先にしないと `rust-deploy.sh` が mask を理由に断る）:

```sh
systemctl unmask geoframe-rs
systemctl is-enabled geoframe-rs                   # disabled（unit が残っているとき）／ not-found（失敗で外れたとき）
```

3. **§2 をやり直す**（Python の `/api/v1/photos` の ID の一覧・件数・STATUS を控え直す。Python のあいだに写真が増減している）。`--check-only` も通す。
4. **§4 の保全を取り直す**（`$TS` を新しくする。名前は `p5-precutover-…` のままでよい。前回のものは消さない）。`geoframe-backup.timer` を止めてから。
5. **§5 → §6** を実行する。バイナリは同じものでよい（`--expect-sha256` は同じ）。

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
fuser -v /var/lib/geoframe/catalog.db*; echo "fuser rc=$?"      # 空・rc=1
```

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

```sh
BK=/var/backups/geoframe/p5-precutover-<時刻>.db.gz          # 復元に使うバックアップ
TMP=/var/lib/geoframe/.catalog.db.restore-$TS                  # 同じディレクトリの一時名
gzip -dc "$BK" > "$TMP"
chown geoframe:geoframe "$TMP"
chmod --reference="$ASIDE/catalog.db" "$TMP" 2>/dev/null || chmod 0640 "$TMP"    # 元と同じモード
sudo -u geoframe sqlite3 "file:$TMP?immutable=1" 'PRAGMA integrity_check'                 # ok の 1 行
sudo -u geoframe sqlite3 "file:$TMP?immutable=1" .dump | sha256sum | cut -d' ' -f1        # = バックアップの .dump の SHA-256
```

- 確かめること: `integrity_check` が `ok`。`.dump` の SHA-256 が、**取ったときに控えた値**（`p5-precutover-*` なら §4.3 の `$DUMPSHA`）と一致する。他のバックアップ（`catalog-*.db.gz`）は控えが無いので、`$WORK` に別に展開して同じ方法でハッシュを取り、照合の基準にする（`/var/lib/geoframe` の外・root の `sqlite3` で）。
- 失敗したら: 一致しなければ、その一時ファイルを置かない（`rm "$TMP"`）。別のバックアップを試す。**いまの DB は `$ASIDE` にあるので、戻せる**（`mv "$ASIDE"/catalog.db* /var/lib/geoframe/`）。

### 10.4 置き換えて、所有者を確かめる

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
python3 -I "$WORK/check_files.py" "$WORK/restored-feed.json"        # 画像の欠け（200でない）の数
sudo -u geoframe sqlite3 'file:/var/lib/geoframe/catalog.db?mode=ro' < "$WORK/counts.sql"
```

- 確かめること: 配信が 200。表示の画像の欠けが 0。
- **画像の欠け**: 回転は新しい画像を作って古い画像を消す。DB だけを古い時点に戻すと、`display` が消えた画像を指す行ができる。`fetch_derivatives` は `display IS NULL` の行だけを取り直すので、**取り込みのサイクルでは自動では直らない**。管理画面の回転は、今の画像が無いと 400 で、回転では直らない。件数（`check_files.py` の「200でない」）を記録し、直し方は **演習（T5.4）で確かめて `p5-acceptance.md` に記録した手順**に従う（候補: 該当行の `display` を `NULL` に戻して取得し直させる。DB への書き込みなので、サービスを止めて `geoframe` ユーザーで、ユーザーの許可を得て行う）。
- 失敗したら: 起動しない・画像の欠けが多い・件数が想定と違うときは、`$ASIDE` の DB に戻せる（止める→3 つを入れ替える→起動）。原因をユーザーに報告する。

### 10.6 タイマーを戻す

```sh
systemctl start geoframe-backup.timer
systemctl is-active geoframe-backup.timer                          # active
```

途中で止めたときも戻す。`$ASIDE` は P5 の記録が終わるまで消さない（ユーザーの許可で消す）。

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
journalctl -u geoframe-rs --since '<T0の日>' -p warning --no-pager | cut -c1-160 | sort | uniq -c | sort -rn | head
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
2. venv を作り直す: [deploy/README.md §1](../../deploy/README.md) の手順（`python3 -m venv app/.venv`・`app/.venv/bin/pip install -r app/requirements.txt`）。
3. Python 版の unit（そのコミットの `deploy/geoframe-server.service`）を `/usr/local/lib/systemd/system/` に置き、§3.3 の drop-in と `/etc/default/geoframe-py-rollback` を作り直す（AI は無効）。固定の印も置き直す。
4. §8（Rust を止めて mask → Python を unmask して起動 → 確かめる）。

## 14. 記録

`p5-acceptance.md`（T5.4 の演習と T5.5 の本番）に書く。**リポジトリ・文書・PR には、件数・ハッシュ・時間・commit・sha256 だけを書く。**

**書くもの**

- 日付、立ち会い、許可を得た段（事前にまとめて許可した範囲）。
- `COMMIT`（P5a のマージのコミット）と演習のコミット、その差が無いことの確認。バイナリの sha256。
- 件数: Python の写真の件数、Rust の件数、Python だけ（0）・Rust だけの件数と群の数、`--expect-min-photos`、`db.loc.clue_pending` の初期値。保全の `.dump` の SHA-256、`photo_quality` の行数・グループ数（`algorithm` 別）、手動・お気に入り・除外・回転の件数。
- 時間: T0〜T1（停止から配信の再開まで）、`--wait-secs`、各段の時間。戻した場合は戻すのにかかった時間。
- 終了コード（`rust-deploy.sh`）、止める判断を使ったか、その理由。
- 取り込みの 1 サイクルの結果（表示可能／取得待ち／全の件数）、`clue_pending` が 0 になった時刻、索引の公開の時刻、最初のバックアップの成功。
- 見つかったこと（手順書の直し）と、直し方。

**書かないもの**

- ホスト名・LAN のアドレス・ユーザー名、写真のファイル名・座標・地名、設定の値（`GEOFRAME_*` の値・AI 設定の JSON の中身・URL・モデル名・project・region・鍵のパス）、鍵・トークン、差分の中身。これらは作業用の置き場（`$WORK`）に残し、記録が終わったらユーザーの許可で消す。
