#!/usr/bin/env python3
"""Python の固定 Decision 質問を抽出する。アプリの import・通信・画像読取はしない。"""
import ast
import json
from pathlib import Path

root = Path(__file__).resolve().parent.parent
module = ast.parse((root / "app/geoframe/loc_suggest.py").read_text())
names = {"DECISION_STATE", "DECISION_QUESTIONS"}
values = {}
for node in module.body:
    if (isinstance(node, ast.Assign) and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name) and node.targets[0].id in names):
        values[node.targets[0].id] = ast.literal_eval(node.value)
fixture = {"state": values["DECISION_STATE"], "questions": values["DECISION_QUESTIONS"]}
output = root / "crates/geoframe/tests/fixtures/location-clue-python.json"
output.write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n")
