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
> まで存在します）。本番の戻しの猶予期間のあいだ、本番の`/opt/geoframe`はそのコミットに固定されたままで、
> Python版へ戻す手順は[`docs/rust-migration/cutover.md`](../docs/rust-migration/cutover.md)にあります。

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

リポジトリはPrivateのため、読み取り専用のDeploy Keyで認証します。鍵は
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

> [!IMPORTANT]
> 本番の切替の直後は、**戻しの猶予期間**です。このあいだ本番の`/opt/geoframe`はP5aのマージのコミットに
> 固定されていて（`/etc/geoframe/checkout-pinned`）、この手順の2（checkoutの更新）はしません。
> 期間が終わってからの片付けと、新しいmainへの更新は
> [`cutover.md` §13](../docs/rust-migration/cutover.md)の手順に従ってください。

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

Python版へ戻す手順（猶予期間中の数分で戻す・片付けのあとに戻す）は、[`cutover.md` §8・§13.2](../docs/rust-migration/cutover.md)にあります。

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
| [`../tools/miharashi-feed-check/`](../tools/miharashi-feed-check/README.md) | miharashi の取得・読み込みのコードを、画面なしで Rust 版に向けて動かす確認用ハーネス |

### 7.1 unit の要点

- バイナリは`/usr/local/bin/geoframe`、`geoframe serve`をそのまま起動する（`app/`も`.venv`も要らない）。
- **unit の本体は`/usr/local/lib/systemd/system/geoframe-rs.service`に置く**（`rust-deploy.sh`が入れる。猶予期間中の本番では、Python 版も切替の手順で同じ場所へ移してある）。
  `/etc/systemd/system/`には、`systemctl mask`のリンクと drop-in（`*.service.d/`）だけを置く（mask は`/etc/systemd/system/<unit>`に`/dev/null`へのリンクを作るので、
  そこに本体のファイルがあると失敗する）。`/etc/systemd/system/geoframe-rs.service`に古い本体があると、systemd はそちらを優先するので、`rust-deploy.sh`は断る（先に退避する）。
- `User=geoframe`、`StateDirectory=geoframe`（`/var/lib/geoframe`）、`EnvironmentFile=-/etc/default/geoframe`（Python 版と共通）のあとに
  `EnvironmentFile=/etc/default/geoframe-rs`（Rust だけ。`-`無し: 無ければ起動しない）。**`GEOFRAME_DATA_DIR`・`GEOFRAME_DRIVE_KEY`・`TZ`は unit に書かず、`/etc/default/geoframe-rs`に置く**
  （`0600 root:root`。名前と意味だけを文書に書き、値は書かない）。unit と`geoframe-admin.sh`が同じファイルを読むので、同じ DATA_DIR を指す。
  `EnvironmentFile=`は、unit に書いた順に関係なく`Environment=`に勝ち、`EnvironmentFile=`どうしは後に書いたものが勝つ（同じ名前を両方に置かない）。
- `TimeoutStopSec=90s`: 停止（SIGTERM）はHTTP・同期・AI・キャッシュ保存・runtimeの停止を合わせて受信から60秒の共通期限で打ち切る（[server.md §22](../docs/rust-migration/server.md)）。unitは期限のあとの余裕を含めて90秒待ち、終わらなければSIGKILLする（実行中のAIジョブは次の起動が回収する）。
- `RestartPreventExitStatus=2`: 設定不正（終了コード 2）は再起動しても直らないので、再起動ループにせず`failed`で止める。
  DB が開けない（終了コード 1）は再起動対象。`LimitNOFILE=4096`（同時の接続 256＋配信 32＋SQLite で 300 前後を使う）。
