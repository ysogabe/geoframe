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
- サーバーからgithub.comへのSSH到達性（TCP 22）。コードは本機がgitで取得します（§5参照）

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
（ロールバックの手段を残すため。§6参照）。

> [!NOTE]
> `git clone`は空でないディレクトリに対しては失敗します。rsync方式で運用していた
> 既存機を移行する場合は、`/opt/geoframe`に`app/.venv`などが既にあるため、
> cloneではなく既存ディレクトリをそのままgit管理下に置いてください。この手順なら
> 未追跡の仮想環境を作り直さずに済みます。
>
> rsync方式では`/opt/geoframe`は`geoframe:geoframe`所有でした。**`git init`より先に
> 所有者を`root:root`へ揃えてください。** 順序を逆にすると、`root`が`geoframe`所有の
> ツリーに対してgitを実行することになり`fatal: detected dubious ownership`で止まり、
> 対処のつもりでトラブルシュートの`safe.directory`例外登録に頼りたくなりますが、
> それは所有者を直さない限り安全な代替にはなりません（トラブルシュート参照）。
>
> ```sh
> sudo chown -R root:root /opt/geoframe
> cd /opt/geoframe
> sudo git init
> sudo git remote add origin git@github.com:ysogabe/geoframe-ambient-display.git
> # 上記の core.sshCommand / gc.auto を設定してから
> sudo git fetch origin
> sudo git reset --hard origin/main
> # rsync方式で配布された旧update-app.shは追跡外のため`reset --hard`では消えません。
> # 実行するとGEOFRAMEのデータを削除しうる`--delete`付きrsyncが走るため、移行時に削除します。
> sudo rm -f /opt/geoframe/deploy/update-app.sh
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
```

認証情報を含む可能性があるため、読み取り権限を制限します。

```sh
sudo chown root:geoframe /etc/default/geoframe
sudo chmod 0640 /etc/default/geoframe
```

unitは`GEOFRAME_DATA_DIR=/var/lib/geoframe`と、上記認証ファイルを指す
`GEOFRAME_DRIVE_KEY`を設定します。ホストが日本標準時以外の場合はunitの`TZ=`も変更してください。

### 2.1 ローカルembedding（任意）

`GEOFRAME_EMBED_BACKEND=local`にすると、写真と検索語のembeddingをこのサーバーのCPUで
embeddinggemma-2により計算し、写真をembeddingのためにGoogleへ送らなくなります（比較評価に回す
同一場面の候補だけは引き続きGeminiへ送ります）。目安はCPU 4スレッドで1枚1.3秒、
プロセスのメモリ+1.5GBです。既定は`gemini`で、設定しなければ何も変わりません。

1. `catalog.db`をバックアップします（§4）。
2. 追加の依存を入れます。torchはCPU版です。`deploy-pull.sh`はこのファイルを見ないため、
   変わったときは同じコマンドで入れ直してください。

   ```sh
   sudo /opt/geoframe/app/.venv/bin/pip install -r /opt/geoframe/app/requirements-local-ai.txt
   ```

3. モデルを`/var/lib/geoframe/models/embeddinggemma-2`へ置きます（起動時にネットワークから
   取得しません。サービスは`ProtectHome=true`なので、ホームディレクトリには置けません）。
   Hugging Faceで`google/embeddinggemma-2`の利用条件に同意したアカウントで取得し、
   検証済みのリビジョン`914f7f89142e33e77833254d9c9b90c3cef7303b`を使います。

   ```sh
   sudo install -d -o geoframe -g geoframe -m 0750 /var/lib/geoframe/models
   sudo cp -rL /path/to/embeddinggemma-2 /var/lib/geoframe/models/embeddinggemma-2
   sudo chown -R geoframe:geoframe /var/lib/geoframe/models/embeddinggemma-2
   ```

4. 切り替える前に、全写真のembeddingをキャッシュへ作っておきます。サービスは今のまま動かして
   かまいません。8スレッドで1秒に約1枚（6,500枚で約2時間）です。省くと、切り替え後に
   ai_workerが1分12枚ずつ作り直すため数時間かかり、その間は未embeddingの写真を含む場面が
   グループになりません（写真は表示されたままです）。

   ```sh
   cd /opt/geoframe/app
   sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe \
     nice -n 10 .venv/bin/python -m geoframe.prefill_embeddings --threads 8
   ```

