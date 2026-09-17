#!/usr/bin/env bash
#
# measure-display-node.sh — 表示ノード（キオスク機）の負荷を一定時間サンプリングする
#
# 目的:
#   「サーバーは別機体、表示のみ」構成へ移す判断の材料として、表示機の
#   CPU/RSS/温度/throttle/GPUクロック/画面出力状態/通信量を同じ手順で記録する。
#   Raspberry Pi（vcgencmd あり）と x86 ミニPC（vcgencmd なし）の両方で動く。
#   N100実機の測定結果を「比較基準（control）」として、Pi400の測定値と並べる。
#
# 使い方:
#   ./measure-display-node.sh                       # 10秒×6回（既定）
#   ./measure-display-node.sh --duration 300 --interval 10 --out /tmp/kiosk-pi400.csv
#   ./measure-display-node.sh --procs 'cog'         # Cog構成のとき
#   ./measure-display-node.sh --procs 'chromium' --cdp-port 9222   # 描画FPSも取る
#
# 出力: CSV（1サンプル=1行）。--out 省略時は標準出力。
#   ts,epoch,n,display_rss_mb,display_cpu_pct,dpms,temp_c,throttled,v3d_mhz,core_mhz,rx_kb_s,tx_kb_s,raf_fps
#
# 事前条件（測定の妥当性）:
#   このスクリプトは「画面が出ていて描画が動いている」ことを前提にする。
#   dpms=Off や raf_fps=0（raf-stalled）の行は、負荷が低いのではなく
#   **止まっている**だけなので、その回の測定値は採用しないこと。
#
# 依存: 標準の procps/bash のみ。vcgencmd は Raspberry Pi のみ。
#       --cdp-port を指定した場合のみ node が必要（tools/measure-display-client.mjs）。
#
set -uo pipefail

DURATION=60
INTERVAL=10
PROCS="${PROCS:-chromium|cog}"
OUT=""
CDP_PORT=""
CDP_SCRIPT="$(cd "$(dirname "$0")/../.." 2>/dev/null && pwd)/tools/measure-display-client.mjs"
DRM_CONNECTOR="${DRM_CONNECTOR:-}"

while [ $# -gt 0 ]; do
    case "$1" in
        --duration) DURATION="$2"; shift 2 ;;
        --interval) INTERVAL="$2"; shift 2 ;;
        --procs)    PROCS="$2"; shift 2 ;;
        --out)      OUT="$2"; shift 2 ;;
        --cdp-port) CDP_PORT="$2"; shift 2 ;;
        --drm)      DRM_CONNECTOR="$2"; shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

HZ="$(getconf CLK_TCK 2>/dev/null || echo 100)"

# 表示コネクタの自動検出（connected な最初のもの）。DPMS状態の読み出しに使う。
if [ -z "$DRM_CONNECTOR" ]; then
    for c in /sys/class/drm/card*-*/; do
        if [ "$(cat "$c/status" 2>/dev/null)" = "connected" ]; then
            DRM_CONNECTOR="$(basename "$c")"; break
        fi
    done
fi

NET_IF="$(ip route get 1.1.1.1 2>/dev/null | sed -n 's/.*dev \([^ ]*\).*/\1/p' | head -1)"

emit() { printf '%s\n' "$1"; [ -n "$OUT" ] && printf '%s\n' "$1" >> "$OUT"; }

log() { echo "[measure-display-node] $*" >&2; }

# --- ヘッダ ---
log "duration=${DURATION}s interval=${INTERVAL}s procs='${PROCS}' connector=${DRM_CONNECTOR:-none} net=${NET_IF:-unknown} out=${OUT:-stdout}"
[ -n "$OUT" ] && mkdir -p "$(dirname "$OUT")"
emit "ts,epoch,n,display_rss_mb,display_cpu_pct,dpms,temp_c,throttled,v3d_mhz,core_mhz,rx_kb_s,tx_kb_s,raf_fps"

pids() { pgrep -f "$PROCS" 2>/dev/null | tr '\n' ' '; }
sum_rss_kb() { local p; p="$(pids)"; [ -z "$p" ] && { echo 0; return; }; ps -o rss= -p $(echo $p | tr ' ' ',') 2>/dev/null | awk '{s+=$1} END{print s+0}'; }
sum_ticks() { local tot=0 f; for f in $(pids); do [ -r "/proc/$f/stat" ] || continue; tot=$((tot + $(awk '{print $14+$15}' "/proc/$f/stat" 2>/dev/null || echo 0))); done; echo "$tot"; }
vcm() { command -v vcgencmd >/dev/null 2>&1 && vcgencmd "$@" 2>/dev/null | sed 's/.*=//' || echo ""; }
net_kb() { [ -z "$NET_IF" ] && { echo "0 0"; return; }; awk -v i="$NET_IF:" '$1==i{print int($2/1024), int($10/1024)}' /proc/net/dev; }

# 事前条件チェック（画面が出ていない場合に黙って測らない）
if [ -n "$DRM_CONNECTOR" ] && [ "$(cat /sys/class/drm/$DRM_CONNECTOR/dpms 2>/dev/null)" = "Off" ]; then
    log "WARNING: $DRM_CONNECTOR の dpms=Off。画面が消灯している間の値は「低負荷」ではなく「停止」なので採用しないこと"
fi
if [ "${PROCS}" = "chromium" ] && [ -z "$CDP_PORT" ]; then
    log "WARNING: --cdp-port 未指定。描画が生きているか(raf_fps)は確認できない"
fi

prev_ticks="$(sum_ticks)"; prev_net="$(net_kb)"
elapsed_total=0
while [ "$elapsed_total" -lt "$DURATION" ]; do
    sleep "$INTERVAL"; elapsed_total=$((elapsed_total + INTERVAL))

    rss_kb="$(sum_rss_kb)"
    n="$(pids | wc -w)"
    ticks="$(sum_ticks)"
    dpms="$(cat /sys/class/drm/$DRM_CONNECTOR/dpms 2>/dev/null || echo '')"
    temp="$(vcm measure_temp | sed 's/[^0-9.]//g')"
    thr="$(vcm get_throttled)"
    v3d="$(vcm measure_clock v3d)"; core="$(vcm measure_clock core)"
    read -r rx tx <<<"$(net_kb)"
    rx_s=$(( (rx - ${prev_net%% *}) / INTERVAL )); tx_s=$(( (tx - ${prev_net##* }) / INTERVAL ))
    cpu_pct="$(awk -v d="$((ticks - prev_ticks))" -v hz="$HZ" -v iv="$INTERVAL" 'BEGIN{printf "%.1f", (d/hz)/iv*100}')"

    raf=""
    if [ -n "$CDP_PORT" ] && [ -f "$CDP_SCRIPT" ] && command -v node >/dev/null 2>&1; then
        raf="$(node "$CDP_SCRIPT" --port "$CDP_PORT" --seconds 3 2>/dev/null \
               | sed -n 's/.*"fpsMean": *\([0-9.]*\).*/\1/p' | head -1)"
        [ -z "$raf" ] && raf="stalled"
    fi

    emit "$(date +%H:%M:%S),$(date +%s),$n,$((rss_kb/1024)),${cpu_pct:-},${dpms},${temp},${thr},${v3d},${core},${rx_s},${tx_s},${raf}"
    prev_ticks="$ticks"; prev_net="$rx $tx"
done

log "done. $( [ -n "$OUT" ] && echo "saved: $OUT" || echo "stdout only" )"