- **取り込み（DB への書き込みと同期サイクル）は`GEOFRAME_INGEST=1`のときだけ動く**（既定は`0`）。Python 版と共通の
  `/etc/default/geoframe`をそのまま使って Rust 版を起動しても DB には書かない。取り込みを使うときは、`/etc/default/geoframe-rs`（検証環境では drop-in でもよい）で`GEOFRAME_INGEST=1`を足す。
  **書き込みの前に、root で実行している、または`geoframe-server`が非稼働と確認できないときは、書かずに終了コード 2 で断る**（状態を取得できないときだけ終了コード 1 で、unit の`Restart=on-failure`が上限（`StartLimitBurst=`）まで再試行する）。
  **書き込み系の CLI（`sync`・`organize`・`quality-scan`・`ai-jobs retry|release-pause|deactivate-space`）は、`DATA_DIR`に`catalog.db`が無ければ作らずに終了コード 2 で断る**（`serve`は`GEOFRAME_INGEST=1`なら空の DB を作る）。
  検証環境の印は、バイナリの検査としてはもう見ない（`rust-deploy.sh`の検証環境の経路だけが見る）。**root で直接動かさず**、サービスと同じユーザー・設定で動かすには`deploy/geoframe-admin.sh`を使う。
  `ActiveState`だけを見る: unit ファイルが無くても動いているサービスは断る（詳細は[server.md §11.1.1](../docs/rust-migration/server.md)）。
  有効にすると、`serve`は DB が無ければ新しく作り、スキーマの作成・移行をして、
  `GEOFRAME_DRIVE_POLL_MIN`（既定 10 分）ごとに同期サイクルを回す（[server.md §11](../docs/rust-migration/server.md)）。**Python 版と同じ`catalog.db`には絶対に向けない**。
- `Conflicts=geoframe-server.service`と`After=geoframe-server.service`: 取り込みのガードは Python の非稼働を**起動時に 1 回しか**確かめないので、その後に Python が起動されたときは systemd が Rust を止める（Rust を起動すれば Python が止まる）。
  `Conflicts=`で相手の停止と自分の起動が同じ処理に入ったとき、systemd は順序の向きにかかわらず停止を起動より先に済ませるので、`After=`と合わせて、旧プロセスが終わってから新しいプロセスが起動する。
  `geoframe-server.service`が無い機体では何も起きない。反映の確認は`systemctl show geoframe-rs -p Conflicts -p After`。止める側は disable でなく mask する（切替の手順は`cutover.md`）。
- `StartLimitIntervalSec=120`・`StartLimitBurst=5`: Python 版の状態を取得できない（終了コード 1）などの失敗を 5 秒おきに再試行するが、120 秒に 5 回で止める（止まったら`systemctl reset-failed geoframe-rs`のあとで起動する）。
- 制限（`geoframe-server.service`にはない、Rust 版だけ付けられるもの）: `CapabilityBoundingSet=`（空）、`RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`
  （`AF_UNIX`は Python 版の検査の`systemctl show`に要る）、`ProtectKernelTunables`・`ProtectKernelModules`・`ProtectKernelLogs`・`ProtectControlGroups`、`RestrictNamespaces`、
  `LockPersonality`、`RestrictSUIDSGID`、`PrivateDevices`、`SystemCallArchitectures=native`、`UMask=0027`（作るファイルは所有者とグループだけが読める）。
- `ReadOnlyPaths=`は、`photos/`・`drive_key.json`・`models/`だけ（writer が書く`catalog.db`・`derivatives/`は P2 で、旅行ブログの保存・書き出しが書く`travel_blog/`・`blog_exports/`は P3 / T3.6 で外した。`tmp/`・`.cache.json`・`.ingest.lock`は元から対象外）。
  WAL の DB は読み取りでも同じディレクトリに`-shm`/`-wal`を作るので、ディレクトリ自体は書き込み可のまま。
  **`GEOFRAME_INGEST=0`（既定）の間は、旅行ブログの保存・書き出し（`POST /api/blog/save`・`export`）は書かずに 503 を返す**ので、外しても下書き・書き出しは変わらない（切替の前に下書きを書いていたのは Python 版。書き出しの `dry_run` だけは何も書かないので答える）。取り込みを有効にした検証環境では、Rust も`travel_blog/drafts/`と`blog_exports/`に書く（検証環境で Python と同じ`DATA_DIR`を使うときは、Python と Rust が同じ下書きを書きうる。比較用 Python には別の`DATA_DIR`を使う）。
