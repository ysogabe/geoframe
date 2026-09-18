#!/usr/bin/env python3
"""Publish a non-sensitive Hermes heartbeat summary as an atomic status drop."""
import json
import os
import tempfile
import time
from pathlib import Path

SOURCE = Path(os.environ.get("HERMES_HEARTBEAT_FILE", "/home/hermes/.hermes/state/gateway.heartbeat"))
DEST = Path(os.environ.get("GEOFRAME_AGENT_STATUS_DIR", "/run/geoframe-agent-status")) / "hermes.json"
INTERVAL = 10
HEARTBEAT_MAX_AGE = 30
ALLOWED_TASKS = {"idle", "coding", "testing", "review", "research", "maintenance", "unknown"}
ALLOWED_STATES = {"live", "idle", "busy", "error", "unknown"}


def _number(value, default=0):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and value >= 0 else default


def _unknown_summary(now):
    return {"id": "hermes", "published_at": now, "state": "unknown",
            "task_kind": "unknown", "metrics": [], "hb_age_s": 0}


def read_source(now=None):
    now = time.time() if now is None else now
    try:
        st = SOURCE.stat()
        with SOURCE.open(encoding="utf-8") as fp:
            raw = json.load(fp)
    except (OSError, ValueError, TypeError):
        return _unknown_summary(now)
    hb_age_s = max(0, int(now - st.st_mtime))
    if not isinstance(raw, dict):
        raw = {}
    task = raw.get("task_kind", "idle")
    state = raw.get("state", "live" if hb_age_s <= HEARTBEAT_MAX_AGE else "unknown")
    task = task if task in ALLOWED_TASKS else "unknown"
    state = state if state in ALLOWED_STATES else "unknown"
    metrics = []
    for name in ("queue", "active", "completed", "errors", "uptime_s"):
        if name in raw:
            metrics.append({"name": name, "value": _number(raw[name])})
    return {"id": "hermes", "published_at": st.st_mtime,
            "state": state, "task_kind": task, "metrics": metrics, "hb_age_s": hb_age_s}


def publish(summary):
    if summary is None:
        return False
    DEST.parent.mkdir(parents=True, exist_ok=True)
    fd, temp = tempfile.mkstemp(prefix=".hermes.", suffix=".tmp", dir=DEST.parent)
    os.fchmod(fd, 0o640)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fp:
            json.dump(summary, fp, ensure_ascii=False, separators=(",", ":"))
            fp.flush()
            os.fsync(fp.fileno())
        os.replace(temp, DEST)
        return True
    finally:
        try:
            os.unlink(temp)
        except FileNotFoundError:
            pass


def main():
    while True:
        publish(read_source())
        time.sleep(INTERVAL)


if __name__ == "__main__":
    main()
