/* ============================================================
   modes.js — 表示モードラベルの一元化（#mode 要素の唯一の writer）
   ※読み込み順が契約: config.js/util.js/data.js の後、events.js/panels.js/
     scheduler.js より前（後者は実行時に setMode を呼ぶだけなので順序は緩い）
   宣言する主なグローバル: setMode, pickMode, MODE_PRIORITY
   依存する他ファイルのグローバル: なし
   ============================================================
   従来は events.js（5箇所）・panels.js（4箇所）・scheduler.js（3箇所）の
   計12箇所が `document.getElementById('mode').textContent = 'MODE: …'` を
   直接書き換えていた（創発的な状態）。本モジュールを唯一の writer にし、
   既存の表示ラベル（TRANSIT / PHOTO JOURNEY / INTERCEPT / MEMORY LINK /
   SEISMIC WATCH / SLEEP / TRIP REPLAY / TRAVEL STATS / WEEKLY FORECAST /
   ON THIS DAY）はそのまま引き継ぐ。

   受け入れ条件: `git grep -n "getElementById('mode')" app/static/js/` の
   一致が本ファイル以外で0件になること。
   ============================================================ */
const MODE_EL_ID = 'mode';

/* `MODE: ` 接頭辞を付けて #mode へ書き込む唯一の関数。label は接頭辞より後ろの
   文字列（例: setMode('TRANSIT ▸ タイトル') → 'MODE: TRANSIT ▸ タイトル'）。 */
function setMode(label) {
  const el = document.getElementById(MODE_EL_ID);
  if (el) el.textContent = 'MODE: ' + label;
  return label;
}

/* ADR 0004 の優先度ラダー（SLEEP > ALERT > PINNED > AMBIENT > ROTATION）の明文化。
   既存の呼び出し箇所は、各イベントが自分の優先度に応じて setMode を呼ぶ順序
   （eventUntil / sleeping ガード）で既にラダーを実現しているため、ここは「判定の
   一元化」ではなく「優先度の文書化された参照点」として持つ。将来の新モード
   （NIGHT CLOCK / AGENTS 等）は pickMode を唯一の判定点へ昇格させる。 */
const MODE_PRIORITY = ['SLEEP', 'ALERT', 'PINNED', 'AMBIENT', 'ROTATION'];

function pickMode(state) {
  const s = state || {};
  if (s.sleep) return 'SLEEP';
  if (s.alert) return 'ALERT';
  if (s.pinned) return 'PINNED';
  if (s.ambient) return 'AMBIENT';
  return 'ROTATION';
}