- 地図タイル（`/tiles/`）と地名検索（`/api/admin/geocode`）は外向きに通信する。OSM のタイルは`GEOFRAME_TILE_UA`（連絡先入り。未設定なら既定値）、地名検索（地理院・Nominatim）も同じ`GEOFRAME_TILE_UA`を User-Agent に送る（Python は地名検索に固定の`GeoFrame/0.2`）。地理院・OSM のタイル・Nominatim の実通信は、検証環境で確認済み（P3。OSM と Nominatim は連絡先入りの`GEOFRAME_TILE_UA`を足した2026-10-25。[plan.md](../docs/rust-migration/plan.md)・[drive-acceptance.md](../docs/rust-migration/drive-acceptance.md) §6）。タイルのキャッシュ（`tile_cache/`）は`GEOFRAME_INGEST=0`でも書く（`catalog.db`ではなく`DATA_DIR`の下）。
- 旅行ブログの旅程 JSON（`web/travel_blog/plans/*.json`）は**バイナリに埋め込まれる**（実行時に`web/`を読まない）。旅程を足す・直すのは再ビルド・再配布。再ビルドなしで差し替えるときだけ、`GEOFRAME_BLOG_PLANS_DIR`にディレクトリを与える（Rust だけの項目。そのディレクトリの`*.json`だけを読み、埋め込みは使わない。[server.md §20.1](../docs/rust-migration/server.md)）。下書きと書き出しは`DATA_DIR`の下（`travel_blog/drafts/`・`blog_exports/`）で、読み取りの API（`/api/blog/trips`・`trip`・`candidates`・`draft`）は`GEOFRAME_INGEST=0`でも動く。
  `catalog.db`を別の inode に置き換えたら（バックアップからの復元など）再起動する。
- 取り込みを 1 回だけ手で動かす: `geoframe sync`（`GEOFRAME_INGEST=1`が要る）。**書かずに計画だけ**見るなら`geoframe sync --dry-run`（行・写真・キャッシュ・ロックを変えず、件数だけを出す。ただし SQLite が読み取りでも作る`catalog.db-wal`・`-shm`はできうる。Python 版の確認も要らない。ガードを通さないのは書かないからで、本番機で実行してよいという意味ではない: 向けるのは検証環境の本番コピー（そのバックアップのコピー）だけで、本番の`catalog.db`には向けない。docs/rust-migration/plan.md §2）。**Drive の鍵（`GEOFRAME_DRIVE_KEY`）がある機体では、`--dry-run`も Drive の一覧を読みに接続する**（原本のダウンロードも書き込みもしない。鍵が無ければ通信しない）。通信を避けたいときは、**存在しない鍵ファイルの絶対パスを`GEOFRAME_DRIVE_KEY`に明示する**（例: `GEOFRAME_DRIVE_KEY=/nonexistent/drive_key.json geoframe sync --dry-run`）。`--data-dir`を変えても鍵の設定は変わらず、`GEOFRAME_DRIVE_KEY`を空文字にしても既定の`drive_key.json`（カレントディレクトリ基準）に戻るので、どちらも通信を避けられない。Driveの一覧が空、または消す行が多すぎる（50件超かつDriveの行の20%超）ときは、削除を保留して件数だけ警告する。意図した削除は`geoframe sync --allow-mass-delete`の1回だけで外れる（`--dry-run`と併せて件数を見られる。`serve`の定期実行では外せない）。位置の推定（`infer`）と地名（`enrich`）の段は、`--dry-run`でも書かず、**`enrich`は地理院に問い合わせない**（件数だけ。`infer`は推定・取り消しの件数、`enrich`は国外・キャッシュで済む・問い合わせるはずの件数）。取り込み済みの本番コピーでは、`infer`の`inferred`・`updated`・`cancelled`と`enrich`の`candidates`がほぼ0になる。実行では`enrich`が地理院へ問い合わせる（1サイクル40件まで、通信を試した行ごとに0.2秒待ち。件数は`queried`＝試した行、`resolved`＝うち確定した行）。通信の障害（切断・時間切れ・429・5xx）は短い待ちで2回まで取り直し、接続できない・429はすぐ段を中断して失敗にする。5xx・時間切れ・切断は、その座標を30分以上飛ばして次へ進み、続けて3つの座標で起きたら中断する。片方のAPI（逆ジオか標高）だけが障害のあいだは、その座標を確定しない（Pythonは片方の応答だけで確定する）。
- **Drive の大量削除の保留（T3.7）**: 一覧から消える行が 50 件を超えかつ Drive の行の 20% を超える、または一覧が空のとき、`index_drive`は削除を保留する。**取り込みが有効な`serve`は、保留中だけ`/api/admin/status`の`sync.mass_delete_hold`に件数・理由・開始時刻を出し、管理画面の STATUS タブに赤い注意を出す**（取り込み無効のときと最初のサイクルの前は出ない。記憶はプロセスの中だけで、再起動後の最初のサイクルでまた付く）。意図した削除なら、`serve`を止めて（`.ingest.lock`が二重の取り込みを断る）`geoframe sync --allow-mass-delete`を 1 回実行し、`serve`を起動し直す。**画面からは解除できない**。手動で直した位置（`loc_source='manual'`）は、Drive の`md5`・ローカルの`mtime`が変わっても書き換わらない（T3.7）。詳細は[server.md §10.6](../docs/rust-migration/server.md)。
- 品質（`quality`の段。孤児の特徴量を消す→未解析を`GEOFRAME_QUALITY_PER_CYCLE`枚（既定は`GEOFRAME_FETCH_PER_CYCLE`）まで解析→未解析が無くて`photo_quality_dirty=1`のときだけ全写真を再グルーピングして`photo_quality`を全差し替え）は同期サイクルの最後に動く。**`--dry-run`は解析せず**、消す孤児・未解析・今回解析する枚数・再構築をするかを件数で出す（取り込み済みの本番コピーでは全部ほぼ0。Python が作った特徴量の行も有効として使うので、Rust に切り替えても全写真を解析し直さない。Python へ切り戻したときの扱い（`algorithm`は結果を作った実装の印で、Python は読まない）は[server.md §16.10](../docs/rust-migration/server.md)。[server.md §16](../docs/rust-migration/server.md)）。一括解析（現行の`server.py --quality-scan`）は`geoframe quality-scan [--dry-run] [--data-dir <dir>]`（`GEOFRAME_INGEST=1`と、`sync`と同じ印・Python の確認が要る）。
  外向き HTTPS の確認は`geoframe selfcheck-net`（状態と HTTP ステータスだけを出す）。
