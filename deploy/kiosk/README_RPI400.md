# Raspberry Pi 400 — 表示専用ノード構築手順

GeoFrame のサーバーは別機体（現行の常時稼働ミニPC、将来はLXC）に置いたまま、
**Raspberry Pi 400 を「表示だけ」の常設キオスクにする**ための手順です。

- 位置づけと判断の記録: [`../../docs/archive/rpi400-kiosk-migration-baseline.md`](../../docs/archive/rpi400-kiosk-migration-baseline.md)
- Pi400単体でサーバーと表示の両方を持つ旧案: [`../../docs/archive/rpi-cog-display-design.md`](../../docs/archive/rpi-cog-display-design.md)

```
[表示機: Raspberry Pi 400 4GB, USB3 SSD]        [サーバー: 既存の常時稼働機 / 将来LXC]
 Raspberry Pi OS (64-bit) Lite (Debian 13)       server.py
  ├ Cog + WPE (DRM直結)   ← 2560x1080           catalog.db / derivatives / tile_cache
  │   もしくは cage + Chromium (Wayland)         ↑ HA / Drive / JMA / 地図タイル
  ├ screen-ctl-pi.sh（DPMS制御。Xなしで動く）
  └ kiosk.env（GEOFRAME_URL でサーバーを指す）
        ↑ 有線LAN（写真配信は切替10秒毎で約276kbps≒誤差）
```

**この手順で作らないもの**: GeoFrameサーバー本体（`geoframe-server.service` は有効化しない）。
表示ノードはブラウザランタイムとDPMS制御だけを持ちます。Drive鍵・HAトークンは置きません。

---

## Phase 0 — 起動媒体を作る（Macで作業）

1. **媒体は USB3接続のSSD を推奨**（microSDやUSBメモリは避ける）。常時書き込みがあるため。
2. Raspberry Pi Imager を起動し、Device=**Raspberry Pi 400**、OS=**Raspberry Pi OS (64-bit) Lite** を選ぶ。
   - Lite は Debian 13 (trixie) ベース。Desktop版/Full版は使わない（表示機にデスクトップは不要）
3. OS customization（歯車アイコン）で先に仕込む:

| 項目 | 値 |
|---|---|
| hostname | 例 `geoframe-display` |
| ユーザー | 管理用のユーザー（例 `yoshio`）。パスワードは設定する |
| SSH | 有効 + **公開鍵を貼る**（パスワードログインは使わない） |
| locale | `ja_JP.UTF-8` |
| timezone | `Asia/Tokyo` |
| Wi-Fi | 初回構築だけでも可（運用は有線推奨） |

4. 書き込み後に **verifyを完了**させ、安全に取り外す。
5. Pi400 の **USB3ポート**に挿し、microSDは挿さないで起動する。

> micro-HDMI → HDMI のケーブル/アダプタが必要です（Pi400の映像出力は micro-HDMI）。

## Phase 1 — USBブートを確定させる

初回起動後、SSHで入って実行します。

```bash
# OS/ファームウェアを最新化
sudo apt update && sudo apt full-upgrade -y
sudo rpi-eeprom-update -a
sudo reboot

# 再起動後: ブートローダと起動順の確認
rpi-eeprom-update
rpi-eeprom-config | grep BOOT_ORDER
findmnt /
lsblk -o NAME,TRAN,SIZE,FSTYPE,MOUNTPOINTS,MODEL
```

- `BOOT_ORDER=0xf14` になっていること（右から左に解釈: USB mass storage(4) → microSD(1) → 再試行(f)）
- `/` が USB 媒体上にあり、`TRAN` が `usb` であること
- 設定は `sudo raspi-config` → `6 Advanced Options` → `A4 Boot Order` → `B2 NVMe/USB Boot` でも同じ
- **合格条件**: microSDを抜いた状態で、電源再投入を3回行い毎回USBから起動する

> `program_usb_boot_mode=1`（古いPi向け）は Pi400 では使いません。USB相性問題が無い限り `USB_MSD_*` のタイムアウト調整も不要です。

## Phase 2 — OSの基本設定

