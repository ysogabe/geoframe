# GEOFRAMEのデプロイ

このディレクトリには、Linuxサーバー向けのsystemd unitと運用スクリプトの構成例があります。
アプリケーションを`/opt/geoframe`へ配置し、変更されるデータを`/var/lib/geoframe`へ保存し、
権限を制限した`geoframe`ユーザーでサービスを実行する構成を想定しています。

> [!WARNING]
> GEOFRAMEには利用者認証がありません。localhostへバインドするか、信頼できるLAN内だけで
> 使用してください。ポート8080をインターネットへ直接公開しないでください。

## 前提条件

- Python 3.10以降を利用できるDebianまたはUbuntu系Linux
- `python3-venv`、`sqlite3`、`rsync`
- 同梱unitを使う場合は`systemd`
- 同じ機体を表示端末として使う場合だけChromium

## 1. アプリケーションの配置

ログインシェルとホームディレクトリを持たないシステムユーザーを作ります。

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin geoframe
sudo install -d -o root -g root -m 0755 /opt/geoframe
```

このリポジトリの`app/`と`deploy/`を`/opt/geoframe/`へコピーします。開発機の`.env`、
`drive_key.json`、`var/`、仮想環境、キャッシュはコピーしないでください。

仮想環境を作成します。

```sh
sudo python3 -m venv /opt/geoframe/app/.venv
sudo /opt/geoframe/app/.venv/bin/pip install -r /opt/geoframe/app/requirements.txt
sudo chown -R root:root /opt/geoframe/app /opt/geoframe/deploy
sudo find /opt/geoframe/app /opt/geoframe/deploy -type d -exec chmod 0755 {} +
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

`update-app.sh`は、データと秘密情報を除外して`app/`と`deploy/`を同期します。転送中は
サービスを停止し、権限を正規化してから再起動し、HTTPの正常性確認まで行います。

必ず接続先を明示し、最初にdry-runを実行してください。

```sh
./deploy/update-app.sh --dry-run --target operator@geoframe.example.test
./deploy/update-app.sh --target operator@geoframe.example.test
```

DBスキーマを変更する更新の前には、必ず手動バックアップを取得してください。

## トラブルシュート

- `status=200/CHDIR`: `/opt/geoframe/app`と親ディレクトリを`geoframe`ユーザーが通過できるか
  確認します。
- キオスクが白画面・黒画面: `curl`でサーバーURLを確認し、キオスク利用者のjournalと
  Chromium引数を調べます。
- Home Assistantが無効のまま: トークンとURLを確認し、
  [`../docs/integrations/home-assistant.md`](../docs/integrations/home-assistant.md)を参照します。
- 古いJavaScriptが表示される: キオスクをハードリロードするか、更新後に再起動します。
