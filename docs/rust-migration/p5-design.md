# P5（本番切替）設計

## 概要

[P5 PRD](p5-prd.md)の設計と手順の骨組み。本番機での実際の手順は、T5.3で手順書`cutover.md`に書く。P5は**2つのPRに分ける**:

- **P5a（切替の準備）**: ガードの見直し・本番への配布・Decisionの待ちの表示・手順書・演習（T5.1〜T5.4）。切替の**前に**mainへマージする。
- **P5b（Pythonの撤去）**: 本番の切替（T5.5）のあとで、**リポジトリから**Pythonの実装と依存を外す（T5.6a〜b）。切替の直後に進める。

分ける理由: 本番の`/opt/geoframe`はmainのgitの取り込み（`deploy/deploy-pull.sh`）でPython版を動かしている。Pythonを消したコミットを本番のcheckoutへ取り込むと、Python版が動かなくなる。切替のための変更はレビュー済みのmainから本番へ入れ、撤去は切替を確かめてから別のPRにする。PythonはP5の中で撤去する（2026-10-29 ユーザー決定）。

1つのPRにまとめる案（本番のcheckoutを固定して準備と撤去を1つのPRにする）も考えたが、切替に使うコードがmainに入らないまま本番で動く期間ができるので選ばない。

**戻しの猶予期間**: リポジトリの撤去（P5b）と、**本番機の片付け**は分ける。本番の`/opt/geoframe`のcheckout（P5aのマージのコミット）・`/opt/geoframe/app/.venv`・Python版のunit（maskした状態）は、P5bのマージのあとも残す。猶予期間のあいだ、本番の`/opt/geoframe`はP5b以降を取り込まない（checkoutの固定の印で`deploy-pull.sh`が断る。§本番への配布）。この期間はPythonへ数分で戻せる。期間が終わったら、ユーザーの許可を得て、印を外し、`/opt/geoframe`を手で新しいmainへ合わせ（`deploy-pull.sh`のPython版の部分はP5bで消えている）、venv・Python版のunitを片付ける。期間の既定は「クラウドAIを有効にしてから1週間」。

## 今の作り（調査の結果）

### Rustの守り