5. `/etc/default/geoframe`に`GEOFRAME_EMBED_BACKEND=local`を足して再起動します。最初のサイクルで
   キャッシュから入れ替わり、Geminiの比較評価は写真の中身が同じなら残ります。同一場面の
   しきい値は既定で0.82から0.83に変わるため、候補の組が変わったグループ（実データでは約800中
   約210）だけが比べ直しになります（`GEOFRAME_AI_JUDGE_PER_CYCLE`件ずつ）。

   ```sh
   sudo systemctl restart geoframe-server.service
   curl --fail http://127.0.0.1:8080/api/ai/status
   ```

   `embedding_backend`が`local`、`other_model_embeddings`が0になれば入れ替え完了です。

戻すときは`GEOFRAME_EMBED_BACKEND`の行を消して再起動します。Geminiのembeddingはキャッシュに
残っているので、作り直し（課金）は起きません。

### 2.2 学習済みの順位付け（任意）

同一場面グループの中の順位（Geminiに見せる最大6枚の選択と、その並び）を、過去のGemini比較評価から
学習した線形モデルで付けられます。モデルは家庭の写真の評価から作るため、リポジトリには入れず、
`/var/lib/geoframe/models/rank_model.json`（`GEOFRAME_RANK_MODEL`）に置きます。読み込みは
ファイルの更新を検知して自動で行うので、再起動は要りません。

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
geoframe は同じ機体の LAN 入口（既定 `GEOFRAME_DECISION_URL=http://127.0.0.1:8100/llm-decision`）経由で
呼び、どの要求にも呼び出し元の名前 `X-Client-Name: geoframe` を付けます（入口は名前ごとに日ごとにまとめます）。
Decision が常駐している間は、この節のタイマーは「人が起動済み」として何もしません。
常駐をやめて必要なときだけ動かす運用に戻す場合のために、毎朝5:00に次の順で確かめ、
必要なときだけ Decision を動かすタイマーを用意しています
（[`loc_decision_morning.py`](loc_decision_morning.py)。確認の要求には `X-Client-Name: geoframe-loc-morning` を付けます）。

1. STATUS の判定待ち（`db.loc.clue_pending`）が0なら何もしない
2. 人が Decision を起動済みなら何もしない（止めるのも人に任せる）
3. GPU が空いていなければ何もせず、翌朝にもう一度見る。空いているとみなすのは、llm-manager の
   lease が0件、ほかのプロセスの VRAM が1GB未満、GPU 使用率が10%未満、Ollama がモデルを
   読み込んでいない、のすべてを満たすとき。llm-manager か Ollama の状態を取れないときも使わない
4. Decision を起動し、判定待ちが0になったら止める。2時間を過ぎた、15分間判定が進まない、
   別の処理が lease を取った・GPU を使い始めたときも止める（続きは翌朝）

判定が終わった日の Gemini への問い合わせは geoframe-server が続けるので、Decision を止めても
候補づくりは進みます。

llm-manager は誰かが lease を取ると Decision を含む常駐を止め、lease が終わると起動し直します。
このタイマーが起動した Decision がそうして戻されたまま動き続けないよう、起動したときに
`~/.local/state/geoframe-loc-decision/decision-owned`へ印を残します。lease のために止められた
ときは上限の時間まで戻るのを待って止め、それでも戻らなければ翌朝、印のある Decision を
引き継いで止めます（人が起動した Decision には印が無いので触りません）。

Decision を動かしているユーザーで入れます。

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
同一で、**かつ追跡ファイルが直接編集されていない場合**は「変更なし」と表示してサービスに
触れずに終了します。サーバー上で直接編集された追跡ファイルがある場合は、取り込むコミットが
無くても「変更なし」とは扱わず、該当ファイルを一覧表示したうえで`git reset --hard`で正規の
状態へ戻します（`--dry-run`では一覧表示までで、`reset --hard`は行いません）。

`deploy-pull.sh`は仮想環境そのものを作りません。§1で`.venv`と依存を作成済みであることが
前提で、以降は`requirements.txt`に差分があるときだけ`pip install`を実行します。

動作上の要点は次のとおりです。

