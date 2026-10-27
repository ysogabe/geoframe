#!/usr/bin/env python3
"""Run Python scene/rank code with synthetic inputs only; never read installed weights/DB."""
import argparse
import json
import os
import sys
import struct
import tempfile
from datetime import datetime, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'crates/geoframe/src/ai/groups/python_fixture.json'


def generate():
    with tempfile.TemporaryDirectory(prefix='geoframe-scene-fixture-') as tmp:
        os.environ['GEOFRAME_DATA_DIR'] = tmp
        sys.path.insert(0, str(ROOT / 'app'))
        from geoframe import ai_pipeline as pipeline, rank_model
        from geoframe.quality import Metrics
        pipeline.AI_TIME_WINDOW_MIN = 15
        pipeline.AI_SIMILARITY = 0.5
        pipeline.AI_SHORTLIST_MAX = 6
        epoch = datetime(2026, 1, 1)
        cases = [
            ('chain_boundary', [(1, 0, [1., 0.]), (2, 900, [.6, .8]),
                                (3, 1800, [-.28, .96]), (4, 2701, [-.28, .96])], False),
            ('pending_equal', [(1, 0, [1., 0.]), (2, 100, [1., 0.]), (3, 1000, None)], False),
            ('pending_beyond', [(1, 0, [1., 0.]), (2, 100, [1., 0.]), (3, 1001, None)], False),
            ('cosine_equal', [(1, 0, [1., 0.]), (2, 100, [.6, .8])], False),
            ('shortlist', [(i, i, [1., 0.]) for i in range(1, 9)], False),
            ('linear', [(i, i, [1., 0.]) for i in range(1, 5)], True),
        ]
        raw = dict(format='geoframe-rank-linear-v1', name='synthetic-only',
                   embedding_model='model-a', embedding_dim=2,
                   base_features=['sharpness'], base_weights=[-1.],
                   pca_mean=[1., 2.], pca_components=[[1., 2.]], pca_weights=[.5])
        model = rank_model.RankModel(raw)
        output = []
        for name, inputs, learned in cases:
            photos = []
            for pid, seconds, vector in inputs:
                # Include ties and normalization to two decimals.
                sharpness = [3., 4., 4., 7.][(pid - 1) % 4]
                metrics = Metrics(sharpness, 128., 90., 0., 0., 100.)
                captured = epoch + timedelta(seconds=seconds)
                photos.append(pipeline.PipelinePhoto(pid, 'synthetic', '', None, None,
                    captured.strftime('%Y-%m-%d %H:%M:%S'), '', f'{pid:064x}', '0', metrics,
                    tuple(struct.unpack('<f', struct.pack('<f', n))[0] for n in vector) if vector else None, captured))
            similarity = pipeline.cosine(photos[0].embedding, photos[1].embedding) if name == 'cosine_equal' else 0.5
            pipeline.AI_SIMILARITY = similarity
            pipeline.load_rank_model = lambda *_: model if learned else None
            groups = pipeline.build_scene_candidates(photos)
            groups = [dict(ids=[p.id for p in g['photos']], scores=[p.local_score for p in g['photos']],
                           shortlist=[p.id for p in g['shortlist']]) for g in groups]
            groups.sort(key=lambda g: sorted(g['ids']))
            output.append(dict(name=name, learned=learned, similarity=similarity,
                inputs=[dict(id=p.id, seconds=int((p.captured-epoch).total_seconds()),
                             vector=p.embedding, sharpness=p.metrics.sharpness) for p in photos], groups=groups))
        return json.dumps(dict(model=raw, cases=output), ensure_ascii=False, indent=2) + '\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    result = generate()
    if args.check:
        if DEST.read_text() != result:
            raise SystemExit('scene fixture mismatch')
    else:
        DEST.write_text(result)
