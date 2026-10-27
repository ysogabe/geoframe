//! Read-only E5/E6 projections. Pointer, vectors, generations and card fields share one snapshot.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{Value, json};

use super::http::Deadline;
use super::query::Error;
use super::search::QueryVector;
use crate::config::{Config, EmbedBackend};

const JOINS: &str = "FROM photos p
 JOIN photo_quality_features f ON f.photo_id=p.id AND f.display=p.display
  AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime AND f.error='' AND f.content_sha256 IS NOT NULL
 LEFT JOIN photo_ai_features a ON a.photo_id=p.id AND a.display=p.display
  AND a.source_md5 IS p.md5 AND a.source_mtime IS p.mtime AND a.error='' AND a.content_sha256=f.content_sha256
 LEFT JOIN photo_quality q ON q.photo_id=p.id AND q.display=p.display
  AND q.source_md5 IS p.md5 AND q.source_mtime IS p.mtime";

struct Photo {
    id: i64,
    key: String,
    card: Value,
    vector: Option<Arc<Vec<f32>>>,
    score: f64,
}
fn list(v: Option<String>) -> Value {
    v.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_array)
        .unwrap_or(json!([]))
}
fn round4(v: f64) -> f64 {
    // Python round(x, 4) rounds the binary float directly to decimal digits.
    // Scaling first loses the side of the halfway boundary (e.g. 0.12345).
    format!("{v:.4}").parse().unwrap_or_default()
}
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (&a, &b) in a.iter().zip(b) {
        let (a, b) = (f64::from(a), f64::from(b));
        dot += a * b;
        aa += a * a;
        bb += b * b;
    }
    if aa == 0.0 || bb == 0.0 {
        0.0
    } else {
        dot / (aa.sqrt() * bb.sqrt())
    }
}
fn legacy_vector(blob: Option<Vec<u8>>, dim: Option<i64>) -> Option<Arc<Vec<f32>>> {
    let blob = blob?;
    let dim = usize::try_from(dim?)
        .ok()
        .filter(|n| (1..=4096).contains(n))?;
    if blob.len() != dim * 4 {
        return None;
    }
    let values: Vec<_> = blob
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    if !values.iter().all(|v| v.is_finite()) || !values.iter().any(|v| *v != 0.0) {
        return None;
    }
    Some(Arc::new(values))
}

