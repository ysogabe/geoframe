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
    key: Arc<str>,
    card: Value,
    vector: Option<Arc<Vec<f32>>>,
    /// Placement score: the similarity (semantic) or the old `keep_score`/`quality_score` scale.
    /// A local scene score is normalised inside its own scene, so it is never used across groups.
    score: f64,
    /// Rank inside a local-ranked scene. Partial-overlap (review) scenes carry none.
    scene_rank: Option<usize>,
    scene_representative: bool,
    /// Stored scene position, written into the card only for the groups that are returned.
    scene: Option<SceneMark>,
    /// The adopted Gemini comparison recommends this photo (set only for the returned disjoint scenes).
    gemini: bool,
}
/// A returned scene's group-level facts (the JSON is built only for returned groups).
struct SceneGroup<'a> {
    state: &'static str,
    overlap: &'static str,
    result_id: &'a str,
    /// A scene without an old group of its own (not an exact copy of one) gets scene metadata.
    own_meta: bool,
    group: &'a super::groups::Group,
}
#[derive(Clone, Copy)]
struct SceneMark {
    rank: usize,
    score: Option<f64>,
    representative: bool,
    state: &'static str,
    overlap: &'static str,
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
    // Scene results are attached as information. Only a local-ranked scene (disjoint or an exact
    // copy of an old group) orders anything; a partial overlap is left to the old selection.
    let mut local: HashMap<i64, (Option<Arc<str>>, SceneMark)> = HashMap::with_capacity(
        scenes
            .as_ref()
            .map_or(0, |s| s.groups().iter().map(|g| g.members().len()).sum()),
    );
    // Only the groups that are returned get their JSON; building it for every scene would be per-request work
    // proportional to the library.
    let mut scene_groups: HashMap<Arc<str>, SceneGroup> = HashMap::new();
    if let Ok(scenes) = &scenes {
        for g in scenes.groups() {
            // Waiting is never a representative or a rank. A result whose photos do not all have a
            // vector that passed the search's own check (deleted, wrong shape, non-finite values) is
            // not used either: the cheap stored-scene guard cannot see a damaged BLOB, this can.
            if g.state() == super::groups::State::Waiting
                || !has_active
                || g.members()
                    .iter()
                    .any(|m| !active.contains_key(&m.photo_id))
            {
                continue;
            }
            let (state, overlap) = (g.state().as_str(), g.overlap().as_str());
            let ordering = g.state() == super::groups::State::LocalRanked;
            // An exact scene is the old group itself: it keeps the old key (and so the paid summary,
            // kind and similarities). A disjoint scene gets its own key.
            let exact = g
                .legacy_origins()
                .first()
                .filter(|_| g.overlap() == super::groups::Overlap::Exact)
                .map(|o| o.group_id().to_string());
            let key = ordering.then(|| {
                Arc::<str>::from(exact.clone().unwrap_or_else(|| g.result_id().to_string()))
            });
            if let Some(key) = &key {
                scene_groups.insert(
                    Arc::clone(key),
                    SceneGroup {
                        state,
                        overlap,
                        result_id: g.result_id(),
                        own_meta: exact.is_none(),
                        group: g,
                    },
                );
            }
            for m in g.members() {
                let mark = SceneMark {
                    rank: m.rank,
                    score: m.score,
                    representative: m.representative,
                    state,
                    overlap,
                };
                local.insert(m.photo_id, (key.clone(), mark));
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
        let scene_entry = local.get(&id);
        let scene_key = scene_entry.and_then(|(k, _)| k.as_ref());
        let key: Arc<str> = match scene_key {
            Some(k) => Arc::clone(k),
            None => ai_group.filter(|s| !s.is_empty()).unwrap_or_else(|| {
                if card["quality_group_kind"] == "similar" { format!("local-similar-{}",card["quality_group_id"]) } else { format!("photo-{id}") }
            }).into(),
        };
        let (scene_rank, scene_representative) = match (scene_key, scene_entry) {
            (Some(_), Some((_, mark))) => (Some(mark.rank), mark.representative),
            _ => (None, false),
        };
        let scene = scene_entry.map(|(_, mark)| *mark);
        let values = new.map(|(v,_)| Arc::clone(v)).or_else(|| {
            if has_active { None } else { legacy_vector(r.get("embedding").ok().flatten(), r.get("embedding_dim").ok().flatten()) }
        });
        let score = if vector.is_some() { new.unwrap().1 } else { card["keep_score"].as_f64().or_else(|| card["quality_score"].as_f64()).unwrap_or_default()/100.0 };
        Ok(Some(Photo { id, key, card, vector:values, score, scene_rank, scene_representative, scene, gemini:false }))
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
    let mut meta = HashMap::new();
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
    let mut members = HashMap::<Arc<str>, Vec<Photo>>::new();
    for photo in photos {
        if seen.contains(&photo.key) {
            members.entry(photo.key.clone()).or_default().push(photo);
        }
    }
    let threshold = config.ai_visual_similarity;
    // A paid Gemini comparison is used on the disjoint scenes it was adopted for: shown, and it picks the
    // representative and the order inside the group. It never hides a photo or reorders the groups. The
    // history table is looked up once per request, and only when a returned scene needs it.
    let mut archive: Option<Option<super::scene_judge::Archive>> = None;
    let mut result = Vec::new();
    for (key, best) in group_order {
        if whole.remaining().is_none() {
            return Ok(Err(Error::Unavailable));
        }
        let mut rows = members.remove(&key).unwrap();
        let scene = scene_groups.get(&key);
        let saved = match scene.filter(|s| s.own_meta) {
            Some(scene) => {
                if archive.is_none() {
                    archive = Some(super::scene_judge::Archive::open(conn, config)?);
                }
                match archive.as_ref().and_then(Option::as_ref) {
                    Some(archive) => archive.applied(scene.group)?,
                    None => None,
                }
            }
            None => None,
        };
        if let Some(saved) = &saved {
            for row in &mut rows {
                row.gemini = saved.verdict().recommended(saved.position(row.id));
            }
        }
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
                .then_with(|| b.gemini.cmp(&a.gemini))
                .then_with(|| b.scene_representative.cmp(&a.scene_representative))
                .then_with(|| {
                    // Recommended listing follows the scene's own rank; a query keeps its similarity order.
                    if vector.is_some() {
                        std::cmp::Ordering::Equal
                    } else {
                        a.scene_rank
                            .unwrap_or(usize::MAX)
                            .cmp(&b.scene_rank.unwrap_or(usize::MAX))
                    }
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
            if let Some(mark) = row.scene {
                row.card["local_rank"] = json!(mark.rank);
                row.card["local_score"] = json!(mark.score);
                row.card["local_representative"] = json!(mark.representative);
                row.card["scene_state"] = json!(mark.state);
                row.card["scene_overlap"] = json!(mark.overlap);
            }
        }
        let mut group = meta.remove(&*key).unwrap_or_else(|| {
            if scene.is_some_and(|s| s.own_meta) {
                json!({"kind":"scene","summary":"","min_similarity":null,"max_similarity":null,"avg_similarity":null})
            } else {
                json!({"kind":if key.starts_with("local-similar-") {"similar"} else {"single"},"summary":"","min_similarity":null,"max_similarity":null,"avg_similarity":null})
            }
        });
        if let Some(scene) = scene {
            group["scene_state"] = json!(scene.state);
            group["scene_overlap"] = json!(scene.overlap);
            group["scene_result_id"] = json!(scene.result_id);
            if let Some(saved) = &saved {
                group["summary"] = json!(saved.verdict().summary());
                group["same_scene"] = json!(saved.verdict().same_scene());
                group["judge_model"] = json!(saved.model());
                group["judge_source"] = json!("gemini");
                for row in &mut rows {
                    let position = saved.position(row.id);
                    row.card["gemini_recommended"] = json!(saved.verdict().recommended(position));
                    row.card["gemini_keep_score"] = json!(
                        position
                            .and_then(|i| saved.verdict().photos().get(i))
                            .map(super::scene_judge::PhotoVerdict::keep_score)
                    );
                }
            }
        }
        group["group_id"] = json!(&*key);
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
    let scene_summary = super::groups::runtime::status_summary(&prepared, &tx);
    let selection = crate::selection::admin_counts(&tx)?;
    let building = super::index::building_in(
        &tx,
        super::embedding::configured_identity(
            &config.ai,
            super::settings::TaskKind::ImageEmbedding,
        )
        .as_ref(),
        identity.as_ref().map(super::space::SpaceIdentity::space_id),
    )?;
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
    let mut out = json!({"ok":true,"photos":photos,"judged":if active {all_judged}else{judged},"recommended":if active {all_recommended}else{recommended},"groups":group_status.values().sum::<i64>(),"eligible":index.progress.eligible,"pending_embeddings":if active {index.progress.pending}else{(index.progress.eligible-legacy).max(0)},"group_status":group_status,
        "embedding_model":identity.as_ref().map_or(model,|i|i.model_id()),"embedding_backend":if active {if identity.as_ref().is_some_and(|i| i.provider_space_id() == super::embedding::PROVIDER_SPACE) {"local_http"} else {"active_index"}}else{match config.embed_backend {EmbedBackend::Local=>"local",EmbedBackend::Gemini=>"gemini"}},"other_model_embeddings":if active {index.other_shas}else{other},
        "pipeline":{"index_state":state,"query_compatible":compatible,"indexed":index.progress.indexed,"pending":index.progress.pending,"failed_excluded":index.progress.failed_excluded,"failed_reasons":index.progress.failed_reasons.into_iter().collect::<BTreeMap<_,_>>(),"corrupt":index.corrupt,"selection_judged":all_judged,"selection_recommended":all_recommended,"selection_groups":selection.groups,"selection_suppressed":selection.feed_suppressed,"scenes":scene_summary}});
    if let Some(building) = building {
        out["pipeline"]["building"] = building_json(&building);
    }
    Ok(out)
}

/// `pipeline.building`: 公開前の空間（設定が指す画像 Embedding の空間で、まだ検索に使われていないもの）の進み具合。
/// 件数と固定のコードだけ（空間の ID・モデル・SHA は出さない）。`indexed` はベクトルの行の有無（BLOB は読まない）。
fn building_json(building: &super::index::Building) -> Value {
    let p = &building.progress;
    json!({
        "eligible": p.eligible,
        "indexed": p.indexed,
        "pending": p.pending,
        "failed_excluded": p.failed_excluded,
        "failed_reasons": p.failed_reasons.iter().copied().collect::<BTreeMap<_, _>>(),
        "coverage": round4(building.coverage),
        "min_coverage": building.min_coverage,
        "publish": building.publish.as_str(),
    })
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