- **`git pull --ff-only`ではなく`git reset --hard`を使います。** GitHub側はsquash merge /
  force-pushの運用が続く前提で、履歴が線形に伸びるとは限らず、`--ff-only`はforce-pushの
  たびに失敗して手作業を要求します。サーバーはデプロイ専用でローカル編集を持たない
  （持ってはいけない）ため、「originが常に正、手元の差分は捨てる」という運用と
  `reset --hard`の意味が一致します。**サーバー上で`/opt/geoframe`のファイルを直接
  編集しないでください。次の更新で無言のうちに捨てられます。**これは`origin/main`側に
  新しいコミットが無い場合も同じです（スクリプトは作業ツリーの汚れも検出し、
  `reset --hard`で戻します）。
- **本番データは影響を受けません。** `git reset --hard`が触るのは追跡下のファイルだけで、
  `git clean`は実行しません。`app/.venv/`、`app/var/`、`.env`、`drive_key.json`はいずれも
  `.gitignore`済みで追跡されておらず、そのまま残ります。実データは`/var/lib/geoframe`にあり、
  リポジトリの外です。
- **ファイルのパーミッションはgitに任せます。** `reset --hard`はインデックスに記録された
  モード（`100644` / `100755`）を復元するため、rsync方式で必要だった「ファイルを一律644に
  してから`*.sh`へ`+x`を付け直す」後処理は不要です。むしろ有害で、実行すると
  `backup-catalog.sh`の実行ビットを潰して`geoframe-backup.service`が`203/EXEC`で
  失敗します。スクリプトが正規化するのは**ディレクトリの755のみ**です。
- **`requirements.txt`が変わったときだけ`pip install`が走ります。** 適用前後のコミットの
  比較に加えて、作業ツリーでの直接編集も判定に含めます。差分の確認自体に失敗した場合は
  安全側へ倒して再インストールします（判定を誤ってスキップすると、古い`.venv`のまま起動して
  後から`ImportError`としてしか露見しないためです）。変更が無ければスキップした旨をログへ出します。
- **`git gc` / `git prune`は実行しません。** ロールバック用に古いコミットオブジェクトを
  残すためです（§6）。
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

## 6. ロールバック

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

## 7. Rust 版（移行中。検証環境専用）

