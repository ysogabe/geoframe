# P5（本番切替）設計

## 概要

[P5 PRD](p5-prd.md)の設計と手順の骨組み。本番機での実際の手順は、T5.2で手順書`cutover.md`に書く。P5は**2つのPRに分ける**:

- **P5a（切替の準備）**: ガードの撤去・本番への配布・手順書・演習（T5.1〜T5.3）。切替の**前に**mainへマージする。
- **P5b（Pythonの撤去）**: 本番の切替（T5.4）のあとで、Pythonの実装と依存を外す（T5.5a〜b）。

分ける理由: 本番の`/opt/geoframe`はmainのgitの取り込み（`deploy/deploy-pull.sh`）でPython版を動かしている。Pythonを消したコミットが切替の前にmainへ入ると、次の取り込みでPython版が壊れる。切替のための変更は、レビュー済みのmainから本番へ入れたい。このため、撤去は切替を確かめたあとの別のPRにする。PythonはP5の中で撤去する（2026-10-29 ユーザー決定）ので、P5bは切替の直後に進め、安定を待つ期間は置かない。

## 今の作り（調査の結果）

### Rustの守り

- 取り込みのガード: `crates/geoframe/src/guard.rs`。`GEOFRAME_INGEST=1`のとき、書く前に`/etc/geoframe/staging`の印と`geoframe-server.service`の非稼働を確かめ、だめなら終了コード2（[server.md §11.1.1](server.md#1111-本番への誤指定の防止ガード)）。呼び出しは`ingest::start_with`、拒否は`IngestError::Refused`→`cli.rs`の`exit_for_ingest_error`。試験は`tests/ingest_cli.rs`。
- unit: `deploy/geoframe-rs.service`（`User=geoframe`、`ExecStart=/usr/local/bin/geoframe serve`、`Conflicts=geoframe-server.service`、`ReadOnlyPaths=-/var/lib/geoframe/photos`、`EnvironmentFile=-/etc/default/geoframe`、`RestartPreventExitStatus=2`）。ポートは既定で8080で、Pythonと同じ（`config.rs:9`、`app/server.py:34`）。miharashiの接続先は変わらない。
- 配布: `deploy/rust-deploy.sh`。手元でmuslビルド→転送→`.prev`を残して入れ替え→`/healthz`と`/api/v1/photos`の確認、失敗したら前の版へ戻す。相手に印が無い、またはPython版が稼働中なら断る（`preflight`、:148-162）。試験は`deploy/rust-deploy-test.sh`。

### Python版の運用

- `deploy/geoframe-server.service`（venvの`python server.py`、`WorkingDirectory=/opt/geoframe/app`）。
- 更新は`deploy/deploy-pull.sh`（git pull・`pip install`・`systemctl restart geoframe-server`・`/api/admin/status`）と`deploy/deploy.sh`（Macからの薄いラッパー）。ロールバックは[deploy/README.md §6](../../deploy/README.md)（gitのSHAを戻す）。
- バックアップ: `deploy/backup-catalog.sh`と`geoframe-backup.timer`（6時間ごと。`sqlite3 .backup`・`integrity_check`・gzip・`latest.db.gz`）。Pythonに依存しない。**復元の手順はどこにも無い**。
- 毎朝のDecision: `deploy/loc_decision_morning.py`（標準ライブラリだけ・システムの`python3 -I`）と`geoframe-loc-decision.timer`。STATUSの`db.loc.clue_pending`を読む。Rustも同じキーを出す（`admin.rs:498`）ので、そのまま使える。

### Pythonへの依存（撤去で直すもの）

- Rustのビルドが`app/static`・`app/vendor`・`app/travel_blog/plans`を埋め込む（`crates/geoframe/build.rs`・`build_plans.rs`）。試験の一部も`app/`を読む（`tests/admin_ui_ai_waits.rs`、`static_files.rs`など）。
- Rustの試験がPythonを呼ぶ: `ai/groups/tests.rs`、`ai/scene_judge/tests.rs`・`runtime/tests.rs`、`ai/location_suggest/tests.rs`・`runtime/tests.rs`、`ai/location_clue/tests.rs`、`tests/ai_index_python_rollback.rs`。CIのRustのジョブはそのためにPythonを入れている。
- CIのPython・文書のジョブ: `app/tests`の単体試験、`tools/gen_*_fixture.py --check`、`tools/gen_env_reference.py --check`（`app/geoframe/config.py`の注釈から`docs/reference/configuration.md`を作る）、`tools/check_markdown_links.py`。
- `tools/`のPython: `gen_*_fixture.py`（約24本。`app/`をimportする）、`compare_with_python.py`、`image-poc/`、`loc_clue_eval/`。`miharashi-feed-check`はRust。
- `app/tests/admin_ai_ops_render.mjs`（nodeで`admin.js`を試す）。

## 方針

### ガード（R1）

ガードはコードから**外す**（plan.mdの案のうち、印を本番用に置き換える案は選ばない）。理由: 切替のあとは本番がRustの唯一の書き手で、印を足しても「本番で書いてよい」を毎回言わせるだけになる。PythonとRustが同時に書かない守りは次の3つで持つ:

1. 手順: Pythonを`stop`・`disable`し、`systemctl is-active geoframe-server`が`inactive`と確かめてからRustを起動する。
2. `Conflicts=geoframe-server.service`を**残す**（既定）。一方を`start`すると他方をsystemdが止める。演習（T5.3）で、ロールバックと再切替の向きが手順と合うことを確かめる。
3. 二重起動のロック（`DATA_DIR/.ingest.lock`）はそのまま。

`sync --dry-run`は今までどおり書かない。

### 本番への配布（R2）

`rust-deploy.sh`に**本番の指定**（`--production`）を足す。`--production`のときは印の検査を省き、それ以外の検査は保つ: 相手でPython版が稼働中・状態不明なら断る、バイナリのsha256の照合、`.prev`の保存、確認と失敗時の戻し。印の有無で本番と検証環境を見分けるのはやめる。`--production`が無いのに相手に印が無ければ、今までどおり断る（検証環境への配布で本番を指す誤りを止める）。

本番機はこの開発機と同じ機体なので、`--target`には自分の機体を指す。配布はユーザーの立ち会いと許可のもとで行う。

切替のあとの本番の更新は、`rust-deploy.sh --production`でバイナリを入れ替える形にする。`deploy-pull.sh`はP5bで、git（`deploy/`の道具と文書）の取り込みだけにするか、外す（T5.5bで決める。Python版の`pip install`と`geoframe-server`の再起動は外す）。

### バックアップと復元（R3）

バックアップは今の`backup-catalog.sh`とtimerをそのまま使う（`sqlite3 .backup`はRustが動いていても取れる）。復元の手順を手順書に書く:

1. Rustを止める（`systemctl stop geoframe-rs`）。
2. 今の`catalog.db`・`-wal`・`-shm`を退避する（消さない）。
3. `latest.db.gz`（または選んだ世代）を`gunzip`して`catalog.db`に置き、所有者を`geoframe`にする。
4. `sqlite3 catalog.db 'PRAGMA integrity_check'`がok。
5. Rustを起動し、`/healthz`・`/api/v1/photos`の件数を確かめる。

### 切替（R4・R7）

手順書の骨組み（細部はT5.2で書く）:

1. 準備（前日まで）: 本番の`/etc/default/geoframe`にRust用の値を足した案をユーザーと確かめる（`GEOFRAME_INGEST=1`、`GEOFRAME_AI_CONFIG`（ローカルの用途だけ）、`GEOFRAME_TILE_UA`など）。Rustは`app/.env`を読まない（`config.rs:35-38`）ので、Python版が`app/.env`で持っている値があれば移す。AI設定のJSONと鍵の権限（`0600`・`geoframe`の所有）を確かめる。切替に使うバイナリは、T5.3の演習で使ったものと同じsha256。
2. 切替の直前: 手動でバックアップを1回取る（`backup-catalog.sh`）。`/api/v1/photos`の件数・STATUSを控える。
3. Pythonを止める: `systemctl disable --now geoframe-server`、`is-active`が`inactive`。
4. Rustを入れる: `rust-deploy.sh --production`（unitの導入・`enable`・起動・確認まで）。
5. 確かめる: `/healthz`・`/api/v1/photos`が200で件数が控えと同じ、STATUS、管理画面、miharashiの表示、取り込みの1サイクルの成功、`ai-jobs status`（ローカルの用途が「使える」、索引の構築中）。
6. 止める判断: 5のどれかが30分のうちに直らなければ、R5の手順でPythonへ戻す。

切替のあとの数日（R9）: Decisionの全件再判定（約4時間）の終わり、索引の公開（約48分）、バックアップのtimerの成功、journalの警告を見る。そのあとクラウドAIを有効にする（日の上限を小さく。設定と`release-pause`の要否は[ai-config.md](ai-config.md)）。

### Pythonへ戻す・切り替え直す（R5）

戻す（AIは無効）:

1. `systemctl stop geoframe-rs`、`disable`。
2. 必要なら切替の前のバックアップから復元する（Rustが書いたものを捨ててよいとき）。そうでなければ、Rustが書いたDBのまま戻す（P4で、PythonはRustが足した表・列を無視して読めると確認済み。[p4-acceptance.md §7](p4-acceptance.md)）。
3. `/etc/default/geoframe`で`GEOFRAME_AI_ENABLED=0`（PythonのAIを止める）。
4. `systemctl enable --now geoframe-server`。`Conflicts=`でRustが動いていれば止まる。
5. 確かめる: 配信の件数、管理画面、手動の位置・お気に入り・除外・回転が保たれている。品質の切り戻しは[server.md §16.10](server.md#1610-rustで作った結果をpythonへ切り戻すp5のロールバック)の確認をする。

P5bのマージのあとで戻すときは、Python版のコードがmainに無い。`/opt/geoframe`を撤去の前のコミット（P5aのマージコミット）に戻し、venvを作り直す手順を手順書に書く。

切り替え直す: R4の3〜5と同じ。

### 演習（R6）

検証環境（lxc-geoframe）で、本番のバックアップから作ったコピーを使う。検証環境は本番のバックアップを回収している（`deploy/backup-pull.sh`）。新しいコピーを検証環境へ置く操作が分類器に止められたら、ユーザーに実行してもらう。

1. 復元の演習（R3の手順）。
2. 検証環境の比較用Python（`/opt/geoframe-pyref`、P5の前に更新する）を本番の役にして動かし、手順書のとおりにRustへ切り替える。
3. シャドー比較: 同じDBの複製でRustと比較用Pythonを動かし、`tools/compare_with_python.py`で比べる。
4. 戻す演習（AI無効）と、切り替え直す演習。`Conflicts=`の向きを確かめる。
5. 演習のあと、検証環境を元に戻す（今までの受入と同じ）。

演習のバイナリのsha256を記録し、本番には同じものを入れる。

### Pythonの撤去（R8）

- 埋め込む画面のファイルと旅程を`app/`から**`web/`**（リポジトリの直下）へ移す: `web/static`・`web/vendor`・`web/travel_blog/plans`。`build.rs`・`build_plans.rs`・試験・`rust-deploy.sh`（:540の差分確認）・文書のパスを直す。配るURL（`/static/`・`/vendor/`）と中身は変えない（埋め込みの表の比較で確かめる）。
- Rustの試験のうちPythonを呼ぶ部分を外す。Pythonと同じ結果になることは、生成済みのJSONの基準（`crates/geoframe/tests/fixtures`など）との比較で今後も確かめる。Pythonだけを流す試験（例: `ai_index_python_rollback.rs`、品質の`switchbacks`）は、切り戻しの先のPythonが無くなるので消す。
- `app/`のPython（`geoframe/`・`server.py`・`requirements*.txt`・`tests/`のPythonの試験）、`tools/gen_*_fixture.py`、`tools/compare_with_python.py`、`tools/image-poc`・`tools/loc_clue_eval`のうち`app/`に依存するものを消す。生成済みのJSONは残す。
- `docs/reference/configuration.md`はPythonの注釈から作っていた。Rustでは手で保ち、Rustのコードが読む`GEOFRAME_*`の名前がすべて載っていることを試験で確かめる（`gen_env_reference.py --check`の代わり）。
- `app/tests/admin_ai_ops_render.mjs`は`web/`の近く（`web/tests/`）へ移し、CIで続けて回す。
- `deploy/loc_decision_morning.py`は残す（標準ライブラリだけ）。試験`app/tests/test_loc_decision_morning.py`は`deploy/tests/`へ移す。
- CIのPythonのジョブを「文書・道具」のジョブにする: `check_markdown_links.py`、`loc_decision_morning`の試験（`pip install`なし）、nodeの描画試験。RustのジョブからPythonの導入を外す。
- `deploy/geoframe-server.service`、`deploy/README.md`のPython版の節（§1のvenv、§3、§5、§6、トラブルシュートのCHDIR）をRust版に書き直す。README §7の「検証環境専用」を外し、本文にする。本番の`/opt/geoframe/.venv`と`geoframe-server.service`の撤去は、P5bのマージのあとにユーザーの許可を得て行う。

## インターフェース

- `rust-deploy.sh --production`（新しい指定）。他の引数・環境変数は今までどおり。
- `geoframe serve`の終了コード2のうち「ガードが断った」は無くなる（設定の誤りの2は残る）。
- 本番の`/etc/default/geoframe`: Rust用に`GEOFRAME_INGEST=1`・`GEOFRAME_AI_CONFIG`などを足す。値は文書に書かない（名前と意味だけ）。
- 埋め込みの元が`app/`から`web/`へ。配るURLとファイルの中身は変わらない。

## データ

- `catalog.db`はそのまま使う。Rustは起動時にM0〜M7の移行（何もしないことを確認済み）とAIの表の作成をする。PythonはRustが足した表・列を無視して読める（P4で確認）。
- 切替の直前のバックアップを、P5の記録が終わるまで消さない（`backup-catalog.sh`の28世代の外に1つ残す）。

## 安全面

- 本番機での操作（停止・起動・配布・設定・`/var/lib/geoframe`）は、ユーザーの立ち会いと段ごとの許可のもとでだけ行う。
- ガードを外したあとの守りは、手順・`Conflicts=`・ロック。配布の道具はPython版が稼働中なら断る。
- 秘密情報（鍵・`/etc/default/geoframe`の値・AI設定の中身）は、文書・PR・ログに書かない。鍵の権限は`0600`・`geoframe`の所有を確かめる（中身は読まない）。
- 検証環境へ本番のコピーを置くとき、コピーは検証環境の中だけで扱い、演習のあとに消す（バックアップの回収先は除く）。

## テスト計画

| AC | 確かめ方 |
| --- | --- |
| AC1 | `cargo test`（ガードの試験を消し、印の無い一時DATA_DIRで`INGEST=1`の起動が書く試験を足す）、`grep`の0件、演習で`Conflicts=`の向き |
| AC2 | `rust-deploy-test.sh`に`--production`の場合を足す（Python稼働中・状態不明は断る、印が無くても通る、`--production`無しで印が無ければ断る、失敗時の戻し） |
| AC3 | 演習の記録（`integrity_check`・`.dump`のSHA-256・件数） |
| AC4 | 演習の記録（手順書の各段・`compare_with_python.py`の結果・miharashi・サイクル・`ai-jobs status`） |
| AC5 | 演習の記録（戻したあとの件数・手動の列・品質の件数、切り替え直しのあとの確認） |
| AC6 | 本番の切替の記録 |
| AC7 | CI（Pythonの導入なし）、埋め込みの表の前後比較（パスと中身のハッシュが同じ）、`grep -rn "app/" crates deploy .github`の確認 |
| AC8 | 文書のレビュー |

## リスクと対策

| リスク | 対策 |
| --- | --- |
| Pythonを止めずにRustを起動して二重に書く | 手順で`inactive`を確かめる、`Conflicts=`、配布の道具の検査 |
| Python版が`app/.env`で持つ値をRustが読まず、動きが変わる | 準備で`/etc/default/geoframe`へ移す。演習で同じ設定を使う |
| 切替のあと問題が出る | 30分の判断の基準、戻す手順の演習、直前のバックアップ |
| P5bのマージのあと戻せない | P5aのマージコミットへ戻す手順を書く。P5bは切替を確かめてから |
| Decisionの全件再判定で本番機が重くなる | 受け入れ済み（約4時間）。切替の時刻をユーザーと決める |
| 撤去で配る画面が変わる | 埋め込みの表のパスと中身のハッシュを前後で比べる |

## 実装の順番

| ID | 内容 | 種類 | PR |
| --- | --- | --- | --- |
| T5.1 | ガードの撤去（コード・試験・文書）、`rust-deploy.sh --production`とその試験、unitの記述（本番用。`Conflicts=`は残す） | full | P5a |
| T5.2 | 手順書`cutover.md`（準備・バックアップと復元・切替・確認・止める判断・戻す・切り替え直す・切替のあとの数日・クラウドAIの有効化）、`deploy/README.md` §7の本番向けの記述 | full | P5a |
| T5.3 | 検証環境での演習（復元・切替・シャドー比較・戻す・切り替え直す）と`p5-acceptance.md`。比較用Pythonの更新を含む | ops | P5a |
| （P5aのPR・マージ） | | | |
| T5.4 | 本番の切替（ユーザーの立ち会い）と記録 | ops | P5b |
| T5.5a | 埋め込みの元を`web/`へ移し、Rustの試験からPythonを外す | full | P5b |
| T5.5b | Pythonの実装・道具・CIのジョブ・unitの撤去、文書の書き直し、設定の参照の試験 | full | P5b |
| （P5bのPR・マージ、そのあと本番のvenv・Python版unitの撤去をユーザーの許可を得て） | | | |
