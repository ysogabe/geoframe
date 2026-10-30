# Pi400キオスク移行のベースライン（表示専用化と負荷測定）

> [!NOTE]
> 2026-09-13時点の**検討ベースライン**です。決定は「まず表示専用で動かして測定する」までで、
> サーバーのLXC移設は測定結果を見てから改めて判断します。数値はすべて実機実測か、
> その実測からの推定であることを明記しています。

- 日付: 2026-09-13
- 状態: 検討ベースライン（未採用・作業開始前）
- 対象ハード: Raspberry Pi 400（4GB、USB3ブート想定）
- 構築手順: [`README_RPI400.md`](https://github.com/ysogabe/geoframe/blob/display-final/deploy/kiosk/README_RPI400.md)（OS導入〜常設化〜負荷測定）
- 関連: [`rpi-cog-display-design.md`](rpi-cog-display-design.md)（Pi400単体でサーバー+表示を持つ案・アーカイブ）、
  [ADR 0002](../decisions/0002-screen-power-override-via-ha.md) / [0003](../decisions/0003-screen-control-state-machine.md) /
  [0004](../decisions/0004-frontend-display-mode-arbitration.md) / [0005](../decisions/0005-agent-status-via-ha-bus.md) /
  [0006](../decisions/0006-implementation-sequencing.md)

---

## 1. 今回決めたこと

1. **Pi400は「表示専用」で動かす。** サーバー（`server.py`）の移設はこの段階では行わない。
2. **OSは Raspberry Pi OS (64-bit) Lite（Debian 13 trixie ベース）を新規導入する。** Desktop版/Full版は使わない。
3. **順序は「①表示専用で稼働 → ②負荷測定 → ③サーバー移設の判断」**。②を飛ばして③に進まない。
   （同時に進めると、画面が出ない原因がPi400側か移設側か切り分けできなくなる）
4. サーバーをLXCへ移す場合に**必ずセットで必要になる作業**を本ベースラインに記録する（§3・§5）。

## 2. 実測した前提値（2026-09-13 / 常時稼働ミニPC=N100）

| 項目 | 実測値 | 取得方法 |
|---|---|---|
| キオスク(Chromium, 12プロセス) RSS | 1,299–1,348 MB（60秒間で増加傾向なし） | `ps`/`/proc` サンプリング |
| キオスク CPU | 0.2–0.3 %（ただし画面Off中＝後述の注意） | `measure-display-node.sh` |
| キオスク WebGL経路 | `ANGLE (Intel, Mesa Intel(R) Graphics (ADL-N), OpenGL 4.6)` = ハードウェア経路 | CDP `WEBGL_debug_renderer_info` |
| 画面出力 | HDMI接続モニター 2560×1080 / 21:9（`card0-HDMI-A-2`） | `/sys/class/drm/*` |
| Hermes側常駐（同一機体・別ユーザー） | 2,123 MB（gateway×2 / dashboard / MCP群 / dockerd） | `ps` RSS合成 |
| 同機体の常駐合計 | 3.39 GB（キオスク1,348 + Hermes側2,123） | 上記の合計 |
| サーバー `server.py` | RSS 375 MB / 生涯平均CPU 10.6 %（稼働2日21時間）/ 直近30秒 0.4 % | `/proc/<pid>/stat` |
| 写真カタログ | 3,099枚（全行にderivativeあり） | `GET /api/photos` |
| derivative（長辺2560px JPEG） | 20枚サンプル平均 336 KB / 中央値 290 KB / 最小 161 KB / 最大 655 KB | `GET /derivatives/*` の Content-Length |
| 上記からの推定 | 全3,099枚で **約 1.0 GiB**（推定・移設コピー対象の下限） | 平均×枚数 |
| 写真配信帯域 | 切替10秒毎で 約276 kbps（34 KB/s）、7秒毎で約394 kbps | 平均サイズから算出 |

**解釈**: 「サーバーは別機体」にしても、写真配信は**LANにとって誤差**（有線1GbEでもWi-Fiでも足りる）。
重いのは表示機側ではなく、Driveからの原本取得とderivative生成（＝サーバー側の仕事）である。

## 3. 分離時に壊れる箇所と、その解き方

### 3.1 いちばん重要な構造的論点: 画面制御はサーバーホストで実行される

`GEOFRAME_SLEEP_CMD` / `GEOFRAME_WAKE_CMD` / `GEOFRAME_SCREEN_STATUS_CMD` は
`config.py` → `ha.py` から **`subprocess.Popen(cmd, shell=True)` で「サーバーが動いているホスト上で」実行**される。
サーバーをLXCへ移すと `xset` は表示機のXを叩けず、**画面スリープ・復帰と、ADR 0003 の照合（reconciliation）が丸ごと機能しなくなる**。

**推奨する解き方: 画面制御を表示側の責務へ移す。**
表示機に小さな `screen-agent`（10秒周期）を置き、`GET http://<server>:8080/api/presence` の
**読み取り専用・無認証のLAN内GET**から `desired` を読み、ローカルで `xset dpms force on|off` を適用する。
照合はエージェント自身が「命令した状態」と「実機の状態」を持つため、サーバー側の `GEOFRAME_SCREEN_STATUS_CMD` は未設定にする。

この形なら **表示機にDrive鍵もHAトークンも置かない**（既存の無認証GETをポーリングするだけ）ので、
ADR 0002/0003/0005 が守っている「受信面を増やさない」「新規credentials配布なし」と整合する。

**却下した代替案**:

| 案 | 却下理由 |
|---|---|
| LXCからPi400へSSHしてコマンド実行 | 秘密鍵の配布先が増える。表示機に新たな認証経路を作る |
| Pi400側にHTTP受信面を作りサーバーから叩く | ADR 0002 で却下した「無認証のmutating面」を新設する方向 |
| Pi400がHAを直接読む | HAトークンを表示機へ配布することになり ADR 0005 の方針に反する |

### 3.2 既に用意がある点（新規実装不要）

- `deploy/kiosk/kiosk.sh` は **`GEOFRAME_URL` で別機体のサーバーを指せる**（`deploy/README.md` §4）
- `backup-pull.sh` が**LXC向け**、`backup-pull-n100.sh` がSSH向けとして既にある
- Hostヘッダ検証（`app/geoframe/config.py`）は **IPリテラル宛を無条件に許可**する。
  `http://<server-ip>:8080/` で開くなら設定追加は不要。**ホスト名で開く場合のみ** `GEOFRAME_ALLOWED_HOSTS` への追加が必要
- 写真原本はDriveにのみ存在し、ローカルにはderivativeしか無い → 移設対象は「`catalog.db` ＋ `derivatives/` ＋ `tile_cache/` ＋ `hazard_cache/` ＋ `.cache.json`」で完結（`photos/` はほぼ空）

### 3.3 新しく必要になる要件: オフライン時の表示

サーバーが別機体（特にLXC）へ移ると、**その機体の再起動やLAN断で画面が「接続できません」になる**。
`rpi-cog-display-design.md` の DEGRADED は「サーバーは生きている」前提の縮退なので、分離構成では別途
**表示機側にローカルのフォールバック画面（最後の成功時刻と時計だけ）** を1枚用意する必要がある。

## 4. OS選定の根拠

- 本家配布ページで確認: **Raspberry Pi OS (64-bit) Lite = Debian 13 (trixie) ポート / kernel 6.18 / 対応機種に「400」**。
  Desktop/Fullはデスクトップ環境とアプリを同梱するため常設表示機には過剰（`rpi-cog-display-design.md` §6.4の初期対象外とも一致）。
- 表示ランタイム候補がarm64で揃うことをリポジトリで確認:
  - `cog` = **Debian trixie main**、arm64のモジュールに `libcogplatform-drm.so` / `-wl.so` / `-headless.so` が存在 → DRM直結の第一候補が実在する
  - `cage` = Raspberry Piリポジトリ trixie arm64 `0.2.0-2+rpt1`、`chromium` = 同 `152.0.7977.82` → Cage + Chromium の第二候補も自前ビルド無しで試せる
- 実機での初回確認コマンド: `uname -m`（aarch64）/ `apt-cache policy cog` / `ls /usr/lib/aarch64-linux-gnu/cog/modules/` / `kmsprint`
- 起動媒体は **USB3接続のSSD** を推奨（microSDやUSBメモリではなく）。キオスクは書き込み量こそ少ないが、
  journalとChromiumプロファイルで常時書き込みが発生する。`rpi-cog-display-design.md` §6.1/§7 のUSBブート手順をそのまま使う
- Raspberry Pi Imager の customization で hostname / ユーザー / SSH公開鍵 / locale(`ja_JP.UTF-8`) / timezone(`Asia/Tokyo`) を先に仕込む

## 5. 負荷測定プロトコル

### 5.1 道具（本ベースラインで追加）

| ファイル | 実行場所 | 役割 |
|---|---|---|
| [`deploy/kiosk/measure-display-node.sh`](https://github.com/ysogabe/geoframe/blob/display-final/deploy/kiosk/measure-display-node.sh) | 表示機 | RSS/CPU/温度/throttle/GPUクロック/DPMS/通信量/描画FPSをCSVで1行ずつ記録。Pi（`vcgencmd`あり）とx86の両方で動作 |
| [`tools/measure-display-client.mjs`](https://github.com/ysogabe/geoframe/blob/display-final/tools/measure-display-client.mjs) | 表示機 | CDP経由で描画FPS・フレーム時間分布・長時間タスク・WebGL renderer文字列・viewport・JSヒープを取得 |

### 5.2 事前条件（これを満たさない測定値は捨てる）

**「画面が出ていて描画が動いている」こと。** 具体的には `dpms=On` かつ `raf_fps > 0`。
画面Off中の値は「負荷が低い」ではなく「**止まっている**」であり、性能評価には使えない（§6参照）。

### 5.3 指標と判定閾値

`rpi-cog-display-design.md` §10.1 の目標を、そのまま測定指標にする。

| 指標 | 目標 | 取得元 |
|---|---|---|
| 通常HUDのフレームレート | 55 fps以上 | `raf_fps`（CDP） |
| 写真transition | 30 fps以上 | `raf_fps` + フレーム時間P95 |
| 常用CPU | 平均70%未満 | `display_cpu_pct` |
| 温度/throttle | throttlingなし | `temp_c` / `throttled`（`vcgencmd get_throttled`） |
| GPU経路 | software rasterizerでないこと | CDP `webgl.renderer` に `V3D`/`Mesa` が出ること（`llvmpipe`/`SwiftShader` は不合格） |
| 電源投入〜表示 | 60秒以内 | 手動計測（journalのタイムスタンプ） |
| 長時間安定性 | 24時間でRSS単調増加・描画停止なし | `display_rss_mb` の時系列 |

### 5.4 手順

1. **比較基準**: 現行のChromiumキオスク機（N100）で `measure-display-node.sh` を実行
2. **Pi400（第一候補・Cog + DRM直結）**: 同一ページを表示し同じ手順で測定
3. **Pi400（第二候補・Cage + Chromium）**: 第一候補で不合格の場合のみ
4. 最終表示器（2560×1080 / 21:9）で再測定
5. 合格した構成で24時間 → 7日の連続運転

## 6. 測定準備中に見つかった既存の不整合（要対応）

2026-09-13、比較基準を取ろうとした時点で次を観測した。

- `GET /api/presence` は `screen: "on"` / `present: true` / `ha_ok: true` / `last_motion` は数秒前 / `mode: "auto"` を返している
- しかし実機は `card0-HDMI-A-2` の **`dpms = Off`**、Chromiumのページは `visibilityState = "hidden"` で
  **rAFが1フレームも回っていない（3秒間で0フレーム）**、ChromiumプロセスのCPU時間は完全に横ばい
- `screen_confirmed` は `null` のまま＝**照合（reconciliation）が無効**なため、意図と実機の食い違いが検出されていない
  （`GEOFRAME_SCREEN_STATUS_CMD` 未設定時は照合自体が動かない。`deploy/README.md` §4 がN100実機での設定を推奨している箇所）

**影響**: この状態で測ると「CPU 0.2%・RSS 1.3GB」という**停止中の値**を性能値と誤認する。
測定手順に §5.2 の事前条件を必ず入れること。また ADR 0003 の「N100実機で照合を有効化」が未実施であることが
この観測で裏付けられたので、表示専用化より先に片付ける価値がある。

## 7. 未解決・確認待ち

1. サーバー移設先: `lxc-webapps` 同居か、GeoFrame専用LXCか（測定後に判断）
2. キオスクの接続先: IP直（設定変更ゼロ）か、ホスト名（`GEOFRAME_ALLOWED_HOSTS` 追加・caddy前段でTLS）
3. 画面ON/OFFのHA経由オーバーライド（ADR 0002/0003）を今回のスコープに含めるか
4. 2560×1080 のモニターの去就: 現行機からPi400へ移設するか、別モニターを付けるか
5. `/var/lib/geoframe` の実サイズ（要 `sudo du -sh /var/lib/geoframe/*`。本ベースラインでは未測定）

## 8. 既存文書との関係

- `rpi-cog-display-design.md`（2026-07-30・アーカイブ）= **Pi400単体でサーバーと表示の両方を持つ案**。
  4GBにサーバー（Drive同期・derivative生成・AI選定・地図タイル）と表示を同居させる前提だった。
- 本ベースライン = その**表示側だけをPi400へ**出し、サーバーは当面現行機に残す版。
  サーバーをLXCへ移す判断は §5 の測定結果を待つ。
