#!/usr/bin/env python3
"""Publish a non-sensitive Hermes heartbeat summary as an atomic status drop."""
import json
import os
import stat
import sys
import tempfile
import time
from pathlib import Path

HEARTBEAT_RELATIVE = ("state", "gateway.heartbeat")
DEST = Path(os.environ.get("GEOFRAME_AGENT_STATUS_DIR", "/run/geoframe-agent-status")) / "hermes.json"
INTERVAL = 10
HEARTBEAT_MAX_AGE = 30
ALLOWED_TASKS = {"idle", "coding", "testing", "review", "research", "maintenance", "unknown"}
ALLOWED_STATES = {"live", "idle", "busy", "error", "unknown"}


def _number(value, default=0):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and value >= 0 else default


def hermes_home():
    """``HERMES_HOME`` when set, otherwise the per-user default (``~/.hermes``)."""
    override = os.environ.get("HERMES_HOME", "").strip()
    return Path(override) if override else Path.home() / ".hermes"


def candidate_sources(home=None):
    """Heartbeats to consider when nothing is pinned, most-preferred first.

    Covers both layouts Hermes uses: ``<home>/state/gateway.heartbeat`` (default profile)
    and ``<home>/profiles/<name>/state/gateway.heartbeat`` (named profiles), so running a
    named profile needs no unit override.
    """
    root = Path(home) if home is not None else hermes_home()
    candidates = [root.joinpath(*HEARTBEAT_RELATIVE)]
    try:
        named = sorted(entry for entry in (root / "profiles").iterdir() if entry.is_dir())
    except OSError:
        named = []
    candidates.extend(entry.joinpath(*HEARTBEAT_RELATIVE) for entry in named)
    return tuple(candidates)


def pinned_source(home=None):
    """``HERMES_PROFILE``'s heartbeat when that profile exists, otherwise ``None``.

    Pinning by profile name (not by path) keeps the unit free of host-specific paths and
    makes the reported agent deterministic on a host that runs several profiles at once.
    """
    profile = os.environ.get("HERMES_PROFILE", "").strip()
    if not profile:
        return None
    root = Path(home) if home is not None else hermes_home()
    profile_dir = root / "profiles" / profile
    if not profile_dir.is_dir():
        return None
    return profile_dir.joinpath(*HEARTBEAT_RELATIVE)


def _stat_regular(path):
    """``os.stat`` result for a regular file, else ``None``.

    FIFOs and directories must never reach ``open()``: a FIFO would block the loop forever
    without raising, so ``Restart=always`` would not even fire.
    """
    try:
        st = os.stat(path)
    except OSError:
        return None
    return st if stat.S_ISREG(st.st_mode) else None


def pinned_candidate(home=None):
    """The heartbeat pinned by env (``HERMES_HEARTBEAT_FILE`` or ``HERMES_PROFILE``), else ``None``.

    ``None`` means "discover", which is what the ambiguity notice in ``main()`` keys off.
    """
    explicit = os.environ.get("HERMES_HEARTBEAT_FILE", "").strip()
    if explicit:
        return Path(explicit)
    return pinned_source(home)


def select_source(home=None):
    """Resolve the heartbeat to report.

    1. ``HERMES_HEARTBEAT_FILE`` — an explicit file pin.
    2. ``HERMES_PROFILE`` — that profile's heartbeat, when the profile exists.
    3. The newest mtime among ``candidate_sources()``; the default path when nothing exists
       yet. Only the running profile keeps its heartbeat fresh, so the newest is the live
       agent. When several profiles are fresh at once the newest one wins and ``main()``
       logs the ambiguity (reporting one card per profile is a separate change).
    """
    pinned = pinned_candidate(home)
    if pinned is not None:
        return pinned
    candidates = candidate_sources(home)
    best = None
    best_mtime = None
    for candidate in candidates:
        st = _stat_regular(candidate)
        if st is None:
            continue
        if best_mtime is None or st.st_mtime > best_mtime:
            best, best_mtime = candidate, st.st_mtime
    return best if best is not None else candidates[0]


def fresh_sources(home=None, now=None):
    """Candidates written within ``HEARTBEAT_MAX_AGE``, in discovery order.

    More than one means the host runs several profiles at once, and a single card cannot
    say which one it is reporting.
    """
    now = time.time() if now is None else now
    fresh = []
    for candidate in candidate_sources(home):
        st = _stat_regular(candidate)
        if st is not None and now - st.st_mtime <= HEARTBEAT_MAX_AGE:
            fresh.append(candidate)
    return tuple(fresh)


def ambiguity_notice(source, fresh, previous=None):
    """Notice for discovery that chose among several fresh heartbeats, else ``None``.

    Deduplicated by the fresh *set*, not by the winner: when several profiles stay fresh,
    the newest one flips every cycle and a winner-dependent comparison would log each time
    — exactly while the ambiguity is live.
    """
    fresh = tuple(fresh)
    if len(fresh) <= 1 or fresh == tuple(previous or ()):
        return None
    return (f"{len(fresh)} fresh heartbeats found; reporting {source} "
            "(pin one with HERMES_PROFILE)")


def _unknown_summary(now):
    return {"id": "hermes", "published_at": now, "state": "unknown",
            "task_kind": "unknown", "metrics": [], "hb_age_s": 0}


def read_source(now=None, source=None):
    now = time.time() if now is None else now
    path = select_source() if source is None else Path(source)
    st = _stat_regular(path)
    if st is None:
        return _unknown_summary(now)
    try:
        # O_NONBLOCK keeps a swapped-in FIFO (stat→open race) from wedging the loop.
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        with os.fdopen(fd, encoding="utf-8") as fp:
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


def tick(reported_fresh=None):
    """Run one cycle and return the state to pass to the next call.

    Kept separate from ``main()`` so the warning's dedup can be exercised without sleeping.
    """
    pinned = pinned_candidate()
    source = select_source() if pinned is None else pinned
    if pinned is None:
        fresh = fresh_sources()
        notice = ambiguity_notice(source, fresh, reported_fresh)
        if notice is not None:
            print(f"[exporter] {notice}", file=sys.stderr, flush=True)
        reported_fresh = fresh if len(fresh) > 1 else None
    publish(read_source(source=source))
    return reported_fresh


def main():
    reported_fresh = None
    while True:
        reported_fresh = tick(reported_fresh)
        time.sleep(INTERVAL)


if __name__ == "__main__":
    main()
