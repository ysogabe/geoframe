# GEOFRAMEのデプロイ

このディレクトリには、Linuxサーバー向けのsystemd unitと運用スクリプトの構成例があります。
GEOFRAMEはRustの単一バイナリ（`/usr/local/bin/geoframe`）です。リポジトリのcheckoutを`/opt/geoframe`へ置いて
unitとスクリプトの元にし、変更されるデータを`/var/lib/geoframe`へ保存し、権限を制限した
`geoframe`ユーザーでサービス（`geoframe-rs.service`）を実行する構成を想定しています。

> [!WARNING]
> GEOFRAMEには利用者認証がありません。localhostへバインドするか、信頼できるLAN内だけで
> 使用してください。ポート8080をインターネットへ直接公開しないでください。

> [!NOTE]
> Python版（`geoframe-server.service`・`deploy-pull.sh`・venv）は、P5bで本リポジトリから削除しました
> （Python版は、P5aのマージのコミット
> [`a804623`](https://github.com/ysogabe/geoframe/tree/a804623029ad67d415692d08ea3182fd765eb2a4)
> まで存在します）。本番機のPython版のunitとvenvは片付け済みで、Rust版は`geoframe-server.service`の
> 有無を見ません。Python版へ戻す手順は[`docs/rust-migration/cutover.md`](../docs/rust-migration/cutover.md)（§8・§13.2）にあります。

## 前提条件

- x86_64のDebianまたはUbuntu系Linux。`sqlite3`、`git`、`curl`
- `systemd`（同梱unitを使う場合）
- サーバー上にPython・venv・Rustのツールチェーンは**要りません**。バイナリは別の機体でビルドして配ります
  （`x86_64-unknown-linux-musl`の完全静的ビルド。[build.md](../docs/rust-migration/build.md)）。
- システムの`python3`（標準ライブラリだけ。pipで何も入れない）は、§2.3の毎朝のDecisionタイマー
  （`loc_decision_morning.py`）を使う機体にだけ要ります。`geoframe-loc-decision.service`は
  `/usr/bin/python3 -I`で起動します。その試験は`python3 -m unittest discover -s deploy/tests`です。
- サーバーからgithub.comへのSSH到達性（TCP 22）。`/opt/geoframe`のcheckoutを`git fetch`で更新するときに使います（§5参照）

## 1. checkoutの配置

ログインシェルとホームディレクトリを持たないシステムユーザーを作ります。

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin geoframe
sudo install -d -o root -g root -m 0755 /opt/geoframe
```

`/opt/geoframe`はコードを動かす場所ではなく、`deploy/`のスクリプトとunitの元を置くcheckoutです
（`rust-deploy.sh --production`はここから実行し、ここの`deploy/geoframe-rs.service`を入れます）。

### 1.1 Deploy Keyの用意

リポジトリはPublicなのでHTTPSでも取得できますが、本番機はPrivateだった時期から、読み取り専用のDeploy Keyを使ったSSHで取得しています。鍵は
`/etc/geoframe/ssh/`へ置きます。`geoframe`ユーザーはnologinでホームディレクトリを
持たず、更新は`root`が実行するため、どのユーザーのホーム（`/root/.ssh`を含む）にも
依存しない専用の場所に置くのが目的です。

```sh
sudo install -d -o root -g root -m 0700 /etc/geoframe/ssh
sudo ssh-keygen -t ed25519 -N '' -C 'geoframe-deploy' \
  -f /etc/geoframe/ssh/geoframe_deploy_key
sudo cat /etc/geoframe/ssh/geoframe_deploy_key.pub
```

表示された公開鍵を、GitHubのリポジトリ設定 **Settings > Deploy keys > Add deploy key**
へ登録します。**「Allow write access」は絶対に有効にしないでください。** 本機はコードを
取得するだけで、書き戻すことはありません。読み取り専用にしておけば、鍵が漏れても
リポジトリを改変されません。

`known_hosts`は、GitHubが公開しているフィンガープリントと照合してから固定します。
`ssh-keyscan`の結果をそのまま信用しないでください（初回接続時の中間者攻撃を防ぐため）。

```sh
ssh-keyscan github.com | sudo tee /etc/geoframe/ssh/known_hosts >/dev/null
sudo ssh-keygen -lf /etc/geoframe/ssh/known_hosts
```

出力されたフィンガープリントが、GitHubのドキュメント
（[SSH key fingerprints](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints)）
に掲載された値と一致することを確認します。一致しない行は削除してください。

### 1.2 リポジトリの取得

Deploy KeyはSSHでのみ使えるため、HTTPSではなくSSHのURLでcloneします。cloneの時点では
まだ`/opt/geoframe`がgitリポジトリになっておらず、以降で使う`git config --local`
（作業ツリー専用の設定）が使えません。そこでこの一回だけ`GIT_SSH_COMMAND`環境変数で
Deploy Keyを指定します。

```sh
sudo env GIT_SSH_COMMAND='ssh -i /etc/geoframe/ssh/geoframe_deploy_key -o IdentitiesOnly=yes -o UserKnownHostsFile=/etc/geoframe/ssh/known_hosts' \
  git clone git@github.com:ysogabe/geoframe.git /opt/geoframe
```

cloneしたら、この作業ツリー専用のgit設定を入れます。以降の`git fetch`など毎回の操作で
Deploy Keyが使われるよう、**`core.sshCommand`の設定は必須です。** これが無いと、
`root`で実行するgitは`/root/.ssh/`を探しにいき、上で作った専用の鍵と
`known_hosts`が使われません。

```sh
cd /opt/geoframe
sudo git config --local core.sshCommand \
  'ssh -i /etc/geoframe/ssh/geoframe_deploy_key -o IdentitiesOnly=yes -o UserKnownHostsFile=/etc/geoframe/ssh/known_hosts'
sudo git config --local gc.auto 0
sudo git fetch origin
```

`gc.auto 0`は、自動gcが到達不能になった古いコミットを回収してしまうのを防ぎます
（ロールバックの手段を残すため。§6参照）。

所有者は`root:root`に揃えます。**更新スクリプトは`root`で動くため、リポジトリの
所有者を`geoframe`などに変えないでください**（`dubious ownership`でgitが停止します。
トラブルシュート参照）。

```sh
sudo chown -R root:root /opt/geoframe
sudo find /opt/geoframe -path /opt/geoframe/.git -prune -o -type d -exec chmod 0755 {} +
```

## 2. データと秘密情報の設定

同梱のunit（`geoframe-rs.service`）は`StateDirectory=geoframe`により`/var/lib/geoframe`を作成します。
変更されるデータと、任意のGoogle Drive認証情報はここへ置きます。

```sh
sudo install -d -o geoframe -g geoframe -m 0750 /var/lib/geoframe
sudo install -o geoframe -g geoframe -m 0600 \
  /path/to/drive_key.json /var/lib/geoframe/drive_key.json
```

環境固有の値は、systemdの`EnvironmentFile=`で渡します（Rust版は`.env`を読みません）。**値は文書・PR・ログに
書きません**（ここには名前だけを示します）。全項目は[設定リファレンス](../docs/reference/configuration.md)にあります。

| ファイル | 内容 | モード |
| --- | --- | --- |
| `/etc/default/geoframe` | 共通の設定（例: `GEOFRAME_ALLOWED_HOSTS`、`GEOFRAME_TILE_UA`）。無くても起動する | `0600 root:root` |
| `/etc/default/geoframe-rs` | Rustだけの設定。`GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`を必ず置く（無ければ起動しない）。取り込みを使うときは`GEOFRAME_INGEST=1`も | `0600 root:root` |
| AI設定のJSON（`GEOFRAME_AI_CONFIG`が指す） | AIの用途ごとの接続先・モデル・上限・予算。鍵のパスは持つが中身は持たない | `0640 root:geoframe` |

旅行ブログの機能は削除しました（2026-11。書き出しは`geoframe-export`が引き継ぎました）。環境ファイルに`GEOFRAME_BLOG`・`GEOFRAME_BLOG_PLANS_DIR`が残っていても読まずに無視するので、起動に影響せず、消さなくても構いません。`/blog-admin`は管理画面を返します。`GET /api/blog/*`と`POST /api/blog/save`・`/api/blog/export`は404、`HEAD /api/blog/*`とその他の`POST /api/blog/*`は403です（削除前の、無効にしていたときと同じ応答です）。書き出し済みの`blog_exports/`は、今までどおり`/blog_exports/`で配信します。

```sh
sudo install -d /etc/geoframe
sudo install -o root -g root -m 0600 /dev/null /etc/default/geoframe-rs   # 中身は各自で書く
```

LAN内の別端末からHTTP 403になる場合は、アクセスに使った正確なホスト名とポートを
`GEOFRAME_ALLOWED_HOSTS`へ追加します。IPリテラルによるアクセスは許可されますが、名前付き
ホストは明示的な許可が必要です。ホストが日本標準時以外の場合は`TZ`も変更してください。

### 2.1 embeddingとAI（任意）

AIは用途（画像のembedding・検索語のembedding・同一場面の比較・撮影地の手がかりと推定）ごとに、
`GEOFRAME_AI_CONFIG`のJSONで接続先・モデル・上限を決めます（[ai-config.md](../docs/rust-migration/ai-config.md)、
embeddingの契約は[ai-embedding.md](../docs/rust-migration/ai-embedding.md)）。このJSONが無ければ、Rust版は
推論を始めません（写真の配信・取り込み・品質解析は動きます）。旧Python版にあった、このサーバーのCPUで
embeddinggemma-2を動かす方式（`GEOFRAME_EMBED_BACKEND=local`の`requirements-local-ai.txt`と
`prefill_embeddings`）はRust版にはありません。ローカルのembeddingは、別のサービスのAPIを
AI設定の`local_base_url`で指します。

### 2.2 学習済みの順位付け（任意）

同一場面グループの中の順位（Geminiに見せる最大6枚の選択と、その並び）を、過去のGemini比較評価から
学習した線形モデルで付けられます。モデルは家庭の写真の評価から作るため、リポジトリには入れず、
`/var/lib/geoframe/models/rank_model.json`（`GEOFRAME_RANK_MODEL`）に置きます。読み込みは
ファイルの更新を検知して自動で行う（AIワーカーのサイクルごとに読み直す）ので、再起動は要りません。

```sh
sudo install -o geoframe -g geoframe -m 0640 rank_model.json /var/lib/geoframe/models/rank_model.json
```

- モデルの`embedding_model`が今のembeddingと違う場合（例: ローカル用のモデルで`gemini`のまま）、
  ファイルが壊れている場合は使わず、固定の式で順位を付けます（ログに理由が出ます）。
- 7枚以上のグループでは選ばれる6枚が変わるため、そのグループは比べ直しになります。
- 使っているモデルは`photo_ai_groups.algorithm`の末尾（`:rank-<名前>`）で分かります。
  戻すときはファイルを消します。

### 2.3 撮影地の手がかり判定を毎朝動かす（任意）

位置なし写真の撮影地候補は、Decision が動いている間だけ判定が進みます
（[写真パイプライン](../docs/architecture/photo-pipeline.md)）。gpu-eval の Decision は今は常駐で、
geoframe は同じ機体の LAN 入口（AI 設定の`local_base_url`に固定のパスを付ける。
[ai-config.md](../docs/rust-migration/ai-config.md)）経由で呼び、どの要求にも呼び出し元の名前 `X-Client-Name: geoframe` を付けます（入口は名前ごとに日ごとにまとめます）。
Decision が常駐している間は、この節のタイマーは「人が起動済み」として何もしません。
常駐をやめて必要なときだけ動かす運用に戻す場合のために、毎朝5:00に次の順で確かめ、
必要なときだけ Decision を動かすタイマーを用意しています
（[`loc_decision_morning.py`](loc_decision_morning.py)。確認の要求には `X-Client-Name: geoframe-loc-morning` を付けます）。

1. STATUS で、今 Decision を動かせば進む判定待ちが0なら何もしない。数えるのは `db.loc.clue_runnable`
   （Rust 版が判定するときだけ出る。Provider の停止中・品質の解析待ち・期限前の後回しだけの写真は含まない。
   理由は `db.loc.clue_pause`）で、無ければ（旧 Python など）`db.loc.clue_pending`。総待ち `clue_pending` が
   残っていても `clue_runnable` が0なら起動しない（起動しても進まないため）
2. 人が Decision を起動済みなら何もしない（止めるのも人に任せる）
3. GPU が空いていなければ何もせず、翌朝にもう一度見る。空いているとみなすのは、llm-manager の
   lease が0件、ほかのプロセスの VRAM が1GB未満、GPU 使用率が10%未満、Ollama がモデルを
   読み込んでいない、のすべてを満たすとき。llm-manager か Ollama の状態を取れないときも使わない
4. Decision を起動したあとは、総待ち `clue_pending` の減り方で進み具合を追い、次のどれかで止める
   （続きは翌朝）。`clue_pending` が0になった、`db.loc.clue_pause` が空でない（Provider・用途の停止。待っても
   進まない）、総待ちが15分間減らない、2時間を過ぎた、別の処理が lease を取った・GPU を使い始めた。
   `clue_runnable` が一時的に0になっただけ（最後の1件が429などで後回し）で総待ちが残っていれば、
   15分の猶予のあいだは Decision を動かしたまま待つ。旧 Python など `clue_runnable` が無いときは
   `clue_pending` だけで見る

判定が終わった日の Gemini への問い合わせは geoframe（`geoframe-rs.service`）が続けるので、Decision を止めても
候補づくりは進みます。

llm-manager は誰かが lease を取ると Decision を含む常駐を止め、lease が終わると起動し直します。
このタイマーが起動した Decision がそうして戻されたまま動き続けないよう、起動したときに
`~/.local/state/geoframe-loc-decision/decision-owned`へ印を残します。lease のために止められた
ときは上限の時間まで戻るのを待って止め、それでも戻らなければ翌朝、印のある Decision を
引き継いで止めます（人が起動した Decision には印が無いので触りません）。

Decision を動かしているユーザーで入れます（`ExecStart`はシステムの`/usr/bin/python3 -I`。標準ライブラリだけなので、pipもvenvも要りません）。

```sh
install -m 0644 /opt/geoframe/deploy/geoframe-loc-decision.service \
  /opt/geoframe/deploy/geoframe-loc-decision.timer ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now geoframe-loc-decision.timer
systemctl --user list-timers geoframe-loc-decision.timer
```

ログイン中でなくても動くよう、そのユーザーで lingering を有効にしておきます
（`loginctl show-user <ユーザー> -p Linger`が`yes`）。結果は
`journalctl --user -u geoframe-loc-decision.service`で確かめます。すぐに1回動かすときは
`systemctl --user start geoframe-loc-decision.service`です。やめるときは
`systemctl --user disable --now geoframe-loc-decision.timer`を実行します。

## 3. サーバーの有効化

unitは`rust-deploy.sh`が`/usr/local/lib/systemd/system/geoframe-rs.service`へ入れます（§7.1・§7.3。手でunitを置かない）。
本番の初回の切替は、手順書（[`cutover.md`](../docs/rust-migration/cutover.md)）の段に従ってください。有効化したあとの確認とログは次のとおりです。

```sh
systemctl is-enabled geoframe-rs && systemctl is-active geoframe-rs
curl --fail http://127.0.0.1:8080/healthz
curl --fail http://127.0.0.1:8080/api/admin/status
journalctl -u geoframe-rs.service -f
```

## 4. バックアップ

`catalog.db`には、有料生成したAI埋め込み、手動GPS修正、お気に入りなどが含まれます。
稼働中のWALモードDBにはSQLiteの`.backup`を使い、単純な`cp`は使用しないでください。

[`backup-catalog.sh`](backup-catalog.sh)は一貫した圧縮スナップショットを作成し、
`PRAGMA integrity_check`で検証してから世代管理します。既定のパスは
`/var/lib/geoframe/catalog.db`と`/var/backups/geoframe`です。

```sh
sudo install -d -o geoframe -g geoframe -m 0750 /var/backups/geoframe
sudo install -o root -g root -m 0644 \
  /opt/geoframe/deploy/geoframe-backup.service \
  /etc/systemd/system/geoframe-backup.service
sudo install -o root -g root -m 0644 \
  /opt/geoframe/deploy/geoframe-backup.timer \
  /etc/systemd/system/geoframe-backup.timer
sudo systemctl daemon-reload
sudo systemctl enable --now geoframe-backup.timer
sudo systemctl start geoframe-backup.service
```

タイマーと最新バックアップを確認します。

```sh
systemctl list-timers geoframe-backup.timer
sudo -u geoframe gzip -t /var/backups/geoframe/latest.db.gz
```

少なくとも1系統は別の機体へ保管してください。`backup-pull.sh`はLXC向け、
`backup-pull-n100.sh`はSSH向けのpull型構成例です。接続先などを環境変数で明示して使用します。

## 5. 更新

Rust版の更新は、**バイナリの入れ替え**です。コードを`/opt/geoframe`で動かすのではなく、別の機体で
ビルドしたバイナリを`rust-deploy.sh`が`/usr/local/bin/geoframe`へ入れ、unitを入れ直し、確認します。
Python版の更新に使っていた`deploy-pull.sh`（fetch・`git reset --hard`・`pip install`・再起動）と、そのMac側の
ラッパー`deploy.sh`は、Pythonの運用専用だったのでP5bで削除しました。

1. **ビルド**（Rustのツールチェーンのある機体で）: `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`
   （[build.md](../docs/rust-migration/build.md)）。成果物の`sha256`を控える。
2. **checkoutを手で更新**（本番機の`/opt/geoframe`。unitとスクリプトの元を新しくするため）:

   ```sh
   sudo git -C /opt/geoframe rev-parse HEAD     # 更新の前のSHA。戻すときのために控える
   sudo git -C /opt/geoframe fetch origin
   sudo git -C /opt/geoframe reset --hard <SHA>
   sudo git -C /opt/geoframe rev-parse HEAD     # 更新後のSHA。--expect-commit に渡す
   ```

   **サーバー上で`/opt/geoframe`のファイルを直接編集しないでください。** 配布の道具は追跡下のファイルの変更があると
   断ります（§7.3）。`git reset --hard`が触るのは追跡下のファイルだけで、`git clean`は実行しません。
   実データは`/var/lib/geoframe`にあり、リポジトリの外です。ファイルのモードはgitに任せ、
   一律の`chmod`はしません（`*.sh`の実行ビットを潰すと`geoframe-backup.service`が`203/EXEC`で失敗します）。
   ディレクトリは`root:root`の755のままにします。`git gc`・`git prune`は実行しません（§6）。
3. **バックアップ**: DBスキーマを変える更新の前には、必ず手動で取る。

   ```sh
   sudo -u geoframe /opt/geoframe/deploy/backup-catalog.sh
   ```

4. **配布**: `sudo /opt/geoframe/deploy/rust-deploy.sh --production=<ホスト名> --bin=<バイナリ> --expect-sha256=<値> --expect-commit=<SHA> --expect-min-photos=<件数>`
   （引数・拒否の条件・終了コードは§7.3。`--check-only`で変更の前の検査だけ先にできる）。検証環境へは、手元から`deploy/rust-deploy.sh`（ssh・ビルドあり）。

## 6. ロールバック

Rust版の戻しは2段あります。

- **直前のバイナリとunit（`.prev`）へ戻す**: `sudo /opt/geoframe/deploy/rust-deploy.sh --rollback --production=<ホスト名>`
  （§7.3）。戻せるのは直前の1世代だけです。初回の配布には`.prev`が無いので使えません。
- **checkoutを前のコミットへ戻す**: 更新後に、`/opt/geoframe`のunitやスクリプトを前の状態へ戻したいとき。
  更新の前の`HEAD`は、§5の手順2の最初の`rev-parse`の出力に控えておきます。

  ```sh
  sudo git -C /opt/geoframe reset --hard <控えたSHA>
  ```

  出力を控えていない場合は、サーバー上でreflogから探せます。reflogのエントリには有効期限があります
  （既定で到達可能なもの90日、到達不能なもの30日）。`gc.auto 0`（§1.2）によりコミットオブジェクト自体は
  残るので、確実な記録は更新時に控えたSHAです。GitHub側でforce-pushやsquash mergeが行われて
  `origin/main`の履歴から消えたコミットも、ローカルにオブジェクトが残っていれば戻せます。

Python版へ戻す手順は、[`cutover.md` §8・§13.2](../docs/rust-migration/cutover.md)にあります。

## 7. Rust 版の unit と配布の道具

本番は Rust の単一バイナリで動きます（P5aで切替の道具を入れ、P5bでPython版をリポジトリから外しました。
計画は[plan.md](../docs/rust-migration/plan.md)）。**Rust 版は既定で読み取り専用です（`GEOFRAME_INGEST`を設定しない限り DB に書かない）。**
**本番の切替・戻し・復元の手順（準備・保全・切替・確認・AI の書き込み系の操作・戻す・切り替え直す・復元・切替のあとの数日・クラウド AI の有効化・戻しの猶予期間と片付け）は、[`docs/rust-migration/cutover.md`](../docs/rust-migration/cutover.md)にあります。**
本番の操作はこの節ではなく手順書に従ってください（各段に目的・コマンド・確かめること・失敗したときの対処があります）。
`rust-deploy.sh --production`（§7.3）は、手順書のとおりの段で、ユーザーの立ち会いのもとでだけ実行します。
この節は、道具の引数・拒否の条件・終了コードの**リファレンス**です。

| ファイル | 役割 |
| --- | --- |
| [`geoframe-rs.service`](geoframe-rs.service) | Rust 版の systemd unit。ハードニングに加えて、Rust 版だけ付けられる`MemoryDenyWriteExecute=true`と、追加の制限を足す |
| [`rust-deploy.sh`](rust-deploy.sh) | 検証環境へ: 手元でビルド（`x86_64-unknown-linux-musl`の完全静的）→ 検証環境の確認 → 転送 → 入れ替え → 確認 → 失敗時の復元。本番へ: `--production=…`（ssh なし・ビルドなし・その機体で root として直接。§7.3） |
| [`rust-deploy-test.sh`](rust-deploy-test.sh) | `rust-deploy.sh`の本体（検証環境の経路と`--production`）を、モックの`systemctl`/`curl`/`hostname`で試験する（ssh もサービスも使わない） |
| [`geoframe-admin.sh`](geoframe-admin.sh) | `geoframe`の CLI（`ai-jobs`など）を、サービスと同じユーザー・同じ設定のファイル・同じ制限で動かす（`systemd-run`。root で直接動かさない）。動かす前に DATA_DIR（パスだけ）を出す |
| [`geoframe-admin-test.sh`](geoframe-admin-test.sh) | `geoframe-admin.sh`の拒否を、モックの`systemd-run`/`systemctl`で試験する |
| [`loc_decision_morning.py`](loc_decision_morning.py)・[`tests/`](tests/) | 毎朝のDecision起動（§2.3）。標準ライブラリだけのPython。システムの`python3`で動き、試験は`python3 -m unittest discover -s deploy/tests`（pip不要） |
| [`geoframe-export.service`](geoframe-export.service) | note 記事用の写真書き出し（`geoframe-export serve`）の systemd unit の雛形。配置は§8 |
| [`geoframe-export-admin.sh`](geoframe-export-admin.sh)・[`geoframe-export-admin-test.sh`](geoframe-export-admin-test.sh) | `geoframe-export`の CLI（`run`・`recover`）を、サービスと同じ専用ユーザー・同じ設定・同じ制限で動かす（`systemd-run`）。試験は、モックの`systemd-run`/`systemctl`/`id`と、unit との食い違いの検査 |
| [`../tools/miharashi-feed-check/`](../tools/miharashi-feed-check/README.md) | miharashi の取得・読み込みのコードを、画面なしで Rust 版に向けて動かす確認用ハーネス |

### 7.1 unit の要点

- バイナリは`/usr/local/bin/geoframe`、`geoframe serve`をそのまま起動する（`app/`も`.venv`も要らない）。
- **unit の本体は`/usr/local/lib/systemd/system/geoframe-rs.service`に置く**（`rust-deploy.sh`が入れる）。
  `/etc/systemd/system/`には、`systemctl mask`のリンクと drop-in（`*.service.d/`）だけを置く（mask は`/etc/systemd/system/<unit>`に`/dev/null`へのリンクを作るので、
  そこに本体のファイルがあると失敗する）。`/etc/systemd/system/geoframe-rs.service`に古い本体があると、systemd はそちらを優先するので、`rust-deploy.sh`は断る（先に退避する）。
- `User=geoframe`、`StateDirectory=geoframe`（`/var/lib/geoframe`）、`EnvironmentFile=-/etc/default/geoframe`（旧 Python 版と共通の設定。無くてもよい）のあとに
  `EnvironmentFile=/etc/default/geoframe-rs`（Rust だけ。`-`無し: 無ければ起動しない）。**`GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`は unit に書かず、`/etc/default/geoframe-rs`に置く**
  （`0600 root:root`。名前と意味だけを文書に書き、値は書かない）。unit と`geoframe-admin.sh`が同じファイルを読むので、同じ DATA_DIR を指す。
  `EnvironmentFile=`は、unit に書いた順に関係なく`Environment=`に勝ち、`EnvironmentFile=`どうしは後に書いたものが勝つ（同じ名前を両方に置かない）。
- `TimeoutStopSec=90s`: 停止（SIGTERM）はHTTP・同期・AI・キャッシュ保存・runtimeの停止を合わせて受信から60秒の共通期限で打ち切る（[server.md §22](../docs/rust-migration/server.md)）。unitは期限のあとの余裕を含めて90秒待ち、終わらなければSIGKILLする（実行中のAIジョブは次の起動が回収する）。
- `RestartPreventExitStatus=2`: 設定不正（終了コード 2）は再起動しても直らないので、再起動ループにせず`failed`で止める。
  DB が開けない（終了コード 1）は再起動対象。`LimitNOFILE=4096`（同時の接続 256＋配信 32＋SQLite で 300 前後を使う）。
- **取り込み（DB への書き込みと同期サイクル）は`GEOFRAME_INGEST=1`のときだけ動く**（既定は`0`）。`GEOFRAME_INGEST`が無ければ、設定のファイルがどうであっても DB には書かない。取り込みを使うときは、`/etc/default/geoframe-rs`（検証環境では drop-in でもよい）で`GEOFRAME_INGEST=1`を足す。
  **書き込みの前に、root で実行しているときは、書かずに終了コード 2 で断る**（詳細は[server.md §11.1.1](../docs/rust-migration/server.md)）。**root で直接動かさず**、サービスと同じユーザー・設定で動かすには`deploy/geoframe-admin.sh`を使う。
  **書き込み系の CLI（`sync`・`organize`・`quality-scan`・`ai-jobs retry|release-pause|deactivate-space`）は、`DATA_DIR`に`catalog.db`が無ければ作らずに終了コード 2 で断る**（`serve`は`GEOFRAME_INGEST=1`なら空の DB を作る）。
  有効にすると、`serve`は DB が無ければ新しく作り、スキーマの作成・移行をして、
  `GEOFRAME_DRIVE_POLL_MIN`（既定 10 分）ごとに同期サイクルを回す（[server.md §11](../docs/rust-migration/server.md)）。
- `StartLimitIntervalSec=120`・`StartLimitBurst=5`: DB が開けないなどの失敗（終了コード 1）を 5 秒おきに再試行するが、120 秒に 5 回で止める（止まったら`systemctl reset-failed geoframe-rs`のあとで起動する）。
- 制限（Rust 版だけ付けられるもの）: `CapabilityBoundingSet=`（空）、`RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`
  （`AF_UNIX`は以前の Python 版の検査の`systemctl show`に要った。今は使わないが、絞るなら検証環境で確かめてから）、`ProtectKernelTunables`・`ProtectKernelModules`・`ProtectKernelLogs`・`ProtectControlGroups`、`RestrictNamespaces`、
  `LockPersonality`、`RestrictSUIDSGID`、`PrivateDevices`、`SystemCallArchitectures=native`、`UMask=0027`（作るファイルは所有者とグループだけが読める）。
- `ReadOnlyPaths=`は、`photos/`・`drive_key.json`・`models/`だけ（writer が書く`catalog.db`・`derivatives/`は P2 で、旅行ブログの保存・書き出しが書いていた`travel_blog/`・`blog_exports/`は P3 / T3.6 で外した。旅行ブログの機能は削除したが、unit の許可は変えていない。`tmp/`・`.cache.json`・`.ingest.lock`は元から対象外）。
  WAL の DB は読み取りでも同じディレクトリに`-shm`/`-wal`を作るので、ディレクトリ自体は書き込み可のまま。
- 地図タイル（`/tiles/`）と地名検索（`/api/admin/geocode`）は外向きに通信する。OSM のタイルは`GEOFRAME_TILE_UA`（連絡先入り。未設定なら既定値）、地名検索（地理院・Nominatim）も同じ`GEOFRAME_TILE_UA`を User-Agent に送る（Python は地名検索に固定の`GeoFrame/0.2`）。地理院・OSM のタイル・Nominatim の実通信は、検証環境で確認済み（P3。OSM と Nominatim は連絡先入りの`GEOFRAME_TILE_UA`を足した2026-10-25。[plan.md](../docs/rust-migration/plan.md)・[drive-acceptance.md](../docs/rust-migration/drive-acceptance.md) §6）。タイルのキャッシュ（`tile_cache/`）は`GEOFRAME_INGEST=0`でも書く（`catalog.db`ではなく`DATA_DIR`の下）。
- `catalog.db`を別の inode に置き換えたら（バックアップからの復元など）再起動する。
- 取り込みを 1 回だけ手で動かす: `geoframe sync`（`GEOFRAME_INGEST=1`が要る）。**`serve`が動いているあいだは、管理画面の運用タブの「同期をいま回す」「品質解析の一括」から同じことができる**ので、CLI は`serve`が動いていないときに使う。**書かずに計画だけ**見るなら`geoframe sync --dry-run`（行・写真・キャッシュ・ロックを変えず、件数だけを出す。ただし SQLite が読み取りでも作る`catalog.db-wal`・`-shm`はできうる。ガードを通さないのは書かないからで、本番機で実行してよいという意味ではない: 向けるのは検証環境の本番コピー（そのバックアップのコピー）だけで、本番の`catalog.db`には向けない。docs/rust-migration/plan.md §2）。**Drive の鍵（`GEOFRAME_DRIVE_KEY`）がある機体では、`--dry-run`も Drive の一覧を読みに接続する**（原本のダウンロードも書き込みもしない。鍵が無ければ通信しない）。通信を避けたいときは、**存在しない鍵ファイルの絶対パスを`GEOFRAME_DRIVE_KEY`に明示する**（例: `GEOFRAME_DRIVE_KEY=/nonexistent/drive_key.json geoframe sync --dry-run`）。`--data-dir`を変えても鍵の設定は変わらず、`GEOFRAME_DRIVE_KEY`を空文字にしても既定の`drive_key.json`（カレントディレクトリ基準）に戻るので、どちらも通信を避けられない。Driveの一覧が空、または消す行が多すぎる（50件超かつDriveの行の20%超）ときは、削除を保留して件数だけ警告する。意図した削除は、通常は管理画面の運用タブから解除し、`serve`が動いていないときは`geoframe sync --allow-mass-delete`の1回で外す（`--dry-run`と併せて件数を見られる。`serve`の定期実行では外れない。下の「Drive の大量削除の保留」）。位置の推定（`infer`）と地名（`enrich`）の段は、`--dry-run`でも書かず、**`enrich`は地理院に問い合わせない**（件数だけ。`infer`は推定・取り消しの件数、`enrich`は国外・キャッシュで済む・問い合わせるはずの件数）。取り込み済みの本番コピーでは、`infer`の`inferred`・`updated`・`cancelled`と`enrich`の`candidates`がほぼ0になる。実行では`enrich`が地理院へ問い合わせる（1サイクル40件まで、通信を試した行ごとに0.2秒待ち。件数は`queried`＝試した行、`resolved`＝うち確定した行）。通信の障害（切断・時間切れ・429・5xx）は短い待ちで2回まで取り直し、接続できない・429はすぐ段を中断して失敗にする。5xx・時間切れ・切断は、その座標を30分以上飛ばして次へ進み、続けて3つの座標で起きたら中断する。片方のAPI（逆ジオか標高）だけが障害のあいだは、その座標を確定しない（Pythonは片方の応答だけで確定する）。
- **Drive の大量削除の保留（T3.7）**: 一覧から消える行が 50 件を超えかつ Drive の行の 20% を超える、または一覧が空のとき、`index_drive`は削除を保留する。**取り込みが有効な`serve`は、保留中だけ`/api/admin/status`の`sync.mass_delete_hold`に件数・理由・開始時刻を出し、管理画面の STATUS タブに赤い注意を出す**（取り込み無効のときと最初のサイクルの前は出ない。記憶はプロセスの中だけで、再起動後の最初のサイクルでまた付く）。**意図した削除なら、通常は管理画面の運用タブから解除する**（保留中の件数・理由と、消える行の集合を確かめて承認する。承認した集合に無い行は消さず、取り直した一覧が空なら再び保留する）。一覧が空のための保留（`empty_listing`）は画面からは解除できず、CLI だけ。削除の直前に、解除前の写しを`DATA_DIR/tmp/catalog-before-release.db`に残す（下の「解除前の写しの戻し方」）。`serve`が動いていないときは`geoframe sync --allow-mass-delete`を 1 回実行する（`serve`が動いていると`.ingest.lock`が二重の取り込みを断る。`sudo deploy/geoframe-admin.sh sync --allow-mass-delete`）。手動で直した位置（`loc_source='manual'`）は、Drive の`md5`・ローカルの`mtime`が変わっても書き換わらない（T3.7）。詳細は[server.md §10.6](../docs/rust-migration/server.md)。
- **解除前の写しの戻し方**: 運用タブの解除（`Only`）で消す行があるときだけ、削除の直前に`DATA_DIR/tmp/catalog-before-release.db`へ`catalog.db`の写しを作る（`.part`に書いてから置き換える。権限は`catalog.db`と同じ。失敗したら削除しない）。次の手順で戻す。
  1. `serve`を止める（`sudo systemctl stop geoframe-rs`）。
  2. `catalog.db`・`catalog.db-wal`・`catalog.db-shm`を別の場所へ退かし、写しを`catalog.db`として置く（所有者・権限は元の`catalog.db`に合わせる）。
  3. 戻した Drive の行の表示用画像は削除のときに消えていて戻らない。`fetch`に取り直させるため、戻す行は、手順 2 で退かした`catalog.db`を`ATTACH`して決める（`sudo -u geoframe sqlite3 <DATA_DIR>/catalog.db`で開く。root で開くと`-wal`・`-shm`が root の所有になりサービスが書けなくなる。退かした`-wal`・`-shm`は退かした`catalog.db`と同じディレクトリに同じ名前で置いておく。そうしないと退かした DB の最新の中身が読まれない。例: `ATTACH '<退かした catalog.db>' AS cur;`のあと、写しに戻った行のうち`cur.photos`に無い Drive の行 → `UPDATE photos SET display=NULL, fetched_at=NULL WHERE source='drive' AND NOT EXISTS (SELECT 1 FROM cur.photos c WHERE c.id = photos.id AND c.drive_id IS photos.drive_id);`。`id`だけでなく`drive_id`も突き合わせるのは、解除のあとに写真 ID が再利用されていても取り違えないため）。Drive 側にファイルが戻っていなければ、次のサイクルでまた保留になる。列は[schema.rs](../crates/geoframe/src/schema.rs)の`photos`。`fetch`は`display IS NULL`の行だけ取る。
  4. `serve`を起動する。

  注意: 写しは**解除した時点の DB 全体**なので、そのあとの変更（★・位置・AI の結果）は戻すと失われる。バックアップのタイマー（`catalog.db`だけ）には写しが含まれない。写しは次の解除まで残り、手で消してよい（作った時刻はファイルの更新時刻で見る）。
- 品質（`quality`の段。孤児の特徴量を消す→未解析を`GEOFRAME_QUALITY_PER_CYCLE`枚（既定は`GEOFRAME_FETCH_PER_CYCLE`）まで解析→未解析が無くて`photo_quality_dirty=1`のときだけ全写真を再グルーピングして`photo_quality`を全差し替え）は同期サイクルの最後に動く。**`--dry-run`は解析せず**、消す孤児・未解析・今回解析する枚数・再構築をするかを件数で出す（取り込み済みの本番コピーでは全部ほぼ0。Python が作った特徴量の行も有効として使うので、Rust に切り替えても全写真を解析し直さない。Python へ切り戻したときの扱い（`algorithm`は結果を作った実装の印で、Python は読まない）は[server.md §16.10](../docs/rust-migration/server.md)。[server.md §16](../docs/rust-migration/server.md)）。一括解析（現行の`server.py --quality-scan`）は`geoframe quality-scan [--dry-run] [--data-dir <dir>]`（`GEOFRAME_INGEST=1`と既存の`catalog.db`が要る。root では断り、取り込みのロックで`serve`と二重に動かない）。
  外向き HTTPS の確認は`geoframe selfcheck-net`（状態と HTTP ステータスだけを出す）。
- **AIの背景ワーカー（P4 / T4.1c）**: `GEOFRAME_INGEST=1`かつ`GEOFRAME_AI_ENABLED=1`かつ有効な背景用途（`GEOFRAME_AI_CONFIG`のJSON。[ai-config.md](../docs/rust-migration/ai-config.md)）があるときだけ起動する。**いまの配布バイナリは用途別の処理を登録していない**ので、ジョブを1件も実行しない（実処理はT4.2・T4.4・T4.6）。`INGEST=0`ではAIの表を作らず、回収もしない。AIの設定が不正でも写真の配信・管理は続き、STATUSタブに設定の誤りの固定コードが出る。
  状態の確認とやり直しは`geoframe ai-jobs`: `status`（読み取りだけ。稼働中でも可。件数と固定の理由コードだけ）、`retry (--kind <種類>|--all)`（失敗だけを再検査。外部へ送らない。検査できない種類は保持して件数だけ報告）、`release-pause (--task <用途>|--all)`（認証・契約の不一致で止まったProvider・用途の明示解除）。`retry`と`release-pause`は、**通常は管理画面の運用タブから**（STATUSタブの要対応の行からも移れる。`POST /api/admin/ops/ai-retry`・`ai-release-pause`。種類ごと／すべて、用途ごと／すべて。結果は運用タブの履歴に残る）。**`serve`が動いていないときはCLI**で、書くので**サービスを止めてから**`GEOFRAME_INGEST=1`で実行する（`sync`と同じく root では断る。稼働中は`.ingest.lock`で断る）。運用タブの解除は、認証が直っていれば背景ワーカーが次に取り出すときからクラウドへ送り直す（料金がかかる。日の上限はそのまま効く）。設定ファイルを直した場合は再起動するまで新しい設定にならず、直っていなければ同じ理由で再び止まる。一時待機（`ai_waits`）と各ジョブの再試行の予定時刻は残るので、その期限までは送らない。
- **待ち受けは`--listen`を書かない**。`GEOFRAME_HOST`/`GEOFRAME_PORT`（既定`0.0.0.0:8080`）で決まる。

### 7.2 別のポートで動かす（検証）

同じ機体で2つ目の Rust 版（別の`DATA_DIR`）を動かすなど、待ち受けを分けたいときは、drop-in の`ExecStart=`に`--listen`を書く
（`--listen`は`GEOFRAME_HOST`/`GEOFRAME_PORT`より優先され、それらを読まない）。**同じ`catalog.db`を2つのプロセスで同時には開かない**。

```sh
sudo systemctl edit geoframe-rs
```

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/geoframe serve --listen 0.0.0.0:8081
```

### 7.3 配布と確認（検証環境と本番）

```sh
# 接続先は引数か環境変数で必ず指定する（既定値は無い）
export GEOFRAME_SSH_TARGET=user@検証環境
deploy/rust-deploy.sh --dry-run        # ビルドと手順の表示だけ（ssh もしない）
deploy/rust-deploy.sh                  # ビルド→検査→転送→入れ替え→/healthz と /api/v1/photos の確認
deploy/rust-deploy.sh --skip-build     # ビルド済みの成果物をそのまま配る
deploy/rust-deploy.sh --rollback       # 直前のバイナリ（.prev）へ手で戻す
bash deploy/rust-deploy-test.sh        # 失敗経路と --production をモックで試験（ssh なし）。2 つの試験とも CI（test.yml の「配布スクリプト（モックの試験）」ジョブ）で回る
bash deploy/geoframe-admin-test.sh     # geoframe-admin.sh をモックの systemd-run で試験
```

以下の「検証環境の印」は検証環境の経路（`--target`）の話。本番の指定（`--production`）は次の「本番へ」にある。

**検証環境の印（誤配布の防止）。** 変更の前に、接続先に次を確かめ、満たさなければ何も変更せずに終了コード 4 で拒否する（`--rollback`も同じ）。

- 検証環境の印`/etc/geoframe/staging`（通常ファイル。シンボリックリンクは不可）があること。**検証環境で一度だけ作る**（本番機には作らない）:
  `sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging`（`/etc/geoframe/`は本番機にも SSH の鍵置き場として存在するので、ディレクトリではなくファイルを見る）。

検証環境の経路（`--target`）は、印の無い機体には配らない。unit の本体は`/usr/local/lib/systemd/system/geoframe-rs.service`へ入れ、
`/etc/systemd/system/geoframe-rs.service`に古い本体のファイルがあれば（systemd がそちらを優先するので）何も変更せずに断る（先に退避する。mask のリンクは本体ではないので通る）。

**本番へ（`--production`）。** 本番の切替の手順（[`cutover.md`](../docs/rust-migration/cutover.md)）の中で、ユーザーの立ち会いのもとでだけ使う。段取り・終了コードごとの対処・戻し方は手順書にあり、ここには引数と拒否の条件を書く。
ssh を使わず、**その機体で root として直接**動く（ビルドもしない）。`sudo`は環境変数を消すので、値はすべて引数で渡す。実行場所は本番の checkout（`/opt/geoframe`）。
本番の経路は`GEOFRAME_RS_*`（試験用の差し替え・配置先・確認の URL・待ち秒数）を**一切使わない**。配置先（`/usr/local/bin/geoframe`）・確認の URL（`http://127.0.0.1:8080`）・unit の置き場所・
印・設定のファイル・root の UID は、スクリプトの中の固定値（`PROD_*`）で、環境が 1 つでも`GEOFRAME_RS_*`を持っていれば終了コード 4 で断る（`sudo -E`などで環境を引き継いでも検査を外せない）。
モックの試験は、一時コピーの`PROD_*`の行を書き換える（`geoframe-admin-test.sh`と同じ方式）。

```sh
# 変更の前の検査だけ
sudo /opt/geoframe/deploy/rust-deploy.sh --production=<ホスト名> --bin=<バイナリ> --expect-sha256=<値> \
    --expect-commit=<SHA> --expect-min-photos=<件数> --check-only
# 配布（unit の導入・enable・起動・確認まで。--wait-secs は演習で測った /healthz を待つ秒数）
sudo /opt/geoframe/deploy/rust-deploy.sh --production=<ホスト名> --bin=<バイナリ> --expect-sha256=<値> \
    --expect-commit=<SHA> --expect-min-photos=<件数> [--wait-secs=<秒>]
# 直前のバイナリと unit（.prev）へ戻す
sudo /opt/geoframe/deploy/rust-deploy.sh --rollback --production=<ホスト名>
```

`--rollback --production`は`.prev`（Rust の直前の版）へ戻す道具で、**初回の配布には`.prev`が無いので、初回の切替の戻しには使えない**（初回の失敗は`rust-deploy.sh`が自分で戻す。Python へ戻す手順は[`cutover.md`](../docs/rust-migration/cutover.md) §8）。

次のどれかなら、何も変更せずに終了コード 4（引数の誤りは 2）で断る: `hostname`が`--production`の値と違う／検証環境の印`/etc/geoframe/staging`がある（検証環境に本番の指定は使えない）／
`geoframe-rs`が mask されている（切り替え直すときは先に`unmask`）／
古い置き場所（`/etc/systemd/system/geoframe-rs.service`）に本体がある／`/etc/default/geoframe-rs`が通常のファイルとして無い・モードが`0600`でない・所有者が root でない（中身は読まない）／
checkout の`HEAD`が`--expect-commit`（40 桁）と違う／追跡下のファイルに変更がある（未追跡の`app/.venv`・`.env`・鍵は見ない。root で動くので、この checkout だけ`safe.directory`にして「dubious ownership」で落ちない）／
`--bin`の写し（root 所有の一時ディレクトリへ写してから`sha256`を取る。入れるのはその写し）が`--expect-sha256`と違う。
配布のあと`/api/v1/photos`の件数が`--expect-min-photos`より少なければ、DATA_DIR の誤りなどとして失敗にし、配布前の状態へ戻す。
数の引数は桁を絞る: `--expect-min-photos`は 1 以上 999999999 以下（9 桁まで）、`--wait-secs`は 0〜3600。範囲外・数でない値は終了コード 2 で断る（Bash の整数比較は大きすぎる値で異常終了し、
`if`の中では「下限を満たした」ように進んでしまうため）。確認の本体も、比較が成り立つときだけ通し、比較の異常は失敗として戻す。Python 版を止めたあとの初回の配布が 0 でなければ、
すぐ Python 版へ戻す（手順は`cutover.md` §8）。ただし終了コード 2・4（何も変えずに断った）は、原因がすぐ直せて配信停止の期限内に再実行できるときだけ再実行する（`cutover.md` §0.4）。入れる unit は、その checkout の`deploy/geoframe-rs.service`。

`serve`が動いていないときに CLI を動かすとき（AI の書き込み系の操作、`sync`など。稼働中は運用タブから行うので、CLI のときはサービスを止めてから）は、`sudo deploy/geoframe-admin.sh <コマンド> …`（例: `ai-jobs …`・`sync --allow-mass-delete`）を使う（root で直接`geoframe`を動かさない。動かす前に DATA_DIR のパスが表示される）。
配置先のバイナリは入れ替えの直前に`/usr/local/bin/geoframe.prev`へ（unit も`.prev`へ）残す。**直前の 1 世代だけ**。

**失敗したときの復元。** 入れ替え以降（unit の導入・`daemon-reload`・`enable`・`restart`・確認）のどこで失敗しても、
さらにリモートの本体が中断されても（ssh の切断・SIGTERM）、配布前の状態へ戻す。

| 状況 | 戻し方 |
| --- | --- |
| 既存の配布を更新 | バイナリを`.prev`から、unit を`.prev`から戻す。配布前に稼働していれば再起動して確認、止まっていれば止めたまま。配布前に`enable`でなかったなら`disable`する。失敗した方のバイナリは`geoframe.failed`に残る |
| 初回の配布（戻すバイナリが無い） | サービスを止め、配布前に`enable`でなかったなら`disable`し、新しく入れた unit を撤去し、バイナリを`geoframe.failed`へ退避する（以前の世代の`.prev`は戻す先にしない） |

終了コード（`--production`も同じ）: 0 成功 / 1 失敗したが配布前の状態へ戻した / **3 戻す途中でも失敗した**（途中で失敗した操作と、手作業の復旧手順が出る。
「戻した」とは表示しない）/ 4 誤配布の防止で拒否 / 2 引数の誤り。

初回だけ、検証環境で次を整える（以後は`rust-deploy.sh`が unit を入れる）。

```sh
sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging  # 検証環境の印（上記。本番の指定ではこの印があると断る）
sudo chown -R geoframe:geoframe /var/lib/geoframe   # WAL の -shm を作れる持ち主にそろえる
sudo install -m 0644 /dev/null /etc/default/geoframe # 出力に影響する設定だけ書く。本番に合わせて GEOFRAME_EMBED_BACKEND=local など
```

## 8. note 記事用の写真書き出し（geoframe-export）の配置

日付（または期間）の写真を、表示用 JPEG と`manifest.json`にして Google Drive の`yamareco/`・`travel-journal/`へ書き出す、別プロセスのサービスです
（`geoframe-export`。写真サービスとは HTTP だけでつながります）。仕様は[PRD](../docs/note-photo-export/prd.md)、設計は[design.md](../docs/note-photo-export/design.md)にあります。
Drive は **Drive API を直接呼んで**操作します（rclone は使いません）。フォルダは Drive の ID で指し、資格情報は専用の JSON のファイルで渡します。設計は[drive-api.md](../docs/note-photo-export/drive-api.md)です。
この節は**配置の手順**で、配置は PRD・設計の「配置後」の確認（§8.8）まで、ユーザーの立ち会いのもとで行います。
**値（トークン・client_id・client_secret・Drive のフォルダの ID・実際の IP）は、文書・PR・ログに書きません**（ここの IP は文書用の予約アドレス`192.0.2.x`です。実際は tailnet の IP を使います）。

本番では 2026-11-22 から、rclone を呼ぶ版が動いています。その版から切り替えるときは、§8.1 の順番と§8.6 の切り替えの手順に従い、**前のバイナリと前の unit を残して**から入れ替えます（戻し方は§8.9）。

### 8.1 前提と順番

1. 写真サービスを先に更新します。書き出しは写真サービスの一覧 API（`GET /api/admin/export/photos`）を読むので、その API を足したバージョンを先に本番へ入れます
   （いつもの手順。§5・§7.3、本番は[`cutover.md`](../docs/rust-migration/cutover.md)）。入れたあと、一覧 API が答えることを確かめます。
   `curl --fail -s -o /dev/null -w '%{http_code}\n' 'http://127.0.0.1:8080/api/admin/export/photos?from=<YYYY-MM-DD>&to=<YYYY-MM-DD>'`が`200`なら先へ進みます。
2. 次に`geoframe-export`を入れます（このあとの節）。写真サービスの unit と DB には触れません。
3. Drive に、書き出し先の親フォルダ（`yamareco/`・`travel-journal/`）と作業用のフォルダ（§8.5）が先に存在している必要があります。その ID を、`/etc/default/geoframe-export`に書きます（§8.4）。
4. Drive の資格情報のファイル（`/etc/geoframe-export/drive-credentials.json`）を作ります（§8.3）。rclone 版から切り替えるときは、今の`rclone.conf`から**1 回だけ**取り込みます（同意のやり直しはしません）。

### 8.2 ビルドと専用ユーザー・置き場所

バイナリは写真サービスと同じ musl の完全静的ビルドです（[build.md](../docs/rust-migration/build.md)。配るのは`geoframe-export`だけです）。

```sh
cargo build --release --target x86_64-unknown-linux-musl -p geoframe-export --bin geoframe-export
sha256sum target/x86_64-unknown-linux-musl/release/geoframe-export   # 控える。本番機で入れる前に同じ値か確かめる
```

専用ユーザーはログインシェルとホームを持たない`geoframe-export`です（写真サービスの`geoframe`とも、オーナーのユーザーとも別にします）。
状態ディレクトリ（`/var/lib/geoframe-export`）は、unit の`StateDirectory=`が起動時に作ります。トークンのファイル（§8.4）を先に置くため、先に作っておきます。

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin geoframe-export
sudo install -d -o geoframe-export -g geoframe-export -m 0700 /var/lib/geoframe-export
sudo install -o root -g root -m 0755 geoframe-export /usr/local/bin/geoframe-export
sudo install -D -o root -g root -m 0644 /opt/geoframe/deploy/geoframe-export.service /usr/local/lib/systemd/system/geoframe-export.service
```

unit は`geoframe-rs.service`と同じく`/usr/local/lib/systemd/system/`に置きます（`/etc/systemd/system/`には drop-in と mask のリンクだけを置きます。§7.1）。
`rust-deploy.sh`は`geoframe-export`を扱わないので、入れ替えは手で行います。**更新・切り替えのときは、戻せるよう、上書きの前に今のバイナリと unit を残します**:

```sh
sudo cp -p /usr/local/bin/geoframe-export /usr/local/bin/geoframe-export.prev
sudo cp -p /usr/local/lib/systemd/system/geoframe-export.service /usr/local/lib/systemd/system/geoframe-export.service.prev
```

（`.prev`は上書きされるので、2 回続けて入れ替える前に、戻す必要が無いことを確かめます。）
Drive の操作は HTTPS で直接行うので、rclone 本体は要りません。

### 8.3 Drive の資格情報（専用の client_id・取り込み・再認可）

書き出しは**このサービス専用の client_id** で認可した Drive の資格情報だけを使います（rclone の共用の client_id は 2026 年中に使えなくなります）。権限は Drive 全体のままです
（設計 U4。書き出し先が今の`yamareco/`・`travel-journal/`で、受け取り側の経路を変えないため）。

資格情報は、`/etc/geoframe-export/drive-credentials.json`（**root 所有・0600**。`client_id`・`client_secret`・`refresh_token`の 3 つのキーだけの JSON）に置きます。
`/etc/geoframe-export`は root 所有の 0700 です。unit の`LoadCredential=`が、起動のたびに、サービスのユーザーだけが読める写し（`drive-credentials`）にして渡します。
アクセストークンはメモリだけに持ち、**バイナリは資格情報のファイルを書き換えません**（トークンを取り直してもファイルは変わりません）。

#### 専用の client_id を作る（初めてのとき・作り直すとき）

次の手順は rclone の公式の文書（[Making your own client_id](https://rclone.org/drive/#making-your-own-client-id)）に基づきます。
Google API Console の画面の項目名は変わることがあるので、公式の文書も合わせて見てください。既に専用の client_id があれば、1〜9 は要りません。

1. Google API Console（<https://console.developers.google.com/>）にログインします（Drive の持ち主と同じアカウントでなくてもかまいません）。
2. プロジェクトを作ります（既にあれば選びます）。
3. 「ENABLE APIS AND SERVICES」で「Google Drive API」を有効にします。
4. 左の「Credentials」を開きます（「Create credentials」ではありません）。
5. 同意画面が未設定なら、「CONFIGURE CONSENT SCREEN」→「Get started」を押します。アプリ名（例: `geoframe-export`）、サポートのメール、Audience は「External」、連絡先を入れて「Create」します。
   「Data Access」で scope `https://www.googleapis.com/auth/drive`を足して保存します（書き出しは Drive 全体の権限のままです。設計 U4）。
6. 「Audience」→「+ Add users」で、自分をテストユーザーに足します。
7. 「Overview」→「Create OAuth client」を押し、種類は「Desktop app」にして「Create」します。
8. client ID と client secret を控えます。**チャット・リポジトリ・コミット・ログには書きません。**
9. 「Audience」→「PUBLISH APP」で公開します。**「Testing」のままだと、許可（リフレッシュトークン）が 7 日で切れ**、そのたびに再認可が要ります。
   「PUBLISH APP」が押せないときは、「Branding」でホームページとプライバシーポリシーの URL、Authorized domains を入れます。
   個人で使う（100 人未満）なら審査は要りません。ただし、認可のときに「確認されていないアプリ」の警告が出るので、そのまま先へ進みます。

#### 初回: 今の rclone.conf から取り込む（rclone 版からの切り替え）

rclone 版が使っている`/var/lib/geoframe-export/rclone/rclone.conf`（専用の client_id で認可済み）から、`client_id`・`client_secret`・`refresh_token`を取り出して、資格情報のファイルを**1 回だけ**作ります。
Google の同意のやり直しはしません。値は画面にも標準出力・標準エラー・ログにも出ません（出るのは、書いたパスと「書いた」ことだけです）。

```sh
sudo install -d -o root -g root -m 0700 /etc/geoframe-export
sudo /usr/local/bin/geoframe-export import-credentials \
  --rclone-config /var/lib/geoframe-export/rclone/rclone.conf --remote gdrive \
  --out /etc/geoframe-export/drive-credentials.json
sudo ls -l /etc/geoframe-export/drive-credentials.json   # -rw------- root root（中身は表示しない）
```

- root で動かすので、`--rclone-config`には**絶対パス**を渡します。パスは、根（`/`）から 1 段ずつ、シンボリックリンクをたどらずに開いて確かめます。次のどれかに当たると断ります。
  - 相対パス、`..`・`.`・空の要素（`//`や末尾の`/`）を含む。
  - パスの途中に、シンボリックリンクがある。
  - パスの途中の各ディレクトリ（設定ファイルの親ディレクトリより上）の持ち主が、root でも設定ファイルの持ち主でもない。
  - パスの途中の各ディレクトリに、グループまたは他人の書き込み権限がある。ただし、root 所有でスティッキービットが付いたディレクトリ（`/tmp`など）は、書き込めても受け付けます。

  `rclone.conf`は、セクション（`--remote`の名前）に専用の`client_id`・`client_secret`と、`token`の`refresh_token`が入っていて、ファイルが 0600・親ディレクトリが 0700 である必要があります。持ち主は問いません（root が、サービスのユーザーの持ち物を読みます）。
- 出力先は、`O_EXCL`で作ります。**既にあるファイルは上書きしません**（終了コード 1）。出力先の親（`/etc/geoframe-export`）も作りません（無ければ終了コード 1）ので、先に上の`install -d`で作ります。
  作り直すときは、ファイルがあれば先に退避してから動かします（`sudo mv /etc/geoframe-export/drive-credentials.json /etc/geoframe-export/drive-credentials.json.old`。退避した写しも root の 0600 のまま、不要になったら消します）。ファイルが無い新規の配置では、退避は要りません。
- 読めない・形が違う・`refresh_token`が無いときは、何も作らずに終了コード 2 です。
- 資格情報のファイルは**一度作れば、サービスの起動のたびに読まれる**だけです。rclone の設定（`rclone.conf`）の更新とは連動しません。

rclone 版を動かしていない新しい機体で、`rclone.conf`が無いときは、次の「再認可」の手順で`rclone authorize`のトークンから一時的な`rclone.conf`を作って取り込みます
（この機体には`/etc/geoframe-export`も資格情報のファイルもまだ無いので、手順 3 で、取り込みの前に`/etc/geoframe-export`を作ります）。

#### 再認可（`drive_auth_invalid`・起動時の終了コード 2）

更新トークンが失効すると（`invalid_grant`。「PUBLISH APP」が済んでいない場合の 7 日の失効、Google アカウントでのアクセス権の取り消し、パスワードの変更など）、
起動時の検査が標準エラー（journal）に「**Drive の認可が失効しています。再認可が要ります（deploy/README.md の手順）**」と出して、**終了コード 2** で止まります（`failed`のまま。再起動のループにはなりません）。
ジョブの途中で失効したときは、そのジョブが`failed`（`drive_auth_invalid`）になります。変更系の途中で失効して結果が分からないときは、`blocked`になります。

手順（ブラウザのある別の機体と、このホストで行います。**値はチャット・リポジトリ・コミット・ログ・シェルの履歴に残しません**）:

1. ブラウザのある別の機体（rclone を入れたもの）で、**同じ専用の client_id・client_secret**（「専用の client_id を作る」の 8 で控えた値）を指定して認可します。
   ```sh
   rclone authorize "drive" "<client_id>" "<client_secret>"
   ```
   ブラウザが開くので、Drive の持ち主のアカウントで許可します（「確認されていないアプリ」の警告は、そのまま先へ進みます）。
   終わると、`Paste the following into your remote machine --->`と`<---End paste`の印の間に、トークン（JSON）が出ます。
   実行の前に、履歴へ残らない設定にします（bash なら`HISTCONTROL=ignorespace`にして、コマンドの先頭に空白を 1 つ置きます。zsh なら`setopt HIST_IGNORE_SPACE`）。実行のあとは、履歴に残っていないかを確かめ、画面とスクロールバックも消します。
2. このホストで、**一時的な`rclone.conf`**（0600）を、root の 0700 のディレクトリに作ります。トークンは、標準入力から貼り付けて（コマンドの引数にせず、履歴に残さず）書きます。
   ```sh
   sudo install -d -o root -g root -m 0700 /root/geoframe-reauth
   sudo sh -c 'umask 077; set -C; cat > /root/geoframe-reauth/rclone.conf'
   ```
   貼り付ける内容（最後に Enter を押して、Ctrl-D で終わります。`<…>`の所に、控えた値と、1 のトークン〔`{`から`}`まで、1 行〕を入れます）:
   ```ini
   [gdrive]
   type = drive
   client_id = <client_id>
   client_secret = <client_secret>
   scope = drive
   token = <1 のトークン>
   ```
3. 取り込みの前に、出力先の親を作り、既存の資格情報のファイルがあれば**退避します**（出力先は上書きされません）。新しい機体では`/etc/geoframe-export`が無いので、先に作ります（root 所有・0700。既にあれば、そのまま）。
   取り込みが失敗したら、退避したファイルを元の名前へ戻します（`sudo mv -f /etc/geoframe-export/drive-credentials.json.old /etc/geoframe-export/drive-credentials.json`。退避していなければ要りません）。
   ```sh
   sudo install -d -m 0700 -o root -g root /etc/geoframe-export
   if sudo test -e /etc/geoframe-export/drive-credentials.json; then
     sudo mv /etc/geoframe-export/drive-credentials.json /etc/geoframe-export/drive-credentials.json.old
   fi
   sudo /usr/local/bin/geoframe-export import-credentials \
     --rclone-config /root/geoframe-reauth/rclone.conf --remote gdrive \
     --out /etc/geoframe-export/drive-credentials.json
   sudo ls -l /etc/geoframe-export/drive-credentials.json   # -rw------- root root
   ```
4. サービスを再起動します（資格情報は起動のたびに読まれるので、動いているサービスは再起動するまで古い値のままです）。
   ```sh
   sudo systemctl reset-failed geoframe-export
   sudo systemctl restart geoframe-export
   journalctl -u geoframe-export -n 20   # 起動時の検査（トークンと 3 つのフォルダ）を通ったことを見る
   ```
   取り込みはできたのに、起動時の検査で失敗したとき（終了コード 2 のまま）は、journal で原因を見ます。**新しい資格情報が原因でない（フォルダの ID の誤りなど）なら**、原因を直してから起動し直します。
   新しい資格情報が原因で、退避した`.old`があるときは、元へ戻して起動し直します（`.old`が無い新規の機体では、新しいファイルを消して、手順 1 からやり直します）。
   ```sh
   sudo mv -f /etc/geoframe-export/drive-credentials.json.old /etc/geoframe-export/drive-credentials.json   # 退避していたときだけ
   sudo systemctl reset-failed geoframe-export
   sudo systemctl restart geoframe-export
   ```
5. **起動時の検査を通ったのを確かめてから**、一時ファイルと、退避した古い資格情報を消します（どちらも Drive 全体の権限の値です）。通らなかったときは、`.old`を消さずに原因を調べます。
   ```sh
   sudo rm -rf /root/geoframe-reauth
   sudo rm -f /etc/geoframe-export/drive-credentials.json.old
   ```
6. 認可の失効でジョブが途中で止まっていたら（`blocked`、または`running`のまま）、`recover`で結果を確かめます。
   - `running`のまま残ったジョブは、サービスの起動時（または次の書き出しの前）の復旧が、Drive と照らして片付けます。手順 4 の再起動のあとに、引数なしの`recover`で、`running`が残っていないことを確かめます。
   - `blocked`は、自動では片付きません。引数なしの`recover`が表示する、Drive の今の状態と動きを読み、Drive の画面で必要な片付けを済ませてから、`--resolve`で解除します。`--resolve`は、未解除の`blocked`だけを受け付けます（`running`など、ほかの状態のジョブは断られます）。`--resolve`は Drive に触れず、解除の日時を記録に書くだけです（設計 4.2 の復旧）。
   ```sh
   sudo /opt/geoframe/deploy/geoframe-export-admin.sh recover
   sudo /opt/geoframe/deploy/geoframe-export-admin.sh recover --resolve <job_id>   # blocked だけ。Drive の画面で片付けたあと
   ```

`rclone authorize`で作る`rclone.conf`の`token`の中のアクセストークンは約 1 時間で切れますが、取り込むのは`refresh_token`だけです。取り込んだあとは、この`rclone.conf`は要りません。

### 8.4 トークンと環境ファイル

呼び出し側（別 VM のエージェント）が使う Bearer トークンを作ります。専用ユーザーの持ち物で、モード`0600`、32 バイト以上です
（`geoframe-export`は、持ち主・モード・長さ・弱い値を起動時に検査して、満たさなければ起動しません。値は文書・ログに出しません）。

```sh
# 専用ユーザーとして、umask 077 で最初から 0600 で作る（root で作ってから chmod しない）。set -C で、既にあるファイルやリンクには書かない。値は画面に出さない
sudo -u geoframe-export sh -c 'umask 077; set -C; openssl rand -hex 32 > /var/lib/geoframe-export/token'
sudo ls -l /var/lib/geoframe-export/token   # -rw------- geoframe-export geoframe-export
```

root でトークンを作って`chmod`すると、専用ユーザーが書き換えられるディレクトリでは、シンボリックリンクにすり替えられてたどられるおそれがあります。
置き場所は unit から見える場所にします。**`/home`の下は、unit の`ProtectHome=yes`で見えません。** 状態ディレクトリの下の絶対パス（上の`/var/lib/geoframe-export/token`）にしてください。
呼び出し側には安全な経路で別に渡します。

tailnet の IP・許可する送信元・トークンのファイルのパス・**Drive のフォルダの ID** は、unit に書かず`/etc/default/geoframe-export`（`0600 root:root`。無ければ起動しない）に置きます。
drop-in で`ExecStart=`を上書きする形にはしません: CLI の包み（`geoframe-export-admin.sh`）が、unit と同じ設定の引数を使うためです。

| 変数 | 意味 | 例（文書用の予約アドレス） |
| --- | --- | --- |
| `GEOFRAME_EXPORT_LISTEN` | 待ち受ける tailnet の IP とポート。tailnet の帯（IPv4 は`100.64.0.0/10`、IPv6 は`fd7a:115c:a1e0::/48`）とループバック以外の IP、`0.0.0.0`・`::`は起動時に拒否される | `192.0.2.1:8090` |
| `GEOFRAME_EXPORT_TOKEN_FILE` | トークンのファイルのパス | `/var/lib/geoframe-export/token` |
| `GEOFRAME_EXPORT_ALLOW_ARGS` | 許可する送信元 IP を`--allow <IP>`の並びで書く（呼び出し側と、このホスト自身）。値に空白があるので二重引用符で囲む | `"--allow 192.0.2.10 --allow 192.0.2.11"` |
| `GEOFRAME_EXPORT_FOLDER_YAMARECO` | 宛先`yamareco`の親フォルダの Drive ID | （値は書かない） |
| `GEOFRAME_EXPORT_FOLDER_TRAVEL_JOURNAL` | 宛先`travel-journal`の親フォルダの Drive ID | （値は書かない） |
| `GEOFRAME_EXPORT_WORK_FOLDER` | 作業用フォルダ（中に`incoming/`・`replaced/`を作る）の Drive ID | （値は書かない） |
| `GEOFRAME_EXPORT_VERIFY_FOLDER_YAMARECO`・`…_VERIFY_FOLDER_TRAVEL_JOURNAL`・`…_VERIFY_WORK_FOLDER` | 検証用の入口（`geoframe-export-admin.sh --verify`。§8.8 の 4）が使う、検証用の同じ 3 つのフォルダの ID。サービスは読まない | （値は書かない） |

**新規の配置**で、ファイルがまだ無いときだけ、空のファイルを作ります（`install`は既にあるファイルを空にするので、無条件には実行しません）。中身は各自で書きます（上の変数）。

```sh
sudo test -e /etc/default/geoframe-export || sudo install -o root -g root -m 0600 /dev/null /etc/default/geoframe-export
sudoedit /etc/default/geoframe-export   # 値をコマンドの引数に書かず、シェルの履歴にも残さない
```

**rclone 版からの切り替え**では、ファイルが既にあります。**空にせず**、先に退避してから、Drive の ID の行（上の表の、フォルダの 3 つと検証用の 3 つの計 6 つの変数）を**足します**。既にある行は消しません。

```sh
sudo cp -p /etc/default/geoframe-export /etc/default/geoframe-export.bak   # 退避（持ち主・モードも保つ）
sudoedit /etc/default/geoframe-export                                      # 6 つの変数の行を足す
sudo ls -l /etc/default/geoframe-export                                    # -rw------- root root のままであること
```

足した行の形は、次の項のとおりです。古い版は新しい変数を読まないので、戻すときに行を消す必要はありません（`.bak`は、切り替えが落ち着いてから消します）。


- Drive のフォルダの ID は、Drive の画面でそのフォルダを開いたときの URL の`folders/`の後ろの文字列です（`https://drive.google.com/drive/folders/<ID>`）。
  `KEY=値`の形で**1 変数 1 行**、値は英数字・`_`・`-`だけ（引用符・空白・コメントは付けません。包みはこの形だけを読み、形が違うと断ります）。
- 3 つの ID は**別のフォルダ**で、どれも他のどれかの中にあってはいけません（書き出し先の親が作業用フォルダの中にある、など）。ショートカット・ゴミ箱の中のフォルダも使えません。
  資格情報の持ち主が読み書きできるフォルダを指します。起動時に Drive へ問い合わせて確かめ、違うと終了コード 2 になります（ID の値は出ません。引数名だけが出ます）。

systemd の展開では、`${GEOFRAME_EXPORT_LISTEN}`のような`${…}`は値が 1 つの引数になり、`$GEOFRAME_EXPORT_ALLOW_ARGS`は空白で分かれた 0 個以上の引数になります
（unit の`ExecStart`と`systemd.service(5)`）。どれかが空・未設定だと、`geoframe-export`が引数を断って終了コード 2 になり、`RestartPreventExitStatus=2`で再起動ループにはなりません。

### 8.5 Drive のフォルダ

書き出し先の親フォルダ（`yamareco/`・`travel-journal/`）とは別の、最上位のフォルダ（例: `geoframe-export-work`）を、Drive の画面で作ります（作業用フォルダ）。
受け取り側が一覧する親フォルダの下に、作業用のものを見せないためです。このフォルダは誰とも共有しません。`yamareco/`・`travel-journal/`も先に存在している必要があります。
フォルダの名前はサービスには関係ありません（ID で指します）。ID は§8.4 のとおり`/etc/default/geoframe-export`に書きます。作業用フォルダの中には、書き出しのたびに`incoming/`・`replaced/`が作られます。
検証用の書き出し（§8.8 の 4）には、別の作業用フォルダ（例: `geoframe-export-verify-work`）と、検証用の宛先の親フォルダ（例: `geoframe-export-verify`の中の`yamareco`・`travel-journal`）を、同じように作ります（本番のフォルダとは入れ子にしません）。

### 8.6 unit の有効化（ユーザーの立ち会いで）

1. `systemd-analyze verify /usr/local/lib/systemd/system/geoframe-export.service`（バイナリを置いたあとなら警告は出ません）。
2. CLI の包みで、まず**書かずに確認**します（§8.7 の`dry_run`。サービスの起動の前でも使えます）。包みの`systemd-run`の`-p`（`CapabilityBoundingSet=`の空の値など）を本物の`systemd-run`が受け付けるかは、試験がモックなので、この最初の実行で確かめます。
3. 起動します。`sudo systemctl daemon-reload && sudo systemctl enable --now geoframe-export`、確認は`systemctl is-active geoframe-export`と`journalctl -u geoframe-export -f`です。
   待ち受けは`/healthz`（認証なし。送信元の許可は効く）で確かめます。

#### 止める前の確認（切り替え・切り戻しの両方）

`sudo systemctl stop geoframe-export`は、サービスだけを止めます。CLI（`geoframe-export-admin.sh`が`systemd-run`で作る一時 unit `geoframe-export-cli-*`）は止まりません。
実行中のジョブ（`running`）や、解除していない`blocked`を残したまま、バイナリと unit を入れ替えないために、**入れ替えの前に**次の（a）〜（c）をこの順に済ませます。
確認には、**今動いている版**のバイナリと、それに合う`geoframe-export-admin.sh`を使います（切り替えなら、`/opt/geoframe`を更新する前。切り戻しなら、戻す前の今の版）。

- （a）新しい受付と CLI の起動を止めます。呼び出し側（別 VM のエージェント）に、書き出しの依頼を止めてもらいます。このホストで`geoframe-export-admin.sh`を使う人（自分を含む）にも、使わないよう伝えます。
  確かめたあとに新しい書き出しが始まると、確認が無効になります。入れ替えを終えて、起動時の検査と`/healthz`を確かめるまで、止めたままにします。
- （b）実行中のジョブを終わらせます。一時 unit が残っていないことを確かめます。

  ```sh
  systemctl list-units 'geoframe-export-*'   # geoframe-export-cli-*（検証用の入口の分も同じ名前）が出ないこと
  journalctl -u geoframe-export -n 20        # サービスが書き出しの最中でないこと
  ```

  一時 unit があれば、終わるのを待ちます。待てないときは`sudo systemctl stop <unit 名>`で止めます（飛んでいる Drive の要求の終わりを待ってから、後始末をして終わります）。止めたジョブは`running`のまま残るので、（c）で片付けます。
- （c）今動いている版の`recover`（引数なし。Drive も記録も変えません）で、`running`（未完了）と未解除の`blocked`が無いことを確かめます。

  ```sh
  sudo /opt/geoframe/deploy/geoframe-export-admin.sh recover; echo "exit=$?"
  ```

  「未完了のジョブも、blocked のジョブも、片付けの知らせが残っているジョブもありません。」と出て、終了コードが 0 なら、先へ進みます。
  - 終了コード 4（実行枠が取れない）は、書き出しが実行中です。（b）に戻ります。
  - `running`が表示されたら、今の版のサービスを再起動して（`sudo systemctl restart geoframe-export`）、起動時の復旧に Drive と照合させます（数分は`/healthz`が答えません）。そのあと、もう一度`recover`で確かめます。
  - 未解除の`blocked`が表示されたら、表示された Drive の今の状態を読み、Drive の画面で必要な片付けを済ませてから、`recover --resolve <job_id>`で解除します。解除してから、もう一度`recover`で確かめます。
  - 「知らせ」だけが表示された記録は、手作業の片付けの知らせです。入れ替えを止める条件ではありませんが、読んで、必要なら片付けます。

（a）〜（c）が済んだら、（d）サービスを止めて、バイナリと unit を入れ替えます。

**rclone 版から切り替えるとき**は、次の順に行います。**古い unit（`--rclone*`の引数）のまま新しいバイナリを動かさない**ため、サービスを止めてからバイナリを入れ替えます（`import-credentials`は、新しいバイナリがディスクにあれば、サービスが止まっていても動かせます）。

1. 上の（a）〜（c）を、**`/opt/geoframe`を更新する前に**済ませます（古い版のバイナリと、古い包みで確かめます）。
2. 今のバイナリと unit を`.prev`に残す（§8.2）。
3. `/opt/geoframe`を更新する（§5 の手順 2。更新の前の SHA を控える。unit・包み・この文書が新しくなる）。
4. `sudo systemctl stop geoframe-export`で止める（上の（d））。
5. 新しいバイナリと unit を置く（§8.2 の`install`）。資格情報を取り込む（§8.3。`/etc/geoframe-export`を先に作る）。`/etc/default/geoframe-export`は、退避してから、Drive のフォルダの 3 つの ID（と検証用の 3 つ）を**足す**（§8.4 の切り替えの手順、§8.5）。
6. `sudo systemctl daemon-reload && sudo systemctl start geoframe-export`で起動し、journal の起動時の検査（トークン・3 つのフォルダ）が通ったことと`/healthz`を確かめます。通ったら、呼び出し側に再開を伝えます。
7. §8.8 の確認へ進みます。**rclone 本体と`rclone.conf`（`/var/lib/geoframe-export/rclone/`）は、§8.8 の確認と切り戻しの練習が済み、しばらく様子を見るまで残します**（戻すときに前のバイナリが使います。撤去は別途）。

起動の最初に、止まっていたジョブの復旧（Drive の照合）を済ませてから待ち受けます。止まったジョブがあると、再起動のあと数分は`/healthz`も答えず、接続が拒否されます
（unit は`Type=simple`なので、systemd は待ちません）。呼び出し側は拒否されたら間隔を空けて再試行します。
停止（SIGTERM）では接続を最大 75 秒待って打ち切り、飛んでいる Drive の要求（1 つにつき最大 100 秒）の終わりを待って、アップロードのスレッドを全部止めて後始末を終えてから終わります（`TimeoutStopSec=180s`、`KillMode=mixed`）。
tailnet の IP が起動直後にまだ無いときの bind の失敗（終了コード 1）は 15 秒おきに再試行します（300 秒に 10 回まで）。

### 8.7 CLI（`geoframe-export-admin.sh`）

`geoframe-export`を root で直接動かさず（バイナリが断ります）、サービスと同じ専用ユーザー・設定・状態ディレクトリ・ハードニングで動かすには、`deploy/geoframe-export-admin.sh`を使います
（`geoframe-admin.sh`と同じ考え方で、`systemd-run`の一時サービス（`geoframe-export-cli-<日時>-<番号>-<乱数>`）で動かします。本番の入口は、サービスと同じ実行枠`export.lock`を取るので、書き出しの実行中は終了コード 4 になります。検証用の入口（`--verify`）は、本番とは別の状態ディレクトリ・別の実行枠・別の宛先の親フォルダと作業用フォルダ（Drive の ID は`/etc/default/geoframe-export`の`…_VERIFY_…`）で動かし、本番のロックも本番の記録も使いません。資格情報のファイルは本番と同じものです（バイナリはファイルを書き換えないので、検証用の写しは要りません）。
一時サービスの名前は呼び出しごとに変えます。固定の名前だと、実行中かどうかを調べてから作るまでの間に別の呼び出しが入ったとき、名前の衝突で失敗して終了コード 4 になりません。
実行中かどうかは`export.lock`に任せます）。

```sh
# 書かずに確認（dry_run）: 候補の数・フォルダ名・宛先の状態・既存のフォルダを管理しているか
sudo /opt/geoframe/deploy/geoframe-export-admin.sh run --from <YYYY-MM-DD> --destination yamareco --dry-run
# 書き出し（期間は --to。置き換えは --on-exists replace。終了コード 0 成功・0 枚 / 1 失敗 / 2 引数・設定 / 3 conflict / 4 実行中）
sudo /opt/geoframe/deploy/geoframe-export-admin.sh run --from <YYYY-MM-DD> --destination yamareco
# 止まったジョブの確認（Drive は変えない）と、blocked の解除（Drive の状態を確かめたあと）
sudo /opt/geoframe/deploy/geoframe-export-admin.sh recover
sudo /opt/geoframe/deploy/geoframe-export-admin.sh recover --resolve <job_id>
# 検証用の入口（--verify を最初の引数にする。別の状態ディレクトリ・宛先・作業用フォルダで動かす。Drive の準備は先に済ませる。§8.8 の 4）
sudo /opt/geoframe/deploy/geoframe-export-admin.sh --verify run --from <YYYY-MM-DD> --destination yamareco --dry-run
# 途中で止める（起動時に標準エラーへ出る一時サービスの名前を指定する。飛んでいる Drive の要求の終わりを待ち、アップロードを止めて後始末をしてから終わる。ジョブの記録は次の復旧が片付ける）
sudo systemctl stop geoframe-export-cli-<日時>-<番号>-<乱数>
```

一時サービスの名前を控えそこねたら、`systemctl list-units 'geoframe-export-cli-*'`で探します。
引数の中の`$`は`systemd-run`の`--expand-environment=no`で展開させず、そのまま`geoframe-export`へ渡します（systemd 254 以降のオプションです。それより古いと`systemd-run`が断ります）。
宛先の親フォルダ・作業用フォルダ（`--folder-yamareco`・`--folder-travel-journal`・`--work-folder`）と、資格情報・通信先（`--drive-credentials`・`--drive-endpoint`）は、包みでは指定できません（包みが断ります。理由は§8.8 の 4）。検証は`--verify`で行います。

`serve`は包みでは動かさず、サービスで動かします。unit の固定の引数（`--drive-credentials`・`--state-dir`・`--feed-base`）を変えるときは、
包みの同じ値も変えます。どちらもリポジトリ側で変えてコミットし、§5 の手順で本番へ入れます（本番の`/opt/geoframe`のファイルは直接編集しません）。`bash deploy/geoframe-export-admin-test.sh`が、unit と包みの食い違いを検査します。

### 8.8 配置の後の確認（PRD・設計 10 章）

起動の前後に、次を確かめて記録します（記録は公開リポジトリの外に置きます）。

1. 上りの帯域を測ります。転送時間の仮定（1 日の最多 322 枚・約 260MB を、上り 20Mbps で約 2 分、5Mbps で約 7 分。14 日の最多は約 580MB）と、
   Drive の操作の期限（写真の送信全体は`--drive-upload-timeout`の既定 45 分、一覧・フォルダの作成・移動・ゴミ箱へ移す・読み出しは`--drive-timeout`の既定 5 分。ジョブ全体は 60 分）を見直します。
2. 共有範囲を確かめて記録します（設計 U7）。`yamareco/`・`travel-journal/`・作業用のフォルダの共有設定を Drive の画面で確認します。
   「リンクを知っている全員」や、オーナーと受け取り側以外への共有があれば、配置を止めてオーナーに確かめます（manifest に正確な座標が入り、全写真が出るため）。
   このサービスは共有設定を変えません。公開したフォルダは親フォルダの共有を受け継ぎます。
3. 受け取り側が`lat`/`lon`の`null`と増えた枚数を扱えるか確かめます。位置の無い写真も出るので、受け取り側の検証と記事の作成が`null`で落ちないことを確かめます。
4. 旧 manifest のある日で比べます。検証は、本番の記録・ロック・宛先のどれにも触れない**検証用の入口**（`geoframe-export-admin.sh --verify`）で行います。
   本番の入口に宛先のフォルダだけを渡す形は使えません（包みが断ります）。それだと本番の状態ディレクトリを共有して、同じ日の本番の公開の記録（`published/`の下のその宛先のファイル）を検証用フォルダの ID で上書きしてしまい、
   以後の`replace`が`not_managed`になるためです。途中で止まれば、復旧が本番の宛先で Drive を照合することにもなります。

   | | 本番の入口 | 検証用の入口（`--verify`） |
   | --- | --- | --- |
   | 状態ディレクトリ（`jobs/`・`published/`・`export.lock`・作業領域） | `/var/lib/geoframe-export` | `/var/lib/geoframe-export-verify` |
   | `yamareco`の親フォルダ（Drive の ID を書く変数） | `GEOFRAME_EXPORT_FOLDER_YAMARECO` | `GEOFRAME_EXPORT_VERIFY_FOLDER_YAMARECO`（Drive 上は例えば`geoframe-export-verify/yamareco`） |
   | `travel-journal`の親フォルダ | `GEOFRAME_EXPORT_FOLDER_TRAVEL_JOURNAL` | `GEOFRAME_EXPORT_VERIFY_FOLDER_TRAVEL_JOURNAL`（例: `geoframe-export-verify/travel-journal`） |
   | 作業用フォルダ | `GEOFRAME_EXPORT_WORK_FOLDER` | `GEOFRAME_EXPORT_VERIFY_WORK_FOLDER`（例: `geoframe-export-verify-work`） |
   | 資格情報 | `/etc/geoframe-export/drive-credentials.json` | 同じファイル（バイナリは書き換えないので、写しは要りません） |

   - 準備（Drive）: Drive の画面で、共有せずに、最上位に検証用の親フォルダ（その中に`yamareco`と`travel-journal`）と検証用の作業用フォルダを作ります（§8.5）。
     本番の親フォルダ・作業用フォルダとは入れ子にしません。それぞれの ID を、`/etc/default/geoframe-export`の`…_VERIFY_…`の 3 変数に書きます（§8.4）。
     `--verify`の起動時の検査が照合するのは、**検証用の 3 つの ID の間だけ**です。検証用の変数に本番の ID を入れ間違えても、そのまま起動します（本番の宛先に書いてしまいます）。
     そこで、**最初の書き込みの前に**、本番用の 3 つと検証用の 3 つの計 6 つを、次のように見比べます。
     1. 6 つの変数が全部あり、値が全部違うこと（`6`と出ること。値は画面に出ません）。
        ```sh
        sudo grep -E '^GEOFRAME_EXPORT_(VERIFY_)?(FOLDER_(YAMARECO|TRAVEL_JOURNAL)|WORK_FOLDER)=' /etc/default/geoframe-export | cut -d= -f2 | sort -u | wc -l
        ```
     2. Drive の画面で、本番側の 3 つのフォルダと検証用の 3 つのフォルダを開き、URL の`folders/`の後ろが、`/etc/default/geoframe-export`の対応する変数の値であることを、1 つずつ確かめます
        （変数名とフォルダの名前を対応づけて見ます。値をチャットや記録に貼りません）。
     3. 本番側のフォルダが検証用のフォルダの中に、検証用のフォルダが本番側のフォルダの中にないこと（同じフォルダや入れ子がないこと）を、Drive の画面で親をたどって確かめます。
   - 検証用の状態ディレクトリは、一時サービスの`StateDirectory=geoframe-export-verify`が、専用ユーザーの持ち物・0700 で作ります（先に作る必要はありません）。
     検証用の入口は、本番の`export.lock`を取らず、本番の状態ディレクトリにも触れません。本番の書き出しと同時に動かしても、互いの記録・ロックは書き換わりません。
   - 旧 manifest と比べる手順は、次の 3 つを、この順に行います（ここに 1 か所だけ書きます）。
     1. 本番の入口で`--dry-run`します（`sudo /opt/geoframe/deploy/geoframe-export-admin.sh run --from <日付> --destination yamareco --dry-run`）。
        旧 manifest のある日を使います。Drive を読むだけ（一覧と読み出し）で、本番の記録・Drive は変わりません。
        宛先の状態（`target`・`existing_files`）と、書き出す枚数（`candidate_count`）を控えます。
        旧 manifest のフォルダは旧実装が書いたもので、公開の記録（`published/`の下のその宛先のファイル）が無いので、`managed`が`false`なのは正しい挙動です（この入口では`replace`しません）。
     2. 同じ日を`--verify`で書き出します（`sudo /opt/geoframe/deploy/geoframe-export-admin.sh --verify run --from <日付> --destination yamareco`）。
        書き出しは検証用の宛先（上の表）にだけ行い、本番の宛先には書きません。途中で止まったら、`sudo /opt/geoframe/deploy/geoframe-export-admin.sh --verify recover`で確かめます（本番の`recover`は使いません）。
     3. 比べます。検証用の宛先にできた`manifest.json`と旧 manifest を比べ、増えた枚数（フィード外の写真）以外の枚数・ファイル名・サイズが合うことを見ます。
        呼び出し側の実際の検証（サイズの突き合わせ）まで通ることも確かめます。あわせて、検証の前後で、本番の`published/`の下のファイルの更新時刻が変わらないことを確かめます
        （検証用の書き出しの記録は、検証用の状態ディレクトリにだけ残ります）。
   - **検証が終わったら、検証用の記録を消してかまいません**（本番の記録ではありません）。検証用の Drive のフォルダも、ゴミ箱へ移してかまいません。

     ```sh
     sudo rm -rf /var/lib/geoframe-export-verify   # 検証用の記録ごと消す場合
     ```

5. **トークンを取り直しても、資格情報のファイルが変わらないこと**を確かめます。アクセストークンは約 1 時間で切れ、取り直しはメモリの中だけで行います（ファイルは書き換えません）。
   `sudo stat -c '%y %U %a' /etc/geoframe-export/drive-credentials.json`の更新時刻・持ち主・モードを控え、書き出しの実行中か、1 時間を空けた 2 回目の実行のあとにもう一度見て、同じであること、
   journal に認可のエラー（`drive_auth_invalid`・401）が無いことを見ます。
6. unit の`LoadCredential=`と`ExecStart`の`--drive-credentials %d/drive-credentials`が、実際に展開されること（サービスが起動し、journal に資格情報を読めないという理由の終了が無いこと）を確かめます。`LoadCredential=`の写しは、所有者 root・グループ root・0440 で、サービスのユーザーには ACL で読み取りが付きます。バイナリはこの形（グループの読み取りは、グループが root のときだけ許す）を受け付けます。断られて終了コード 2 になるときは、journal の「資格情報のファイルを読めません」を見ます。
7. unit のハードニングの値を、API 版で測り直します。値は、rclone 版を動かしていた 2026-11-22 に本番で実測して決めたもので、API 版は送信を並列 4 で行い（multipart は 1 本につき、中身と本文で最大 16MiB 弱をメモリに持ちます。4 本で 64MiB 前後）、スレッドも増えます
   （`geoframe-export.service`のコメントにも同じ値と理由を残してあります）。14 日の最多（880 枚・690MB）の書き出しで、次を測ります。
   - `RestrictAddressFamilies=AF_INET AF_INET6`で足りること（名前解決が`AF_UNIX`・`AF_NETLINK`を要しないこと。rustls・ring を使うので、`MemoryDenyWriteExecute=true`も合わせて見ます）。
   - `MemoryMax=512M`・`TasksMax=64`で足りること（journal に OOM・タスク上限の記録が無く、書き出しが成功すること。測った値を控えて、足りなければ値を直してコミットします）。
     この値は、書き出す画像が表示用の大きさ（長辺 2560px）であることが前提です。派生の大きさを変えるときは、`MemoryMax`を測り直します。
     同じ値を`geoframe-export-admin.sh`にも入れてあります（食い違いは`geoframe-export-admin-test.sh`が検査します）。
8. **本物の Drive で確かめる点**（試験は偽の Drive の HTTP サーバーで行ったため、本物の挙動はここで初めて確かめます。検証用の入口（`--verify`）で行う）。
   - 起動時の検査（トークン・3 つのフォルダとその祖先）と、ジョブの始めの確認の所要時間。`--drive-timeout`の既定（5 分）で足りること。
   - 移動（公開・退避）で、フォルダの ID が保たれること（移したあとの ID の照合で`blocked`にならないこと）。
   - 宛先と同じ名前のフォームやショートカットが、`ambiguous_destination`として見えること。
   - `md5Checksum`が、Drive のバイナリのファイル（写真と`manifest.json`）に付くこと。その値が、書き出したファイルの MD5 と一致すること。フォルダ・Google のネイティブ文書には付かないこと
     （一式に無いファイルとして断られます）。
     作業領域のファイルはジョブの終わりに消えるので、書き出しのあとに`md5sum`では比べられません。成功のあとは、**検証用の状態ディレクトリの`published/`に保存されたファイルごとの MD5**を使います。
     `/var/lib/geoframe-export-verify/published/<種類>-<フォルダ名>.json`（例: `yamareco-<日付>.json`。専用ユーザーの 0700 なので`sudo`で読みます）の`entry.files[]`に、一式の全部（写真と`manifest.json`）の`path`と`md5`（小文字の 16 進 32 文字）があります。
     Drive の`md5Checksum`は、Drive の画面には出ません。Drive API の`files.get`（`fields=md5Checksum`。API Explorer でも読めます）で、検証用のフォルダの中のファイルごとに読み、`entry.files[]`の`md5`と照らします。
     画面からファイルをダウンロードして、その`md5sum`を`entry.files[]`の`md5`と照らす方法でも確かめられます。`md5sum -c`に使う一覧は、次のように作れます（ダウンロードしたファイルのあるディレクトリで）。
     ```sh
     sudo python3 -I -c 'import json,sys; [print(f["md5"] + "  " + f["path"]) for f in json.load(open(sys.argv[1]))["entry"]["files"]]' \
       /var/lib/geoframe-export-verify/published/<種類>-<フォルダ名>.json > md5.list
     md5sum -c md5.list
     ```
     `replace`の前の確認（`modified`）と、ゴミ箱へ移す前の確認（`retired_content_changed`）は、この MD5 に頼ります。
     書き出し直したフォルダに、Drive の画面で同じ名前・同じ大きさで中身だけ違うファイルを上書きしたときも、`--dry-run`の`managed`は`true`のままです
     （公開の記録にそのフォルダの ID と MD5 があり、manifest の`generator`・`destination`・`folder`が合っていれば、管理フォルダとして扱います）。`replace`は中身の MD5 の照合で`conflict`（`modified`）になり、終了コード 3 で断られます。
     フォルダの中に Google ドキュメントがある場合も、同じく`modified`で断られます。これらも見ます。
   - **rclone 版が公開したフォルダ**を、API 版が管理しているフォルダとして扱えること（本番の入口の`--dry-run`で、`managed`が`true`になること）。そのうえで、オーナーが選んだ日で`--on-exists replace`を 1 回行い、置き換えが通ることを確かめます。
   - 置き換えで退避した旧フォルダが、**Drive のゴミ箱へ移り、フォルダと中身ごと戻せる**こと（Drive の画面のゴミ箱で「復元」できること。完全な削除はしません）。
   - 429・403（流量制限）の実際の応答と、再試行の回数（journal の操作名・状態コード・再試行の回数だけが出ます。URL のクエリ・トークン・ファイル名は出ません）。
   - **8MiB を超えるファイル（resumable）を 1 つ**、本物の Drive へ送ること。本番の写真は 1 枚 1MB 前後で、通常の関門ではこの経路が通りません。チャンクの`PUT`に`Authorization`を付けない判断（セッションの URL が認可する）、
     `Content-Range`・308・`Range`の形、同じ ID での始め直しも、ここで本物で確かめます。
   - SIGTERM（`systemctl stop`）から 180 秒以内に止まること。書き出しの途中で止めたあと、`running`のまま残った記録は、次の起動時（または次の書き出しの前）の復旧が Drive と照らして片付けます。
     サービスを起動し直し、引数なしの`recover`で`running`が残っていないことを確かめます。`blocked`になった記録だけは自動では片付かないので、`recover`が表示する Drive の状態を読み、Drive の画面で片付けてから`recover --resolve <job_id>`で解除します（`--resolve`は未解除の`blocked`だけを受け付けます）。
9. 切り戻しの練習をします（§8.9）。前のバイナリと前の unit で起動できること、`/healthz`が答えることを、書き出しのない時間に確かめます。

### 8.9 切り戻し

API 版で問題が出たら、前のバイナリ（rclone 版）と前の unit に戻します。ジョブの記録と公開の記録の形式は変わらないので、前のバイナリが読めます。
**戻すには、§8.2 で`.prev`に残した 2 つと、rclone 本体（`/usr/local/bin/rclone`）・`rclone.conf`（`/var/lib/geoframe-export/rclone/`）が要ります**（切り替えのあとも、しばらく残します）。

**戻す前に、§8.6 の「止める前の確認」の（a）〜（c）を、API 版（今動いている版）で済ませます**。`systemctl stop`は CLI の一時 unit を止めず、実行中のジョブや未解除の`blocked`を残したまま入れ替えると、前のバイナリが片付けを引き受けることになるためです。
（a）呼び出し側の依頼と CLI の起動を止める、（b）実行中のジョブを終わらせ、`systemctl list-units 'geoframe-export-*'`で一時 unit が無いことを確かめる、（c）API 版の`recover`で`running`と未解除の`blocked`が無いことを確かめる、の順です。
そのあと、（d）サービスを止めて入れ替えます。

```sh
sudo systemctl stop geoframe-export
sudo cp -p /usr/local/bin/geoframe-export.prev /usr/local/bin/geoframe-export
sudo cp -p /usr/local/lib/systemd/system/geoframe-export.service.prev /usr/local/lib/systemd/system/geoframe-export.service
sudo systemctl daemon-reload
sudo systemctl start geoframe-export
systemctl is-active geoframe-export && curl --fail -s -o /dev/null -w '%{http_code}\n' http://<待ち受けの IP>:<ポート>/healthz
```

起動時の検査と`/healthz`が通ったら、呼び出し側に再開を伝えます。`/etc/default/geoframe-export`は、足した行を残したままで足ります（前の版は読みません）。

- 戻したあと、止まっていたジョブは起動時の復旧が片付けます（`recover`で確かめられます）。`blocked`があれば、§8.10 のとおり Drive の状態を確かめて解除します。
- CLI の包み（`geoframe-export-admin.sh`）は、`/opt/geoframe`の checkout に含まれる**新しい版の引数**（`--drive-credentials`・`--folder-*`）を使うので、前のバイナリには使えません。
  前のバイナリで CLI を使うときは、§5 の手順 2 の最初の`rev-parse`で控えた SHA へ、§6 の`git reset --hard`で checkout を戻してから使います（このとき、unit・包み・この文書の§8 も前の版になります）。サービス（`serve`）だけを戻すなら、unit の入れ替えで足ります。
- rclone 本体と`rclone.conf`の撤去は、切り替えが落ち着いてから別に行います（この文書では扱いません）。

### 8.10 トラブルシュート

- 起動しない（終了コード 2、`failed`のまま止まる）: `journalctl -u geoframe-export`に、検査に通らなかった項目の名前（`--listen`・`--token-file`・`--drive-credentials`・`--folder-yamareco`・`--folder-travel-journal`・`--work-folder`など）が出ます（値は出ません）。
  Drive の資格情報のファイルを読めない（持ち主・権限・形）、フォルダの ID の誤り（フォルダでない・ゴミ箱の中・権限が無い・3 つが入れ子）なども、ここで分かります。
  `Drive の認可が失効しています。再認可が要ります`と出ていれば、§8.3 の「再認可」の手順です。
  直したあと、`sudo systemctl reset-failed geoframe-export`で失敗の印を消し、`sudo systemctl start geoframe-export`で起動します。
- 起動しない（終了コード 1）: Drive に接続できない（通信の失敗・5xx・期限）、tailnet の IP がまだ無い、など、再試行で直りうる失敗です。15 秒おきに再起動します（300 秒に 10 回まで）。
- ジョブが`drive_auth_invalid`で失敗する: 更新トークンが失効しています。§8.3 の「再認可」を行い、サービスを再起動します。
- 接続が拒否される・`/healthz`が答えない: 起動時の復旧の最中です（§8.6）。journal を見て、数分待ちます。
- `blocked`（次の書き出しがすべて 503）: Drive の状態を`recover`で確かめ、Drive の画面で片付けてから`recover --resolve <job_id>`で解除します（設計 4.2 の復旧）。

## トラブルシュート

- `status=203/EXEC`（`geoframe-rs`）: `/usr/local/bin/geoframe`が無い、または実行ビットが無い。`rust-deploy.sh`で入れ直す（§7.3）。
  `geoframe-backup.service`が`203/EXEC`なら、`/opt/geoframe/deploy/backup-catalog.sh`の実行ビットが消えている（checkoutの一律`chmod`をしない。§5）。
- `geoframe-rs`が`failed`で止まる: `journalctl -u geoframe-rs.service`で理由を見る。終了コード2は再起動しても直らない
  ものです（設定の不正、rootでの実行。`RestartPreventExitStatus=2`で止まる）。
  終了コード1（DBが開けない）は再起動の対象で、`StartLimitBurst=`まで再試行して止まります。
  直したあとは`systemctl reset-failed geoframe-rs`のあとで起動します（§7.1）。`/etc/default/geoframe-rs`が無いと
  unitは起動しません（§2）。
- `fatal: detected dubious ownership in repository at '/opt/geoframe'`: リポジトリの所有者と
  gitを実行しているユーザーが一致していないときに出ます。`/opt/geoframe`は`root:root`所有、
  `git`の操作は`root`実行が前提です（§1.2）。まず`ls -ld /opt/geoframe`で所有者を確認し、
  意図せず変わっていれば`sudo chown -R root:root /opt/geoframe`で戻してください
  （所有者を直すのが本筋です）。所有者が正しいのに出る場合だけ、例外として登録します。
  **`--system`（マシン全体・全ユーザーに適用）ではなく`--global`（root専用の
  `/root/.gitconfig`にのみ書き込む）を使ってください。** `--system`にすると、
  `geoframe`ユーザーが何らかの理由で所有者に戻った場合でも例外が効いたままになり、
  root実行の`git fetch`が`geoframe`所有下の`.git/hooks/*`や`core.sshCommand`を
  root権限で実行してしまう経路が残ります。

  ```sh
  sudo git config --global --add safe.directory /opt/geoframe
  ```

- `git@github.com: Permission denied (publickey).`: Deploy Keyが使われていません。
  `core.sshCommand`が設定されているか（`sudo git -C /opt/geoframe config --local --get core.sshCommand`）、
  鍵が読めるか、GitHub側にDeploy Keyが登録されているかを順に確認します。疎通は次で確認できます。

  ```sh
  sudo ssh -i /etc/geoframe/ssh/geoframe_deploy_key \
    -o IdentitiesOnly=yes -o UserKnownHostsFile=/etc/geoframe/ssh/known_hosts \
    -T git@github.com
  ```

  成功すると`Hi ysogabe/geoframe! You've successfully authenticated, but GitHub does not provide shell access.`
  のように表示されます（シェルが得られないのは正常です）。`Host key verification failed`の
  場合は`known_hosts`の登録（§1.1）をやり直してください。
- 管理ページに古いJavaScriptが表示される: ブラウザをハードリロードします。
