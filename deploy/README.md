# GEOFRAMEのデプロイ

このディレクトリには、Linuxサーバー向けのsystemd unitと運用スクリプトの構成例があります。
アプリケーションを`/opt/geoframe`へ配置し、変更されるデータを`/var/lib/geoframe`へ保存し、
権限を制限した`geoframe`ユーザーでサービスを実行する構成を想定しています。

> [!WARNING]
> GEOFRAMEには利用者認証がありません。localhostへバインドするか、信頼できるLAN内だけで
> 使用してください。ポート8080をインターネットへ直接公開しないでください。

## 前提条件

- Python 3.10以降を利用できるDebianまたはUbuntu系Linux
- `python3-venv`、`sqlite3`、`git`、`curl`
- 同梱unitを使う場合は`systemd`
- 同じ機体を表示端末として使う場合だけChromium
- サーバーからgithub.comへのSSH到達性（TCP 22）。コードは本機がgitで取得します（§6参照）

## 1. アプリケーションの配置

ログインシェルとホームディレクトリを持たないシステムユーザーを作ります。

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin geoframe
sudo install -d -o root -g root -m 0755 /opt/geoframe
```

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
  git clone git@github.com:ysogabe/geoframe-ambient-display.git /opt/geoframe
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
（ロールバックの手段を残すため。§7参照）。

> [!NOTE]
> `git clone`は空でないディレクトリに対しては失敗します。rsync方式で運用していた
> 既存機を移行する場合は、`/opt/geoframe`に`app/.venv`などが既にあるため、
> cloneではなく既存ディレクトリをそのままgit管理下に置いてください。この手順なら
> 未追跡の仮想環境を作り直さずに済みます。
>
> ```sh
> cd /opt/geoframe
> sudo git init
> sudo git remote add origin git@github.com:ysogabe/geoframe-ambient-display.git
> # 上記の core.sshCommand / gc.auto を設定してから
> sudo git fetch origin
> sudo git reset --hard origin/main
> ```

所有者は`root:root`に揃えます。**更新スクリプトは`root`で動くため、リポジトリの
所有者を`geoframe`などに変えないでください**（`dubious ownership`でgitが停止します。
トラブルシュート参照）。

```sh
sudo chown -R root:root /opt/geoframe
sudo find /opt/geoframe -path /opt/geoframe/.git -prune -o -type d -exec chmod 0755 {} +
```

仮想環境を作成します。`.venv`はgitの管理外（`.gitignore`済み）なので、更新時に
作り直されることはありません。

```sh
sudo python3 -m venv /opt/geoframe/app/.venv
sudo /opt/geoframe/app/.venv/bin/pip install -r /opt/geoframe/app/requirements.txt
```

## 2. データと秘密情報の設定

同梱のサーバーunitは`StateDirectory=geoframe`により`/var/lib/geoframe`を作成します。
変更されるデータと、任意のGoogle Drive認証情報はここへ置きます。

```sh
sudo install -d -o geoframe -g geoframe -m 0750 /var/lib/geoframe
sudo install -o geoframe -g geoframe -m 0600 \
  /path/to/drive_key.json /var/lib/geoframe/drive_key.json
```

環境固有の値は`/etc/default/geoframe`へ設定します。

```dotenv
GEOFRAME_HOST=0.0.0.0
GEOFRAME_ALLOWED_HOSTS=geoframe.example.test:8080
GEOFRAME_AI_ENABLED=0

# 任意の外部連携
# GEOFRAME_MLIT_KEY=
# GEOFRAME_HA_URL=https://home-assistant.example.test
# HOME_ASSISTANT_API_KEY=
# GEOFRAME_HA_MOTION_ENTITY=binary_sensor.display_motion
# GEOFRAME_HA_ENV_ENTITIES=sensor.room_temperature,sensor.room_humidity
```

認証情報を含む可能性があるため、読み取り権限を制限します。

```sh
sudo chown root:geoframe /etc/default/geoframe
sudo chmod 0640 /etc/default/geoframe
```

unitは`GEOFRAME_DATA_DIR=/var/lib/geoframe`と、上記認証ファイルを指す
`GEOFRAME_DRIVE_KEY`を設定します。ホストが日本標準時以外の場合はunitの`TZ=`も変更してください。

## 3. サーバーの有効化

```sh
sudo install -o root -g root -m 0644 \
  /opt/geoframe/deploy/geoframe-server.service \
  /etc/systemd/system/geoframe-server.service
