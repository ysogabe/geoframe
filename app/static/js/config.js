/* ============================================================
   config.js — 起動時の基本設定・全体で使う定数
   ※読み込み順が契約: このファイルを最初に読むこと（他の全ファイルが依存）
   宣言する主なグローバル: LOW_POWER, DWELL_MS, FLY_MS,
                           NIGHT_FROM_H, NIGHT_TO_H
   依存する他ファイルのグローバル: なし
   ============================================================ */
/* ============================================================
   Phase 1: server.py 起動時は /api/photos から実写真（EXIF自動抽出）を読む。
   file:// で直接開いた場合やサーバー無しの場合は下のモックデータで動く。

   軽量モード: URLに ?low を付けると RPi400 等の非力なGPU向けに
   アニメーションを簡略化する（pitch無し・飛行時間短縮・パルス削減）
   例: chromium --kiosk "http://localhost:8080/?low"
   ============================================================ */
const LOW_POWER = new URLSearchParams(location.search).has('low');
const DWELL_MS = 16000;      // 1地点の滞在時間
const FLY_MS   = LOW_POWER ? 3200 : 6500;   // 移動アニメーション時間

/* ナイトモード（減光）の時刻フォールバック。
   Home Assistant の sun.sun が取れるときは /api/presence の night を優先し、
   HA未設定・取得失敗（night が null）のときだけこの固定時刻で判定する */
const NIGHT_FROM_H = 22;   // この時刻以降は夜
const NIGHT_TO_H   = 6;    // この時刻より前は夜
