#!/usr/bin/env python3
"""毎朝、位置なし写真の撮影地の手がかり判定に必要なあいだだけ Decision を動かす。

geoframe-loc-decision.timer（Decision を動かすユーザーのユーザーunit、毎朝5:00）から呼ばれる。

1. geoframe の STATUS で判定待ち（db.loc.clue_pending）が無ければ何もしない
2. 人が Decision（llm-decision）を起動済みなら何もしない（止めるのも人に任せる）
3. GPU が空いていなければ何もしない（翌朝もう一度見る）
4. Decision を起動し、判定待ちが0になったら止める。上限の時間を過ぎた・判定が
   進まない・別の処理が GPU を使い始めたときも止める（続きは翌朝）

なお gpu-eval の Decision は今は常駐しているので、現状では 2. で「人が起動済み」と
みなして何もしない（常駐をやめて必要なあいだだけ起動する運用に戻したときのために残している）。
Decision への確認は同じ機体の LAN 入口（既定 `http://127.0.0.1:8100/llm-decision`）経由で、
`X-Client-Name: geoframe-loc-morning` を付ける。

判定そのものと Gemini への問い合わせは geoframe 本体（ai_worker）が1分ごとに行う。
Gemini は GPU を使わないので、判定が終われば Decision を止めてよい。

llm-manager は誰かが GPU の lease を取ると、動いている常駐（Decision を含む）を止め、
lease が0に戻ると猶予の後に起動し直す。このスクリプトが起動した Decision がそうして
戻されても動き続けないよう、起動したら印（OWNED_FILE）を残し、印のある Decision は
人が起動したものとみなさずに引き継いで止める。

標準ライブラリだけで書く（そのユーザーのシステムPythonで動かすため）。
"""
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path

GEOFRAME_STATUS_URL = os.environ.get(
    "LOC_MORNING_GEOFRAME_STATUS_URL", "http://127.0.0.1:8080/api/admin/status")
LLM_MANAGER_URL = os.environ.get("LOC_MORNING_LLM_MANAGER_URL", "http://127.0.0.1:8096")
OLLAMA_URL = os.environ.get("LOC_MORNING_OLLAMA_URL", "http://127.0.0.1:11434")
DECISION_URL = os.environ.get("LOC_MORNING_DECISION_URL", "http://127.0.0.1:8100/llm-decision")
DECISION_CLIENT_NAME = "geoframe-loc-morning"   # 入口が呼び出し元ごとに数えるための名前
DECISION_UNIT = os.environ.get("LOC_MORNING_DECISION_UNIT", "llm-decision")
DECISION_PROFILE = "decision"     # llm-manager のプロファイルid（lease.paused に出る名前）
STATE_DIR = Path(os.environ.get("STATE_DIRECTORY")
                 or Path.home() / ".local/state/geoframe-loc-decision")
OWNED_FILE = STATE_DIR / "decision-owned"

# GPU が空いているとみなす条件（どれか1つでも外れたら使わない）
GPU_VRAM_FREE_BELOW_MIB = 1024    # ほかの処理が使っている VRAM がこれ未満
GPU_UTIL_FREE_BELOW_PCT = 10      # GPU 使用率がこれ未満
STATUS_FRESH_S = 30               # llm-manager の状態がこれより古ければ信用しない

READY_TIMEOUT_S = 180     # 起動から応答まで（実測4〜37秒）
RUN_LIMIT_S = 2 * 3600    # 1回の上限
POLL_S = 60               # geoframe は1分ごとに判定する
STALL_S = 15 * 60         # 判定待ちが減らないまま、これだけ経ったら諦める


def log(msg: str) -> None:
    print(f"[loc-morning] {msg}", flush=True)


def get_json(url: str, timeout: float = 5.0, headers: dict | None = None):
    req = urllib.request.Request(url, headers=headers or {})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode("utf-8"))


def clue_pending() -> int:
    return int(get_json(GEOFRAME_STATUS_URL)["db"]["loc"]["clue_pending"])