sudo systemctl daemon-reload
sudo systemctl enable --now geoframe-server.service
curl --fail http://127.0.0.1:8080/api/admin/status
```

ログは次のコマンドで確認します。

```sh
journalctl -u geoframe-server.service -f
```

LAN内の別端末からHTTP 403になる場合は、アクセスに使った正確なホスト名とポートを
`GEOFRAME_ALLOWED_HOSTS`へ追加します。IPリテラルによるアクセスは許可されますが、名前付き
ホストは明示的な許可が必要です。

## 4. Chromiumキオスク（任意）

[`kiosk/kiosk.sh`](kiosk/kiosk.sh)はGEOFRAMEサーバーの起動を待ってからChromiumを
キオスクモードで開始します。サーバーが別の機体にある場合は`GEOFRAME_URL`を設定します。

```sh
GEOFRAME_URL=http://geoframe.example.test:8080/ /opt/geoframe/deploy/kiosk/kiosk.sh
```

`kiosk/geoframe-kiosk.service`は、グラフィカルセッション開始後に動くユーザーunitです。
デスクトップ環境によって`graphical-session.target`の扱いが異なるため、安定して起動しない場合は
デスクトップ環境の自動起動機能を使用してください。

画面のスリープ・復帰制御は任意です。`GEOFRAME_SLEEP_CMD`と`GEOFRAME_WAKE_CMD`を設定する前に、
`kiosk/screen-ctl.sh`を手動で確認してください。

## 5. バックアップ

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

## 6. 更新

更新はサーバー自身が`origin/main`を取り込むpull方式です。スクリプトは2つに分かれます。

| スクリプト | 実行場所 | 役割 |
| --- | --- | --- |
| [`deploy-pull.sh`](deploy-pull.sh) | サーバー上（root） | 実体。fetch、差分表示、サービス停止、`git reset --hard`、依存の再インストール、起動、HTTP正常性確認 |
| [`deploy.sh`](deploy.sh) | Mac（このリポジトリ内） | 薄いラッパー。`ssh -t <target> "sudo /opt/geoframe/deploy/deploy-pull.sh ..."`を実行するだけ |

Macから使う場合は、必ず接続先を明示し、最初にdry-runを実行してください。

```sh
./deploy/deploy.sh --dry-run --target operator@geoframe.example.test
./deploy/deploy.sh --target operator@geoframe.example.test
```

サーバーへ直接入って実行することもできます。

```sh
sudo /opt/geoframe/deploy/deploy-pull.sh --dry-run
sudo /opt/geoframe/deploy/deploy-pull.sh
```

`--dry-run`は取り込まれるコミットと変更ファイルを表示するだけで、サービスの停止・起動、
`git reset --hard`、`pip install`のいずれも行いません。`HEAD`が既に`origin/main`と
同一の場合は「変更なし」と表示してサービスに触れずに終了します。

`deploy-pull.sh`は仮想環境そのものを作りません。§1で`.venv`と依存を作成済みであることが
前提で、以降は`requirements.txt`に差分があるときだけ`pip install`を実行します。

動作上の要点は次のとおりです。

- **`git pull --ff-only`ではなく`git reset --hard`を使います。** GitHub側はsquash merge /
  force-pushの運用が続く前提で、履歴が線形に伸びるとは限らず、`--ff-only`はforce-pushの
  たびに失敗して手作業を要求します。サーバーはデプロイ専用でローカル編集を持たない
  （持ってはいけない）ため、「originが常に正、手元の差分は捨てる」という運用と
  `reset --hard`の意味が一致します。**サーバー上で`/opt/geoframe`のファイルを直接
  編集しないでください。次の更新で無言のうちに捨てられます。**
- **本番データは影響を受けません。** `git reset --hard`が触るのは追跡下のファイルだけで、
  `git clean`は実行しません。`app/.venv/`、`app/var/`、`.env`、`drive_key.json`はいずれも
  `.gitignore`済みで追跡されておらず、そのまま残ります。実データは`/var/lib/geoframe`にあり、
  リポジトリの外です。
- **ファイルのパーミッションはgitに任せます。** `reset --hard`はインデックスに記録された
  モード（`100644` / `100755`）を復元するため、rsync方式で必要だった「ファイルを一律644に
  してから`*.sh`へ`+x`を付け直す」後処理は不要です。むしろ有害で、実行すると
  `backup-catalog.sh`の実行ビットを潰して`geoframe-backup.service`が`203/EXEC`で
  失敗します。スクリプトが正規化するのは**ディレクトリの755のみ**です。
- **`requirements.txt`が変わったときだけ`pip install`が走ります。** 適用前後のコミットを
  比較して判定し、変更が無ければスキップした旨をログへ出します。
- **`git gc` / `git prune`は実行しません。** ロールバック用に古いコミットオブジェクトを
  残すためです（§7）。
- 適用に失敗した場合は、サービスが停止したままになる可能性があります。スクリプトは
  失敗時に復旧コマンド（適用前のSHAへの`git reset --hard`とサービス起動）をログへ出します。

> [!IMPORTANT]
> pull方式では、**サーバーからgithub.comへ外部到達できることが新たに必要**になります。
> rsync方式ではLAN内で完結していました。ファイアウォールやプロキシでサーバーの外向き
> 通信を制限している場合は、github.comへのSSH（TCP 22）を許可してください。
> なお、これは更新時の外向き通信だけで、GEOFRAME自体を外部へ公開するものではありません。

DBスキーマを変更する更新の前には、必ず手動バックアップを取得してください。

```sh
ssh operator@geoframe.example.test \
  'sudo -u geoframe /opt/geoframe/deploy/backup-catalog.sh'