- **AIの背景ワーカー（P4 / T4.1c）**: `GEOFRAME_INGEST=1`かつ`GEOFRAME_AI_ENABLED=1`かつ有効な背景用途（`GEOFRAME_AI_CONFIG`のJSON。[ai-config.md](../docs/rust-migration/ai-config.md)）があるときだけ起動する。**いまの配布バイナリは用途別の処理を登録していない**ので、ジョブを1件も実行しない（実処理はT4.2・T4.4・T4.6）。`INGEST=0`ではAIの表を作らず、回収もしない。AIの設定が不正でも写真の配信・管理は続き、STATUSタブに設定の誤りの固定コードが出る。
  状態の確認とやり直しは`geoframe ai-jobs`: `status`（読み取りだけ。稼働中でも可。件数と固定の理由コードだけ）、`retry (--kind <種類>|--all)`（失敗だけを再検査。外部へ送らない。検査できない種類は保持して件数だけ報告）、`release-pause (--task <用途>|--all)`（認証・契約の不一致で止まったProvider・用途の明示解除）。`retry`と`release-pause`は書くので、**サービスを止めてから**`GEOFRAME_INGEST=1`で実行する（`sync`と同じ印・Pythonの確認を通す。稼働中は`.ingest.lock`で断る）。
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

**検証環境の印（誤配布の防止）。** 変更の前に、接続先に次の 2 つを確かめ、満たさなければ何も変更せずに終了コード 4 で拒否する（`--rollback`も同じ）。

- 検証環境の印`/etc/geoframe/staging`（通常ファイル。シンボリックリンクは不可）があること。**検証環境で一度だけ作る**（本番機には作らない）:
  `sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging`（`/etc/geoframe/`は本番機にも SSH の鍵置き場として存在するので、ディレクトリではなくファイルを見る）。
- Python 版の`geoframe-server.service`が稼働中でないこと（稼働中なら本番機の可能性が高い）。`systemctl show`の`ActiveState`が`inactive`／`failed`のときだけ通し、`LoadState`は見ない（unit ファイルが無い`not-found`でも、動いているサービスは動き続けうるので、`not-found/active`も拒否する。Python が入っていない機体は`not-found/inactive`で通る）。`LoadState=`・`ActiveState=`はそれぞれちょうど1行で値が空でないときだけ受け入れ、重複（同じ値の重複も）は拒否する（先頭の行だけを使うと矛盾した出力を取り違えるため）。

検証環境の経路（`--target`）は、印の無い機体には配らない。unit の本体は`/usr/local/lib/systemd/system/geoframe-rs.service`へ入れ、
`/etc/systemd/system/geoframe-rs.service`に古い本体のファイルがあれば（systemd がそちらを優先するので）何も変更せずに断る（先に退避する。mask のリンクは本体ではないので通る）。