pub fn search(
    conn: &Connection,
    config: &Config,
    query: &str,
    groups: usize,
    hidden: bool,
    vector: Option<&QueryVector>,
    whole: &Deadline,
) -> rusqlite::Result<Result<Value, Error>> {
    if whole.remaining().is_none() {
        return Ok(Err(Error::Unavailable));
    }
    let prepared = super::groups::runtime::Prepared::read(conn, config);
    let tx = conn.unchecked_transaction()?;
    let scenes = prepared.and_then(|p| p.load_in(&tx));
    let loaded = super::search::load_in(&tx)?;
    if vector.is_some_and(|v| loaded.index.as_ref().map(|i| i.identity()) != Some(v.identity())) {
        return Ok(Err(Error::Unavailable));
    }
    let mut local = HashMap::new();
    let mut scene_meta = HashMap::new();
    if let Ok(scenes) = &scenes {
        for g in scenes.groups() {
            if g.state() == super::groups::State::Waiting {
                continue;
            }
            let state = if g.state() == super::groups::State::LocalRanked {
                "local_ranked"
            } else {
                "review_overlap"
            };
            let overlap = match g.overlap() {
                super::groups::Overlap::Exact => "exact",
                super::groups::Overlap::Disjoint => "disjoint",
                super::groups::Overlap::Partial => "partial",
            };
            scene_meta.insert(g.result_id().to_string(),json!({"kind":"scene","summary":"","min_similarity":null,"max_similarity":null,"avg_similarity":null,"scene_state":state,"scene_overlap":overlap}));
            for m in g.members() {
                local.insert(m.photo_id, (g.result_id(), state, overlap, m));
            }
        }
    }
    let has_active = loaded.index.is_some();
    let mut active = HashMap::new();
    if let Some(index) = loaded.index {
        for entry in index.entries {
            let values = Arc::new(entry.unit);
            let score = vector.map_or(0.0, |v| cosine(&values, &v.unit));
            for id in entry.photo_ids {
                active.insert(id, (Arc::clone(&values), score));
            }
        }
    }
    let sql = format!("SELECT p.id,p.name,p.display,p.taken_at,p.place,p.pref,p.city,p.lat,p.lon,
        COALESCE(p.favorite,0) favorite,COALESCE(p.hidden,0) hidden,
        a.embedding_model,a.embedding_dim,{} AS embedding,
        a.subject,a.pet_state,a.eyes,a.face_crop,a.horizon_status,a.horizon_score,
        a.subject_sharpness,a.aesthetic_score,a.keep_score,a.recommended,a.reasons_json,a.concerns_json,
        (SELECT MIN(m.group_id) FROM photo_ai_group_members m JOIN photo_ai_groups g ON g.group_id=m.group_id WHERE m.photo_id=a.photo_id) ai_group_id,
        q.group_id quality_group_id,q.group_kind quality_group_kind,q.recommended quality_recommended,q.score quality_score
        {JOINS} WHERE p.display IS NOT NULL AND p.display!=''
        AND (?1 OR COALESCE(p.hidden,0)=0)
        AND NOT (COALESCE(q.group_kind,'')='exact' AND q.recommended=0) ORDER BY p.id",
        if has_active || vector.is_some() { "NULL" } else { "a.embedding" });
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt.query_map([hidden], |r| {
        let id = r.get("id")?;
        let old_model: Option<String> = r.get("embedding_model")?;
        let new = active.get(&id);
        if new.is_none() && (vector.is_some() || old_model.as_deref() != Some(config.embed_backend.model())) { return Ok(None); }
        let mut card = json!({"id":id,"thumb":format!("/{}",r.get::<_,String>("display")?.trim_start_matches('/'))});
        for key in ["name","taken_at","place","pref","city","subject","pet_state","eyes","face_crop","horizon_status","quality_group_kind"] {
            card[key] = json!(r.get::<_,Option<String>>(key)?.unwrap_or_default());
        }
        for key in ["lat","lon","quality_score"] { card[key] = json!(r.get::<_,Option<f64>>(key)?); }
        for key in ["horizon_score","subject_sharpness","aesthetic_score","keep_score"] { card[key] = json!(r.get::<_,Option<i64>>(key)?); }
        card["quality_group_id"] = json!(r.get::<_,Option<i64>>("quality_group_id")?);
        for key in ["favorite","hidden"] { card[key] = json!(r.get::<_,i64>(key)? != 0); }
        for key in ["recommended","quality_recommended"] { card[key] = json!(r.get::<_,Option<i64>>(key)?.map(|v| v != 0)); }
        card["reasons"] = list(r.get("reasons_json")?);
        card["concerns"] = list(r.get("concerns_json")?);
        let ai_group: Option<String> = r.get("ai_group_id")?;
        let mut key = ai_group.filter(|s| !s.is_empty()).unwrap_or_else(|| {
            if card["quality_group_kind"] == "similar" { format!("local-similar-{}",card["quality_group_id"]) } else { format!("photo-{id}") }
        });
        if let Some((result,state,overlap,m)) = local.get(&id) {
            card["local_rank"] = json!(m.rank);
            card["local_score"] = json!(m.score);
            card["local_representative"] = json!(m.representative);
            card["scene_state"] = json!(state);
            card["scene_overlap"] = json!(overlap);
            // Partial overlap remains a management result; the old selection unit is retained.
            if *state == "local_ranked" { key = result.to_string(); }
        }
        let values = new.map(|(v,_)| Arc::clone(v)).or_else(|| {
            if has_active { None } else { legacy_vector(r.get("embedding").ok().flatten(), r.get("embedding_dim").ok().flatten()) }
        });
        let score = if vector.is_some() { new.unwrap().1 } else { card["local_score"].as_f64().or_else(|| card["keep_score"].as_f64()).or_else(|| card["quality_score"].as_f64()).unwrap_or_default()/100.0 };
        Ok(Some(Photo { id, key, card, vector:values, score }))
    })?;
    let mut photos = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    drop(stmt);
    let searched = photos.len();
    if photos.is_empty() {
        tx.commit()?;
        return Ok(Ok(
            json!({"ok":true,"query":query,"mode":"empty","groups":[],"searched_photos":0}),
        ));
    }
    let mut meta = scene_meta;
    let mut stmt = tx.prepare("SELECT group_id,kind,summary,min_similarity,max_similarity,avg_similarity FROM photo_ai_groups")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_,String>(0)?, json!({"kind":r.get::<_,String>(1)?,"summary":r.get::<_,String>(2)?,"min_similarity":r.get::<_,Option<f64>>(3)?,"max_similarity":r.get::<_,Option<f64>>(4)?,"avg_similarity":r.get::<_,Option<f64>>(5)?}))))? { let (k,v)=row?; meta.insert(k,v); }
    drop(stmt);
    // Complete the read transaction before formatting/clustering; all row values are now immutable.
    tx.commit()?;
    photos.sort_by(|a, b| {
        b.score.total_cmp(&a.score).then_with(|| {
            b.card["favorite"]
                .as_bool()
                .cmp(&a.card["favorite"].as_bool())
        })
    });
    let mut group_order = Vec::new();
    let mut seen = HashSet::new();
    for photo in &photos {
        if seen.insert(photo.key.clone()) {
            group_order.push((photo.key.clone(), photo.score));
            if group_order.len() == groups {
                break;
            }
        }
    }
    let mut members = HashMap::<String, Vec<Photo>>::new();
    for photo in photos {
        if seen.contains(&photo.key) {
            members.entry(photo.key.clone()).or_default().push(photo);
        }
    }
    let threshold = config.ai_visual_similarity;
    let mut result = Vec::new();
    for (key, best) in group_order {
        if whole.remaining().is_none() {
            return Ok(Err(Error::Unavailable));
        }
        let mut rows = members.remove(&key).unwrap();
        // Python's second stable sort starts from the original database order.
        rows.sort_by_key(|p| p.id);
        rows.sort_by(|a, b| {
            b.card["favorite"]
                .as_bool()
                .cmp(&a.card["favorite"].as_bool())
                .then_with(|| {
                    b.card["recommended"]
                        .as_bool()
                        .unwrap_or(false)
                        .cmp(&a.card["recommended"].as_bool().unwrap_or(false))
                })
                .then_with(|| {
                    b.card["local_representative"]
                        .as_bool()
                        .cmp(&a.card["local_representative"].as_bool())
                })
                .then_with(|| b.score.total_cmp(&a.score))
                .then_with(|| {
                    b.card["keep_score"]
                        .as_f64()
                        .filter(|v| *v != 0.0)
                        .unwrap_or(-1.0)
                        .total_cmp(
                            &a.card["keep_score"]
                                .as_f64()
                                .filter(|v| *v != 0.0)
                                .unwrap_or(-1.0),
                        )
                })
        });
        let mut reps: Vec<Option<Arc<Vec<f32>>>> = Vec::new();
        let mut sizes = Vec::<usize>::new();
        let mut assignment = Vec::new();
        for row in &rows {
            if whole.remaining().is_none() {
                return Ok(Err(Error::Unavailable));
            }
            let (mut best, mut cluster) = (0.0, None);
            if let Some(v) = &row.vector {
                for (i, rep) in reps.iter().enumerate() {
                    if let Some(rep) = rep {
                        let score = cosine(v, rep);
                        if score > best {
                            best = score;
                            cluster = Some(i);
                        }
                    }
                }
            }
            let i = match cluster.filter(|_| best >= threshold) {
                Some(i) => i,
                None => {
                    reps.push(row.vector.clone());
                    sizes.push(0);
                    best = 1.0;
                    reps.len() - 1
                }
            };
            sizes[i] += 1;
            assignment.push((i, best, sizes[i] == 1));
        }
        for (row, (i, similarity, representative)) in rows.iter_mut().zip(assignment) {
            row.card["similarity"] = json!(round4(row.score));
            row.card["visual_cluster"] = json!(i + 1);
            row.card["visual_similarity"] = json!(round4(similarity));
            row.card["visual_representative"] = json!(representative);
            row.card["visual_cluster_size"] = json!(sizes[i]);
        }
        let mut group = meta.remove(&key).unwrap_or_else(|| json!({"kind":if key.starts_with("local-similar-") {"similar"} else {"single"},"summary":"","min_similarity":null,"max_similarity":null,"avg_similarity":null}));
        group["group_id"] = json!(key);
        group["best_similarity"] = json!(round4(best));
        group["visual_similarity_threshold"] = json!(threshold);
        group["visual_cluster_count"] = json!(reps.len());
        group["collapsed_photo_count"] = json!(rows.len() - reps.len());
        group["photos"] = Value::Array(rows.into_iter().map(|r| r.card).collect());
        result.push(group);
    }
    Ok(Ok(
        json!({"ok":true,"query":query,"mode":if vector.is_some(){"semantic"}else{"recommended"},"groups":result,"searched_photos":searched,"include_hidden":hidden}),
    ))
}

