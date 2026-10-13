# ADR: GEOFRAMEを写真管理基盤にし、表示はmiharashiへ移す

- 日付: 2026-10-13
- 状態: 採用

## 文脈

GEOFRAMEは写真カタログとWeb表示（キオスク・地図・気象・地震・航空機・室内環境・
エージェント稼働状況）を1つのサーバーで持っていた。その後、DRM/KMSへ直接描画する
[miharashi](https://github.com/ysogabe/miharashi)が実機の表示を担うようになり、
Chromiumキオスクは使われなくなった。画面電源制御はXの`xset`に依存していたため、
X無しで動くmiharashiの構成では機能していなかった。

表示の実装が2か所に分かれたまま、どちらにも手を入れる状態は保守の負担になる。

## 決定

- GEOFRAMEは写真管理基盤とする。カタログ、取り込み（ローカル・Google Drive）、表示用画像、
  品質解析、AIによる検索・整理、位置修正などの管理画面、旅行ブログを残す。
- 表示はmiharashiに集約する。GEOFRAMEとの接点は[写真フィードの契約](../architecture/photo-feed-contract.md)
  （`/api/v1/photos`と`/derivatives/`）だけにする。
- 次の機能はGEOFRAMEから削除し、必要なものはmiharashiの画面として作り直す。データの
  取得もmiharashi側に置く。
  - Webキオスク、ネイティブ表示（C++/SDL2）、macOS WKWebViewクライアント、3Dフォトジャーニー
  - Hermesエージェント稼働状況（連携そのものを廃止）
  - キー割当、画面電源制御とHome Assistant連携
  - 気象庁（天気・地震）、ADS-B航空機、国土交通省（防災・周辺情報）
- 削除直前の状態は`display-final`タグで参照できるようにし、移植時の参照に使う。
- 外部からのアクセスは、作り直しが済んだ後にCloudflare Tunnelで公開し、認証は
  Cloudflare Accessで行う。

## 結果

- 表示に関するADR 0002〜0007は廃止とした。
- 管理ページの位置修正に使う背景地図タイル（OpenStreetMap・地理院の淡色地図）の
  プロキシは残す。
- トップページ（`/`）は管理ページを返す。写真ライブラリの画面は後続の変更で作る。
