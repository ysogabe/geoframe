# P5の受入確認（T5.4 演習・T5.5 本番の記録）

- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）/ 要求: [p5-prd.md](p5-prd.md)「受け入れ条件」/ 設計: [p5-design.md](p5-design.md) §演習 / 手順書: [cutover.md](cutover.md)（§15 が演習のチェックリスト）/ タスク: [tasks.md](tasks.md) T5.4・T5.5 / 前フェーズ: [p4-acceptance.md](p4-acceptance.md)
- **写真のid・ファイル名・座標・地名・撮影時刻・検索語・旅程・設定の値・ホスト名・LANのアドレス・ユーザー名は書かない。件数・状態コード・時間・ハッシュ・commit・sha256・固定の理由だけ**（[cutover.md §14](cutover.md#14-記録)）

## 本番で使うもの（T5.5 の前提）

| 項目 | 値 |
| --- | --- |
| 演習のコミット（ブランチ `feat/rust-migration-p5`） | `712e61a038c76a739f291c9f5e7976a86669927a` |
| P5a のマージのコミット（本番の `--expect-commit`） | `a804623029ad67d415692d08ea3182fd765eb2a4`（geoframe-ambient-display の PR #63「Rust移行 P5a: 本番切替の準備」） |
| 演習のコミットとマージのコミットの差（`crates`・`Cargo.*`・`app`・`deploy`。[cutover.md §1.2](cutover.md#12-演習のコミットと-p5a-のマージのコミットに差が無いことを確かめる)） | **空**（本番の §1.2、2026-10-30） |
| バイナリ | `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`（演習のコミット）。20,272,504 B、static-pie |
| バイナリの sha256（本番の `--expect-sha256`） | `65755b969077a608a54ddacbd651495f56afd1ca9e6f1f4a32a3466d199406e9` |
| バイナリの保管先 | 開発機（本番と同じ機体）の `~/.cache/geoframe-t54/geoframe-65755b969077`（`0444`。演習の前と後に sha256 が上と一致）。検証環境へはこれを写して使い、演習で入れた `/usr/local/bin/geoframe` と `$BIN` も同じ sha256 だった（[cutover.md §15.6](cutover.md#156-元に戻す流れ-9) の 2） |
| `--wait-secs` | **30**（起動から `/healthz` 200 まで 0.2〜0.4 秒を測った。§2.3） |
| 停止時間の目安 | T0〜T1 が 42〜43 秒（コマンドを続けて打った場合。§2.2・§5） |

## T5.4 検証環境の演習

- 日時: 2026-10-30 06:51〜07:20。検証LXC（lxc-geoframe）で実施。本番機ではない。接続先の値は Git 管理外のメモにあり、ここには書かない
- 許可: ユーザーが演習の開始を許可した（[cutover.md §0.6](cutover.md#06-演習では本番との違い)「立ち会い・許可: 演習の開始の許可だけでよい」）。本番の新しいバックアップは、ユーザーが開発機に置いたもの（`gzip -t` は ok）を検証環境へ送って使い、演習のあと両方の機械から消した
- 本番機では、サービスの操作・`/var/lib/geoframe`・`/etc`・`/usr/local` への書き込み・本番の `catalog.db` への接続をしていない（開発機でしたのは、保管済みのバイナリと bundle の転送、`cargo test` 1 件、`miharashi-feed-check` のビルドと検証環境への実行だけ）
- 鍵: 演習のデータに Drive の鍵・AI の鍵を入れていない。`GEOFRAME_DRIVE_KEY` は存在しないパス。検証環境の `/var/lib/geoframe` にあった鍵のファイル（2 つ）は、中身を読まず、DATA_DIR ごと `mv` で退避して戻しただけ。AI 設定はローカルの用途（`image_embedding`・`query_embedding`・`location_clue`）だけで、**クラウドへは送っていない**（`photo_judge`・`location_suggest` は `task_not_configured`）
- 演習だけの調整: Decision と Embedding の入口は本番機の上にあるので、`geoframe-rs` に `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE=1`・`GEOFRAME_AI_BACKLOG_POLL_S=60` を足した（本番では足さない。[cutover.md §15.2](cutover.md#152-演習の前の準備と各節の省略置き換え) の §1.5 の行）
- 実行の順: 流れ 2 → 3 → 4 → 5 → 6 → 7 → 1 → 8 → 9（[cutover.md §15.1](cutover.md#151-設計の流れ-19-とこの手順書の対応)）

### 0. 結果の要約

**直すべきスクリプト（`deploy/*.sh`）・Rust の不具合: なし。** 手順書の直しは §9 の 15 件（すべて cutover.md に反映した）。観察は §9.2 の 4 件。

| AC | 結果 | 根拠 |
| --- | --- | --- |
| AC1（演習の部分: `Conflicts=`・`After=` の両方向） | 合格 | 向き B（Rust が動いているときに Python を起動）は Rust の停止の完了から 0.009 秒後に Python が起動。向き A（Python が動いているときに Rust を起動）は Python の停止の完了から 0.016 秒後に Rust が起動し、`[guard]` は 0.090 秒後、最初の書き込み（サイクル完了）は 0.255 秒後。どちらも旧が先（§8）。`[guard]` は Python 版が inactive であることを確かめて起動した |
| AC2（演習の部分: 本番と同じ経路） | 合格 | `rust-deploy.sh --production` を 2 回（切替・切り替え直し）通して終了コード 0。`--check-only` は 3 回（§1.10・§2・§9.2）とも 0。印がある・Python が稼働中のときは終了コード 4 で何も変えずに断った。`--expect-min-photos`（3,003・3,006）を下限として確認が通った。`deploy-pull.sh --dry-run` は固定の印で 3（§1・§2・§5） |
| AC3 | 合格 | §10 の手順で、起動の前に `integrity_check` ok・`.dump` の SHA-256 がバックアップの控えと一致・DATA_DIR の直下に `geoframe` 以外の所有のファイル 0。起動のあと配信 200。画像の欠けは 1 件（バックアップのあとに回転した 1 件だけで、想定と一致）。直し方を確定して §10.5 に書いた（§7） |
| AC4 | 合格 | 新しい本番のコピーと本番と同じ配置で、手順書のとおり切り替えた。T0〜T1 は 43 秒（5 分以内）。Python のフィードの 3,065 件がすべて Rust にあり、Rust だけ 0 件。シャドー比較は許容外 0（`shadow_judge.py` 終了コード 0）。取り込みの 1 サイクル（Drive の段は鍵が無いので省略）と、ローカルの AI（Embedding 108 件・手がかり 6 件が成功）が動いた。miharashi の取得 200 件・Retrying 0（§2・§3） |
| AC5 | 合格 | Rust→Python は 3 秒。共通の設定が `GEOFRAME_AI_ENABLED=1` のまま、Python のプロセスの環境は `GEOFRAME_AI_ENABLED=0`・`GEOFRAME_LOC_SUGGEST=0`（drop-in が勝つ）。AI を示す語の journal の行 0、例外の行 0。Rust の期間の手動の位置・お気に入り・除外・回転（ID ごと 20 行）が不一致 0、下書きの全ハッシュの差 0、`.cache.json` は Python の経路で読めた。品質は場合 1・2 とも件数が基準と同じで「説明できない変化 0」。再び Rust へ切り替えて 42 秒で配信が戻った（§4・§5）。Rust が作った品質の結果（`rust-phash-v1`）を戻す経路は演習で起きず、§4.4 の根拠（CI の `switchbacks` と Rust の試験、品質の実装に P5 の変更なし）で代えた |
| AC8（演習の部分） | 合格 | 旧 Python の判定済みの行（`photo_loc_clues` 1,216 行。Python の STATUS は `clue_pending=0`）がある DB で、Rust の `db.loc.clue_pending` が 1,215（`clue_runnable` も 1,215・`clue_pause` は空）。Decision が動いて 1,215 → 1,210 → 1,209 と減った（§6） |

### 1. 準備（[cutover.md §15.2](cutover.md#152-演習の前の準備と各節の省略置き換え)・§1）

| 手順 | 結果 |
| --- | --- |
| 演習の前の控え | §15.2 のコマンドに、設定のファイルのモード・内容の sha256、比較用 Python のファイルの名前・内容のハッシュ、DATA_DIR の直下の名前などを足した 1 つのスクリプトで取った（§15.6 の 8 で同じスクリプトと `diff`） |
| 検証環境の Rust の退避 | stop → disable → `/etc/systemd/system/geoframe-rs.service`（と `.prev`）・空の drop-in のディレクトリ・`/usr/local/bin/geoframe*` を root の退避先へ → `daemon-reload`。`is-enabled` は `not-found`。元の DB の `.dump` は `57f6dceff3b48bc5…`（P4 の受入と同じ） |
| 印・共通の設定 | 印 `/etc/geoframe/staging` を退避。共通の `/etc/default/geoframe`（検証環境では `0644`）を `cp -p` で退避し、演習のあいだは `0600 root:root` で、元の名前に `GEOFRAME_AI_ENABLED=0` を足したものにした |
| DATA_DIR | 元の DATA_DIR をディレクトリごと退避（鍵のファイル 2 つも中身を読まずに一緒に動く）。新しい DATA_DIR に derivatives（6,535 ファイル）・旅行ブログの下書き（1 ファイル）・書き出し（44 ファイル）・`photos/`（0 ファイル）・`tmp/` を `cp -a` で写し、本番のバックアップを `geoframe` で展開。`integrity_check` ok、`.dump` は `5175bcbc4f5db6a5…` |
| 本番のコピーの件数 | photos 6,535（すべて `drive`。`display` あり 6,535）・手動 17・お気に入り 0・除外 0・回転 0・`photo_quality` 559 行／243 群（`poc-phash-v1` 559、行の `kind` は exact 185・similar 374）・旧 Python の手がかりの行 1,216 |
| 比較用 Python | `/opt/geoframe-pyref` を退避し、開発機で作った git bundle から演習のコミットの checkout にして、退避した `app/.venv` を同じパスへ写した（`requirements.txt` は変わっていない）。HEAD が演習のコミット、`git status --short` が空（`app/.venv/` だけ無視）。8090 の `geoframe-pyref.service` は止めずに動き続けた |
| 一時的な Python 版 unit | `deploy/geoframe-server.service` の `WorkingDirectory=`・`ExecStart=` の 2 行だけを `/opt/geoframe-pyref` に向けて `/usr/local/lib/systemd/system/` に置き、enable して起動（2 秒で `/api/admin/status` 200、プロセスの環境の `GEOFRAME_AI_ENABLED=0`） |
| ダミーのバックアップのタイマー | §15.2 の §4 の行のとおり `/etc/systemd/system/` に置いた。`enabled`・`active`・`NEXT` が翌日の 0 時 |
| §1.3 | 道具はすべてある（psmisc もある。SQLite 3.45.1）。Python 版の venv あり |
| §1.4 | 補助スクリプトは手順書のブロックをそのまま流して作った（直していない）。`cache_load.py` の空打ちは `file_keys=1 python_loaded=1 python_will_drop=0 python_keeps=1`・rc=0 |
| §1.5・§1.6 | 3 つとも `600 root:root`、`geoframe-rs` に必要な 5 つの名前と演習だけの 2 つ、重複 0、`py-rollback` は 2 行とも `=0`。AI 設定の JSON は検証環境に無かったので作った（`640 root:geoframe`、用途は 3 つとも `local`） |
| §1.7 | Decision の `health` は `ready`・画像に対応 |
| §1.8・§1.9 | sha256 一致・static-pie・`/usr/local/bin/geoframe` は無い。印なし・固定の印なし・古い本体なし・`geoframe-rs` は `not-found`・Python 版の `FragmentPath` は `/usr/local/lib/…`・バックアップ先は `geoframe` の `0750` |
| §1.10 | `--check-only` 終了コード 0 |
| 拒否の確認（本番の経路） | 仮の印を置くと `--check-only` が終了コード 4（印がある）。Python が稼働中に `--check-only` なしで動かすと終了コード 4（Python 版が稼働中）。どちらも何も変えていない（バイナリ・unit は無いまま） |

### 2. 流れ 2: Python → Rust（§2〜§7）

#### 2.1 §2 の控え

| 項目 | 結果 |
| --- | --- |
| Python のフィード | 3,065 件。`--expect-min-photos=3003`、`--wait-secs=30` |
| STATUS | `db.loc.clue_pending=0`（Python は AI 無効）、AI の追加キーはすべて `なし` |
| 下書き | 1 ファイル（rc=0） |
| 画像の欠けの基準（演習だけ） | `check_files.py` で 3,065 件・200 でない 0 |
| `--check-only` | 終了コード 0 |
| Python の journal | 例外 1 件（`BrokenPipeError`。HTTP の相手が先に接続を閉じた。§9.2） |

#### 2.2 §3〜§5 と時間

| 段 | 結果 |
| --- | --- |
| §3 | stop・disable・drop-in・`show`（`EnvironmentFiles` に 2 つが並ぶ）・mask・固定の印。**1 秒**。§3.5 の直後に共通の設定を `GEOFRAME_AI_ENABLED=1` にした（§9 の 4） |
| §4.1・§4.2 | タイマーとサービスは inactive。`dbopen.sh` rc=0（`fuser` の経路）、`pgrep -u geoframe` rc=1 |
| §4.3 | `mode=ro` の `.backup` が通った（コピーの代わりは不要だった）。`integrity_check` ok、`p5-precutover-*.db.gz` は `0640 geoframe`、一時ファイルなし。T0 から **41 秒**（ほとんどが `.backup` と `gzip -9`） |
| §4.4 | tar 8 項目（`app/.env` は無い）。`0600 root`・`$WORK` の中 |
| §5 | `rust-deploy.sh --production` 終了コード **0**、「確認 OK（/healthz 200、/api/v1/photos は配列で 3065 件 >= 下限 3003）」。**2 秒**。`enabled`・`active`、Python は `inactive`、`.prev` は無い（初回）。journal に `[guard]` の 1 行、`スキーマを更新しました: 作成 14件` |
| T0〜T1 | **43 秒** |

#### 2.3 `--wait-secs` の測定

| 測り方 | 結果 |
| --- | --- |
| `systemctl start` から `/healthz` 200 まで（§7 の起動し直し） | 0.23 秒 |
| `systemctl restart` から `/healthz` 200 まで（3 回） | 0.36 秒・10.34 秒・10.33 秒。10 秒の 2 回は**停止**の待ち（AI の背景ワーカーが Decision・Embedding の要求の途中で、終わりを待つ。P4 の受入の「ワーカーの join の上限 約 10 秒」と同じ）で、起動の時間ではない |
| `rust-deploy.sh` の配布（停止なしの初回） | 2 秒で確認まで終わった |

起動そのものは 0.4 秒以内なので、`--wait-secs` は既定の **30** にする（DB が同じ大きさの本番でも十分な余裕。長くしても失敗の検出が最大 30 秒遅れるだけで、§0.4 の 10 分の期限に収まる）。

#### 2.4 §6 の確認

| 項目 | 結果 |
| --- | --- |
| §6.8 | 展開と控え OK。`.dump` は `5175bcbc…`（本番のコピーと同じ。Python の期間は DB を変えていない）。件数は §1 と同じ |
| §6.2 | Python 3,065・Rust 3,065、Python だけ 0、Rust だけ 0（`extra.py`: 取り込み 0・既知の差 0・説明できない 0、rc=0）。群は `ai_group` 0・`quality_group` 0。画像は 3,065 件・200 でない 0 |
| §6.3 | `ai_task_blocks`・`ai_config_invalid`・`ai_pauses`・`ai_waits` はすべて `なし`。`db.loc.clue_pending=1215`・`clue_runnable=1215`・`clue_pause=''`。Python の控えとの差は `db.ai.suppressed` 2,591 → 2,136（P4 の受入 §7.3 と同じ既知の差）と `clue_*` の 3 行だけ |
| §6.4 | 書き込みの確認（お気に入りを付けて外す）200・200、お気に入りの件数は 0 に戻った。miharashi の取得のハーネス（`tools/miharashi-feed-check`）で 200 件（写真 184・カード 16）、Retrying 0、11 秒、終了コード 0 |
| §6.5 | `サイクル完了: 表示可能 5288 / 取得待ち 0 / 全6535`。`index_drive`・`fetch_derivatives` は「Drive の鍵が無いので飛ばします」。警告 0 |
| §6.6 | タイマー OK（`active`・`enabled`、`NEXT` が先の時刻） |
| §6.7 | `deploy-pull.sh --dry-run` は固定の印で終了コード 3。`ai-jobs status` は AI 設定が有効、用途 3 つが使える、Embedding の待ち 6,427・成功 12、手がかりの待ち 1,215・成功 1、停止中の用途なし。`NRestarts=0` |
| §7 | `ai-jobs retry --kind location_clue` を、止める→CLI→起動し直す の形で。CLI の終了コード 0（戻した 0 など、すべて 0 件）。`geoframe-admin.sh` が対象の DATA_DIR を出した。起動し直したあと `active`・`/healthz` 200 |

### 3. 流れ 3: シャドー比較（[cutover.md §15.3](cutover.md#153-シャドー比較流れ-3)）

| 項目 | 結果 |
| --- | --- |
| 複製 | Rust を §7 の形で止めて 2 つの複製を取り、Rust は自動で起動し直った。2 つの `.dump` は同じ（`b71c9964…`） |
| 一時 unit | Rust は `127.0.0.1:8081`・取り込みなし（「取り込みは無効です」）、Python は `127.0.0.1:8092`・AI 無効（**8090 は検証環境の `geoframe-pyref.service` が使っているので、手順書の 8090 を 8092 に直した**） |
| `compare_with_python.py` | 終了コード 1（`想定外の差 1 項目`）。DIFF は節 2 の 1 行だけで、差のあるキーは `ai_jobs(片方に無い)`・`ai_scenes(片方に無い)`・`db.ai.suppressed` の 3 つ |
| 節ごと | 節 1: JSON 完全一致 3,065/3,065・並びも一致（DIFF なし）。節 3: 状態コードと JSON の一致 222/222。節 4: 表示用画像のバイト一致 3,065/3,065（2,187 MiB）。節 5: ページ 7/7、埋め込みファイル 14/14、Python と状態コード・本文 14/14、拒否 33/33、メソッド 10/10、Host 10/10、既知の差 1/1 |
| フィードの集合 | Python だけ 0、Rust だけ 0（`extra.py` は 0・0・0、rc=0）。`KNOWN=0` |
| STATUS の大小（条件 2） | `diff` に出たのは `db.ai.suppressed` の 1 行だけで、Rust 2,136 ≤ Python 2,591（向きが正しい） |
| 判定 | `shadow_judge.py`: DIFF の行 1・許容 1・Rust だけの写真 0・STATUS の差のキー 3・管理写真の差のキー 0・**許容外 0**、終了コード **0**。例外（条件 3）は使っていない |

一時 unit を止め、複製を消した。

### 4. 流れ 4・5: Rust の期間の操作と、Python へ戻す（§8）

#### 4.1 Rust の期間の操作

手動の位置 1（位置の無い写真に）・お気に入り 1・除外 1・回転 1（90°）・旅行ブログの下書きの保存 1（保存済みの下書きを読み、未知のキーを 1 つ足して保存。内容が変わった）。すべて 200。件数は手動 18・お気に入り 1・除外 1・回転 1。フィード 3,065 件、画像の欠け 0。

#### 4.2 §8.1〜§8.3

| 項目 | 結果 |
| --- | --- |
| §8.1 | 止める前の一覧を控え、stop・disable・mask。`inactive`・`masked`、`dbopen.sh` rc=0 |
| §8.2 | `counts`・`state`（20 行）・`quality`・`drafts` すべて rc=0。切替の前にあった下書きのうち消えたもの 0。`cache_load.py`: `file_keys=1 python_loaded=1 python_will_drop=0 python_keeps=1`・rc=0 |
| §8.3 | unmask・`daemon-reload`。**起動の前の `show`** で `DropInPaths` に `p5-rollback.conf`、`EnvironmentFiles` に共通（`ignore_errors=yes`）と `geoframe-py-rollback`（`ignore_errors=no`）。`enable --now` のあと `active`、`/api/admin/status`・`/api/v1/photos` 200。**戻すのにかかった時間 3 秒** |
| §4.5 | タイマー OK |

#### 4.3 §8.4

| 項目 | 結果 |
| --- | --- |
| Python の AI | 共通の設定は `GEOFRAME_AI_ENABLED=1` のまま。プロセスの環境は `GEOFRAME_AI_ENABLED=0`・`GEOFRAME_LOC_SUGGEST=0`（drop-in が勝つ）。journal の AI を示す語の行 0。例外の行 0（`-q` なしの `wc -l` は、無いときの「-- No entries --」を 1 と数えたので、手順書に `-q` を足した）。Python の STATUS の AI の追加キーはすべて `なし` |
| `.cache.json` | `file_keys=2 python_loaded=2 python_will_drop=0 python_keeps=2`・rc=0（§8.2 の `python_keeps` 1 以上） |
| 件数 | `counts diff` rc=0（手動・お気に入り・除外・回転・品質の行がすべて同じ） |
| ID ごとの状態 | `state_diff.py`: 前 20 件・後 20 件、値が違う 0・前だけ 0・後だけ 0 |
| 下書き | 全ファイルのハッシュの差 0 |
| 画像 | 3,068 件・200 でない 0 |

#### 4.4 品質の 2 つの場合（基準は Rust から戻す直前の値）

| 場合 | `quality_rows` | `quality_groups` | `kind:`（行） | `algorithm` | `quality_diff.py` | `/api/v1/photos` |
| --- | --- | --- | --- | --- | --- | --- |
| 基準（Rust から戻す直前） | 559 | 243 | exact 185・similar 374 | `poc-phash-v1` 559 | — | 3,065 |
| 1 そのまま戻した直後 | 559 | 243 | exact 185・similar 374 | `poc-phash-v1` 559 | 写真: 追加 0 削除 0 更新 0 / 特徴量: 追加 1 削除 0 更新 0 / 変わらない 243・入力の変更に連なる変化 0・説明できない変化 0 | 3,068 |
| 2 `dirty` の再構築のあと | 559 | 243 | exact 185・similar 374 | `poc-phash-v1` 559 | 基準と比べて: 場合 1 と同じ（特徴量の追加 1 だけ）。`dirty` の直前と比べて: 写真も特徴量も 0 件の変更で、変わらない 243・ほかは 0 | 3,068 |

- `algorithm` が `rust-phash-v1` でないのは、Rust の期間に品質の再構築が走らなかった（起動時のサイクルで `pending=0 analyzed=0 rebuilt=0`。演習の期間に新しい写真が入らなかった）ため。手順書の場合 1 の「`rust-phash-v1` のまま」を「基準と同じ」に直した（§9 の 6）。
- 特徴量の追加 1 は、Rust の期間に回転した写真を、戻したあと Python が解析し直したもの（写真は変わっていないので、群は変わらない）。
- フィードの 3,065 → 3,068 は、既知の差 0 と、**Python の最初のサイクルの位置の推定**で位置が付いた既存の写真 3 件（3 件とも `loc_source='inferred'` で、Rust の期間に手動の位置を付けた写真が錨。Rust の期間には次のサイクルがまだ回っていなかった）。`photos` の件数は増えていない。手順書の場合 1 の数え方に、この場合を足した（§9 の 6）。
- 場合 2 は、Python を止め、`geoframe` で `photo_quality_dirty` を `1` にして起動した。再構築は起動から約 5 秒で終わった（`dirty` が `0` に戻り、`max(analyzed_at)` が変わった。`algorithm` が元から `poc-phash-v1` なので、それでは見分けられない。§9 の 7）。

- **Rust が作った品質の結果を Python へ戻す経路（`rust-phash-v1` を基準にした場合）は、この演習では起きなかった**。P5 の全体レビュー（Sol）の指摘を受け、次の根拠でこの経路の確認に代える（管理者の判断。2026-10-30）:
  - `tools/gen_quality_fixture.py` の `switchbacks`（Rust が作った `photo_quality`・特徴量の DB に Python 本体を流し、①そのまま戻した直後は全列が一致して `algorithm` が `rust-phash-v1` のまま、②`dirty` の再構築で全行が `poc-phash-v1` になり、グループの顔ぶれ・`kind` が変わらない、を確かめる）と、Rust 側の `crates/geoframe/src/quality/tests.rs` の切り戻しの試験（[server.md §16.10](server.md#1610-rustで作った結果をpythonへ切り戻すp5のロールバック)）。どちらも CI（`python3 tools/gen_quality_fixture.py --check` と `cargo test`）で回っている。
  - P5（`origin/main...` の演習のコミット）で、品質の実装（`crates/geoframe/src/quality.rs`・`quality/`・`selection.rs`・`sync.rs`）、Python の実装（`app/geoframe/`）、`tools/gen_quality_fixture.py` に変更が無い（`git diff --stat` が空）。
  - 本番では、切替のあと新しい写真が入れば Rust の再構築が走る。猶予期間の中で Python へ戻すときは、cutover.md §8.4 の品質の確認（基準が `rust-phash-v1` の場合の `algorithm` の見分け方を含む）をそのまま使う。

### 5. 流れ 6: 切り替え直す（§9）

| 項目 | 結果 |
| --- | --- |
| §9.1 | unmask のあと `disabled`・`inactive` |
| §9.2 | Python のフィード 3,068 件、下書き 1、画像の欠け 0、`--expect-min-photos=3006`、`--check-only` 0 |
| §9.3 | stop・disable・mask（drop-in は残したまま）。`inactive`・`masked` |
| §9.4 | タイマーを止め、`dbopen.sh` rc=0・`pgrep` rc=1。`.backup`・`integrity_check` ok・`gzip -t`。T0 から 40 秒。tar 8 項目 |
| §9.5 | `rust-deploy.sh --production` 終了コード **0**（3,068 件 >= 3,006）、2 秒。同じバイナリの `.prev` ができた。**T0〜T1 は 42 秒** |
| §6.8 | `.dump` は `515d613c…`。手動 18・お気に入り 1・除外 1・回転 1・品質は §4.4 と同じ |
| §6.2 | Python 3,068・Rust 3,068、Python だけ 0・Rust だけ 0（`extra.py` 0・0・0）、画像の欠け 0 |
| §6.3 | AI の停止・不正はすべて `なし`、`clue_pending=1210`・`clue_runnable=1210`・`clue_pause=''`、`db.ai.suppressed` 2,591 → 2,137 |
| §6.5〜§6.7 | `表示可能 5293 / 取得待ち 0 / 全6535`、警告 0。タイマー OK。`deploy-pull.sh` 3。`ai-jobs status`: Embedding の成功 96・待ち 6,343・`obsolete` 1（回転した写真の古い世代）、手がかりの成功 5・待ち 1,210・`obsolete` 1。`NRestarts=0` |

### 6. 流れ 7: 旧 Python の判定済みの行がある DB での `clue_pending`

| 時点 | `db.loc.clue_pending` | `clue_runnable` | `ai-jobs status` の手がかり |
| --- | --- | --- | --- |
| Python（§2） | 0 | （キー無し） | — |
| 最初の切替の直後 | 1,215 | 1,215 | 待ち 1,215・成功 1 |
| 切り替え直した直後 | 1,210 | 1,210 | 待ち 1,210・成功 5（最初の Rust の期間の成功は、Python の期間をはさんでも保たれた） |
| その約 1 分半後 | 1,209 | 1,209 | 待ち 1,209・成功 6 |

`clue_pause` はずっと空。演習では Decision を 1 周期 1 件に絞った（本番機の CPU を使い続けないため）ので、1 分に 1 件ほどの進み。本番の全件再判定（約 4 時間）は [cutover.md §11.1](cutover.md#111-decision-の全件再判定約-4-時間) で見る。

### 7. 流れ 1: 復元と画像の欠け（§10）

切り替え直したあとの Rust の期間に、§9 で取ったバックアップ（§6.8 で `.dump` を `515d613c…` と控えたもの）のあとに、別の写真を 1 件回転してから戻した。

| 段 | 結果 |
| --- | --- |
| §10.1 | 止める・タイマーを止める・`geoframe-backup.service` は inactive・`dbopen.sh` rc=0 |
| §10.2 | `catalog.db`・`-wal`・`-shm` の 3 つを退避先へ `mv`。残っていない |
| §10.3 | `gzip -t`・展開 OK。`integrity_check` ok。`.dump` の SHA-256 が控え（`515d613c…`）と一致 → 「検査 OK」 |
| §10.4 | `catalog.db` は `geoframe:geoframe`・`0640`。DATA_DIR の直下に `geoframe` 以外の所有のもの 0 |
| §10.5 起動 | 配信 200。画像は 3,068 件・**200 でない 1**。回転の件数は 2 → 1（バックアップのあとの回転が巻き戻った）。手動・お気に入り・除外は同じ |

画像の欠けの直し方（[cutover.md §10.5](cutover.md#105-起動して確かめる) に確定した手順として書いた）を、この 1 件で通した:

| 手順 | 結果 |
| --- | --- |
| 1 対象の限定 | 欠け 1・うち `drive` 1・`local` 0。退避した DB と `display` が違う行（想定）1 件と一致、想定の外の欠け 0 |
| 2 取り直しの印 | サービスを止め、`dbopen.sh` rc=0 を確かめて、`display=NULL` を 1 行（`changes()` 1）。`rotation` は変えない |
| 3 回転の保持 | 対象の行の `rotation`・位置・お気に入り・除外・`md5`・`drive_id` は変わらない。`state_diff.py`（20 行）は差 0 |
| 4 再確認（鍵なし） | 起動直後のサイクルは `fetch_derivatives: 省略（Drive の鍵が無いので飛ばします）`、`表示可能 5292 / 取得待ち 1 / 全6535`（取得待ちが 1 増えた）。`db.no_display` 0 → 1。対象はフィードから外れ（3,067 件・200 でない 0）、miharashi に壊れた画像を渡さない。対象の写真への回転の API は 400（画像が無い） |
| 取り直しの処理本体 | `cargo test -p geoframe --lib a_rotated_photo_is_fetched_again_with_the_same_name_and_orientation` が ok（回転した写真が同じ名前・同じ向きで取り直される） |
| §10.6 | タイマー OK |

**確かめられなかったこと**: 実 Drive からの取り直し（鍵を置かない演習のため）と、`local` の写真の欠け（本番のコピーに `local` が 0 件）。手順書には「本番では対象を 1 件に限って先に取り直し、200・名前が `rotation` と合う・管理画面で向きが合うことを確かめてから残りへ広げる」を条件として書いた。

### 8. 流れ 8: `Conflicts=` と `After=`（[cutover.md §15.5](cutover.md#155-conflicts-と-after-の試験流れ-8手順書の切替とは別)）

両方を unmask した状態（Python は drop-in 付き）で、`systemctl show geoframe-rs` の `Conflicts`・`After` にどちらも `geoframe-server.service` がある。

| 向き | 旧の停止の完了 | 新の起動 | 差 |
| --- | --- | --- | --- |
| B: Rust が動いているときに `systemctl start geoframe-server` | Rust の `Stopped`（停止は 0.07 秒） | Python の `Started` | 0.009 秒あと。Python のプロセスの環境は AI 0・0、`/api/admin/status` 200 |
| A: Python が動いているときに `systemctl start geoframe-rs` | Python の `Stopped` | Rust の `Started` | 0.016 秒あと。`[guard]` は 0.090 秒あと、`スキーマは最新です`、最初の `サイクル完了` は 0.255 秒あと |

どちらも旧プロセスが先に終わった。通常の状態（Rust のみ `enabled`・`active`、Python は `masked`・`inactive`）に戻し、`/healthz` 200。`systemd-analyze security geoframe-rs` の総合は **1.8 OK**。

### 8b. `dbopen.sh` の 2 つの経路と `cache_load.py`

| 確かめたこと | 結果 |
| --- | --- |
| `fuser` の経路（psmisc あり） | DB を開いているとき rc=1、開いていないとき rc=0 |
| `/proc` の経路（`PATH` から `fuser` だけを外す。§15.2 の §1.3 の行） | 出力が「（/proc）」で、開いているとき rc=1、開いていないとき rc=0 |
| root でない | rc=2（「root で実行すること」） |
| 演習の中で使った経路 | §4.2・§8.1・§9.4・§10.1・§15.6 はすべて `fuser`（検証環境に psmisc がある） |
| `cache_load.py` | Python の venv（`geoframe` ユーザー・`-I -B`）で §1.4・§8.2・§8.4 の 3 回とも rc=0 |

### 9. 見つけたこと

#### 9.1 手順書の直し（すべて cutover.md に反映した）

1. **§15.2 比較用 Python の checkout**: `/opt/geoframe-pyref` は git の checkout ではなく、`--production` の HEAD と作業ツリーの検査を満たさない。退避 → `git init` → 開発機の git bundle から `fetch` → `checkout` → `app/.venv` を同じパスへ写す、の手順と、§15.6 での戻し方を書いた。
2. **§15.3 シャドー比較の Python のポート**: 8090 は `geoframe-pyref.service` と重なるので 8092 にした。`systemd-run` の一時 unit のコマンドを、`-E` で環境を明示する形で書いた（`EnvironmentFile=` の共通の設定は `GEOFRAME_AI_ENABLED=1` で `-E` より勝つので渡さない）。`KNOWN` を `extra.py` の出力から取るようにした。
3. **§15.2 DATA_DIR の退避**: 検証環境の DATA_DIR に鍵のファイルがありうる。ディレクトリごと `mv` するだけで中身を読まず、新しい DATA_DIR には `derivatives` などの決めたものだけを `cp -a` で写す（ハードリンクにしない）手順を書いた。
4. **§15.2 §1.3 の `/proc` の経路**: psmisc を消さずに `/proc` の経路を通す方法（merged-usr なので `PATH` から `/usr/bin` を外すだけでは `fuser` が残る。`fuser` だけを除いた道具の置き場を作る）を書いた。
5. **§15.2 §2 の行 `GEOFRAME_AI_ENABLED` を `1` にする時点**: 「流れ 5 の前」だと最初の Rust の期間の AI が無効になり、§6.3 の確認とローカルの AI が最初の切替で確かめられない。§3.5 の mask の直後に変える形にした（§8 の行も合わせた）。演習だけの確認として、§2 の Python のフィードに `check_files.py` を当てて画像の欠けの基準を取ることを足した。
6. **§8.4 品質の場合 1**: `algorithm` は「`rust-phash-v1` のまま」ではなく「基準と同じ」（Rust の期間に再構築が走らなければ `poc-phash-v1` のまま）。フィードの件数の数え方に、取り込みのほか**位置の推定で位置が付いた既存の写真**を入れた（`photos` の件数は増えない）。
7. **§15.4 品質の場合 2 の再構築の終わり**: `algorithm` では見分けられない場合があるので、`photo_quality_dirty` が `0` に戻り `max(analyzed_at)` が変わったことで見るようにした。
8. **§8.4・§6.5・§11.4 の `journalctl`**: 行が無いとき「-- No entries --」を出すので、`wc -l` が 1 になる（例外 0 のはずが 1 と読める）。`-q` を足した。
9. **§10.5**: 「前提未達」の注記を、演習で確かめた手順（対象の限定と想定との照合・`display=NULL`・回転と人の列の保持・再確認）に置き換えた。実 Drive の取り直しは「本番で 1 件ずつ先に試す」を条件として残した。
10. **§15.2 検証環境の Rust の退避の順**: unit を移す前に `disable` する（先に移すと `wants` のリンクが残る）。空の drop-in のディレクトリの扱いと、§15.6 での作り直しを書いた。
11. **§15.2 §1.5 の行**: 検証環境に AI 設定の JSON が無いときの作り方と、演習だけの Decision の絞り（本番機の CPU を使い続けない）を書いた。§1.5 本文に、共通の設定に既にある名前は共通のまま置くことを足した。
12. **§0.4 時間の目安**: 演習で測った値（§3 約 1 秒・§4 約 40 秒・§5 約 2 秒・T0〜T1 42〜43 秒）に置き換えた。§2 の `WAIT` を 30 にした。
13. **§5 の確認**: §9 の切り替え直しでは同じバイナリの `.prev` ができることを足した。
14. **§15.5 の 4**: mask のあとに `daemon-reload` を足した（無いと次の `systemctl stop` が「changed on disk」と警告した）。
15. **§15.1 の順番・§15.6**: 演習の順（2→3→4→5→6→7→1→8→9）と、復元を切り替え直したあとに行う理由を書いた。§15.6 に、比較用 Python の戻し・演習で作った `/usr/local/lib/systemd/system/` の片付け・`geoframe-rs.service.d/` の作り直し・共通の設定の `cp -p` での戻し・戻した DB の `.dump` の照合・開発機で作って保管したバイナリの扱いを書いた。

#### 9.2 観察（不具合ではない）

1. **停止に約 10 秒かかることがある**: AI の背景ワーカーが Decision・Embedding の要求の途中だと、停止はその終わり（join の上限 約 10 秒）を待つ（§2.3。`TimeoutStopSec=90s` に収まる）。§7 の書き込み系の操作・§8.1・§10.1 の停止がその分だけ長くなりうる。
2. **Python の `BrokenPipeError`**: §2 の時点の Python の journal に 1 件。HTTP の相手が応答の前に接続を閉じたときの Python 版の表示で、配信には影響しない（P5b で Python は消える）。
3. **§9 の切り替え直しで `.prev` が同じバイナリになる**: 初回の配布で入れたバイナリが残っているため。`--rollback --production` で戻しても同じものになる。
4. **Rust の期間に品質の再構築が走らなかった**: 演習の期間に新しい写真が入らなかったため（§4.4）。Rust が作った品質の結果を Python へ切り戻す形は、server.md §16.10 と P4 の受入（[p4-acceptance.md](p4-acceptance.md) §7）で確かめてある。

### 10. 検証環境の最終状態（[cutover.md §15.6](cutover.md#156-元に戻す流れ-9)）

| 確かめたこと | 結果 |
| --- | --- |
| 1 止める | タイマー・バックアップ・Rust・Python がすべて inactive、一時 unit 0、`dbopen.sh` rc=0、`sqlite3` のプロセス 0、バックアップの一時ファイルなし |
| 2 バイナリ | `$BIN` と `/usr/local/bin/geoframe` が演習の sha256 と一致。保管物（開発機）も一致 |
| 3・4 ファイルと systemd | 一時的な Python 版 unit・drop-in・`py-rollback`・`geoframe-rs`・AI 設定の JSON・固定の印・ダミーのタイマーを消し、共通の設定（`0644`）・印・元のバイナリ 2 つ・元の unit 2 つ・空の drop-in のディレクトリを戻した。演習で作った `/usr/local/lib/systemd/system/` を消した。比較用 Python を退避から戻した。mask のリンク 0。`geoframe-rs` は `enabled`・`inactive`、ほかは `not-found` |
| 5 DATA_DIR | 退避から戻し、元の DB の `.dump` が演習の前の値（`57f6dceff3b48bc5…`）と一致。直下の名前（鍵のファイルを含む）も同じ。演習の DATA_DIR（本番のコピー）・`p5-precutover-*` の 2 つ・転送の置き場・シャドーの複製を消した |
| 6・7 起動 | 演習の前に active だった `geoframe-rs`（`/healthz` 200）と、止めていない `geoframe-pyref.service`（8090、200）が動いている。タイマーは演習の前から無いので無いまま |
| 8 控えとの比較 | §15.2 の控えのスクリプトの出力が演習の前と**差 0**（unit の状態・ファイルのモード・所有者・内容の sha256・比較用 Python の名前と内容のハッシュ・DATA_DIR の直下・derivatives の件数・バックアップの名前・unit と timer の一覧）。そのあと `$WORK` と退避先を消した |
| 開発機 | 本番のバックアップのコピー（`prod-latest.db.gz`）と bundle を消した。演習のバイナリの保管物だけが残っている |

### 11. 本番切替の前に残ること

1. **P5a のマージ**と、マージのコミットを上の表に書き足すこと。本番の [§1.2](cutover.md#12-演習のコミットと-p5a-のマージのコミットに差が無いことを確かめる) で、演習のコミットとの差（`crates`・`Cargo.*`・`app`・`deploy`）が空であることを確かめる（差があれば演習のやり直しか、無害の判断をユーザーが記録する）。
2. **本番の設定の値をユーザーと決めること**（[§1.5](cutover.md#15-設定のファイルの分割とモード)・[§1.6](cutover.md#16-ai-設定と鍵の権限ローカルの用途だけ)）: `app/.env` から `geoframe-rs` へ移す名前、`GEOFRAME_TILE_UA` の値と置き場所、AI 設定の JSON の中身（ローカルの用途だけ）。演習の値は本番に使わない（演習だけの Decision の絞りも足さない）。
3. **§10.5 の実 Drive からの取り直しは未確認**: 本番で復元して画像が欠けたら、手順書のとおり 1 件で先に確かめる。
4. 演習で確かめていない本番の要素: 本物の `geoframe-backup.timer`（演習はダミー）、Drive の鍵を使った取り込みのサイクル（演習は鍵なし。実 Drive は T3.10 で確認済み）、本番の速さでの Decision の全件再判定と索引の公開（§11）。
5. 停止時間の 42〜43 秒は、コマンドを続けて流した値。本番は段ごとに許可を取るので、§0.2 のとおり §3〜§5 のコマンドを先に画面に出しておく。

## T5.5 本番の切替

2026-10-30、ユーザーの立ち会いで実施（操作は Opus。ユーザーの許可: 準備は「1・3・4 は承認」、§3 からは「開始して」）。

### 0. 結果の要約

| 項目 | 結果 |
| --- | --- |
| T0（Python の停止）→ T1（`/healthz`・`/api/v1/photos` が 200） | 08:25:23 → 08:25:55、**32 秒**（目標 5 分） |
| `rust-deploy.sh --production` | 終了コード 0。「確認 OK（/healthz 200、/api/v1/photos は配列で 3065 件 >= 下限 3003）」「完了: geoframe 0.1.0」 |
| 写真の集合（AC6） | Python 3,065 件 → Rust 3,065 件。Python だけ 0、Rust だけ 0（`extra.py` rc=0、説明できない 0）。画像 3,065 件すべて 200 |
| 取り込みのサイクル | 起動直後に 1 回成功（表示可能 5,288 / 取得待ち 0 / 全 6,535。約 9.4 秒）。Drive の鍵を使った本番の取り込み |
| ローカルの AI | 用途 3 つが「使える」、停止中の Provider・用途なし。Embedding・位置の手がかりのジョブが進んでいる（意味検索は索引の公開まで 503。受入済み） |
| AC6 | **合格** |

### 1. 準備（§1・§2）

| 節 | 結果 |
| --- | --- |
| §1.1 | `deploy-pull.sh` で `/opt/geoframe` を P5a のマージのコミットへ。更新の前に手動のバックアップ。requirements の変更なし。Python は 2 秒で 200 |
| §1.2 | 演習のコミットとの差（`crates`・`Cargo.*`・`app`・`deploy`）は空 |
| §1.3 | `fuser` なし（入れていない。`dbopen.sh` は `/proc` の経路）。ほかの道具と venv はある |
| §1.4 | 補助スクリプト 14 本。`cache_load.py` を本番の venv で空打ち rc=0。`dbopen.sh` は Python 稼働中に rc=1（正常） |
| §1.5 | 3 つのファイルとも `600 root:root`。`geoframe-rs` は `GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`（Python 版 unit の `Environment=` と同じ値を移した）・`GEOFRAME_INGEST=1`・`GEOFRAME_AI_CONFIG` の 5 つ。共通の設定との重複 0。`GEOFRAME_TILE_UA` は共通の設定にあったので共通のまま。本番に `app/.env` は無かった（移すもの無し） |
| §1.6 | AI 設定の JSON は本番に無かったので作った（`640 root:geoframe`）。[ai-embedding.md](ai-embedding.md) の固定の契約と、用途 `image_embedding`・`query_embedding`・`location_clue` だけ（すべて `local`、入口は同じ機体）。クラウドの用途は書いていない。鍵は `600 geoframe:geoframe` |
| §1.7 | Decision は `ready`（`image` を含む） |
| §1.8〜§1.10 | バイナリの sha256 が上の表と一致、static-pie。`GEOFRAME_RS_*` なし。本番の状態は手順書のとおり。`--check-only` rc=0 |
| §2 | Python の配信 3,065 件（`--expect-min-photos=3003`）、STATUS 41 行、下書きの sha256 を控えた。同じ引数の `--check-only` rc=0 |

### 2. 切替（§3〜§6）

| 節 | 結果 |
| --- | --- |
| §3 | 停止・disable・本体を `/usr/local/lib/systemd/system/` へ・drop-in・`EnvironmentFiles` に 2 つが並ぶ・`masked`/`inactive`・固定の印 |
| §4 | タイマーとサービスが `inactive`。`dbopen.sh` rc=0（`/proc`）、`pgrep` rc=1。`.backup` rc=0（`mode=ro` で取れた）、`integrity_check` ok、`p5-precutover-20261030-082525.db.gz`（`640 geoframe`、42,843,429 B）、一時ファイルなし。一次データの tar 8 件（`app/.env` は無いもの）。T0 から 19 秒 |
| §5 | 上の要約のとおり。`geoframe-rs` は `enabled`/`active`、Python は `inactive`。`[guard]` の行 1、警告 0 |
| §6.8 | 展開と控え OK。`.dump` の SHA-256 `c885071535e5135e4b7b506941db327612ca69b933eaad6f0811a85445869cc1`。件数: photos 6,535・manual 17・favorite 0・hidden 0・rotated 0・quality_rows 559・quality_groups 243（exact 185・similar 374） |
| §6.2 | 上の要約のとおり。群は `ai_group` 0・`quality_group` 0（フィード上） |
| §6.3 | `ai_task_blocks`・`ai_config_invalid`・`ai_pauses` はすべて `なし`。Python の控えとの差は `db.ai.suppressed` 2,591 → 2,136（[p4-acceptance.md §7.3](p4-acceptance.md) の既知の差）と、`db.loc.clue_pending` 0 → 1,206・`clue_runnable` 1,206・`clue_pause=''` の行だけ（演習と同じ形） |
| §6.4 | ユーザーが確認: miharashi の表示が続いている。管理画面（STATUS・写真一覧・地図のタイル・地名検索）が動き、お気に入りの付け外しが成功して元に戻った |
| §6.5 | 上の要約のとおり |
| §6.6 | タイマー OK（active / enabled）、次は 12:17 |
| §6.7 | `deploy-pull.sh --dry-run` rc=3。`geoframe-admin.sh ai-jobs status` が動いた（DATA_DIR が出る。Embedding 成功 24・待ち 6,415、手がかり成功 10・待ち 1,205）。`NRestarts=0` |

### 2b. 切替のあとの負荷（10:39）

- CPU は Decision（同じ機体のローカルの常駐）が約 4.2 コア、`geoframe` は約 1%。機体全体で約 25%、load average 約 4。
- 進み: 位置の手がかり 成功 610・待ち 605（1 件約 12 秒で、残り約 2 時間）。画像の Embedding 成功 744・待ち 5,695。
- **意味検索の 503 は約 48 分という見込みより長い**: AI のワーカーは 1 本のスレッドで用途を順に回すので、手がかりの全件再判定のあいだは Embedding が 1 周期に 12 件ずつしか進まない（毎分 5〜6 件）。手がかりが終わってから速くなる。配信・管理には影響しない。§11 で公開の時刻を記録する。

### 3. 手順書と違ったこと

- §4.3: 実行の道具が `rm` を含むコマンドを止めたため、`.tmp-` の削除を `gzip -9 "$SNAP"`（元を置き換える）＋ `gzip -t` ＋ `mv` に置き換えた（結果は同じ。`mode=ro` で取れたので、コピーを使う代わりの経路は使っていない）。
- §1.5 の「`ai-jobs status` が動くこと」は、Rust のバイナリが入る前なので §6.7 で確かめた。

### 4. 切替のあと（§11・§12。2026-10-30 午後）

#### 4.1 §11 の確認（14:06）

- Decision の全件再判定は**終わっていた**: `db.loc.clue_pending=0`・`clue_runnable=0`・`clue_pause` が空。`ai-jobs status` の位置の手がかりは成功 1,216 件。
- 索引は**公開済み**: `index_state=active`・`pending=0`・`building` なし。画像の Embedding は 6,439 件が保存済みで、失敗で除外 0・壊れ 0。意味検索の 503 は解けている（公開の正確な時刻は、確認が午後になったので記録していない。切替の 08:25 から 14:06 までのあいだ）。
- バックアップのタイマー: Rust 稼働中の 12:17 の回が `Result=success`・`integrity_check: ok`・`latest.db.gz` の `gzip -t` OK。次は 18:17。切替前の控え `p5-precutover-*` は残っている（世代の削除はそれを数えない）。
- `geoframe-rs`: `NRestarts=0`、`warning` 以上の journal は 0 行。load average は約 0.3（再判定が終わって Decision の負荷が消えた）。

#### 4.2 §12 クラウド AI の有効化（14:07）

- ユーザーの指示（§11 の完了後すぐ・頻度は適切に・料金は §13 の開始時に確認して調整）で有効にした。値（project・鍵のパス）はここに書かない。
- `tasks` に追加: `photo_judge`（`gemini-3.1-flash-lite`・日の上限 **40**・`include_hidden=false`）と `location_suggest`（`gemini-3.8-flash`・日の上限 **30**）。`region` は PoC で使えた `us`。鍵は Drive の鍵（`600 geoframe:geoframe`）で、AI 設定の JSON は `640 root:geoframe` のまま。直前の控えは `/root/p5-cutover/ai-config.before`。
- 上限の考え方: まず小さく始め、最初の日に実際の積まれる量を見て決める方針。再起動直後に `photo_judge` の新規 34 件が積まれ（上限の 40 以内）、`location_suggest` は 1 日分が動いた。周期（`GEOFRAME_AI_POLL_S` 60 秒・1 周期の `JUDGE` 2・`LOC_SUGGEST_GEMINI` 2）は既定のまま。日の上限が先に効くので、頻度の調整はこの 2 つの上限で行う。
- 再起動後の確認: `/healthz` 200、2 用途が「使える」、`ai_config_invalid`・`ai_task_blocks`・`ai_pauses`・`ai_waits` はすべて「なし」。最初の数分で `photo_judge` 成功 4→6、`location_suggest` 成功 1（`location_geocode` 1）、停止・警告なし。

#### 4.3 あとで行うこと

- §12.3: 最初の日の送信数が上限（40・30）に収まっているか、費用、journal の警告を見る。上限を上げるのはユーザーの判断。
- §13: 戻しの猶予期間（既定はクラウド AI を有効にしてから 1 週間）。**開始時に料金を確認して、上限を調整する**（ユーザー）。`/root/p5-cutover` と `p5-precutover-*` はそれまで残す。
- T5.6a・T5.6b（P5b）は完了（PR はマージ済み）。
