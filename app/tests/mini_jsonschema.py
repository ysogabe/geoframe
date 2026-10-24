"""標準ライブラリだけの小さなJSON Schema検査。

写真フィードのSchema（docs/architecture/photo-feed.schema.json）が使う語彙だけを扱う。
本番の依存（app/requirements.txt）を増やさないための代用で、一般のJSON Schemaの実装ではない。
知らない語彙に出会ったら黙って通さず例外にする。データに関わらず、検査の前にSchema全体を
走査して見つける（Schemaを拡張したらここも直す）。
`$ref` は同じ文書の中（`#/...`）だけを引き、併記された制約も合わせて評価する（2020-12の意味）。
Rust側は jsonschema crate で同じSchemaを検査している。
"""
import re

IGNORED = {"$schema", "$id", "$defs", "title", "description"}
KNOWN = IGNORED | {"$ref", "type", "required", "properties", "items", "enum", "pattern",
                   "minimum", "maximum", "minLength", "additionalProperties"}
TYPES = {"string", "integer", "number", "boolean", "array", "object", "null"}
# ECMA 262 と Python の `re` で意味が違うので、使わせない
UNSUPPORTED_ESCAPES = set("dDwWsSbBpP")


def _is_type(value, kind):
    if kind == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if kind == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    python_type = {"string": str, "boolean": bool, "array": list, "object": dict,
                   "null": type(None)}[kind]
    return isinstance(value, python_type)


def _resolve(root, ref):
    if not ref.startswith("#/"):
        raise ValueError(f"文書内の参照（#/...）だけ対応: {ref!r}")
    target = root
    for part in ref[2:].split("/"):
        if not isinstance(target, dict) or part not in target:
            raise ValueError(f"参照先がない: {ref!r}")
        target = target[part]
    return target


def _ecma_pattern(pattern):
    """JSON Schema の pattern（ECMA 262: 部分一致、複数行モードなし）を Python の `re` に直す。

    Python の `$` は末尾の改行の前にも一致するので、`\\Z`（文字列の末尾だけ）に置き換える。
    """
    out = []
    in_class = False
    i = 0
    while i < len(pattern):
        c = pattern[i]
        if c == "\\":
            nxt = pattern[i + 1:i + 2]
            if nxt in UNSUPPORTED_ESCAPES:
                raise ValueError(f"未対応の正規表現の記号: \\{nxt}（{pattern!r}）")
            out.append(pattern[i:i + 2])
            i += 2
            continue
        if c == "[":
            in_class = True
        elif c == "]":
            in_class = False
        out.append("\\Z" if c == "$" and not in_class else c)
        i += 1
    return "".join(out)


def check_schema(schema, root=None, path="#"):
    """未対応の語彙・不正な参照・compileできない pattern があれば ValueError。

    データを見ずに、`$defs`・`properties`・`items` の中まで全部走査する。
    """
    root = schema if root is None else root
    if not isinstance(schema, dict):
        raise ValueError(f"{path}: Schema がオブジェクトでない")
    unknown = set(schema) - KNOWN
    if unknown:
        raise ValueError(f"{path}: 未対応のSchema語彙: {sorted(unknown)}")
    if schema.get("additionalProperties", True) is not True:
        raise ValueError(f"{path}: additionalProperties は true だけ対応")
    if "type" in schema and schema["type"] not in TYPES:
        raise ValueError(f"{path}: 未対応の type: {schema['type']!r}")
    if "$ref" in schema:
        _resolve(root, schema["$ref"])
    if "pattern" in schema:
        try:
            re.compile(_ecma_pattern(schema["pattern"]))
        except re.error as e:
            raise ValueError(f"{path}: pattern がcompileできない: {e}") from e
    for name, sub in schema.get("$defs", {}).items():
        check_schema(sub, root, f"{path}/$defs/{name}")
    for name, sub in schema.get("properties", {}).items():
        check_schema(sub, root, f"{path}/properties/{name}")
    if "items" in schema:
        check_schema(schema["items"], root, f"{path}/items")


def errors(value, schema):
    """違反の説明のリスト（空なら適合）。Schema 自体が未対応・不正なら ValueError。"""
    check_schema(schema)
    return _errors(value, schema, schema, "$")


def _errors(value, schema, root, path):
    found = []
    if "$ref" in schema:   # 併記された制約も、参照先と合わせて評価する
        found += _errors(value, _resolve(root, schema["$ref"]), root, path)
    kind = schema.get("type")
    if kind and not _is_type(value, kind):
        return found + [f"{path}: {kind} ではない（{value!r}）"]
    if "enum" in schema and value not in schema["enum"]:
        found.append(f"{path}: {schema['enum']} のどれでもない（{value!r}）")
    if isinstance(value, str):
        if "pattern" in schema and not re.search(_ecma_pattern(schema["pattern"]), value):
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
                found += _errors(value[name], sub, root, f"{path}.{name}")
    if isinstance(value, list) and "items" in schema:
        for i, item in enumerate(value):
            found += _errors(item, schema["items"], root, f"{path}[{i}]")
    return found