```bash
# 表示出力の確認（接続モニタとモード）
kmsprint | head -30
kmsprint -m | head -20
# 表示解像度に 2560x1080（21:9）が出るか、EDIDが読めているかを確認する

# 必要なパッケージ（Cog は Debian trixie の配布パッケージ）
sudo apt update
apt-cache policy cog
sudo apt install -y cog curl kms++-utils

# DRMモジュールの存在確認（これが無いとCogのDRM直結ができない）
ls /usr/lib/aarch64-linux-gnu/cog/modules/     # libcogplatform-drm.so があること
uname -m                                        # aarch64
```

**不合格時の判断**: `cog` が出ない場合は OS/architecture/repository を確認して止まる。
独自ビルドには進まない（旧設計書と同じ方針）。

ネットワーク（運用は有線推奨）。固定IPにする場合は NetworkManager で設定します。

```bash
# 推奨: ルーター側のDHCP予約で固定する（機体側の設定ミスで締め出されない）
# 機体側で固定する場合:
nmcli con show
sudo nmcli con mod "<接続名>" ipv4.method manual \
     ipv4.addresses 192.168.0.XX/24 ipv4.gateway 192.168.0.1 ipv4.dns "192.168.0.1"
sudo nmcli con up "<接続名>"
ip -4 addr show
```

任意: コンソールのブランクを止める（キオスクの見た目に影響する場合のみ）

```bash
sudo sed -i 's/$/ consoleblank=0/' /boot/firmware/cmdline.txt && sudo reboot
```

## Phase 3 — 表示ランタイムを入れる

**順序の意味**: 先に **cage + Chromium** で動かす。Chromium は CDP（`--remote-debugging-port`）を持つので
描画FPSを実測でき、**負荷測定がその日に回せる**。Cog は軽い代わりにFPSの外部計測手段が無い。
両方入れて、測定結果で採否を決める。

### 3-A. cage + Chromium（測定用・第二候補）

```bash
sudo apt install -y cage chromium
cage --version; chromium --version
```

### 3-B. Cog + WPE（第一候補・DRM直結）

```bash
cog --version
# 実機の --help で以下が使えることを確認する（配布版で引数名が異なる場合がある）
cog --help | grep -Ei 'platform|webprocess-failure|bg-color'
```

> Cog を動かすときは、X も Wayland コンポジタも起動していない状態にすること（DRM master の競合）。
> Lite なら何も入っていないので、そのままで問題ない。

## Phase 4 — リポジトリ配置とキオスク起動

```bash
# 専用ユーザー（sudo権限なし。DRM/入力に必要なグループだけ与える）
sudo adduser --system --group --home /var/lib/geoframe geoframe
sudo usermod -aG video,render,input geoframe

# リポジトリを /opt/geoframe に置く（表示ノードでもサーバーと同じパス規約にする）
sudo mkdir -p /opt/geoframe /etc/geoframe
sudo git clone <リポジトリURL> /opt/geoframe
sudo chown -R root:root /opt/geoframe

# 設定ファイル
sudo install -d -o geoframe -g geoframe -m 0750 /var/lib/geoframe
sudo cp /opt/geoframe/deploy/kiosk/cog.env.example /etc/geoframe/kiosk.env
sudo chmod 0644 /etc/geoframe/kiosk.env
sudoedit /etc/geoframe/kiosk.env     # GEOFRAME_URL / COG_PLATFORM_DRM_MODE_MAX を設定
sudo chmod +x /opt/geoframe/deploy/kiosk/*.sh
```

Chromium の managed policy を配置する（翻訳ポップアップ等をプロファイル設定に依存せず無効化する）。

```bash
sudo install -d -m 0755 /etc/chromium/policies/managed
sudo install -m 0644 \
  /opt/geoframe/deploy/kiosk/geoframe-kiosk-policy.json \
  /etc/chromium/policies/managed/geoframe-kiosk.json
```

`chromium-policy-disable-translate.json` は同じ `TranslateEnabled` を n100 向けに定義した既存ファイルで、Pi400では上記の `geoframe-kiosk-policy.json` を使う。

`/etc/geoframe/kiosk.env` の要点:

- `GEOFRAME_URL=http://<サーバーIP>:8080/` — **IPで指定すればサーバー側の設定変更は不要**
  （サーバーのHost検証はIPリテラル宛を無条件に許可する。ホスト名で指定する場合のみ
  サーバー側の `GEOFRAME_ALLOWED_HOSTS` にその名前を足す）
- `COG_PLATFORM_DRM_MODE_MAX=2560x1080@60`（最終モニタ）／開発中は `1920x1080@60`