- 取り込みのガード: `crates/geoframe/src/guard.rs`。書く前に、検証環境の印`/etc/geoframe/staging`と`geoframe-server.service`の非稼働（`ActiveState`が`inactive`/`failed`。`decide`、:199-203）を確かめ、だめなら終了コード2（[server.md §11.1.1](server.md#1111-本番への誤指定の防止ガード)）。`serve`・`sync`と、`ingest::start`を通る書き込み系のCLI（`organize`・`quality-scan`・`ai-jobs retry|release-pause|deactivate-space`など。`cli.rs:870-919`）に効く。`ai-jobs status`は読むだけで通さない。
- 二重起動のロック`DATA_DIR/.ingest.lock`はRust同士だけに効く。稼働中の`serve`がロックを持つので、書き込み系のCLIはサービスを止めないと動かない。
- unit: `deploy/geoframe-rs.service`（`User=geoframe`、`ExecStart=/usr/local/bin/geoframe serve`、`Conflicts=geoframe-server.service`、`ReadOnlyPaths=-/var/lib/geoframe/photos`、`EnvironmentFile=-/etc/default/geoframe`、`RestartPreventExitStatus=2`、`Environment=`にDATA_DIR・鍵のパス・TZ）。本体は`/etc/systemd/system/`に置かれる（`rust-deploy.sh:112,358`。Python版も同じ場所。`deploy/README.md:273`）。`Conflicts=`はあるが、Pythonとの順序（`After=`/`Before=`）が無い。冒頭と本文に「検証環境専用」の記述がある。ポートは既定で8080で、Pythonと同じ（`config.rs:9`、`app/server.py:34`）。miharashiの接続先は変わらない。
- 配布: `deploy/rust-deploy.sh`。既定で手元でmuslビルド（`--skip-build`と`GEOFRAME_RS_LOCAL_BIN`で既存のバイナリ。:38,:64）→ssh（`sudo bash -s`）で転送→`.prev`を残して入れ替え→`/healthz`と`/api/v1/photos`が`[`で始まるかを確認（:179-180）、失敗したら前の版へ戻す。初回の配布の失敗はstop・disable・unitの撤去で終わる（:299-315）。相手に印が無い、またはPython版が稼働中なら断る（`preflight`、:148-162）。作業ツリーが汚れていても警告だけ（:540）。試験は`deploy/rust-deploy-test.sh`。

### Python版の運用

- `deploy/geoframe-server.service`（`/opt/geoframe/app/.venv`の`python server.py`、`WorkingDirectory=/opt/geoframe/app`、`Restart=always`）。PythonのAIは既定で有効（`app/geoframe/config.py:83`の`GEOFRAME_AI_ENABLED`）。RustとPythonは同じ`/etc/default/geoframe`を読み、同じ名前の変数を使う。
- 更新は`deploy/deploy-pull.sh`（`SERVICE=geoframe-server`固定。stop→git→`pip install`→start→`/api/admin/status`。:302,:348）と`deploy/deploy.sh`（Macからの薄いラッパー）。
- バックアップ: `deploy/backup-catalog.sh`と`geoframe-backup.timer`（6時間ごと。`sqlite3 'file:…?mode=ro' .backup`・`integrity_check`・gzip・`latest.db.gz`、`catalog-*.db.gz`は7日で消える）。取るのは`catalog.db`だけ。サービスが止まっていると`-shm`が無くて`mode=ro`が失敗しうる（:75-77）。**復元の手順はどこにも無い**。
- 一次データでバックアップに入っていないもの: 旅行ブログの下書き`travel_blog/drafts/`、`.cache.json`、`/etc/default/geoframe`、`app/.env`、AI設定のJSON。
- 回転は新しい画像を作って古い画像を消す（`crates/geoframe/src/rotate.rs:11-16`）。DBだけを古い時点に戻すと、`display`が消えた画像を指しうる。
- 毎朝のDecision: `deploy/loc_decision_morning.py`（標準ライブラリだけ・システムの`python3 -I`）と`geoframe-loc-decision.timer`。STATUSの`db.loc.clue_pending`が0なら何もしない（:56）。今はDecisionが常駐しているので何もしない運用（:12）。**Rustの`clue_pending`は旧Pythonの`photo_loc_clues`の世代一致で数えていて（`admin.rs:372-422,498`）、Rustの全件再判定のジョブ（`ai/location_clue.rs:622-645`）の待ちを表さない**。旧Pythonの判定済みの行があると、Rustに待ちがあっても0になりうる。

### Pythonへの依存（撤去で直すもの）

- Rustのビルドが`app/static`・`app/vendor`・`app/travel_blog/plans`を埋め込む（`crates/geoframe/build.rs`・`build_plans.rs`）。試験の一部も`app/`を読む（`tests/admin_ui_ai_waits.rs`、`static_files.rs`など）。
- Rustの試験がPythonを実行する: `ai/groups/tests.rs`、`ai/scene_judge/tests.rs`・`runtime/tests.rs`、`ai/location_suggest/tests.rs`・`runtime/tests.rs`、`ai/location_clue/tests.rs`、`tests/ai_index_python_rollback.rs`。CIのRustのジョブはそのためにPythonを入れている（`.github/workflows/test.yml:111-121`）。
- CIのPython・文書のジョブ: `app/tests`の単体試験、`tools/gen_*_fixture.py --check`、`tools/gen_env_reference.py --check`（`app/geoframe/config.py`の注釈から`docs/reference/configuration.md`を作る）、`tools/check_markdown_links.py`。
- `tools/`のPython: `gen_*_fixture.py`（約24本。`app/`をimportする）、`compare_with_python.py`、`image-poc/`、`loc_clue_eval/`。`miharashi-feed-check`はRust。
- `app/tests/admin_ai_ops_render.mjs`（nodeで`admin.js`を試す）。
- ルートの`README.md`にPythonの起動・設定・試験の手順がある。

## 方針

### ガード（R1）

**検証環境の印の検査だけを外し、Python版が非稼働かの検査は残す。** 印は「本番では書かない」ための暫定で、切替のあとは本番がRustの唯一の書き手になる。一方、Python版の検査は、手で打つ書き込み系のCLI・直接の起動・設定の書き換えのように、`Conflicts=`が効かない経路の守りとして切替のあとも要る（Pythonへ戻したあとの運用中も）。Python版のunitが無い機体は`not-found/inactive`で通るので、P5bのあとも害が無い。

- 消すもの: `check_marker`と印のパス、`GEOFRAME_TEST_STAGING_MARKER`、`[guard] 検証環境の印を確認しました`の記録、試験の`Staging`。`decide`と`GEOFRAME_TEST_PYTHON_STATE`（デバッグビルドだけ）は残す。
- 終了コード: Python版が**稼働中**なら2（再起動しない）。状態を**取れない**（systemdにつながらないなど）は1にして、unitの`Restart=on-failure`で再試行させる（2だと`RestartPreventExitStatus=2`で止まったままになる）。unitに`StartLimitIntervalSec=`・`StartLimitBurst=`を足し、取れない状態が続いたら再試行を止める。
- `serve`と書き込み系のCLIを**rootで**実行したら断る（ファイルがrootの所有になってサービスが書けなくなるのを防ぐ）。
- 書き込み系のCLIは、DATA_DIRに`catalog.db`が無ければ作らずに断る（DATA_DIRの指定の誤りで別のDBを作らない。`serve`の「無ければ空のDBを作る」は今のまま）。
- `sync --dry-run`は今までどおり書かず、検査も通さない。
- 名前は`guard.rs`のまま（中身を「Python版の検査」と説明し直す）。

unitの守り:

1. **unitの本体を`/usr/local/lib/systemd/system/`に置く**（Python版も、Rust版も）。`/etc/systemd/system/`には、maskのリンクとdrop-in（`*.service.d/`）だけを置く。`systemctl mask`は`/etc/systemd/system/<unit>`に`/dev/null`へのリンクを作るので、そこに本体のファイルがあると失敗する。本番のPython版のunitは、切替の手順3でPythonを止めたあとに移す（§切替）。`rust-deploy.sh`は本体と`.prev`の置き場所をこちらへ変える。
2. `geoframe-rs.service`に`After=geoframe-server.service`を足す。`Conflicts=`で相手の停止と自分の起動が同じ処理に入ったとき、systemdは順序の向きにかかわらず停止を起動より先に済ませる。これで、どちらの向きでも旧プロセスが終わってから新しいプロセスが起動する。
3. 止める側はdisableでなく**mask**する。切替: Pythonを`stop`→`disable`→`mask`。戻す: Rustを`stop`→`disable`（失敗してよい）→`mask`、Pythonを`unmask`→`enable --now`。切り替え直す: Pythonを止めてmask、Rustを`unmask`してから配る。各段の最後に`systemctl is-enabled`と`is-active`で状態を確かめ、maskが失敗したらそこで止まる。両方がenableのまま再起動したときの衝突と、`deploy-pull.sh`などによる起動を防ぐ。
4. 二重起動のロックはそのまま。

### 本番への配布（R2）

`rust-deploy.sh`に本番の指定を足す: `--production=<期待するホスト名> --bin=<パス> --expect-sha256=<値> --expect-commit=<SHA> --expect-min-photos=<件数> [--wait-secs=<秒>] [--check-only]`。`sudo`では環境変数が消えるので、本番の指定では値をすべて引数で渡す。

- `--production`のときは**sshを使わず、その機体で直接**動く（ユーザーが`sudo`を付けて実行する。本番機のsudoやsshdの設定を広げない）。実行する場所は`/opt/geoframe`（P5aのマージのコミット）に決め、`HEAD`が`--expect-commit`と一致し、作業ツリーが汚れていなければ通す。入るunitはこのcheckoutの`deploy/geoframe-rs.service`。
- 相手の`hostname`が期待と違えば断る。相手に検証環境の印が**ある**なら断る（印と本番の指定は互いに排他）。`--production`が無いときは、今までどおり印が無ければ断る。
- Python版が稼働中・状態不明なら断る（今と同じ）。`geoframe-rs`がmaskされていれば何も変えずに断る（切り替え直すときは先にunmask）。
- ビルドしない（`--skip-build`と同じ）。`--bin`のバイナリをまずroot所有の一時ディレクトリへ写し、そのsha256が`--expect-sha256`と一致しなければ断る（確かめた時点と使う時点のあいだに差し替えられないように。演習で使ったバイナリそのものを入れる）。
- 配布のあとの確認で、`/api/v1/photos`の件数が`--expect-min-photos`より少なければ失敗として戻す（DATA_DIRの誤りで空のDBを作ったまま「成功」にしない）。待ち時間は演習で測った値を`--wait-secs`で渡す。
- `--check-only`: 変更の前の検査（ホスト名・印・commit・作業ツリー・バイナリのsha256・`/etc/default/geoframe-rs`の有無とモード・maskの状態）だけを行う。Python版の状態の検査は除く。切替の手順3（Pythonを止める）の**前**に実行し、Pythonを止めたあとに誤りが見つかって配信の停止が延びるのを防ぐ。
- `--rollback --production=<ホスト名>`を定義し、`.prev`へ戻せるようにする。
- 初回の配布が失敗したときは、スクリプトはRustを止めてunitを外す（今と同じ）。手順書には「終了コードが0でなければ、すぐ戻す手順（R5）でPythonを起動する」と書く。

`deploy-pull.sh`には、**checkoutの固定の印**`/etc/geoframe/checkout-pinned`があれば何もせずに断る検査を**P5aで**足す。印は切替の手順で置き、猶予期間が終わるまで外さない（unitの状態に依らないので、Pythonへ戻したあとも断り続ける）。あわせて、`geoframe-rs`がenabled・active・maskedのどれかでも断る。猶予期間中にPythonへ戻したあとのPythonの更新は、P5aのマージのコミットへの手での`reset`だけにする（手順書に書く）。切替のあとの本番のRustの更新は`rust-deploy.sh --production`でバイナリを入れ替える形にする。

### 保全・バックアップ・復元（R3）

切替の直前の保全（Pythonを止めてmaskした**あと**、Rustを起動する前）:

1. `geoframe-backup.timer`を一時的に止める（切替が終わったら戻す）。`fuser /var/lib/geoframe/catalog.db*`と`pgrep -u geoframe`が空（unitの外で動いているPythonなども無い）。`fuser`（psmisc）が本番機にあることは準備で確かめる。
2. `catalog.db`のバックアップを、サービスのユーザーで、書き手がいない状態で取る（`mode=ro`が`-shm`の無さで失敗したときの代わりの手順も書く）。名前は`p5-precutover-<時刻>.db.gz`のように`catalog-*`に当たらない名前にし、`0640`・`geoframe`の所有で、P5の記録が終わるまで残す。`.dump`のSHA-256を記録する。
3. `travel_blog/drafts/`・`.cache.json`・`/etc/default/geoframe*`・`app/.env`・AI設定のJSONを`0600`のtarで保存する。

復元（DBが壊れたときなどの最後の手段。既定の戻しはDBを保ったまま）:

1. Rustを止め、`geoframe-backup.timer`も止める。`fuser`でDBを開いているプロセスが無いことを確かめる。
2. `catalog.db`・`-wal`・`-shm`を**3つ一緒に**同じファイルシステムの退避先へ`mv`し、残っていないことを確かめる。
3. 同じディレクトリの一時名へ`gunzip`する→`chown geoframe:geoframe`・元と同じモードにする→`sudo -u geoframe sqlite3 'file:<一時名>?mode=ro' 'PRAGMA integrity_check'`と`.dump`のSHA-256を確かめる→`mv`で置き換える。
4. `ls -l catalog.db*`で所有者がすべて`geoframe`。ここまでが起動の前の確認（AC3）。
5. 起動し、配信と表示の画像の欠けを確かめる。DBより新しい回転で消えた画像を指す行があれば、その数を記録し、直し方（取り込みのサイクルで作り直されるか）を手順書に書く。復元で失うもの（復元の時点より後の手動の操作・取り込み）も手順書に書く。
6. `geoframe-backup.timer`を戻す（途中で止めたときも戻す）。

### 切替（R4・R7）

手順書の骨組み（細部はT5.3で書く）:

1. 準備（前日まで）:
   - P5aのマージのあと、ユーザーの許可を得て、本番の`/opt/geoframe`を`deploy-pull.sh`でP5aのマージのコミットへ更新する（Pythonが再起動する。この時点ではdrop-inをまだ置かない）。演習のコミットとP5aのマージのコミットのあいだで、`crates`・`Cargo.*`・`app/`（Pythonを含む）・`deploy/`に差が無いことを確かめる。
   - 設定を分ける。共通の`/etc/default/geoframe`（今のもの。`0600 root:root`）と、Rustだけの`/etc/default/geoframe-rs`（`0600 root:root`。`GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`・`GEOFRAME_INGEST=1`・`GEOFRAME_AI_CONFIG`・`GEOFRAME_TILE_UA`など）。`geoframe-rs.service`は共通のファイルの**後に**、`geoframe-rs`を`-`無しの`EnvironmentFile=`で読む（無ければ起動しない。後に書いた`EnvironmentFile=`が勝つ）。同じ名前の値を両方に置かない。unitの`Environment=`にあったDATA_DIR・鍵のパス・TZは`geoframe-rs`へ移し、unitとすべてのCLIが同じファイルから読むようにする。Rustは`app/.env`を読まない（`config.rs:35-38`）ので、Python版が`app/.env`で持つ値は`geoframe-rs`へ移す。
   - Pythonへ戻すときの設定のファイル`/etc/default/geoframe-py-rollback`（`0600 root:root`。中身は`GEOFRAME_AI_ENABLED=0`・`GEOFRAME_LOC_SUGGEST=0`など、PythonのAIをすべて止める値）を用意する。**drop-inはまだ置かない**（`geoframe-server.service`は`Restart=always`なので、切替の前に再起動が起きるとdrop-inが効いて本番のAIが止まる）。
   - AI設定のJSON（ローカルの用途だけ）は`0640 root:geoframe`、鍵は`geoframe`の所有で`0600`（モードと所有者だけを確かめ、中身は読まない）。
   - Decisionが常駐していることを確かめる（全件再判定に約4時間要る。§切替のあとの数日）。
   - 切替に使うバイナリ（T5.4の演習で使ったもの。sha256を記録したもの）を本番機に置く。
2. 切替の直前: `sudo /opt/geoframe/deploy/rust-deploy.sh --production=<ホスト名> --bin=… --expect-sha256=… --expect-commit=… --check-only`が通る。Pythonの`/api/v1/photos`の写真のIDの一覧と件数、STATUSを控える（作業用の置き場に。リポジトリには件数だけ）。
3. Pythonを止める: `systemctl stop geoframe-server`、`disable`。`is-active`が`inactive`（ここから配信が止まる。時刻を記録）。続けて:
   - Python版のunitの本体を`/etc/systemd/system/`から`/usr/local/lib/systemd/system/`へ移す。
   - drop-in`/etc/systemd/system/geoframe-server.service.d/p5-rollback.conf`（`EnvironmentFile=/etc/default/geoframe-py-rollback`、`-`無し）を置く。
   - `daemon-reload`し、`systemctl show -p EnvironmentFiles geoframe-server`でdrop-inが読まれていることを確かめる。
   - `systemctl mask geoframe-server`。`is-enabled`が`masked`。drop-inはmaskのあいだも残り、unmaskで効く。
   - checkoutの固定の印`/etc/geoframe/checkout-pinned`を置く。
4. 保全（§保全・バックアップ・復元の1〜3）。
5. Rustを入れる: `sudo /opt/geoframe/deploy/rust-deploy.sh --production=… --bin=… --expect-sha256=… --expect-commit=… --expect-min-photos=<控えた件数より少し下> --wait-secs=…`（unitの導入・enable・起動・確認まで）。終了コードが0でなければ、すぐR5の手順でPythonへ戻す。
6. 確かめる（配信の再開の時刻を記録）: `/healthz`・`/api/v1/photos`が200。控えたPythonの写真がRustの配信にすべてあり、増えた分は既知の差の範囲（AC4と同じ基準）。STATUS（`ai_task_blocks`が無い、`db.loc.clue_pending`）、管理画面、miharashiの表示、取り込みの1サイクルの成功。`geoframe-backup.timer`を戻す。
7. 止める判断:
   - **配信が止まっている**（`/healthz`か`/api/v1/photos`が200にならない）なら、配信の停止から10分で直らなければ戻す。
   - **配信は続いているが機能に問題がある**（写真の欠け・管理操作の失敗・取り込みの失敗）なら、30分のうちに原因と直し方が見えなければ戻す。

### AIの書き込み系の操作（R4・R10）

`ai-jobs retry|release-pause|deactivate-space`は、稼働中の`serve`がロックを持つので、サービスを止めて行う。サービスと同じユーザー・同じ設定・同じ制限で動かすため、ラッパー`deploy/geoframe-admin.sh`（新しく作る）を使う。ラッパーは`systemd-run --uid=geoframe --wait --pipe -p EnvironmentFile=/etc/default/geoframe -p EnvironmentFile=/etc/default/geoframe-rs -p UMask=0027 -p ReadOnlyPaths=-/var/lib/geoframe/photos /usr/local/bin/geoframe …`を実行し、動かす前に対象のDATA_DIR（パスだけ）を表示する。

- 書き込み系: `systemctl stop geoframe-rs`→`geoframe-admin.sh ai-jobs …`→（成功しても失敗しても）`systemctl start geoframe-rs`→確認。手順書では`stop`のあとに`trap`で起動し直す形にする。
- `ai-jobs status`も同じラッパーで動かす（サービスを止めなくてよい。rootで直接打たない）。

### 切替のあとの数日（R10）

- Decisionの全件再判定（約4時間）が終わること: `ai-jobs status`の位置の手がかりの待ちが0、STATUSの`db.loc.clue_pending`が0。
- 索引の公開（初回のEmbeddingの約48分のあと、E5の`pipeline.building`が消える）。
- `geoframe-backup.timer`の成功（Rustの稼働中のバックアップ）。
- journalの警告。
- そのあとクラウドAIを有効にする: `/etc/default/geoframe-rs`のAI設定に`photo_judge`・`location_suggest`を日の上限を小さくして足し、再起動する。鍵の権限を確かめ、停止が出たら§AIの書き込み系の操作で`release-pause`する。

### Decisionの待ちの表示（R9）

STATUSの`db.loc.clue_pending`を、Rustの位置の手がかりのジョブの待ち（未処理・後回し中で、まだ判定を保存していない写真の数）を含む総待ちにする。旧Pythonの`photo_loc_clues`だけで数える今の計算を、Rustの判定の保存結果も見る計算に直す。ただし総待ちには、Providerの停止中・品質の解析待ち・期限前の後回しのように、Decisionを起動しても進まないものも入るので、timer用に `db.loc.clue_runnable`（今Decisionが動けば進む数。Providerの停止中は0）と停止の理由コード `clue_pause` を足し、`loc_decision_morning.py`は `clue_runnable`（無ければ`clue_pending`）を見る。STATUSは画像ファイルを開かず（fallbackの写真は保存された行とワーカーのジョブから判断）、結果を30秒キャッシュする（[ai-location-clue.md](ai-location-clue.md#statusのclue_pendingclue_runnableclue_pause)）。直せば切替のあとも待ちを見落とさず、無駄な起動もしない。今はDecisionが常駐していてtimerは何もしないが、常駐をやめる運用に戻したときのために直す。

### Pythonへ戻す・切り替え直す（R5）

戻す（AIは無効。既定は**DBを保ったまま**戻す）:

1. `systemctl stop geoframe-rs; systemctl disable geoframe-rs || true; systemctl mask geoframe-rs`。初回の配布の失敗でunitが外れているときも同じ手順で通る。最後に`is-active`が`inactive`、`is-enabled`が`masked`を確かめる。
2. DBは保ったまま（PythonはRustが足した表・列を無視して読める。[p4-acceptance.md §7](p4-acceptance.md)）。DBが壊れたときだけ§復元をする（失うものは手順書に書く）。
3. `systemctl unmask geoframe-server && systemctl enable --now geoframe-server`。drop-inの`p5-rollback.conf`によりPythonのAIは止まっている（`systemctl show -p EnvironmentFiles`で確かめる）。
4. 確かめる: 配信の件数、管理画面、手動の位置・お気に入り・除外・回転・旅行ブログの下書き、PythonがRustの書いた`.cache.json`を読めること、AIの通信が出ていないこと（journalとSTATUS）。品質の切り戻しは[server.md §16.10](server.md#1610-rustで作った結果をpythonへ切り戻すp5のロールバック)の2つの場合（そのまま／`dirty`の再構築）を記録する。
5. checkoutの固定の印は外さない（Pythonへ戻したあとも`deploy-pull.sh`はP5b以降を取り込まない）。

切り替え直す: Pythonを`stop`→`disable`→`mask`（drop-inは残したまま）、`systemctl unmask geoframe-rs`、§切替の4〜6。`p5-rollback.conf`はPythonだけに効くので、RustのAIの設定は変わらない。

猶予期間が終わったあと（本番のPythonのvenv・unitを片付けたあと）にPythonへ戻すには、P5aのマージのコミットを取り出してvenvを作り直す必要があり、数分では戻せない。その手順は手順書に書くが、演習はしない（猶予期間の中で戻すかを判断する）。

### 演習（R6）

検証環境（lxc-geoframe）で行う。

- **配置を本番に合わせる**（配置の表を手順書に書く）:
  - 比較用Python（`/opt/geoframe-pyref`。P5の前にP5aのコミットへ更新する）を、`deploy/geoframe-server.service`と同じ名前・同じ形（`WorkingDirectory=`・`ExecStart=`は`/opt/geoframe-pyref`のcheckoutとvenvを指す）・同じDATA_DIR（`/var/lib/geoframe`）・ポート8080の`geoframe-server.service`として、`/usr/local/lib/systemd/system/`に一時的に置く（今の`geoframe-pyref.service`は8090・別のDATA_DIRで、`Conflicts=`も配布の検査も働かない）。
  - `/etc/systemd/system/geoframe-rs.service`の古い本体は、新しい置き場所より優先されるので、演習の前に退避する。
  - 設定は本番と同じ3つのファイル（共通・`geoframe-rs`・`geoframe-py-rollback`）で置く。
  - 本番の配布の経路を通すため、演習のあいだだけ検証環境の印を退避し、`sudo deploy/rust-deploy.sh --production=<検証環境のホスト名> …`を、P5aのコミットにした`/opt/geoframe-pyref`のcheckoutからその機体で直接動かす（本番では`/opt/geoframe`。新しいバイナリは印を見ない）。
- **データ**: 演習の前に検証環境の`/var/lib/geoframe`（P4の状態・`models/`を含む）を退避する。本番のバックアップ（検証環境が`deploy/backup-pull.sh`で回収しているもの）から作った新しいコピーを置く。`drive_key.json`とAIの鍵は入れない。`GEOFRAME_DRIVE_KEY`は存在しないパスにする（取り込みのサイクルはDriveへ出ずに失敗せず終わることを確かめる。実Driveの取り込みはT3.10で確認済み）。コピーを検証環境へ置く操作が分類器に止められたら、ユーザーに実行してもらう。
- **AI**: 流れの2で本番の役のPythonを動かすときは、P4の受入（[p4-acceptance.md](p4-acceptance.md) §7）と同じくAIを無効・鍵なしにする。Rustの演習のAI設定はローカルの用途だけ。
- **流れ**:
  1. 復元の演習（§復元。回転を1件してから古いバックアップへ戻し、画像の欠けを確かめる）。
  2. Pythonを本番の役で動かし、手順書のとおり（`--check-only`・保全・`--production`の配布を含む）にRustへ切り替える。停止時間を測る。
  3. シャドー比較: 同じDBの複製でRustと比較用Pythonを動かし、`tools/compare_with_python.py`で比べる。
  4. Rustの期間に、手動の位置・お気に入り・除外・回転・下書きの保存をする。
  5. 戻す（AI無効）。このとき共通の`/etc/default/geoframe`に`GEOFRAME_AI_ENABLED=1`を書いた状態にし、drop-inが勝つことを、Pythonのプロセスの環境の`GEOFRAME_AI_ENABLED`が`0`であること（`/proc/<pid>/environ`からこの名前だけを取り出す）とjournal・STATUSで確かめる。戻す時間を測る。
  6. 切り替え直す。
  7. 旧Pythonの判定済みの行があるDBで、`db.loc.clue_pending`とDecisionの進みを確かめる。
  8. `Conflicts=`と`After=`の試験（手順とは別）: 両方をunmaskし、片方がactiveのまま他方を`systemctl start`する。両方の向きで、旧プロセスの終了の時刻と新しいプロセスの起動・最初の書き込みの時刻をjournalで比べる。終わったら通常のmaskの状態に戻す。`systemd-analyze security geoframe-rs`の結果を記録する。
  9. 元に戻す: 一時的なPython版のunitとdrop-inを消して`daemon-reload`、設定のファイルを消す、印を戻す、`/var/lib/geoframe`を退避から戻す、`geoframe-pyref.service`（8090）と`geoframe-rs.service`を演習の前の形に戻す、本番のコピーを消す。
- 記録（`p5-acceptance.md`）には件数・ハッシュ・時間・固定の理由だけを書く。差分の中身（ファイル名・座標・地名）は作業用の置き場に残し、演習のあと消す。
- 演習のバイナリを保管し、sha256を記録する。本番には同じものを入れる。`p5-acceptance.md`には、演習のcommit（ブランチ）と、本番で`--expect-commit`に渡すP5aのマージのcommitの両方と、そのあいだの差が無いことの確認を記録する。

### unitの見直し（T5.1）

`deploy/geoframe-rs.service`（本体は`/usr/local/lib/systemd/system/`に置く）:

- 「検証環境専用」「本番機には入れない」「印が無いと断る」の記述を本番向けに書き直す。`Environment=`と`EnvironmentFile=`の優先の説明を正す（`EnvironmentFile=`が書いた順に関係なく`Environment=`に勝ち、`EnvironmentFile=`どうしは後のものが勝つ）。`deploy/geoframe-server.service`の同じ説明（:43-45）も正す（猶予期間のあいだ残るため）。
- `After=geoframe-server.service`を足す。`Environment=`のDATA_DIR・鍵のパス・TZを外し、`EnvironmentFile=-/etc/default/geoframe`のあとに`EnvironmentFile=/etc/default/geoframe-rs`（`-`無し）を足す。
- `StartLimitIntervalSec=`・`StartLimitBurst=`を足す。
- `ReadOnlyPaths=`に鍵（`-/var/lib/geoframe/drive_key.json`）と`models/`を足す。
- 制限を足す: `CapabilityBoundingSet=`（空）、`RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`（Python版の検査にsystemdへのAF_UNIXが要る）、`ProtectKernelTunables`・`ProtectKernelModules`・`ProtectKernelLogs`・`ProtectControlGroups`、`RestrictNamespaces`、`LockPersonality`、`RestrictSUIDSGID`、`PrivateDevices`、`SystemCallArchitectures=native`、`UMask=0027`。演習で起動・Python版の検査・取り込み・AIが動くことを確かめる。

### Pythonの撤去（R8。P5b）

- 埋め込む画面のファイルと旅程を`app/`から**`web/`**（リポジトリの直下）へ移す: `web/static`・`web/vendor`・`web/travel_blog/plans`。`build.rs`・`build_plans.rs`・試験・`rust-deploy.sh`（:540の差分確認）・文書のパスを直す。配るURL（`/static/`・`/vendor/`）と中身は変えない（埋め込みの表のパスと中身のハッシュを前後で比べる）。
- Rustの試験のうち**Pythonを実行する部分だけ**を外す。生成済みのJSONの基準との比較で、Pythonと同じ結果になることは今後も確かめる（品質の`switchbacks`のように、Pythonの結果をJSONに固めてある回帰試験は残す）。Pythonを実行しないと意味の無い試験（`tests/ai_index_python_rollback.rs`など）は消す。`crates`に`python`を起動するコードが無いことを試験かCIの検査で確かめる。
- `app/`のPython（`geoframe/`・`server.py`・`requirements*.txt`・`tests/`のPythonの試験）、`tools/gen_*_fixture.py`、`tools/compare_with_python.py`、`tools/image-poc`・`tools/loc_clue_eval`のうち`app/`に依存するものを消す。生成済みのJSONは残す。
- `docs/reference/configuration.md`はPythonの注釈から作っていた。Rustでは手で保ち、Rustのコードが読む`GEOFRAME_*`の名前がすべて載っていることを試験で確かめる（`gen_env_reference.py --check`の代わり）。
- `app/tests/admin_ai_ops_render.mjs`は`web/tests/`へ移し、CIで続けて回す。
- `deploy/loc_decision_morning.py`は残す（標準ライブラリだけ。本番機のシステムの`python3`に依存することを文書に書く）。試験`app/tests/test_loc_decision_morning.py`は`deploy/tests/`へ移す。
- CIのPythonのジョブを「文書・道具」のジョブにする: `check_markdown_links.py`、`loc_decision_morning`の試験（`pip install`なし）、nodeの描画試験。RustのジョブからPythonの導入を外す。
- 文書: 現行の文書（ルートの`README.md`、`deploy/README.md`の§1のvenv・§3・§5・§6・§7・トラブルシュートのCHDIR、`backup-catalog.sh`のエラー文）をRust版に書き直す。`docs/rust-migration/`の記録の文書（受入・タスク）は履歴として直さない。
- `deploy/geoframe-server.service`・`deploy-pull.sh`のPython版の部分を消し、`deploy.sh`を直すか消す（リポジトリの変更。本番の`/opt/geoframe`は固定の印があるので取り込まない）。本番の`/opt/geoframe/app/.venv`・Python版のunit・checkoutの片付けは、猶予期間が終わってからユーザーの許可を得て行う（§概要）。

## インターフェース

- `rust-deploy.sh --production=<ホスト名> --bin=<パス> --expect-sha256=<値> --expect-commit=<SHA> --expect-min-photos=<件数> [--wait-secs=<秒>] [--check-only]`（sshを使わない・ビルドしない）、`--rollback --production=<ホスト名>`。
- `deploy/geoframe-admin.sh <geoframeの引数>`（新規。サービスと同じユーザー・設定・制限でCLIを動かす）。
- `deploy-pull.sh`: `/etc/geoframe/checkout-pinned`がある、または`geoframe-rs`がenabled・active・maskedなら断る。
- `geoframe serve`と書き込み系のCLIの終了コード: Python版が稼働中は2、状態を取れないは1、rootでの実行は2、書き込み系のCLIでDATA_DIRに`catalog.db`が無いは2。印による拒否は無くなる。
- 設定のファイル: `/etc/default/geoframe`（共通）、`/etc/default/geoframe-rs`（Rustだけ。DATA_DIR・鍵のパス・TZを含む。必須）、`/etc/default/geoframe-py-rollback`（Pythonへ戻すときのAIの停止。切替の手順3で置くdrop-inで必須）。値は文書に書かない（名前と意味だけ）。
- unitの本体の置き場所: `/usr/local/lib/systemd/system/`。`/etc/systemd/system/`はmaskとdrop-inだけ。
- STATUSの`db.loc.clue_pending`の意味が「Rustの判定の待ち」を含む値になる。
- 埋め込みの元が`app/`から`web/`へ（P5b）。配るURLとファイルの中身は変わらない。

## データ

- `catalog.db`はそのまま使う。Rustは起動時にM0〜M7の移行（何もしないことを確認済み）とAIの表の作成をする。PythonはRustが足した表・列を無視して読める（P4で確認）。
- 切替の直前のバックアップと一次データのtarを、P5の記録が終わるまで消さない。

## 安全面

- 本番機での操作（停止・起動・配布・設定・`/var/lib/geoframe`）は、ユーザーの立ち会いと段ごとの許可のもとでだけ行う。
- 同時に書かない守り: バイナリのPython版の検査（`serve`と書き込み系のCLI）、mask（unitの本体を`/usr/local/lib/systemd/system/`へ）、`Conflicts=`と`After=`、ロック、配布の道具の検査、`deploy-pull.sh`の拒否（checkoutの固定の印）、手順の`fuser`・`pgrep`の確認。
- 秘密情報（鍵・設定の値・AI設定の中身）は文書・PR・ログに書かない。設定のファイルのモード: `/etc/default/geoframe*`は`0600 root:root`、AI設定のJSONは`0640 root:geoframe`、鍵は`0600 geoframe`。確かめるのはモードと所有者だけ。
- 検証環境へ本番のコピーを置くときは鍵を含めず、演習のあと消す（バックアップの回収先は除く）。

## テスト計画

| AC | 確かめ方 |
| --- | --- |
| AC1 | `cargo test`（印の無い一時DATA_DIRで`INGEST=1`が書く、Python版が稼働中なら`serve`と書き込み系のCLIが2で断る、状態を取れないなら1、rootの`serve`・書き込み系のCLIを断る、書き込み系のCLIは`catalog.db`が無ければ断る）、演習の8（両方をunmaskした状態での`Conflicts=`と`After=`の両方の向き） |
| AC2 | `rust-deploy-test.sh`に`--production`の場合を足す（ホスト名の違い・印がある・Python稼働中・状態不明・sha256の違い・commitの違い・汚れた作業ツリー・maskされている・件数の下限・`--check-only`・`--rollback --production`・失敗時の戻し・unitの置き場所）、`deploy-pull.sh`の拒否の試験（固定の印・Rustがenabled/active/masked）、演習の2で本番の経路を通す |
| AC3 | 演習の記録（起動前の`integrity_check`・`.dump`のSHA-256・所有者、起動後の画像の欠け） |
| AC4 | 演習の記録（停止時間・写真の集合の比較・`compare_with_python.py`・サイクル・`ai-jobs status`） |
| AC5 | 演習の記録（戻したあとの手動の列・下書き・AIの通信なし・品質の2つの場合、切り替え直し、戻す時間） |
| AC6 | 本番の切替の記録 |
| AC7 | CI（RustのジョブにPythonの導入なし）、`crates`に`python`の起動が無いことの検査、埋め込みの表の前後比較 |
| AC8 | `cargo test`（Rustの待ちがあるとき`clue_pending`が0でない）、演習の7 |
| AC9 | 文書のレビュー |

## リスクと対策

| リスク | 対策 |
| --- | --- |
| PythonとRustが同時に書く | バイナリのPython版の検査、mask、`Conflicts=`と`After=`、`deploy-pull.sh`の拒否、`fuser`・`pgrep` |
| 戻したPythonがAIを動かして課金する | drop-inの`geoframe-py-rollback`（無ければPythonが起動しない） |
| 本番へ違うバイナリ・違う機体へ配る | `--production=<ホスト名>`、印との排他、`--expect-sha256`、`--skip-build`必須 |
| DATA_DIRの誤りで空のDBを作って「成功」 | `--expect-min-photos` |
| 初回の配布が失敗して配信が止まったまま | 手順書で、終了コードが0でなければすぐPythonへ戻す |
| 復元でDBが壊れる・開けない | 3つのファイルを一緒に退避、置く前の検査、所有者の確認 |
| maskが失敗する／戻したあとに`deploy-pull.sh`がP5bを取り込む | unitの本体を`/usr/local/lib/systemd/system/`へ、checkoutの固定の印 |
| CLIが別のDATA_DIRを相手にする | `geoframe-admin.sh`で同じ設定のファイル、`catalog.db`が無ければ断る |
| 戻すときの設定が切替の前のPythonを変える | drop-inはPythonを止めた後に置く |
| Python版が`app/.env`で持つ値をRustが読まない | 準備で`/etc/default/geoframe-rs`へ移し、演習で同じ設定を使う |
| Decisionの待ちを見落とす | `clue_pending`の計算を直す、演習の7、Decisionの常駐を確かめる |
| 猶予期間のあとは数分で戻せない | 期間を決め、そのあいだは本番のcheckout・venv・unitを残す |
| 撤去で配る画面が変わる | 埋め込みの表のパスと中身のハッシュを前後で比べる |

## 実装の順番

| ID | 内容 | 種類 | PR |
| --- | --- | --- | --- |
| T5.1 | ガードの見直し（印の検査を外し、Python版の検査は残す。終了コード・rootの拒否・`catalog.db`が無いCLIの拒否）、`geoframe-rs.service`の見直し（置き場所・`After=`・設定のファイル・`StartLimit`・`ReadOnlyPaths`・制限・記述）、`rust-deploy.sh --production`とその試験（rootで動くので、checkoutの`git`の検査は所有者で動かすか`safe.directory`を付けて「dubious ownership」で落ちないようにする）、`deploy-pull.sh`の拒否、`geoframe-admin.sh` | full | P5a |
| T5.2 | STATUSの`db.loc.clue_pending`をRustの判定の待ちを含む値に直す | full | P5a |
| T5.3 | 手順書`cutover.md`（準備・保全・復元・切替・確認・止める判断・AIの書き込み系の操作・戻す・切り替え直す・切替のあとの数日・クラウドAIの有効化・猶予期間とそのあとの片付け・猶予期間のあとの戻し）、`deploy/README.md` §7の本番向けの記述 | full | P5a |
| T5.4 | 検証環境での演習（§演習の1〜9）と`p5-acceptance.md`。比較用Pythonの更新と、本番と同じ配置の一時的なPython版を含む | ops | P5a |
| （P5aのPR・マージ。そのあと本番の`/opt/geoframe`をP5aへ更新し、切替の日時をユーザーと決める） | | | |
| T5.5 | 本番の切替（ユーザーの立ち会い）と記録 | ops | P5b |
| T5.6a | 埋め込みの元を`web/`へ移し、Rustの試験からPythonの実行を外す | full | P5b |
| T5.6b | Pythonの実装・道具・CIのジョブ・Python版のunitと配布の撤去、文書の書き直し、設定の参照の試験 | full | P5b |
| （P5bのPR・マージ。猶予期間が終わったら、本番の固定の印・venv・Python版のunit・checkoutをユーザーの許可を得て片付ける） | | | |
