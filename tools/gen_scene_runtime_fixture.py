#!/usr/bin/env python3
"""Create a new, synthetic-only local scene fixture for offline/LXC runtime acceptance.

No application imports, providers, installed model files, existing DBs, keys or network.
The synthetic space deliberately makes no compatibility claim about a real embedding model.
"""
import argparse
from datetime import datetime, timedelta
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import struct

ROOT = Path(__file__).resolve().parents[1]


def fingerprint(parts):
    digest = hashlib.sha256()
    for part in parts:
        data = part.encode()
        digest.update(len(data).to_bytes(8, 'little'))
        digest.update(data)
    return digest.hexdigest()


def generate(destination, count, many, weight):
    # Refuse existing paths, including symlinks. This tool never updates an existing fixture.
    destination.mkdir(parents=True, exist_ok=False)
    schema = (ROOT / 'crates/geoframe/src/schema.rs').read_text()
    index = (ROOT / 'crates/geoframe/src/ai/index.rs').read_text()
    legacy_sql = re.search(r'pub const SCHEMA: &str = r#"(.*?)"#;', schema, re.S)[1]
    index_sql = re.search(r'const SCHEMA: &str = "(.*?)";', index, re.S)[1]
    identity = ['scene-runtime-synthetic', 'shared-synthetic', 'model-synthetic',
                'revision-synthetic', 'image-synthetic', 'text-synthetic', '768', 'none']
    space_fingerprint = fingerprint(['space-identity/v1', *identity])
    blob = struct.pack('<768f', 1., *([0.] * 767))
    with sqlite3.connect(destination / 'catalog.db') as conn:
        conn.executescript(legacy_sql)
        conn.executescript(index_sql)
        conn.execute('INSERT INTO ai_embedding_spaces VALUES(?,?,?,?,?,?,?,?,0)',
                     (*identity[:6], 768, 'none'))
        conn.execute("INSERT INTO settings VALUES('ai_active_space',?)", (identity[0],))
        for pid in range(1, count + 1):
            display = f'derivatives/synthetic-{pid}.jpg'
            sha = f'{pid:064x}'
            captured = datetime(2026, 1, 1) + timedelta(seconds=pid * 1801 if many else 0)
            conn.execute("""INSERT INTO photos(id,source,name,display,md5,mtime,rotation,taken_at,lat,lon)
                VALUES(?,'local','synthetic',?,'synthetic',1,0,?,1,2)""",
                         (pid, display, captured.isoformat(' ')))
            conn.execute("""INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,
                content_sha256,phash,sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at)
                VALUES(?,?,'synthetic',1,?,'0',?,128,90,0,0,100,'','synthetic')""",
                         (pid, display, sha, float(pid % 20)))
            conn.execute('INSERT INTO ai_embedding_vectors VALUES(?,?,?,768,?,0)',
                         (identity[0], sha, 'f' * 64, blob))
    if weight:
        model = dict(format='geoframe-rank-linear-v1', name='synthetic-only',
                     embedding_model=identity[2], embedding_dim=768,
                     base_features=['sharpness'], base_weights=[-1.], pca_mean=[0.] * 768,
                     pca_components=[[0.] * 768], pca_weights=[0.],
                     compatibility=dict(space_fingerprint=space_fingerprint,
                         embedding_processor_version='image_embedding/v1/' + space_fingerprint,
                         input_representation='stored-f32-v1'))
        (destination / 'models').mkdir()
        (destination / 'models/rank_model.json').write_text(json.dumps(model))
    print(f'synthetic_fixture photos={count} dimension=768 weight={int(weight)} scene_tables=0')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--photos', type=int, default=6500)
    parser.add_argument('--many-components', action='store_true')
    parser.add_argument('--weight', action='store_true')
    args = parser.parse_args()
    if not 1 <= args.photos <= 6500:
        raise SystemExit('synthetic_fixture_count_invalid')
    try:
        generate(args.output, args.photos, args.many_components, args.weight)
    except (OSError, sqlite3.Error):
        raise SystemExit('synthetic_fixture_failed') from None