手動で1回起動して確認する（systemd化の前にここで潰す）:

```bash
sudo -u geoframe env $(grep -v '^#' /etc/geoframe/kiosk.env | xargs) \
  /opt/geoframe/deploy/kiosk/cage-kiosk.sh      # A: cage + Chromium
sudo -u geoframe env $(grep -v '^#' /etc/geoframe/kiosk.env | xargs) \
  /opt/geoframe/deploy/kiosk/cog-kiosk.sh       # B: Cog + DRM
```

**合格条件**: モニタに GeoFrame が全画面表示され、写真・地図・HUDが出る。
出ない場合は Phase 8（トラブルシュート）へ。

## Phase 5 — 常設化（systemd）

```bash
# どちらか一方だけを有効化する（同時に有効化するとDRMを取り合って両方落ちる）
sudo install -m 0644 /opt/geoframe/deploy/kiosk/geoframe-cage-kiosk.service /etc/systemd/system/
# もしくは
sudo install -m 0644 /opt/geoframe/deploy/kiosk/geoframe-cog.service /etc/systemd/system/

sudo systemctl daemon-reload
sudo systemctl enable --now geoframe-cage-kiosk.service   # もしくは geoframe-cog.service
systemctl status geoframe-cage-kiosk.service --no-pager
journalctl -u geoframe-cage-kiosk -n 50 --no-pager
```

復旧の確認（設計書§11の障害表と同じ考え方）:

```bash
sudo systemctl restart geoframe-cage-kiosk.service     # 再起動で自動復帰すること
sudo systemctl stop geoframe-cage-kiosk.service        # 停止→再起動でも表示が戻ること
```

**合格条件**: 10回の再起動すべてで無操作自動表示し、プロセスを kill しても復帰する。

### 5-B. サーバー断の表示（オフライン画面）

サーバーが別機体にあるため、**その機体の再起動やLAN断でページが読めなくなります**。
真っ黒だと故障と区別できないので、時計だけを出す画面に落とします。

```bash
# 静的配信（Cog/Chromiumどちらでも参照できるローカルHTTP）
sudo tee /etc/systemd/system/geoframe-offline.service >/dev/null <<'EOF'
[Unit]
Description=GeoFrame offline fallback page
After=network.target
[Service]
Type=simple
ExecStart=/usr/bin/python3 -m http.server 8099 --bind 127.0.0.1 --directory /opt/geoframe/deploy/kiosk
Restart=always
[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload && sudo systemctl enable --now geoframe-offline.service

# kiosk.env に追記
# GEOFRAME_KIOSK_OFFLINE_URL=http://127.0.0.1:8099/offline.html
```

`cog-kiosk.sh` / `cage-kiosk.sh` は起動時にサーバーへ到達できなければ、この URL に切り替えて起動します。
通常画面の表示中にサーバーが落ちた場合は、オフライン画面へ切り替えるための次回キオスク再起動が必要です。
起動時にオフライン画面へ切り替わった場合は、オフライン画面自身が復旧を確認します。

#### オフライン画面からの自動復帰

`cage-kiosk.sh` がサーバー待機後に `offline.html` へ切り替えるとき、元の
`GEOFRAME_URL` を `?url=` クエリとして渡します。オフライン画面はそのURLへ10秒ごとに
GET（キャッシュ無効）を行い、5秒以内の成功が2回連続した場合だけ通常画面へ戻ります。
`no-cors` 方式ではHTTPステータスに関係なく例外なく応答が返れば「サーバーは応答している」とみなします。
サーバー側のCORSヘッダには依存せず、アプリ全体のオリジン境界を緩めない判断です。
URLが空・不正、または確認に失敗した場合は画面をそのまま表示し、書込みリクエストは行いません。
`?url=` は表示機の起動スクリプトが生成する信頼済み入力です。`offline.html` を信頼できない第三者に任意のクエリ付きで開かせないでください（HTTPで配信しない・外部コンテンツからの iframe/navigation を許さない）。

手動で復帰させる場合は、キオスクを再起動します。

```bash
sudo systemctl restart geoframe-cage-kiosk.service
```

### 5-C. 画面ON/OFF（DPMS）

