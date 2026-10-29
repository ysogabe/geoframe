#!/usr/bin/env python3
"""E5/E6 fixture from actual Python API, synthetic DB/fixed vectors only. No AI/network."""
import json
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'app'))
from geoframe import ai_search, store


def build():
    with tempfile.TemporaryDirectory() as tmp, patch.object(store, 'DB_PATH', Path(tmp)/'catalog.db'), patch.object(store, '_db', None):
        try:
            c = store.db()
            vectors = [[1,0,0],[.99,.05,0],[0,1,0],[0,0,1],[-1,0,0],[.5,.5,0],[0,1,0],[0,0,1]]
            for i,v in enumerate(vectors,1):
                c.execute("INSERT INTO photos(id,source,name,display,md5,mtime,taken_at,lat,lon,favorite,hidden,pref,place) VALUES(?,'drive',?,?,?,1,?,35,139,?,?,?,?)",
                          (i,f'synthetic-{i}.jpg',f'derivatives/{i}.jpg',f'm{i}',None if i==3 else '2026-01-01 10:00',int(i==2),int(i==4),'架空県' if i==2 else None,'見本町' if i==1 else None))
                sha = f'{i:064x}'
                c.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,analyzed_at) VALUES(?,?,?,1,?,'fixture')",(i,f'derivatives/{i}.jpg',f'm{i}',sha))
                c.execute("INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,subject,pet_state,eyes,face_crop,horizon_status,horizon_score,subject_sharpness,aesthetic_score,keep_score,recommended,reasons_json,concerns_json,analyzed_at) VALUES(?,?,?,1,?,'gemini-embedding-2',3,?,'dog','active','open','good','level',95,80,80,?,?,?,?,'fixture')",(i,f'derivatives/{i}.jpg',f'm{i}',sha,ai_search.pack_embedding(v),{1:60,2:90,4:95,5:0}.get(i),{1:0,2:1}.get(i),'["合成の理由"]' if i==2 else 'invalid','{"not":"list"}' if i==1 else '[]'))
            for i,g,kind,rec,score in [(3,2,'similar',1,45),(5,2,'similar',0,0),(7,3,'exact',0,99),(8,3,'exact',1,30)]:
                c.execute("INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,group_kind,rank_in_group,recommended,score,algorithm,analyzed_at) VALUES(?,?,?,1,?,?,?,?,?,?,'fixture','fixture')",(i,f'derivatives/{i}.jpg',f'm{i}',f'{i:064x}',g,kind,i,rec,score))
            c.execute("INSERT INTO photo_ai_groups(group_id,kind,summary,min_similarity,max_similarity,avg_similarity,algorithm,analyzed_at) VALUES('scene-a','scene','合成場面',.95,.95,.95,'fixture','fixture')")
            for i in [1,2]:
                c.execute("INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended) VALUES('scene-a',?,?,?)",(i,i,int(i==2)))
            c.commit()
            sql = '\n'.join(c.iterdump())+'\n'
            cases=[]
            with patch.object(ai_search,'AI_ENABLED',True), patch.object(ai_search,'AI_SEARCH_PER_MIN',1000), patch.object(ai_search,'EMBED_MODEL','gemini-embedding-2'), patch.object(ai_search,'EMBED_BACKEND','gemini'), patch.object(ai_search,'VISUAL_CLUSTER_SIMILARITY',.90), patch.object(ai_search,'_embed_query',return_value=[1,0,0]):
                ai_search._search_win.clear()
                status = ai_search.api_ai_status()
                for query,groups,hidden in [('',12,False),('',1,True),('合成検索',12,False),('  合成\u3000検索\x1c ',30,True),('',0,False),('x',100,False)]:
                    cases.append(dict(query=query,groups=groups,hidden=hidden,result=ai_search.api_ai_search(query,groups,hidden)))
            return [(ROOT/'crates/geoframe/tests/fixtures/ai-search-catalog.sql',sql),(ROOT/'crates/geoframe/tests/fixtures/ai-search-python.json',json.dumps(dict(status=status,cases=cases),ensure_ascii=False,sort_keys=True,indent=2)+'\n')]
        finally:
            if store._db is not None: store._db.close()

if __name__ == '__main__':
    for path,text in build():
        if '--check' in sys.argv: assert path.read_text()==text, f'regenerate {path.name}'
        else: path.write_text(text)
    print('AI search Python fixture: OK')
