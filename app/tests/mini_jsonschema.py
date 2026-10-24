"""標準ライブラリだけの小さなJSON Schema検査。

写真フィードのSchema（docs/architecture/photo-feed.schema.json）が使う語彙だけを扱う。
本番の依存（app/requirements.txt）を増やさないための代用で、一般のJSON Schemaの実装ではない。
知らない語彙に出会ったら黙って通さず例外にする（Schemaを拡張したらここも直す）。
Rust側は jsonschema crate で同じSchemaを検査している。
"""
import re

IGNORED = {"$schema", "$id", "$defs", "title", "description"}
KNOWN = IGNORED | {"$ref", "type", "required", "properties", "items", "enum", "pattern",
                   "minimum", "maximum", "minLength", "additionalProperties"}


def _is_type(value, kind):
    if kind == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if kind == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    python_type = {"string": str, "boolean": bool, "array": list, "object": dict,
                   "null": type(None)}[kind]
    return isinstance(value, python_type)


def errors(value, schema, root=None, path="$"):
    """違反の説明のリスト（空なら適合）。"""
    root = root or schema
    unknown = set(schema) - KNOWN
    if unknown:
        raise ValueError(f"未対応のSchema語彙: {sorted(unknown)}")
    if schema.get("additionalProperties", True) is not True:
        raise ValueError("additionalProperties は true だけ対応")
    if "$ref" in schema:
        target = root
        for part in schema["$ref"].removeprefix("#/").split("/"):
            target = target[part]
        return errors(value, target, root, path)
    found = []
    kind = schema.get("type")
    if kind and not _is_type(value, kind):
        return [f"{path}: {kind} ではない（{value!r}）"]
    if "enum" in schema and value not in schema["enum"]:
        found.append(f"{path}: {schema['enum']} のどれでもない（{value!r}）")
    if isinstance(value, str):
        if "pattern" in schema and not re.search(schema["pattern"], value):
            found.append(f"{path}: 書式に合わない（{value!r}）")
        if len(value) < schema.get("minLength", 0):
            found.append(f"{path}: 短すぎる（{value!r}）")
    if _is_type(value, "number"):
        if "minimum" in schema and value < schema["minimum"]:
            found.append(f"{path}: {schema['minimum']} 未満（{value!r}）")
        if "maximum" in schema and value > schema["maximum"]:
            found.append(f"{path}: {schema['maximum']} 超（{value!r}）")
    if isinstance(value, dict):
        for name in schema.get("required", []):
            if name not in value:
                found.append(f"{path}: 必須の {name} がない")
        for name, sub in schema.get("properties", {}).items():
            if name in value:
                found += errors(value[name], sub, root, f"{path}.{name}")
    if isinstance(value, list) and "items" in schema:
        for i, item in enumerate(value):
            found += errors(item, schema["items"], root, f"{path}[{i}]")
    return found