Python 製のサーバー（`app/`）を Rust の単一バイナリへ移す作業（[Issue #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)）の途中経過です。
**Rust 版は既定で読み取り専用（`GEOFRAME_INGEST`を設定しない限り DB に書かない）で、本番では使いません。本番の切替は P5**（[計画](../docs/rust-migration/plan.md)）で、
ユーザーの立ち会いのもとで行います。ここまでの§1〜§6（Python 版）が現行の本番の手順で、変わりません。
本番機（`geoframe-server` が動いている機体）に、この節の unit・スクリプトを使わないでください。

| ファイル | 役割 |
| --- | --- |
| [`geoframe-rs.service`](geoframe-rs.service) | Rust 版の systemd unit。`geoframe-server.service` と同じハードニングに、Rust 版だけ付けられる`MemoryDenyWriteExecute=true`を足す |
| [`rust-deploy.sh`](rust-deploy.sh) | 手元でビルド（`x86_64-unknown-linux-musl`の完全静的）→ 検証環境の確認 → 転送 → 入れ替え → 確認 → 失敗時の復元 |
| [`rust-deploy-test.sh`](rust-deploy-test.sh) | `rust-deploy.sh`の検証環境側の本体を、モックの`systemctl`/`curl`で試験する（ssh もサービスも使わない） |
| [`../tools/compare_with_python.py`](../tools/compare_with_python.py) | Rust 版と比較用 Python 版の全件比較（集計だけを出す） |
| [`../tools/miharashi-feed-check/`](../tools/miharashi-feed-check/README.md) | miharashi の取得・読み込みのコードを、画面なしで Rust 版に向けて動かす確認用ハーネス |

### 7.1 unit の要点

- バイナリは`/usr/local/bin/geoframe`、`geoframe serve`をそのまま起動する（`app/`も`.venv`も要らない）。
- `User=geoframe`、`StateDirectory=geoframe`（`/var/lib/geoframe`）、`Environment=GEOFRAME_DATA_DIR=/var/lib/geoframe`、
  `Environment=TZ=Asia/Tokyo`、`EnvironmentFile=-/etc/default/geoframe`（後勝ち）。Python 版と**同じ設定ファイル・同じデータ置き場**を読む。
- `RestartPreventExitStatus=2`: 設定不正（終了コード 2）は再起動しても直らないので、再起動ループにせず`failed`で止める。
  DB が開けない（終了コード 1）は再起動対象。`LimitNOFILE=4096`（同時の接続 256＋配信 32＋SQLite で 300 前後を使う）。
- **取り込み（DB への書き込みと同期サイクル）は`GEOFRAME_INGEST=1`のときだけ動く**（既定は`0`）。P5 の切替まで本番の書き手は Python 版なので、
  `/etc/default/geoframe`をそのまま使って Rust 版を起動しても DB には書かない。検証環境で取り込みを試すときだけ、
  `/etc/default/geoframe`か drop-in で`GEOFRAME_INGEST=1`を足す。**P5 の切替までは、検証環境の印`/etc/geoframe/staging`が無い、または`geoframe-server`が非稼働と確認できない機体では、書き込みの前に終了コード 2 で断る**（本番機では動かない。[server.md §11.1.1](../docs/rust-migration/server.md)。`systemctl show geoframe-server.service`が使えない・答えないときも断る。`ActiveState`だけを見る: unit ファイルが無くても動いているサービスは断る）。有効にすると、DB が無ければ新しく作り、スキーマの作成・移行をして、
  `GEOFRAME_DRIVE_POLL_MIN`（既定 10 分）ごとに同期サイクルを回す（[server.md §11](../docs/rust-migration/server.md)）。**Python 版と同じ`catalog.db`には絶対に向けない**。
- `ReadOnlyPaths=`は、`photos/`・`blog_exports/`・`travel_blog/`だけ（writer が書く`catalog.db`・`derivatives/`は外した。`tmp/`・`.cache.json`・`.ingest.lock`は元から対象外）。
  WAL の DB は読み取りでも同じディレクトリに`-shm`/`-wal`を作るので、ディレクトリ自体は書き込み可のまま。P3 で手動修正などの更新 API を追加するとき`blog_exports/`などを外す。
  `catalog.db`を別の inode に置き換えたら（バックアップからの復元など）再起動する。
- 取り込みを 1 回だけ手で動かす: `geoframe sync`（`GEOFRAME_INGEST=1`が要る）。**書かずに計画だけ**見るなら`geoframe sync --dry-run`（行・写真・キャッシュ・ロックを変えず、件数だけを出す。ただし SQLite が読み取りでも作る`catalog.db-wal`・`-shm`はできうる。印も Python の確認も要らない）。
  外向き HTTPS の確認は`geoframe selfcheck-net`（状態と HTTP ステータスだけを出す）。
- **待ち受けは`--listen`を書かない**。`GEOFRAME_HOST`/`GEOFRAME_PORT`（既定`0.0.0.0:8080`）で決まる。

### 7.2 Rust 版を Python 版と並走させる（P1〜P4 の検証）

同じ機体で Python 版と Rust 版を同時に動かすなら、**待ち受けとデータ置き場を分ける**。
Python 版と Rust 版は同じ`catalog.db`を同時には開かない（Python は起動時の移行・位置推定で DB へ書く）。
Rust 版へ別のポートを与えるには、drop-in の`ExecStart=`に`--listen`を書く（`--listen`は`GEOFRAME_HOST`/`GEOFRAME_PORT`より優先され、それらを読まない）。

```sh
sudo systemctl edit geoframe-rs
```

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/geoframe serve --listen 0.0.0.0:8081
```

検証環境（lxc-geoframe）では、Python 版を比較用に別のデータ（`/var/lib/geoframe-pyref`）・別のポート（8090）で動かし、
Rust 版は本番のコピーのデータ（`/var/lib/geoframe`）を`:8080`で読んでいる。

### 7.3 配布と確認（検証環境）

```sh
# 接続先は引数か環境変数で必ず指定する（既定値は無い）
export GEOFRAME_SSH_TARGET=user@検証環境
deploy/rust-deploy.sh --dry-run        # ビルドと手順の表示だけ（ssh もしない）
deploy/rust-deploy.sh                  # ビルド→検査→転送→入れ替え→/healthz と /api/v1/photos の確認
deploy/rust-deploy.sh --skip-build     # ビルド済みの成果物をそのまま配る
deploy/rust-deploy.sh --rollback       # 直前のバイナリ（.prev）へ手で戻す
bash deploy/rust-deploy-test.sh        # 失敗経路をモックで試験（ssh なし）
```

**検証環境の印（誤配布の防止）。** 変更の前に、接続先に次の 2 つを確かめ、満たさなければ何も変更せずに終了コード 4 で拒否する（`--rollback`も同じ）。

- 検証環境の印`/etc/geoframe/staging`（通常ファイル。シンボリックリンクは不可）があること。**検証環境で一度だけ作る**（本番機には作らない）:
  `sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging`（`/etc/geoframe/`は本番機にも SSH の鍵置き場として存在するので、ディレクトリではなくファイルを見る）。
- Python 版の`geoframe-server.service`が稼働中でないこと（稼働中なら本番機の可能性が高い）。`systemctl show`の`ActiveState`が`inactive`／`failed`のときだけ通し、`LoadState`は見ない（unit ファイルが無い`not-found`でも、動いているサービスは動き続けうるので、`not-found/active`も拒否する。Python が入っていない機体は`not-found/inactive`で通る）。

本番の切替は P5 で、専用の手順と明示的なフラグをそのとき作る。このスクリプトは、印の無い機体には配らない。

配置先のバイナリは入れ替えの直前に`/usr/local/bin/geoframe.prev`へ（unit も`.prev`へ）残す。**直前の 1 世代だけ**。

**失敗したときの復元。** 入れ替え以降（unit の導入・`daemon-reload`・`enable`・`restart`・確認）のどこで失敗しても、
さらにリモートの本体が中断されても（ssh の切断・SIGTERM）、配布前の状態へ戻す。

| 状況 | 戻し方 |
| --- | --- |
| 既存の配布を更新 | バイナリを`.prev`から、unit を`.prev`から戻す。配布前に稼働していれば再起動して確認、止まっていれば止めたまま。配布前に`enable`でなかったなら`disable`する。失敗した方のバイナリは`geoframe.failed`に残る |
| 初回の配布（戻すバイナリが無い） | サービスを止め、配布前に`enable`でなかったなら`disable`し、新しく入れた unit を撤去し、バイナリを`geoframe.failed`へ退避する（以前の世代の`.prev`は戻す先にしない） |

終了コード: 0 成功 / 1 失敗したが配布前の状態へ戻した / **3 戻す途中でも失敗した**（途中で失敗した操作と、手作業の復旧手順が出る。
「戻した」とは表示しない）/ 4 誤配布の防止で拒否 / 2 引数の誤り。

初回だけ、検証環境で次を整える（以後は`rust-deploy.sh`が unit を入れる）。

```sh
sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging  # 検証環境の印（上記）
sudo chown -R geoframe:geoframe /var/lib/geoframe   # WAL の -shm を作れる持ち主にそろえる
sudo install -m 0644 /dev/null /etc/default/geoframe # 出力に影響する設定だけ書く。本番に合わせて GEOFRAME_EMBED_BACKEND=local など
```

全件比較（Python 版の応答との一致）は`tools/compare_with_python.py --rust <Rust のURL> --python <比較用 Python のURL>`。
JSON は型を区別して比べる（`true`と`1`、`false`と`0`、`35`と`35.0`は別）。画像は両方が 200 を返したうえで、バイト列を比べる
（同じエラー同士は一致に数えない）。`--self-test`で比較関数だけを試せる（接続しない）。
結果の記録は[P1 の受入確認](../docs/rust-migration/p1-acceptance.md)。

## トラブルシュート

- `status=200/CHDIR`: `/opt/geoframe/app`と親ディレクトリを`geoframe`ユーザーが通過できるか
  確認します。
- `fatal: detected dubious ownership in repository at '/opt/geoframe'`: リポジトリの所有者と
  gitを実行しているユーザーが一致していないときに出ます。`/opt/geoframe`は`root:root`所有、
  更新は`root`実行が前提です（§1.2）。まず`ls -ld /opt/geoframe`で所有者を確認し、
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

  成功すると`Hi ysogabe/geoframe-ambient-display! You've successfully authenticated, but GitHub does not provide shell access.`
  のように表示されます（シェルが得られないのは正常です）。`Host key verification failed`の
  場合は`known_hosts`の登録（§1.1）をやり直してください。
- 管理ページに古いJavaScriptが表示される: ブラウザをハードリロードします。