```

## 7. ロールバック

更新後に問題が起きた場合は、適用前のコミットへ戻します。`deploy-pull.sh`は完了時と
失敗時の両方に、適用前の`HEAD`（`OLD_HEAD`）のSHAをログへ出しています。

```
[deploy-pull] 更新完了: 1a2b3c4... -> 5d6e7f8...
[deploy-pull] この更新を取り消すには:
[deploy-pull]   cd /opt/geoframe && sudo git reset --hard 1a2b3c4... && sudo systemctl restart geoframe-server
```

出力を控えていない場合は、サーバー上でreflogから探せます。

```sh
cd /opt/geoframe
sudo git reflog
```

ただしreflogのエントリには有効期限があります（既定で到達可能なもの90日、到達不能なもの30日）。
`gc.auto 0`によりコミットオブジェクト自体は残りますが、reflogから名前が消えるとSHAを
探しづらくなります。**確実な記録は`deploy-pull.sh`の実行ログに出るSHA**で、reflogは
その控えを失った場合の代替手段と考えてください。

戻す手順です。サービスを止めてから戻し、起動して正常性を確認します。
`ROLLBACK_SHA`には戻したいコミットのSHAを入れます。

```sh
ROLLBACK_SHA=1a2b3c4
sudo systemctl stop geoframe-server
cd /opt/geoframe
sudo git reset --hard "$ROLLBACK_SHA"
sudo find /opt/geoframe -path /opt/geoframe/.git -prune -o -path /opt/geoframe/app/.venv -prune -o -type d -exec chmod 755 {} +
sudo systemctl start geoframe-server
curl --fail http://127.0.0.1:8080/api/admin/status
```

戻した先で`requirements.txt`が異なる場合は、依存も戻してください。

```sh
sudo /opt/geoframe/app/.venv/bin/pip install -r /opt/geoframe/app/requirements.txt
```

> [!NOTE]
> GitHub側でforce-pushやsquash mergeが行われると、戻したいコミットが`git log`にも
> `origin/main`の履歴にも現れなくなります。それでもサーバーのローカルにオブジェクトが
> 残っていれば`git reset --hard <SHA>`で戻せます。SHAは`git reflog`から探してください。
> §1.2で`gc.auto 0`を設定しているのは、自動gcがこの到達不能オブジェクトを回収して
> ロールバック手段を失わせるのを防ぐためです。`deploy-pull.sh`も`git gc` / `git prune`を
> 実行しません。
>
> ロールバックした状態は`origin/main`より古いため、次に`deploy-pull.sh`を実行すると
> 再び`origin/main`へ進みます。問題のあるコミットはGitHub側で修正してください。

## トラブルシュート

- `status=200/CHDIR`: `/opt/geoframe/app`と親ディレクトリを`geoframe`ユーザーが通過できるか
  確認します。
- `fatal: detected dubious ownership in repository at '/opt/geoframe'`: リポジトリの所有者と
  gitを実行しているユーザーが一致していないときに出ます。`/opt/geoframe`は`root:root`所有、
  更新は`root`実行が前提です（§1.2）。まず`ls -ld /opt/geoframe`で所有者を確認し、
  意図せず変わっていれば`sudo chown -R root:root /opt/geoframe`で戻してください。
  所有者が正しいのに出る場合だけ、例外として登録します。

  ```sh
  sudo git config --system --add safe.directory /opt/geoframe
  ```

- `git@github.com: Permission denied (publickey).`: Deploy Keyが使われていません。
  `core.sshCommand`が設定されているか（`sudo git -C /opt/geoframe config --local --get core.sshCommand`）、
  鍵が読めるか、GitHub側にDeploy Keyが登録されているかを順に確認します。疎通は次で確認できます。

  ```sh
  sudo ssh -i /etc/geoframe/ssh/geoframe_deploy_key \
    -o IdentitiesOnly=yes -o UserKnownHostsFile=/etc/geoframe/ssh/known_hosts \
    -T git@github.com
  ```

  成功すると`Hi ysogabe/geoframe-ambient-display! You've successfully authenticated, but GitHub does not provide shell access.`
  のように表示されます（シェルが得られないのは正常です）。`Host key verification failed`の
  場合は`known_hosts`の登録（§1.1）をやり直してください。
- キオスクが白画面・黒画面: `curl`でサーバーURLを確認し、キオスク利用者のjournalと
  Chromium引数を調べます。
- Home Assistantが無効のまま: トークンとURLを確認し、
  [`../docs/integrations/home-assistant.md`](../docs/integrations/home-assistant.md)を参照します。
- 古いJavaScriptが表示される: キオスクをハードリロードするか、更新後に再起動します。