**重要**: 既存の [`screen-ctl.sh`](screen-ctl.sh) は `xset dpms` を使う **X11専用**実装なので、
表示専用ノード（Cog=DRM直結 / cage=Wayland）ではそのまま使えません。Xを使わない
[`screen-ctl-pi.sh`](screen-ctl-pi.sh) を使います。

```bash
# sysfs方式（既定。要root。XもWaylandも不要）
sudo ./screen-ctl-pi.sh off && sudo ./screen-ctl-pi.sh status    # off
sudo ./screen-ctl-pi.sh on  && sudo ./screen-ctl-pi.sh status    # on

# cage を使っている場合のWayland経由（同位のユーザーで実行）
./screen-ctl-pi.sh on --method cage
```

終了コードは既存実装と同じ契約（`on=0 / off=1 / unknown=2`）にしてあります。
読み書きする値は測定スクリプトが読む `dpms` と同じなので、**「意図」と「実機」の照合が素直に成立**します。

> `vcgencmd display_power 0/1` は過去のPi 4系ファームウェアで使えましたが、現在の公式
> ドキュメントの `vcgencmd` 一覧には載っていません。sysfs 方式を第一とし、必要なら実機で
> `vcgencmd commands` を見て補助的に使ってください。

## 表示の残課題（翻訳ポップアップ・マウスポインタ）

cage + Chromium で Pi400 の画面中央に出ていた矢印の真の原因は、HDMI-CEC の入力ノードです。
Pi の `vc4-hdmi-0` / `vc4-hdmi-1` が `EV_REL` とポインタ能力を広告するため、実マウスが未接続でも
ポインタとして認識され、cage/wlroots がカーソルを描いていました。CSSやChromiumの設定で
コンポジタが描くカーソルを消すことはできません。

### HDMI-CEC入力ノードの除外（Pi400）

実機で確認済みの対策は、リポジトリの
[`99-geoframe-ignore-hdmi-pointer.rules`](99-geoframe-ignore-hdmi-pointer.rules) を Pi に配置し、
udev から libinput に HDMI-CEC ノードを渡さないようにすることです。実機への適用は次の手順だけを
行い、適用後にキオスクを再起動します。

```bash
sudo install -m 0644 \
  /opt/geoframe/deploy/kiosk/99-geoframe-ignore-hdmi-pointer.rules \
  /etc/udev/rules.d/99-geoframe-ignore-hdmi-pointer.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input
sudo systemctl restart geoframe-cage-kiosk.service
```

検証は、cage が開いている input fd を確認して行います。`Raspberry Pi Internal Keyboard` に対応する
内蔵キーボードの fd（通常 `/dev/input/event0` と `event1`）だけが表示され、`vc4-hdmi-0` /
`vc4-hdmi-1` のノードが含まれないことを確認します。

```bash
ls -l /proc/$(pgrep -xo cage)/fd | grep /dev/input
```

この Pi の VC4／ハードウェアプレーンでは `grim` は黒画像しか返さないため、スクリーンショットに
カーソルが写らないことを検証には使えません。画面のカーソル表示はモニタを目視し、input fd の
構成を上記コマンドで確認してください。

副作用として HDMI-CEC 経由のリモコン入力は無効になります。USBマウスを後から接続した場合は
ルールの対象外なので、cage はカーソルを表示します。`kiosk-cursor-none.css` と
`--user-style-sheet`、`app.css` の `cursor:none` はこの問題の根拠にはなりませんが、USBマウス接続時の
保険として残します。

翻訳ポップアップについては、`cage-kiosk.sh` の Chromium 起動引数で翻訳機能・翻訳UI・自動入力
サーバー通信を無効化し、`--disable-translate` と `--lang=ja` を指定します。

## Phase 6 — 負荷測定（この移行の目的）

**測る前に必ず事前条件を通す**（画面が消えている間の値は「低負荷」ではなく「停止」）。

```bash
# 1) 画面が出ているか
for c in /sys/class/drm/card*-*/; do n=$(basename $c); [ "$(cat $c/status)" = connected ] \
  && echo "$n dpms=$(cat $c/dpms)"; done

# 2) 描画が動いているか（Chromium構成のみ）
node /opt/geoframe/tools/measure-display-client.mjs --seconds 15 --label pi400-cage
#   → how: "raf-stalled" なら描画が止まっている。この測定値は捨てる

# 3) 負荷サンプリング（CSVが出力される）
/opt/geoframe/deploy/kiosk/measure-display-node.sh \
    --duration 300 --interval 10 --procs 'chromium|cog' --cdp-port 9222 \
    --out /tmp/kiosk-pi400.csv
```