**本番へ（`--production`）。** 本番の切替の手順（[`cutover.md`](../docs/rust-migration/cutover.md)）の中で、ユーザーの立ち会いのもとでだけ使う。段取り・終了コードごとの対処・戻し方は手順書にあり、ここには引数と拒否の条件を書く。
ssh を使わず、**その機体で root として直接**動く（ビルドもしない）。`sudo`は環境変数を消すので、値はすべて引数で渡す。実行場所は本番の checkout（`/opt/geoframe`）。
本番の経路は`GEOFRAME_RS_*`（試験用の差し替え・配置先・確認の URL・待ち秒数）を**一切使わない**。配置先（`/usr/local/bin/geoframe`）・確認の URL（`http://127.0.0.1:8080`）・unit の置き場所・
印・設定のファイル・root の UID は、スクリプトの中の固定値（`PROD_*`）で、環境が 1 つでも`GEOFRAME_RS_*`を持っていれば終了コード 4 で断る（`sudo -E`などで環境を引き継いでも検査を外せない）。
モックの試験は、一時コピーの`PROD_*`の行を書き換える（`geoframe-admin-test.sh`と同じ方式）。

```sh
# 変更の前の検査だけ（Python 版を止める前に。Python 版の状態の検査は除く）
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
Python 版（`geoframe-server.service`）が稼働中か状態を取得できない（`--check-only`では除く）／`geoframe-rs`が mask されている（切り替え直すときは先に`unmask`）／
古い置き場所（`/etc/systemd/system/geoframe-rs.service`）に本体がある／`/etc/default/geoframe-rs`が通常のファイルとして無い・モードが`0600`でない・所有者が root でない（中身は読まない）／
checkout の`HEAD`が`--expect-commit`（40 桁）と違う／追跡下のファイルに変更がある（未追跡の`app/.venv`・`.env`・鍵は見ない。root で動くので、この checkout だけ`safe.directory`にして「dubious ownership」で落ちない）／
`--bin`の写し（root 所有の一時ディレクトリへ写してから`sha256`を取る。入れるのはその写し）が`--expect-sha256`と違う。
配布のあと`/api/v1/photos`の件数が`--expect-min-photos`より少なければ、DATA_DIR の誤りなどとして失敗にし、配布前の状態へ戻す。
数の引数は桁を絞る: `--expect-min-photos`は 1 以上 999999999 以下（9 桁まで）、`--wait-secs`は 0〜3600。範囲外・数でない値は終了コード 2 で断る（Bash の整数比較は大きすぎる値で異常終了し、
`if`の中では「下限を満たした」ように進んでしまうため）。確認の本体も、比較が成り立つときだけ通し、比較の異常は失敗として戻す。Python 版を止めたあとの初回の配布が 0 でなければ、
すぐ Python 版へ戻す（手順は`cutover.md` §8）。ただし終了コード 2・4（何も変えずに断った）は、原因がすぐ直せて配信停止の期限内に再実行できるときだけ再実行する（`cutover.md` §0.4）。入れる unit は、その checkout の`deploy/geoframe-rs.service`。

サービスを止めて CLI を動かすとき（AI の書き込み系の操作など）は、`sudo deploy/geoframe-admin.sh ai-jobs …`を使う（root で直接`geoframe`を動かさない。動かす前に DATA_DIR のパスが表示される）。
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

## トラブルシュート

- `status=203/EXEC`（`geoframe-rs`）: `/usr/local/bin/geoframe`が無い、または実行ビットが無い。`rust-deploy.sh`で入れ直す（§7.3）。
  `geoframe-backup.service`が`203/EXEC`なら、`/opt/geoframe/deploy/backup-catalog.sh`の実行ビットが消えている（checkoutの一律`chmod`をしない。§5）。
- `geoframe-rs`が`failed`で止まる: `journalctl -u geoframe-rs.service`で理由を見る。終了コード2は再起動しても直らない
  ものです（設定の不正、rootでの実行、Python版（`geoframe-server`）の稼働中。`RestartPreventExitStatus=2`で止まる）。
  終了コード1（DBが開けない、Python版の状態を取得できない）は再起動の対象で、`StartLimitBurst=`まで再試行して止まります。
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
