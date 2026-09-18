"""Read-only, privacy-preserving agent status drops."""
import json
import math
import os
import re
import stat
import sys
import time
from pathlib import Path

from .config import AGENT_LABELS, AGENT_STATUS_DIR

MAX_BYTES = 64 * 1024
LIVE_MAX_S = 30
STALE_MAX_S = 120
MAX_CARDS = 2
STATES = frozenset(("live", "idle", "busy", "error", "unknown"))
TASK_KINDS = frozenset(("idle", "coding", "testing", "review", "research", "maintenance", "unknown"))
_last_good = {}
_unconfigured_drop_warning_emitted = False
_PROFILE_DROP_ID = re.compile(r"^hermes-[a-z0-9][a-z0-9-]{0,31}$")


def _configured_agents(entries):
    ids = []
    labels = {}
    for entry in entries:
        if "=" not in entry:
            continue
        agent_id, label = (part.strip() for part in entry.split("=", 1))
        if not agent_id or not label or agent_id in labels:
            continue
        # The id becomes a filename; reject path components from configuration.
        if Path(agent_id).name != agent_id or agent_id in {".", ".."}:
            continue
        ids.append(agent_id)
        labels[agent_id] = label
    return tuple(ids), labels


AGENT_IDS, LABELS = _configured_agents(AGENT_LABELS)


def _finite_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def _valid_drop(obj):
    if not isinstance(obj, dict):
        return None
    required = {"id", "published_at", "state", "task_kind", "metrics"}
    if set(obj) != required and not required.issubset(obj):
        return None
    if obj.get("id") not in AGENT_IDS or not _finite_number(obj.get("published_at")):
        return None
    if not isinstance(obj.get("state"), str) or obj["state"] not in STATES:
        return None
    if not isinstance(obj.get("task_kind"), str) or obj["task_kind"] not in TASK_KINDS:
        return None
    metrics = obj.get("metrics")
    if not isinstance(metrics, list) or len(metrics) > 8:
        return None
    for metric in metrics:
        if not isinstance(metric, dict) or set(metric) != {"name", "value"}:
            return None
        if metric["name"] not in {"queue", "active", "completed", "errors", "uptime_s"}:
            return None
        if not _finite_number(metric["value"]) or metric["value"] < 0:
            return None
    return {"id": obj["id"], "published_at": float(obj["published_at"]),
            "state": obj["state"], "task_kind": obj["task_kind"],
            "metrics": [{"name": m["name"], "value": m["value"]} for m in metrics]}


def _read_drop(path):
    """Read one drop without following links and without unbounded allocation."""
    try:
        st = os.lstat(path)
        if not stat.S_ISREG(st.st_mode) or st.st_size > MAX_BYTES:
            return None
        flags = (os.O_RDONLY | os.O_NONBLOCK |
                 getattr(os, "O_NOFOLLOW", 0))
        fd = os.open(path, flags)
        try:
            opened = os.fstat(fd)
            if not stat.S_ISREG(opened.st_mode) or opened.st_size > MAX_BYTES:
                return None
            data = os.read(fd, MAX_BYTES + 1)
        finally:
            os.close(fd)
        if len(data) > MAX_BYTES:
            return None
        return _valid_drop(json.loads(data.decode("utf-8")))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, TypeError, ValueError):
        return None


def _state(published_at, now):
    age = now - published_at
    if age < -STALE_MAX_S or age > STALE_MAX_S:
        return "unknown" if age > STALE_MAX_S else "stale"
    return "live" if age <= LIVE_MAX_S else "stale"


def _public(item, now):
    age = max(0, int(now - item["published_at"]))
    state = "unknown" if item["state"] == "unknown" else _state(item["published_at"], now)
    return {"id": item["id"], "label": LABELS[item["id"]], "state": state,
            "age_s": age, "task_kind": item["task_kind"], "metrics": item["metrics"]}


def _warn_for_unconfigured_drops(directory):
    """Warn once when discovery produced a valid-looking, unconfigured profile drop."""
    global _unconfigured_drop_warning_emitted
    if _unconfigured_drop_warning_emitted:
        return
    try:
        entries = tuple(Path(directory).iterdir())
    except OSError:
        return
    for entry in entries:
        name = entry.name
        agent_id = name[:-5] if name.endswith(".json") else ""
        if _PROFILE_DROP_ID.fullmatch(agent_id) and agent_id not in AGENT_IDS:
            print(f"[agents] drop for unconfigured id '{agent_id}' ignored; "
                  "add it to GEOFRAME_AGENT_LABELS", file=sys.stderr, flush=True)
            _unconfigured_drop_warning_emitted = True
            return


def read_agents(directory=AGENT_STATUS_DIR, now=None):
    now = time.time() if now is None else float(now)
    _warn_for_unconfigured_drops(directory)
    cards = []
    for order, agent_id in enumerate(AGENT_IDS):
        item = _read_drop(Path(directory) / f"{agent_id}.json")
        if item is not None and item["id"] == agent_id:
            _last_good[agent_id] = item
        else:
            item = None
        item = _last_good.get(agent_id)
        if item is not None:
            public = _public(item, now)
            cards.append((public, item["published_at"], order))
    cards.sort(key=lambda card: (card[0]["state"] != "live", -card[1], card[2]))
    result = [card[0] for card in cards[:MAX_CARDS]]
    return {"agents": result, "rotation_allowed": any(a["state"] == "live" for a in result)}


def render_agents(directory=AGENT_STATUS_DIR, now=None):
    return read_agents(directory, now)
