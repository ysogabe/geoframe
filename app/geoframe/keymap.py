"""閉じた語彙によるキーマップの純粋な検証・描画関数。"""
from copy import deepcopy

DEFAULT_KEYS = {"0": "sleep", "1": "rotation", "2": "agents", **{str(i): "reserved" for i in range(3, 10)}}
MODES = (
    {"id": "rotation", "label": "地図と写真"},
    {"id": "sleep", "label": "就寝"},
    {"id": "agents", "label": "エージェント稼働状況"},
    {"id": "reserved", "label": "未割当"},
)
_ALLOWED = frozenset(item["id"] for item in MODES)


def validate(payload):
    """payloadを検証し、正常ならコピーしたkeys、違反なら日本語の理由を返す。"""
    if not isinstance(payload, dict):
        return "JSONオブジェクトを送ってください"
    if set(payload) != {"keys", "base_revision"}:
        return "keys と base_revision だけを指定してください"
    keys = payload["keys"]
    if not isinstance(keys, dict) or set(keys) != set(DEFAULT_KEYS):
        return "キーは0から9までの10個を指定してください"
    for key, value in keys.items():
        if not isinstance(value, str) or value not in _ALLOWED:
            return f"キー{key}の値が不正です"
    if "rotation" not in keys.values():
        return "rotationを最低1つ指定してください"
    if list(keys.values()).count("sleep") > 1:
        return "sleepは1つまで指定できます"
    revision = payload["base_revision"]
    if isinstance(revision, bool) or not isinstance(revision, int) or revision < 0:
        # Noneを許すとexpected_rev=Noneの無条件書込みで競合検出を迂回できる。
        return "base_revisionは0以上の整数を指定してください"
    return deepcopy(keys)


def is_valid_saved_document(doc):
    """保存済み文書がGET/CAS共通の妥当性規則を満たすか返す。"""
    if not isinstance(doc, dict):
        return False
    revision = doc.get("rev")
    if isinstance(revision, bool) or not isinstance(revision, int) or revision < 0:
        return False
    keys = doc.get("keys")
    if not isinstance(keys, dict) or set(keys) != set(DEFAULT_KEYS):
        return False
    if any(not isinstance(value, str) or value not in _ALLOWED
           for value in keys.values()):
        return False
    return "rotation" in keys.values() and list(keys.values()).count("sleep") <= 1


def render(doc):
    """保存文書をAPI応答へ描画する。壊れた保存値は既定値へ閉じる。"""
    if not is_valid_saved_document(doc):
        revision, updated_at, keys = 0, None, dict(DEFAULT_KEYS)
    else:
        revision = doc["rev"]
        updated_at = doc.get("updated_at")
        keys = dict(doc["keys"])
    return {
        "rev": revision,
        "updated_at": updated_at,
        "keys": keys,
        "modes": deepcopy(MODES),
    }