判定は旧設計書 §10.1 の目標に揃えます。

| 指標 | 目標 |
|---|---|
| 通常HUD | 55 fps 以上 |
| 写真transition | 30 fps 以上 |
| 常用CPU | 平均 70% 未満 |
| 温度 | throttling なし（`vcgencmd get_throttled` が `0x0`） |
| GPU経路 | `webgl.renderer` に `V3D`/`Mesa`（`llvmpipe` は不合格） |
| 起動 | 電源投入から表示まで60秒以内 |
| 長時間 | 24時間でRSS単調増加・描画停止なし |

比較基準として、**現行のChromiumキオスク機でも同じコマンドを実行**して並べます（2026-09-13時点の実測は
ベースライン文書に記載: 12プロセス / 1299–1348MB / WebGLはハードウェア経路）。

## Phase 7 — サーバーを別機体（LXC）へ移すとき

表示専用化が済んだ後に行う作業。**表示ノード側の変更は `GEOFRAME_URL` の書き換えだけ**ですが、
サーバー側では次が必要です（詳細はベースライン文書§3）。

1. **画面制御の移設**: `GEOFRAME_SLEEP_CMD` / `WAKE_CMD` / `SCREEN_STATUS_CMD` は
   サーバーホスト上で実行される設計なので、サーバーを移すと `screen-ctl-pi.sh` を呼べなくなる。
   → 表示ノードに `screen-agent`（10秒周期でサーバーの `/api/presence` を読み、ローカルでDPMS適用）を置く。
   **HAトークンとDrive鍵は表示ノードに置かない。**
2. データ移設: `catalog.db` + `derivatives/` + `tile_cache/` + `hazard_cache/` + `.cache.json`
3. バックアップの向き先変更（`backup-pull.sh` がLXC向けとして既にある）
4. サーバー側 `GEOFRAME_ALLOWED_HOSTS`（ホスト名で参照する場合のみ）

## Phase 8 — トラブルシュート

| 症状 | 見るところ |
|---|---|
| 画面が真っ暗／信号なし | `kmsprint` でコネクタとモード。`dtoverlay=vc4-kms-v3d` が有効か（`/boot/firmware/config.txt`）。HDMIケーブルは micro-HDMI 側 |
| `cog` がDRMを掴めない | 他のコンポジタが動いていないか（`systemctl status getty@tty1` / Xorg / labwc）。実行ユーザーが `video,render,input` に入っているか |
| Cog が起動直後に落ちる | `journalctl -u geoframe-cog -n 100`。`libcogplatform-drm.so` の有無、`/dev/dri/*` の権限 |
| ページが表示されない（白／エラー） | 表示ノードから `curl -sv http://<サーバーIP>:8080/`。サーバー側で403ならHost検証（`GEOFRAME_ALLOWED_HOSTS` かIP直列） |
| 画面は点くが更新が止まる | 事前条件チェック（Phase 6）。`dpms` と `raf_fps` を確認。Cog側は `--webprocess-failure=restart` が効いているか |
| 再起動で表示されない | unit の `Restart=always` と `Conflicts=getty@tty1.service`、`TTYPath`。`journalctl -u <unit>` |
| 24時間で挙動が変わる | `vcgencmd get_throttled`（`0x0` でない bit があれば温度/電源）。`temp` と `dmesg` のUSB I/Oエラー |

## チェックリスト

- [ ] USB3 SSDから microSDなしで3回連続起動できる（`BOOT_ORDER=0xf14`）
- [ ] `uname -m` = aarch64 / `apt-cache policy cog` で配布版が出る
- [ ] `kmsprint` で接続モニタと 2560×1080（または開発FHD）を確認した
- [ ] 手動起動で全画面表示できた（cage + Chromium / Cog の両方 or 片方）
- [ ] systemd で10回再起動すべて自動表示した
- [ ] 事前条件（dpms=On / raf_fps>0）を満たした状態で負荷CSVを取得した
- [ ] 24時間運転でRSS単調増加・throttleなし
- [ ] オフライン画面に落ちることを確認した（サーバーを止めて再起動）
- [ ] 画面ON/OFFが `screen-ctl-pi.sh` で効く（`status` が実機と一致する）