def gpu_busy_reasons(status: dict | None, ollama_models: list | None,
                     now: float | None = None, own_pids=(), strict=True) -> list[str]:
    """GPU を使ってはいけない理由の一覧。空なら使ってよい。

    status は llm-manager の /status、ollama_models は Ollama の /api/ps の models。
    strict=True（起動前）は、取れなかった情報も「空いていると確かめられない」として
    理由に数える。strict=False（判定中）は、ほかの処理が使っている証拠だけを数える
    （1回の取りこぼしで朝の判定を打ち切らないため）。
    own_pids の GPU プロセス（自分で起動した Decision）は数えない。"""
    reasons = []
    if status is None:
        return ["llm-manager の状態を取れない"] if strict else []
    generated = status.get("generated_at")
    if strict and now is not None and generated:
        try:
            age = now - datetime.fromisoformat(generated).timestamp()
            if age > STATUS_FRESH_S:
                reasons.append(f"llm-manager の状態が古い（{age:.0f}秒前）")
        except ValueError:
            reasons.append("llm-manager の状態の時刻を読めない")
    lease = (status.get("lease") or {}).get("count", 0)
    if lease:
        reasons.append(f"GPU の lease が {lease} 件ある")
    gpu = status.get("gpu")
    if not gpu:
        if strict:
            reasons.append("GPU の状態を取れない")
    else:
        others = [p for p in gpu.get("processes") or [] if p.get("pid") not in own_pids]
        other_vram = sum(int(p.get("vram_mib") or 0) for p in others)
        if others and other_vram >= GPU_VRAM_FREE_BELOW_MIB:
            names = ", ".join(f"{p.get('name')}({p.get('vram_mib')}MiB)" for p in others)
            reasons.append(f"ほかの処理が GPU を使っている: {names}")
        # 判定中の使用率は Decision 自身なので、起動前だけ見る
        if not own_pids and int(gpu.get("util_pct") or 0) >= GPU_UTIL_FREE_BELOW_PCT:
            reasons.append(f"GPU 使用率が {gpu.get('util_pct')}%")
    if ollama_models is None:
        if strict:
            reasons.append("Ollama の状態を取れない")
    elif ollama_models:
        reasons.append("Ollama がモデルを読み込んでいる: "
                       + ", ".join(str(m.get("name")) for m in ollama_models))
    return reasons


def paused_by_manager(status: dict | None) -> bool:
    """llm-manager が lease のために Decision を止めていて、後で起動し直す予定か"""
    return DECISION_PROFILE in ((status or {}).get("lease") or {}).get("paused", [])


def read_gpu_state():
    try:
        status = get_json(LLM_MANAGER_URL + "/status")
    except (urllib.error.URLError, OSError, ValueError):
        status = None
    try:
        models = get_json(OLLAMA_URL + "/api/ps").get("models") or []
    except urllib.error.URLError as exc:
        # Ollama が止まっている（接続拒否）ならモデルも無い。それ以外は分からない扱い
        refused = isinstance(exc.reason, ConnectionRefusedError)
        models = [] if refused else None
    except (OSError, ValueError):
        models = None
    return status, models


def systemctl(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True)


def decision_active() -> bool:
    return systemctl("is-active", "--quiet", DECISION_UNIT).returncode == 0


def decision_pid() -> int | None:
    out = systemctl("show", "-p", "MainPID", "--value", DECISION_UNIT).stdout.strip()
    return int(out) if out.isdigit() and out != "0" else None


def decision_ready() -> bool:
    try:
        return get_json(DECISION_URL + "/health", timeout=2,
                        headers={"X-Client-Name": DECISION_CLIENT_NAME}).get("status") == "ready"
    except (urllib.error.URLError, OSError, ValueError):
        return False


def owned() -> bool:
    return OWNED_FILE.exists()


def set_owned(on: bool) -> None:
    if on:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
        OWNED_FILE.write_text(datetime.now().isoformat(timespec="seconds") + "\n")
    else:
        OWNED_FILE.unlink(missing_ok=True)