pub fn status(conn: &Connection, config: &Config) -> rusqlite::Result<Value> {
    let prepared = super::groups::runtime::Prepared::read(conn, config);
    let tx = conn.unchecked_transaction()?;
    let scenes = prepared.and_then(|p| p.load_in(&tx));
    let (index, identity, _) = super::index::status_in(&tx, false)?;
    let model = config.embed_backend.model();
    let (legacy, judged, recommended): (i64,i64,i64) = tx.query_row(&format!("SELECT COUNT(DISTINCT a.content_sha256),COALESCE(SUM(a.recommended IS NOT NULL),0),COALESCE(SUM(a.recommended=1),0) {JOINS} WHERE a.embedding_model=?1"),[model],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let (all_judged,all_recommended): (i64,i64) = tx.query_row(&format!("SELECT COALESCE(SUM(a.recommended IS NOT NULL),0),COALESCE(SUM(a.recommended=1),0) {JOINS}"),[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let other: i64 = tx.query_row(
        &format!("SELECT COUNT(DISTINCT a.content_sha256) {JOINS} WHERE a.embedding_model!=?1"),
        [model],
        |r| r.get(0),
    )?;
    let mut group_status = BTreeMap::<String, i64>::new();
    let mut stmt=tx.prepare(&format!("SELECT g.status,COUNT(DISTINCT g.group_id) FROM photo_ai_groups g JOIN photo_ai_group_members m ON m.group_id=g.group_id WHERE EXISTS (SELECT 1 {JOINS} WHERE p.id=m.photo_id AND a.photo_id=p.id) GROUP BY g.status"))?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (k, v) = row?;
        group_status.insert(k, v);
    }
    drop(stmt);
    let scene_summary = super::groups::runtime::summary(scenes.as_ref());
    let selection = crate::selection::admin_counts(&tx)?;
    tx.commit()?;
    let active = identity.is_some();
    let photos = if active {
        index.progress.indexed
    } else {
        legacy
    };
    let state = match index.active {
        super::index::ActiveState::Active(_) => "active",
        super::index::ActiveState::NoTables => "no_tables",
        super::index::ActiveState::Unset => "unset",
        super::index::ActiveState::Unusable => "unusable",
    };
    let compatible = identity.as_ref().is_some_and(|i| {
        super::embedding::configured_identity(&config.ai, super::settings::TaskKind::QueryEmbedding)
            .as_ref()
            == Some(i)
    });
    Ok(
        json!({"ok":true,"photos":photos,"judged":if active {all_judged}else{judged},"recommended":if active {all_recommended}else{recommended},"groups":group_status.values().sum::<i64>(),"eligible":index.progress.eligible,"pending_embeddings":if active {index.progress.pending}else{(index.progress.eligible-legacy).max(0)},"group_status":group_status,
        "embedding_model":identity.as_ref().map_or(model,|i|i.model_id()),"embedding_backend":if active {if identity.as_ref().is_some_and(|i| i.provider_space_id() == super::embedding::PROVIDER_SPACE) {"local_http"} else {"active_index"}}else{match config.embed_backend {EmbedBackend::Local=>"local",EmbedBackend::Gemini=>"gemini"}},"other_model_embeddings":if active {index.other_shas}else{other},
        "pipeline":{"index_state":state,"query_compatible":compatible,"indexed":index.progress.indexed,"pending":index.progress.pending,"failed_excluded":index.progress.failed_excluded,"failed_reasons":index.progress.failed_reasons.into_iter().collect::<BTreeMap<_,_>>(),"corrupt":index.corrupt,"selection_judged":all_judged,"selection_recommended":all_recommended,"selection_groups":selection.groups,"selection_suppressed":selection.suppressed,"scenes":scene_summary}}),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn rounding_preserves_python_halfway_boundaries_and_signs() {
        // Values verified with the actual Python round(float, 4), not a scaled reference.
        for (value, expected) in [
            (0.12345, 0.1235),
            (0.23445, 0.2344),
            (0.06245, 0.0624),
            (-0.12345, -0.1235),
            (0.99995, 1.0),
            (0.00005, 0.0001),
            (0.00015, 0.0001),
            (-0.00005, -0.0001),
        ] {
            assert_eq!(super::round4(value), expected);
        }
    }
}
