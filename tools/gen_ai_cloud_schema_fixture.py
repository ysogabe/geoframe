#!/usr/bin/env python3
"""Extract only Python schema constants by AST; never import or run the app.

python3 tools/gen_ai_cloud_schema_fixture.py [--check]
Rust embeds this fixture with include_str!, including on LXC.
"""

import argparse
import ast
import json
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
FIXTURE = REPO / "crates/geoframe/tests/fixtures/ai-cloud-schemas-python.json"


def extract(relative, name):
    tree = ast.parse((REPO / relative).read_text())
    assignments = {
        target.id: node.value
        for node in tree.body if isinstance(node, ast.Assign)
        for target in node.targets if isinstance(target, ast.Name)
    }

    def literal(node):
        if isinstance(node, ast.Name):
            return literal(assignments[node.id])
        if isinstance(node, ast.Dict):
            return {literal(k): literal(v) for k, v in zip(node.keys, node.values)}
        if isinstance(node, (ast.List, ast.Tuple)):
            return [literal(v) for v in node.elts]
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                and node.func.id == "list" and len(node.args) == 1 and not node.keywords):
            return list(literal(node.args[0]))
        return ast.literal_eval(node)

    return literal(assignments[name])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    sources = {
        "photo_judge": ("app/geoframe/ai_pipeline.py", "JUDGE_SCHEMA"),
        "location_suggest": ("app/geoframe/loc_suggest.py", "SCHEMA"),
    }
    result = {
        "source": {task: f"{path}:{name}" for task, (path, name) in sources.items()},
        "schemas": {task: extract(path, name) for task, (path, name) in sources.items()},
    }
    encoded = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
    if args.check:
        if FIXTURE.read_text() != encoded:
            raise SystemExit("Python schema fixture differs; regenerate it")
        print("Python schema fixture matches both current AST constants")
    else:
        FIXTURE.write_text(encoded)
        print(f"wrote {FIXTURE.relative_to(REPO)}")


if __name__ == "__main__":
    main()