def stop_decision() -> None:
    """自分の Decision を止める。llm-manager が後で起動し直す予定なら印を残す。

    is-active を見てから止めると、llm-manager が起動し直している途中（activating）の
    Decision を見逃すので、状態にかかわらず stop を送る（止まっているunitには何もしない）"""
    r = systemctl("stop", DECISION_UNIT)
    if r.returncode != 0:
        log(f"Decision の停止に失敗: {r.stderr.strip()}")
        return
    log("Decision を停止した")
    status, _ = read_gpu_state()
    if paused_by_manager(status):
        log("llm-manager が lease の後に Decision を起動し直す予定。次に見たときに止める")
        return
    set_owned(False)


def wait_for_restore(sleep, clock, deadline: float) -> None:
    """llm-manager が一時停止した Decision を戻すまで待ち、戻ったら止める"""
    log("ほかの処理の lease で llm-manager が Decision を一時停止した。戻ったら止める")
    while clock() < deadline:
        sleep(POLL_S)
        status, _ = read_gpu_state()
        if status is not None and not paused_by_manager(status):
            return       # 戻された（または一時停止が解かれた）。呼び出し元の finally で止める
    log("上限の時間までに lease が終わらない。次に見たときに止める")


def run(sleep=time.sleep, clock=time.monotonic) -> int:
    try:
        pending = clue_pending()
    except (urllib.error.URLError, OSError, ValueError, KeyError) as exc:
        log(f"geoframe の STATUS を取れない: {exc}")
        return 1
    mine = owned()
    active = decision_active()
    if active and not mine:
        log(f"判定待ち {pending} 枚。Decision は起動済み（人が起動）なので何もしない")
        return 0
    if pending == 0:
        log("判定待ちの写真は無い")
        if mine:
            stop_decision()      # 前回このスクリプトが起動し、llm-manager が戻したもの
        return 0

    start = clock()
    ours = mine        # finally で止めるのは、自分が起動した（または引き継いだ）ものだけ
    try:
        if active:
            log(f"判定待ち {pending} 枚。前回起動した Decision（llm-manager が戻したもの）を引き継ぐ")
        else:
            status, models = read_gpu_state()
            reasons = gpu_busy_reasons(status, models, now=time.time())
            if reasons:
                log(f"判定待ち {pending} 枚。GPU が空いていないので翌朝に回す: "
                    + " / ".join(reasons))
                return 0
            log(f"判定待ち {pending} 枚。Decision を起動する")
            set_owned(True)
            ours = True
            r = systemctl("start", DECISION_UNIT)
            if r.returncode != 0:
                log(f"Decision を起動できない: {r.stderr.strip()}")
                return 1
        while not decision_ready():
            if clock() - start > READY_TIMEOUT_S:
                log(f"Decision が {READY_TIMEOUT_S} 秒たっても応答しない")
                return 1
            sleep(2)
        log(f"Decision が応答した（{clock() - start:.0f}秒）")
        own = {decision_pid()} - {None}
        last_progress, best = clock(), pending
        while True:
            sleep(POLL_S)
            try:
                pending = clue_pending()
            except (urllib.error.URLError, OSError, ValueError, KeyError) as exc:
                log(f"geoframe の STATUS を取れない（続ける）: {exc}")
                pending = best
            if pending == 0:
                log("判定待ちが0になった")
                return 0
            if pending < best:
                best, last_progress = pending, clock()
            if clock() - start > RUN_LIMIT_S:
                log(f"上限の {RUN_LIMIT_S // 60} 分を過ぎた。残り {pending} 枚は翌朝")
                return 0
            if clock() - last_progress > STALL_S:
                log(f"{STALL_S // 60} 分間判定が進まない。残り {pending} 枚は翌朝")
                return 1
            status, models = read_gpu_state()
            reasons = gpu_busy_reasons(status, models, own_pids=own, strict=False)
            if reasons:
                log(f"ほかの処理が GPU を使い始めたので譲る（残り {pending} 枚は翌朝）: "
                    + " / ".join(reasons))
                if paused_by_manager(status):
                    wait_for_restore(sleep, clock, start + RUN_LIMIT_S)
                return 0
    finally:
        if ours:
            stop_decision()


def main() -> int:
    # systemctl stop などで止められても Decision を止めてから終わる（finally を通す）
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    return run()


if __name__ == "__main__":
    sys.exit(main())
