# GEOFRAME macOS WKWebViewクライアント

既存GeoFrame Webアプリを表示するだけの薄いmacOSアプリ。HTML、API、写真処理、Home Assistant連携は
`app/`側に置き、Macアプリには複製しない。

## 位置づけ（どの企画に属するか）

これは **Cog/WPE + RPi400 企画（[`docs/archive/rpi-cog-display-design.md`](../docs/archive/rpi-cog-display-design.md)）の Mac 上での
先行検証**であり、[`docs/archive/native-display-concept.md`](../docs/archive/native-display-concept.md)（C++ Core + SDL、Tab5 移植を目的とする）
とは**別系統**である。両者を混同しないこと。

- 本ディレクトリ: WKWebView で既存 Web UI を出す。最終ターゲットは Pi 400 の WPE WebKit/DRM
- `native-display/`（MacSDL 企画）: ブラウザを使わない C++ Core + SDL Renderer。最終ターゲットは M5Stack Tab5

## 必要環境

- macOS 13以降
- Xcode Command Line ToolsまたはXcode
- 起動済みのGeoFrame server（既定: `http://127.0.0.1:8080/`）

## 開発実行

先に別terminalでserverを起動する。

```bash
cd /path/to/geoframe/app
source .venv/bin/activate
python server.py
```

ウィンドウ表示でアプリを起動する。

```bash
cd /path/to/geoframe/mac-wkwebview
swift run GeoFrameMac --windowed --developer
```

serverがまだ起動していない場合は待機画面を表示し、1秒間隔で再試行する。

## `.app`の作成

```bash
cd /path/to/geoframe/mac-wkwebview
./build-app.sh
open dist/GeoFrame.app
```

`dist/GeoFrame.app`はad-hoc署名したローカル実行用appである。第三者配布用のDeveloper ID署名、notarization、
Python serverの同梱は行わない。

## 設定

| 環境変数 | 既定値 | 内容 |
|---|---|---|
| `GEOFRAME_URL` | `http://127.0.0.1:8080/` | 表示URL |
| `GEOFRAME_FULLSCREEN` | `1` | 起動時fullscreen |
| `GEOFRAME_HIDE_CURSOR` | `1` | fullscreen時cursorを隠す |
| `GEOFRAME_DEVELOPER` | `0` | Web Inspector対象にする |

command lineでは`--url URL`、`--windowed`、`--fullscreen`、`--show-cursor`、`--developer`を指定できる。

```bash
swift run GeoFrameMac --windowed --url 'http://127.0.0.1:8080/?low'
```

Finderから`.app`を開く場合は既定値を使用する。URL等を変更して常用する場合は、LaunchAgent側で環境変数を
渡す構成を後続の運用フェーズで追加する。

## 操作

- `command + R`: 再読み込み
- `control + command + F`: fullscreen切替
- `command + Q`: 終了

## 責務境界

- Mac appはPython serverを起動・停止しない。
- Web画面とAPIは既存`app/`を正とする。
- WKWebViewでの成功は、Pi 400上のWPE WebKit/DRMでの成功を保証しない。
