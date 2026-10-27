#!/usr/bin/env python3
"""Safe AST-only Python contract fixture. No app imports, keys, photos, DB or network.

python3 tools/gen_location_suggest_fixture.py [--check]
--roundtrip reads synthetic Rust rows from stdin and runs the actual Python parser/reuse functions.
--apply-roundtrip also applies the run using injected synthetic geocode hits and an in-memory row collector.
"""
import argparse
import ast
import hashlib
import json
from pathlib import Path
from gen_ai_cloud_schema_fixture import extract

ROOT = Path(__file__).resolve().parents[1]
SOURCE = 'app/geoframe/loc_suggest.py'
FIXTURE = ROOT / 'crates/geoframe/tests/fixtures/location-suggest-python.json'
FUNCTIONS = {'passes', '_rank_key', 'group_days', 'pick_send', 'run_id_for', '_parse_answer', '_reusable_run'}


def functions(model='synthetic-suggest', gap=43200, apply=False):
    tree = ast.parse((ROOT / SOURCE).read_text())
    selected = FUNCTIONS | ({'_geocode', '_apply_run'} if apply else set())
    nodes = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in selected]
    assert {n.name for n in nodes} == selected
    # Only these named, reviewed functions are compiled; apply I/O is injected. No top-level app code/import executes.
    env = {name: extract(SOURCE, name) for name in
           ('CLUE_MIN', 'MAX_SEND', 'PROMPT_VERSION', 'PRECISIONS', 'CONFIDENCES')}
    env.update(hashlib=hashlib, json=json, GROUP_GAP_S=gap, LOC_SUGGEST_MODEL=model)
    if apply:
        env['COARSER'] = extract(SOURCE, 'COARSER')
    exec(compile(ast.Module(body=nodes, type_ignores=[]), SOURCE, 'exec'), env)
    return env


def photo(pid, epoch, day='2026-01-02', clue=0.5, kind='street', classified=True):
    return dict(id=pid, epoch=epoch, taken_at=day + ' 00:00', clue=clue, kind=kind,
                classified=classified, clue_error='')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--roundtrip', action='store_true')
    parser.add_argument('--apply-roundtrip', action='store_true')
    args = parser.parse_args()
    if args.roundtrip or args.apply_roundtrip:
        import sys
        v = json.load(sys.stdin)
        f = functions(v['model'], apply=args.apply_roundtrip)
        row = v['run']
        assert len(row) == 11
        assert row['run_id'] == f['run_id_for'](v['model'], json.loads(row['sent_ids_json']))
        answer = json.loads(row['answer_json'])
        envelope = {'candidates': [{'content': {'parts': [{'text': row['answer_json']}]}}]}
        assert f['_parse_answer'](envelope, len(json.loads(row['sent_ids_json']))) == answer
        assert f['_reusable_run'](row['day'], [{'id': i} for i in v['subset']], [row]) == row
        if args.apply_roundtrip:
            rows = []
            f['nominatim_search'] = lambda q, cc: v['hits'].get(q, [])
            f['dbm'] = lambda sql, values: rows.extend(values)
            assert f['_apply_run'](row, [{'id': i} for i in v['subset']]) == len(v['subset'])
            print(json.dumps(rows, ensure_ascii=False))
            return
        print('Python AST parser / run_id / subset reuse accepted Rust eleven-column projection')
        return
    f = functions()
    cases = []
    for gap in (1, 43200, 60000):
        f['GROUP_GAP_S'] = gap
        for photos in (
            [photo(2, gap), photo(1, 0), photo(3, gap * 2 + 1)],
            [photo(3, 2, '2026-01-03'), photo(2, 1), photo(1, 1)],
            [photo(i, i, clue=0.2 if i % 2 else 0.8,
                   kind='landmark' if i in (3, 9) else 'street') for i in range(10, 0, -1)],
            [photo(1, 0, clue=0.19), photo(2, 1, clue=0.0, kind='landmark')],
        ):
            groups = f['group_days'](photos)
            cases.append(dict(gap=gap, photos=photos,
                              groups=[[p['id'] for p in g] for g in groups],
                              sent=[[p['id'] for p in f['pick_send'](g)] for g in groups]))
    answers = []
    for value in (
        dict(place_name='合成都市', precision='city', confidence='high', geocode_query='Synthetic City',
             country='合成国', country_code='zz', city='合成都市', evidence=['合成看板'],
             photos=[dict(index=1, place_name='合成公園', geocode_query='Synthetic Park', precision='district', confidence='medium')]),
        dict(place_name='合成地域', precision='new', confidence='new', evidence=['合成', True, None, 2, 0.5, 1e-7, 1e-5, 1e16, 1e20, -0.0, ['nested'], {'x': False}],
             photos=[dict(index=2, place_name='合成建物', precision='new', confidence='new'),
                     dict(index=7, place_name='範囲外'), dict(index=1, place_name=2)]),
        dict(place_name='合成都市', precision='city', photos=[dict(index=2, place_name='別合成都市', precision='new')], evidence={}),
    ):
        parsed = f['_parse_answer']({'candidates': [{'content': {'parts': [{'text': json.dumps(value, ensure_ascii=False)}]}}]}, 2)
        # Rust explicitly fills absent optional strings with ""; drops unknown payload fields.
        for name in ('country', 'country_code', 'city', 'geocode_query'):
            parsed.setdefault(name, '')
        for item in parsed['photos']:
            item.setdefault('geocode_query', '')
        answers.append(dict(input=value, normalized=parsed))
    result = dict(source=SOURCE,
                  constants={n: extract(SOURCE, n) for n in ('PROMPT_VERSION', 'MAX_SEND', 'MAX_ATTEMPTS', 'PROMPT', 'SCHEMA')},
                  groups=cases,
                  run_ids=[dict(model=model, sent=ids, run_id=f['run_id_for'](model, ids))
                           for model in ('synthetic-suggest', 'other-model') for ids in ([9, 2, 1], [1, 2, 9], [6])],
                  answers=answers)
    output = json.dumps(result, ensure_ascii=False, indent=2) + '\n'
    if args.check:
        if FIXTURE.read_text() != output:
            raise SystemExit('Location suggest fixture differs; regenerate it')
        print('Python AST constants and pure function fixtures match')
    else:
        FIXTURE.write_text(output)
        print('Wrote', FIXTURE.relative_to(ROOT))


if __name__ == '__main__':
    main()
