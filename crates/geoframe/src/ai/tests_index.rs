//! 検索索引（空間 ID・ベクトル・切替・読み取り・検索。P4 / T4.3）の試験。固定のベクトルと合成の DB だけを使い、
//! 外部通信も実 Embedding の呼び出しもしない。
//!
//! - 識別: 全項目の照合・版の補完なし・設定の宣言だけでは識別にならない・`space_id` の再利用の拒否・識別の組の一意性
//! - ベクトル: 次元・NaN・Infinity・ゼロ・短い BLOB・L2 の安定・`none` の保持・壊れた行は読み飛ばし
//! - 顔ぶれ: 更新・回転・削除・重複・hidden・favorite
//! - 切替: 未完了の待ち・1 件失敗でも公開・全件失敗/閾値未達は旧ポインタ・世代変更・追加が続く・並行する切替の読み取り
//! - 検索: 空間の混在 0・識別の不一致の拒否

use rusqlite::{Connection, params};

use super::activation::{self, ActivateError, Activation};
use super::index::{self, ActiveState, Registered, SpaceError, StoreError, Stored};
use super::jobs::{self, JobKind};
use super::search::{self, QueryVector, SearchError};
use super::settings::Normalization;
use super::space::{self, IdentityError, SpaceIdentity, VectorError};
use crate::schema;
use crate::testutil::TempDir;

const NOW: i64 = 1_792_972_800;

fn ident(space_id: &str, dim: u32, norm: Normalization) -> SpaceIdentity {
    SpaceIdentity::new(
        space_id, "shared-1", "model-a", "rev-1", "img-v1", "txt-v1", dim, norm,
    )
    .unwrap()
}

/// 全項目を 1 つずつ変えた識別（`space_id` は同じ）。
fn variants(base: &SpaceIdentity) -> Vec<SpaceIdentity> {
    let n = base.normalization();
    let d = base.dimension();
    let s = base.space_id();
    let make = |p: &str, m: &str, r: &str, i: &str, t: &str, dim: u32, norm: Normalization| {
        SpaceIdentity::new(s, p, m, r, i, t, dim, norm).unwrap()
    };
    vec![
        make("shared-2", "model-a", "rev-1", "img-v1", "txt-v1", d, n),
        make("shared-1", "model-b", "rev-1", "img-v1", "txt-v1", d, n),
        make("shared-1", "model-a", "rev-2", "img-v1", "txt-v1", d, n),
        make("shared-1", "model-a", "rev-1", "img-v2", "txt-v1", d, n),
        make("shared-1", "model-a", "rev-1", "img-v1", "txt-v2", d, n),
        make("shared-1", "model-a", "rev-1", "img-v1", "txt-v1", d + 1, n),
        make(
            "shared-1",
            "model-a",
            "rev-1",
            "img-v1",
            "txt-v1",
            d,
            if n == Normalization::L2 {
                Normalization::Raw
            } else {
                Normalization::L2
            },
        ),
    ]
}

struct Db {
    _dir: TempDir,
    path: std::path::PathBuf,
    conn: Connection,
}

fn db(tag: &str) -> Db {
    let dir = TempDir::new(tag);
    let path = dir.path().join("catalog.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    schema::migrate(&mut conn).unwrap();
    super::migrate(&mut conn).unwrap();
    Db {
        _dir: dir,
        path,
        conn,
    }
}

impl Db {
    /// 写真と、世代の合う品質特徴量（`content_sha256 = sha`）。
    fn photo(&self, id: i64, sha: &str) {
        self.conn
            .execute(
                "INSERT INTO photos(id,source,name,display,md5,mtime,lat,lon)
                 VALUES(?1,'drive','n',?2,?3,?4,35.0,139.0)",
                params![
                    id,
                    format!("derivatives/p{id}.jpg"),
                    format!("m{id}"),
                    id as f64
                ],
            )
            .unwrap();
        self.conn
            .execute(
                "INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,
                    content_sha256,error,analyzed_at)
                 VALUES(?1,?2,?3,?4,?5,'','t')",
                params![
                    id,
                    format!("derivatives/p{id}.jpg"),
                    format!("m{id}"),
                    id as f64,
                    sha
                ],
            )
            .unwrap();
    }

    fn register(&mut self, identity: &SpaceIdentity) {
        index::register_space(&mut self.conn, identity, NOW).unwrap();
    }

    fn store(&mut self, identity: &SpaceIdentity, sha: &str, values: &[f32]) -> Stored {
        index::store_vector(&mut self.conn, identity, sha, "sent", values, NOW).unwrap()
    }

    fn fail_job(&mut self, identity: &SpaceIdentity, sha: &str) {
        let key = index::image_embedding_key(identity, sha);
        jobs::enqueue(&mut self.conn, &key, NOW).unwrap();
        self.conn
            .execute(
                "UPDATE ai_jobs SET state='failed', terminal=1, attempts=5, last_error_code='bad_response'
                 WHERE kind='image_embedding' AND target_key=?1 AND input_hash=?2
                   AND processor_version=?3",
                params![key.target_key, key.input_hash, key.processor_version],
            )
            .unwrap();
    }

    fn pointer(&self) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key='ai_active_space'",
                [],
                |r| r.get(0),
            )
            .ok()
    }

    fn set_pointer(&self, value: &str) {
        self.conn
            .execute(
                "INSERT INTO settings(key,value) VALUES('ai_active_space',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [value],
            )
            .unwrap();
    }

    fn try_activate(&mut self, identity: &SpaceIdentity, min: f64) -> Activation {
        let snapshot = activation::snapshot_targets(&self.conn, identity).unwrap();
        activation::try_activate(&mut self.conn, &snapshot, identity, min).unwrap()
    }
}

fn unit3(x: f32, y: f32, z: f32) -> Vec<f32> {
    vec![x, y, z]
}

// ---------------------------------------------------------------- 識別

#[test]
fn an_identity_needs_every_field_and_never_fills_in_a_version() {
    assert!(SpaceIdentity::new("s", "p", "m", "r", "i", "t", 768, Normalization::L2).is_ok());
    // 空の版は補わず拒否する
    for bad in [
        ("", "p", "m", "r", "i", "t"),
        ("s", "", "m", "r", "i", "t"),
        ("s", "p", "", "r", "i", "t"),
        ("s", "p", "m", "", "i", "t"),
        ("s", "p", "m", "r", "", "t"),
        ("s", "p", "m", "r", "i", ""),
        ("s", "p", "m", "r", "i", "bad value"),
        ("s", "p", "m", "r", "i", "../x"),
    ] {
        assert_eq!(
            SpaceIdentity::new(
                bad.0,
                bad.1,
                bad.2,
                bad.3,
                bad.4,
                bad.5,
                8,
                Normalization::L2
            )
            .unwrap_err(),
            IdentityError::BadToken,
            "{bad:?}"
        );
    }
    for dim in [0, 4097, u32::MAX] {
        assert_eq!(
            SpaceIdentity::new("s", "p", "m", "r", "i", "t", dim, Normalization::L2).unwrap_err(),
            IdentityError::BadDimension
        );
    }
    assert!(SpaceIdentity::new("s", "p", "m", "r", "i", "t", 4096, Normalization::Raw).is_ok());
}

#[test]
fn any_single_field_difference_makes_a_different_space() {
    let base = ident("s", 3, Normalization::L2);
    assert_eq!(base, ident("s", 3, Normalization::L2));
    for other in variants(&base) {
        assert_ne!(base, other);
        assert_ne!(base.fingerprint(), other.fingerprint());
        assert_ne!(
            index::image_embedding_processor_version(&base),
            index::image_embedding_processor_version(&other)
        );
    }
    // 同名のモデル・同じ次元でも、版や前処理が違えば別の空間
    let a = SpaceIdentity::new(
        "a",
        "p",
        "same-model",
        "r1",
        "i",
        "t",
        768,
        Normalization::L2,
    )
    .unwrap();
    let b = SpaceIdentity::new(
        "a",
        "p",
        "same-model",
        "r2",
        "i",
        "t",
        768,
        Normalization::L2,
    )
    .unwrap();
    assert_ne!(a, b);
}

#[test]
fn a_declared_space_alone_is_not_an_identity() {
    // 設定の宣言（形式を検査しただけ）には Provider の共通の空間 ID が無い。Provider が証明した値を別に渡して初めて識別になる
    let declared = super::tests_settings::sample();
    let declared = declared.embedding_space().expect("sample has a space");
    let identity = SpaceIdentity::from_declared(declared, "provider-shared").unwrap();
    assert_eq!(identity.space_id(), declared.space_id);
    assert_eq!(identity.provider_space_id(), "provider-shared");
    assert_eq!(identity.dimension(), declared.dimension);
    assert_ne!(
        identity,
        SpaceIdentity::from_declared(declared, "another-provider").unwrap()
    );
    // Debug に値（識別子）を出さない
    let text = format!("{identity:?}");
    assert!(
        !text.contains("provider-shared") && !text.contains(&declared.model_id),
        "{text}"
    );
}

// ---------------------------------------------------------------- 空間の登録

#[test]
fn registering_a_space_never_overwrites_a_different_identity() {
    let mut db = db("space-register");
    let base = ident("s1", 3, Normalization::L2);
    assert_eq!(
        index::register_space(&mut db.conn, &base, NOW).unwrap(),
        Registered::Created
    );
    assert_eq!(
        index::register_space(&mut db.conn, &base, NOW + 1).unwrap(),
        Registered::Existing
    );
    // 同じ space_id を別の識別（どの項目でも）で再利用すると拒否。行は変わらない
    for other in variants(&base) {
        assert!(matches!(
            index::register_space(&mut db.conn, &other, NOW).unwrap_err(),
            SpaceError::SpaceIdConflict
        ));
        assert_eq!(index::load_space(&db.conn, "s1").unwrap().unwrap(), base);
    }
    // 同じ識別の組を別の space_id で登録するのも拒否（space_id を除く全項目が UNIQUE）
    let twin = SpaceIdentity::new(
        "s2",
        base.provider_space_id(),
        base.model_id(),
        base.revision(),
        base.image_preprocess(),
        base.text_preprocess(),
        base.dimension(),
        base.normalization(),
    )
    .unwrap();
    assert!(matches!(
        index::register_space(&mut db.conn, &twin, NOW).unwrap_err(),
        SpaceError::IdentityConflict
    ));
    let spaces: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM ai_embedding_spaces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(spaces, 1);
    // 制約そのものも効いている（アプリの検査を通らない直接の INSERT でも重複しない）
    let direct = db.conn.execute(
        "INSERT INTO ai_embedding_spaces(space_id,provider_space_id,model_id,revision,
           image_preprocess,text_preprocess,dimension,normalization,created_at)
         VALUES('s3','shared-1','model-a','rev-1','img-v1','txt-v1',3,'l2',0)",
        [],
    );
    assert!(direct.is_err());
}

// ---------------------------------------------------------------- ベクトル

#[test]
fn l2_vectors_are_stored_normalized_and_raw_vectors_are_kept() {
    let mut db = db("vec-norm");
    let l2 = ident("l2", 3, Normalization::L2);
    let raw = SpaceIdentity::new(
        "raw",
        "shared-1",
        "model-a",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::Raw,
    )
    .unwrap();
    db.register(&l2);
    db.register(&raw);
    assert_eq!(db.store(&l2, "sha-1", &[3.0, 4.0, 0.0]), Stored::Inserted);
    assert_eq!(db.store(&raw, "sha-1", &[3.0, 4.0, 0.0]), Stored::Inserted);
    let blob = |space: &str| -> Vec<u8> {
        db.conn
            .query_row(
                "SELECT vector FROM ai_embedding_vectors WHERE space_id=?1 AND input_sha256='sha-1'",
                [space],
                |r| r.get(0),
            )
            .unwrap()
    };
    let l2_values = space::decode(&l2, &blob("l2")).unwrap();
    assert!((l2_values[0] - 0.6).abs() < 1e-6 && (l2_values[1] - 0.8).abs() < 1e-6);
    let raw_values = space::decode(&raw, &blob("raw")).unwrap();
    assert_eq!(raw_values, [3.0, 4.0, 0.0]);
    // sent_sha256 は監査用に別に記録される
    let sent: String = db
        .conn
        .query_row(
            "SELECT sent_sha256 FROM ai_embedding_vectors WHERE space_id='l2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sent, "sent");
}

#[test]
fn l2_is_stable_for_extreme_magnitudes() {
    let id = ident("s", 4, Normalization::L2);
    for scale in [1e-45_f32, 1e-30, 1.0, 1e30, 3.0e38] {
        let out = space::prepare(&id, &[scale, -scale, scale, 0.0]).unwrap();
        assert!(out.iter().all(|v| v.is_finite()), "{scale}");
        let norm: f64 = out
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "{scale}: {norm}");
        // 向きは変わらない
        assert!((out[0] - 0.5773503).abs() < 1e-5 && out[3] == 0.0);
    }
    // 大きい値どうしでも 2 乗和がオーバーフローしない
    let big = space::prepare(&id, &[f32::MAX, f32::MAX, f32::MAX, f32::MAX]).unwrap();
    assert!(big.iter().all(|v| (v - 0.5).abs() < 1e-6));
}

#[test]
fn bad_vectors_are_rejected_and_nothing_is_written() {
    let mut db = db("vec-bad");
    let id = ident("s", 3, Normalization::L2);
    db.register(&id);
    let cases: [(&[f32], VectorError); 7] = [
        (&[f32::NAN, 0.0, 1.0], VectorError::NotFinite),
        (&[f32::INFINITY, 0.0, 1.0], VectorError::NotFinite),
        (&[f32::NEG_INFINITY, 0.0, 1.0], VectorError::NotFinite),
        (&[0.0, 0.0, 0.0], VectorError::ZeroNorm),
        (&[1.0, 0.0], VectorError::Dimension),
        (&[1.0, 0.0, 0.0, 0.0], VectorError::Dimension),
        (&[], VectorError::Dimension),
    ];
    for (values, expected) in cases {
        let err = index::store_vector(&mut db.conn, &id, "sha-1", "sent", values, NOW).unwrap_err();
        assert!(
            matches!(err, StoreError::Vector(e) if e == expected),
            "{values:?}"
        );
    }
    // 不正な SHA
    for sha in ["", "has space", &"x".repeat(129)] {
        assert!(matches!(
            index::store_vector(&mut db.conn, &id, sha, "sent", &[1.0, 0.0, 0.0], NOW).unwrap_err(),
            StoreError::BadSha
        ));
    }
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM ai_embedding_vectors", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 0);
    // 短い・長い BLOB は表の制約でも拒否される（直接の INSERT）
    for blob in [vec![0u8; 8], vec![0u8; 16], Vec::new()] {
        let r = db.conn.execute(
            "INSERT INTO ai_embedding_vectors(space_id,input_sha256,sent_sha256,dimension,vector,created_at)
             VALUES('s','x','y',3,?1,0)",
            [blob],
        );
        assert!(r.is_err());
    }
}

#[test]
fn storing_into_an_unregistered_or_different_space_is_refused() {
    let mut db = db("vec-space");
    let id = ident("s", 3, Normalization::L2);
    assert!(matches!(
        index::store_vector(&mut db.conn, &id, "a", "b", &[1.0, 0.0, 0.0], NOW).unwrap_err(),
        StoreError::UnknownSpace
    ));
    db.register(&id);
    for other in variants(&id) {
        let values = vec![1.0; other.dimension() as usize];
        assert!(matches!(
            index::store_vector(&mut db.conn, &other, "a", "b", &values, NOW).unwrap_err(),
            StoreError::SpaceMismatch
        ));
    }
}

#[test]
fn the_same_input_keeps_the_first_vector_and_a_corrupt_row_is_repaired() {
    let mut db = db("vec-dup");
    let id = ident("s", 3, Normalization::L2);
    db.register(&id);
    assert_eq!(db.store(&id, "sha-1", &[1.0, 0.0, 0.0]), Stored::Inserted);
    assert_eq!(db.store(&id, "sha-1", &[0.0, 1.0, 0.0]), Stored::Existing);
    let blob: Vec<u8> = db
        .conn
        .query_row("SELECT vector FROM ai_embedding_vectors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(space::decode(&id, &blob).unwrap(), [1.0, 0.0, 0.0]);
    // 壊れた行（NaN）は検出され、保存が置き換える
    let nan: Vec<u8> = [f32::NAN, 0.0, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    db.conn
        .execute("UPDATE ai_embedding_vectors SET vector=?1", [nan])
        .unwrap();
    assert_eq!(db.store(&id, "sha-1", &[0.0, 1.0, 0.0]), Stored::Repaired);
    let blob: Vec<u8> = db
        .conn
        .query_row("SELECT vector FROM ai_embedding_vectors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(space::decode(&id, &blob).unwrap(), [0.0, 1.0, 0.0]);
}

#[test]
fn decoding_a_bad_blob_is_an_error_not_a_panic() {
    let l2 = ident("s", 3, Normalization::L2);
    assert_eq!(
        space::decode(&l2, &[0u8; 11]).unwrap_err(),
        VectorError::Dimension
    );
    assert_eq!(space::decode(&l2, &[]).unwrap_err(), VectorError::Dimension);
    let enc = |v: &[f32]| space::encode(v);
    assert_eq!(
        space::decode(&l2, &enc(&[f32::NAN, 0.0, 0.0])).unwrap_err(),
        VectorError::NotFinite
    );
    assert_eq!(
        space::decode(&l2, &enc(&[0.0, 0.0, 0.0])).unwrap_err(),
        VectorError::ZeroNorm
    );
    // L2 の空間なのに単位ベクトルでない
    assert_eq!(
        space::decode(&l2, &enc(&[3.0, 4.0, 0.0])).unwrap_err(),
        VectorError::NotNormalized
    );
    assert!(space::decode(&l2, &enc(&[0.6, 0.8, 0.0])).is_ok());
}

// ---------------------------------------------------------------- 索引の顔ぶれ

#[test]
fn eligible_targets_follow_the_current_generation_of_each_photo() {
    let db = db("eligible");
    db.photo(1, "sha-a");
    db.photo(2, "sha-a"); // 同じ内容: 1 件に数える（代表は 1）
    db.photo(3, "sha-b");
    db.photo(4, "sha-c"); // hidden・favorite でも対象（検索側で絞る）
    db.conn
        .execute("UPDATE photos SET hidden=1, favorite=1 WHERE id=4", [])
        .unwrap();
    db.photo(5, "sha-d"); // 品質の解析が失敗
    db.conn
        .execute(
            "UPDATE photo_quality_features SET error='x' WHERE photo_id=5",
            [],
        )
        .unwrap();
    db.photo(6, "sha-e"); // md5 が更新された（解析は古い世代）
    db.conn
        .execute("UPDATE photos SET md5='new' WHERE id=6", [])
        .unwrap();
    db.photo(7, "sha-f"); // 回転で display が変わった
    db.conn
        .execute(
            "UPDATE photos SET display='derivatives/p7.r90.jpg' WHERE id=7",
            [],
        )
        .unwrap();
    db.photo(8, "sha-g"); // 削除
    db.conn
        .execute("DELETE FROM photos WHERE id=8", [])
        .unwrap();
    db.photo(9, "sha-h"); // display が空
    db.conn
        .execute("UPDATE photos SET display='' WHERE id=9", [])
        .unwrap();
    db.conn
        .execute(
            "UPDATE photo_quality_features SET display='' WHERE photo_id=9",
            [],
        )
        .unwrap();
    db.photo(10, "sha-i");
    db.conn
        .execute(
            "UPDATE photo_quality_features SET content_sha256=NULL WHERE photo_id=10",
            [],
        )
        .unwrap();

    let rows = index::classify(&db.conn, None).unwrap();
    let got: Vec<(i64, &str)> = rows
        .iter()
        .map(|c| (c.target.photo_id, c.target.input_sha256.as_str()))
        .collect();
    assert_eq!(got, [(1, "sha-a"), (3, "sha-b"), (4, "sha-c")]);
}

#[test]
fn status_counts_progress_from_the_effective_join_only() {
    let mut db = db("status");
    let a = ident("a", 3, Normalization::L2);
    let b = SpaceIdentity::new(
        "b",
        "shared-2",
        "model-b",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    db.register(&a);
    db.register(&b);
    for (i, sha) in ["s1", "s2", "s3", "s4"].iter().enumerate() {
        db.photo(i as i64 + 1, sha);
    }
    db.store(&a, "s1", &unit3(1.0, 0.0, 0.0));
    db.store(&a, "s2", &unit3(0.0, 1.0, 0.0));
    db.store(&b, "s3", &unit3(0.0, 0.0, 1.0)); // 別の空間
    db.store(&b, "s1", &unit3(0.0, 0.0, 1.0)); // 同じ SHA が二空間にあっても 1 つに数える
    db.store(&b, "old-sha", &unit3(0.0, 0.0, 1.0)); // 今の対象に無い
    db.fail_job(&a, "s4");
    db.set_pointer("a");

    let st = index::status(&db.conn).unwrap();
    assert_eq!(st.active, ActiveState::Active("a".into()));
    assert_eq!(st.progress.eligible, 4);
    assert_eq!(st.progress.indexed, 2);
    assert_eq!(st.progress.pending, 1); // s3 は a にまだ無い
    assert_eq!(st.progress.failed_excluded, 1);
    assert_eq!(
        st.progress.failed_reasons,
        [("bad_response".to_string(), 1)]
    );
    assert_eq!(st.other_space_shas, 3); // s3・s1・old-sha（重複なし）
    assert_eq!(st.legacy_shas, 0);
    assert_eq!(st.spaces, 2);
}

// ---------------------------------------------------------------- 切替

#[test]
fn a_partial_build_keeps_the_old_pointer_until_nothing_is_pending() {
    let mut db = db("act-wait");
    let a = ident("a", 3, Normalization::L2);
    db.register(&a);
    db.photo(1, "s1");
    db.photo(2, "s2");
    db.set_pointer("old-space");
    db.store(&a, "s1", &unit3(1.0, 0.0, 0.0));
    // s2 のジョブが pending / running / deferred / 未登録 のどれでも公開を待たせる
    for state in [None, Some("pending"), Some("running"), Some("deferred")] {
        if let Some(state) = state {
            let key = index::image_embedding_key(&a, "s2");
            let _ = jobs::enqueue(&mut db.conn, &key, NOW).unwrap();
            db.conn
                .execute("UPDATE ai_jobs SET state=?1 WHERE input_hash='s2'", [state])
                .unwrap();
        }
        let outcome = db.try_activate(&a, 0.0);
        assert!(
            matches!(outcome, Activation::Waiting(c) if c.pending == 1 && c.indexed == 1),
            "{state:?}"
        );
        assert_eq!(db.pointer().as_deref(), Some("old-space"));
    }
    db.store(&a, "s2", &unit3(0.0, 1.0, 0.0));
    let outcome = db.try_activate(&a, 0.95);
    assert!(outcome.published());
    assert_eq!(db.pointer().as_deref(), Some("a"));
}

#[test]
fn one_failed_in_a_hundred_still_publishes_and_the_retry_joins_without_reactivation() {
    let mut db = db("act-100");
    let a = ident("a", 3, Normalization::L2);
    db.register(&a);
    for i in 1..=100 {
        db.photo(i, &format!("s{i}"));
    }
    for i in 1..=99 {
        db.store(&a, &format!("s{i}"), &unit3(1.0, i as f32, 0.0));
    }
    db.fail_job(&a, "s100");
    let outcome = db.try_activate(&a, index::DEFAULT_ACTIVATION_MIN_COVERAGE);
    let Activation::Published(c) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        (c.valid, c.indexed, c.failed_excluded, c.pending),
        (100, 99, 1, 0)
    );
    assert!((c.coverage - 0.99).abs() < 1e-12);
    assert_eq!(db.pointer().as_deref(), Some("a"));
    let loaded = search::load_active_index(&db.conn).unwrap();
    assert_eq!(loaded.index.unwrap().len(), 99);
    assert_eq!(loaded.status.progress.failed_excluded, 1);

    // 手動再試行が成功してベクトルが保存されると、再切替なしで有効な結合に入る
    db.store(&a, "s100", &unit3(0.0, 0.0, 1.0));
    let loaded = search::load_active_index(&db.conn).unwrap();
    assert_eq!(loaded.index.unwrap().len(), 100);
    assert_eq!(loaded.status.progress.failed_excluded, 0);
    assert_eq!(loaded.status.progress.indexed, 100);
}

#[test]
fn all_failed_or_below_threshold_keeps_the_old_pointer() {
    let mut db = db("act-fail");
    let a = ident("a", 3, Normalization::L2);
    db.register(&a);
    db.set_pointer("old-space");
    for i in 1..=20 {
        db.photo(i, &format!("s{i}"));
        db.fail_job(&a, &format!("s{i}"));
    }
    // 全件失敗は完成扱いにしない（閾値 0 でも）
    let outcome = db.try_activate(&a, 0.0);
    assert!(
        matches!(outcome, Activation::NoVectors(c) if c.failed_excluded == 20 && c.indexed == 0)
    );
    assert_eq!(db.pointer().as_deref(), Some("old-space"));
    // 18/20 = 90% は既定の 95% に届かない
    for i in 1..=18 {
        db.store(&a, &format!("s{i}"), &unit3(1.0, i as f32, 0.0));
    }
    let outcome = db.try_activate(&a, index::DEFAULT_ACTIVATION_MIN_COVERAGE);
    assert!(matches!(outcome, Activation::BelowThreshold(c) if c.indexed == 18 && c.valid == 20));
    assert_eq!(db.pointer().as_deref(), Some("old-space"));
    // 閾値は呼び出し側が決める（結果の識別には入らない）
    assert!(db.try_activate(&a, 0.9).published());
    assert_eq!(db.pointer().as_deref(), Some("a"));
    // 閾値ちょうどは通る
    let spaces: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM ai_embedding_spaces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(spaces, 1);
}

#[test]
fn only_the_exact_failed_job_excludes_a_target() {
    let mut db = db("act-exact");
    let a = ident("a", 3, Normalization::L2);
    let other_space = SpaceIdentity::new(
        "zz",
        "shared-9",
        "model-a",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    db.register(&a);
    db.photo(1, "s1");
    db.store(&a, "s1", &unit3(1.0, 0.0, 0.0));
    db.photo(2, "s2");
    // 別の空間の失敗・別の SHA の失敗・古い処理版の失敗・failed でない状態では除かれない（待たせる）
    db.fail_job(&other_space, "s2");
    db.fail_job(&a, "s-other");
    let old_version = jobs::JobKey::new(
        JobKind::ImageEmbedding,
        index::image_embedding_key(&a, "s2").target_key,
        "s2",
        "image_embedding/v0/old",
    );
    jobs::enqueue(&mut db.conn, &old_version, NOW).unwrap();
    db.conn
        .execute(
            "UPDATE ai_jobs SET state='failed', terminal=1 WHERE processor_version='image_embedding/v0/old'",
            [],
        )
        .unwrap();
    let wrong_kind = jobs::JobKey::new(
        JobKind::LocationClue,
        index::image_embedding_key(&a, "s2").target_key,
        "s2",
        index::image_embedding_key(&a, "s2").processor_version,
    );
    jobs::enqueue(&mut db.conn, &wrong_kind, NOW).unwrap();
    db.conn
        .execute(
            "UPDATE ai_jobs SET state='failed', terminal=1 WHERE kind='location_clue'",
            [],
        )
        .unwrap();
    let outcome = db.try_activate(&a, 0.0);
    assert!(matches!(outcome, Activation::Waiting(c) if c.pending == 1 && c.failed_excluded == 0));
    // 正しいキーの failed は除く
    db.fail_job(&a, "s2");
    assert!(db.try_activate(&a, 0.0).published());
    // obsolete は除かない（入力が戻れば再び走る）
    db.conn
        .execute(
            "UPDATE ai_jobs SET state='obsolete' WHERE input_hash='s2' AND kind='image_embedding'
               AND processor_version=?1",
            [index::image_embedding_processor_version(&a)],
        )
        .unwrap();
    assert!(matches!(db.try_activate(&a, 0.0), Activation::Waiting(_)));
}

#[test]
fn targets_changed_after_the_snapshot_are_dropped_and_new_photos_never_block() {
    let mut db = db("act-stale");
    let a = ident("a", 3, Normalization::L2);
    db.register(&a);
    for i in 1..=5 {
        db.photo(i, &format!("s{i}"));
        db.store(&a, &format!("s{i}"), &unit3(1.0, i as f32, 0.0));
    }
    db.photo(6, "s6"); // ベクトルがまだ無い
    let snapshot = activation::snapshot_targets(&db.conn, &a).unwrap();
    assert_eq!(snapshot.len(), 6);
    // 構築のあいだに: 6 は更新（md5）、5 は削除、4 は回転（display の変更）、新しい写真 7 が増える
    db.conn
        .execute("UPDATE photos SET md5='changed' WHERE id=6", [])
        .unwrap();
    db.conn
        .execute("DELETE FROM photos WHERE id=5", [])
        .unwrap();
    db.conn
        .execute(
            "UPDATE photos SET display='derivatives/p4.r90.jpg' WHERE id=4",
            [],
        )
        .unwrap();
    db.photo(7, "s7");
    let outcome = activation::try_activate(&mut db.conn, &snapshot, &a, 1.0).unwrap();
    let Activation::Published(c) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        (c.snapshot, c.stale, c.valid, c.indexed, c.pending),
        (6, 3, 3, 3, 0)
    );
    assert_eq!(db.pointer().as_deref(), Some("a"));
    // 構築後に追加された写真 7 は待たせず、ベクトルが保存された時点で有効な結合により加わる
    assert_eq!(
        search::load_active_index(&db.conn)
            .unwrap()
            .index
            .unwrap()
            .len(),
        3
    );
    db.store(&a, "s7", &unit3(0.0, 0.0, 1.0));
    assert_eq!(
        search::load_active_index(&db.conn)
            .unwrap()
            .index
            .unwrap()
            .len(),
        4
    );
    // 更新された写真 6 の古い SHA のベクトルは参照されない
    db.store(&a, "s6", &unit3(0.0, 0.0, 1.0));
    assert_eq!(
        search::load_active_index(&db.conn)
            .unwrap()
            .index
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn the_pointer_switches_atomically_between_spaces_and_bad_inputs_are_refused() {
    let mut db = db("act-switch");
    let a = ident("a", 3, Normalization::L2);
    let b = SpaceIdentity::new(
        "b",
        "shared-2",
        "model-b",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    db.register(&a);
    db.register(&b);
    db.photo(1, "s1");
    db.store(&a, "s1", &unit3(1.0, 0.0, 0.0));
    assert!(db.try_activate(&a, 0.95).published());
    // b は未構築 → a のまま
    assert!(matches!(db.try_activate(&b, 0.95), Activation::Waiting(_)));
    assert_eq!(db.pointer().as_deref(), Some("a"));
    db.store(&b, "s1", &unit3(0.0, 1.0, 0.0));
    assert!(db.try_activate(&b, 0.95).published());
    assert_eq!(db.pointer().as_deref(), Some("b"));

    let snapshot = activation::snapshot_targets(&db.conn, &a).unwrap();
    for bad in [f64::NAN, -0.1, 1.1, f64::INFINITY] {
        assert!(matches!(
            activation::try_activate(&mut db.conn, &snapshot, &a, bad),
            Err(ActivateError::InvalidThreshold)
        ));
    }
    // 別の空間の候補・未登録の空間・識別が違う空間
    assert!(matches!(
        activation::try_activate(&mut db.conn, &snapshot, &b, 0.5),
        Err(ActivateError::WrongSnapshot)
    ));
    let ghost = ident("ghost", 3, Normalization::L2);
    let ghost_snapshot = activation::snapshot_targets(&db.conn, &ghost).unwrap();
    assert!(matches!(
        activation::try_activate(&mut db.conn, &ghost_snapshot, &ghost, 0.5),
        Err(ActivateError::UnknownSpace)
    ));
    let impostor = &variants(&a)[1];
    assert!(matches!(
        activation::try_activate(&mut db.conn, &snapshot, impostor, 0.5),
        Err(ActivateError::SpaceMismatch)
    ));
    assert_eq!(db.pointer().as_deref(), Some("b"));
    // 解除（旧結果参照へ）。ベクトル・空間は消さない
    assert!(activation::deactivate(&mut db.conn).unwrap());
    assert!(!activation::deactivate(&mut db.conn).unwrap());
    assert_eq!(db.pointer(), None);
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM ai_embedding_vectors", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 2);
}

// ---------------------------------------------------------------- 読み取り・検索

#[test]
fn an_old_database_without_the_new_tables_reads_as_no_space_without_writing() {
    let dir = TempDir::new("read-old");
    let (path, _keep) = crate::testutil::synthetic_catalog(dir.path(), 2);
    let ro = crate::db::open_read_only(&path).unwrap();
    let loaded = search::load_active_index(&ro).unwrap();
    assert!(loaded.index.is_none());
    assert_eq!(loaded.status.active, ActiveState::NoTables);
    assert_eq!(loaded.status.progress, index::Progress::default());
    assert_eq!(index::status(&ro).unwrap().spaces, 0);
    // ポインタが残っていても、空間の表が無ければ使わない
    assert!(!super::tests::table_exists(&ro, "ai_embedding_spaces"));
}

#[test]
fn spaces_and_legacy_vectors_never_mix_in_the_active_index() {
    let mut db = db("read-mix");
    let a = ident("a", 3, Normalization::L2);
    let b = SpaceIdentity::new(
        "b",
        "shared-2",
        "model-b",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    db.register(&a);
    db.register(&b);
    for (i, sha) in ["s1", "s2", "s3"].iter().enumerate() {
        db.photo(i as i64 + 1, sha);
    }
    db.photo(4, "s1"); // 同じ内容の複製
    db.store(&a, "s1", &unit3(1.0, 0.0, 0.0));
    db.store(&b, "s1", &unit3(0.0, 1.0, 0.0));
    db.store(&b, "s2", &unit3(0.0, 1.0, 0.0));
    // 旧結果: モデル名が空間の model_id と同じでも、新しい空間には入らない（legacy-unverified）
    db.conn
        .execute(
            "INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,
               embedding_model,embedding_dim,embedding,analyzed_at,error)
             VALUES(1,'derivatives/p1.jpg','m1',1.0,'s1','model-a',3,X'00','t','')",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO photo_embeddings VALUES('s3','model-a',3,X'00','t')",
            [],
        )
        .unwrap();
    db.set_pointer("a");

    let loaded = search::load_active_index(&db.conn).unwrap();
    let index = loaded.index.unwrap();
    // a には s1 だけ（同じ内容の複製は 1 項目・photo_ids に両方）
    assert_eq!(index.len(), 1);
    assert_eq!(index.entries()[0].photo_ids(), [1, 4]);
    assert_eq!(index.entries()[0].photo_id(), 1);
    assert_eq!(index.entries()[0].input_sha256(), "s1");
    assert_eq!(loaded.status.legacy_shas, 2); // s1（旧特徴量）・s3（旧キャッシュ）
    assert_eq!(loaded.status.other_space_shas, 2); // b の s1・s2
    assert_eq!(loaded.status.other_shas, 3); // s1・s2・s3（重複なし）
    // 旧結果のモデル名だけで空間が trusted になる経路は無い: 空間の数は変わらない
    assert_eq!(loaded.status.spaces, 2);
    // 旧表は一切変わらない
    let legacy: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM photo_ai_features", [], |r| r.get(0))
        .unwrap();
    assert_eq!(legacy, 1);
}

#[test]
fn a_corrupt_row_is_skipped_and_counted_instead_of_failing_the_whole_read() {
    let mut db = db("read-corrupt");
    let a = ident("a", 3, Normalization::L2);
    db.register(&a);
    for i in 1..=3 {
        db.photo(i, &format!("s{i}"));
        db.store(&a, &format!("s{i}"), &unit3(1.0, i as f32, 0.0));
    }
    db.set_pointer("a");
    let nan: Vec<u8> = [f32::NAN, 0.0, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let not_unit = space::encode(&[3.0, 4.0, 0.0]);
    let zero = space::encode(&[0.0, 0.0, 0.0]);
    for (sha, blob) in [("s1", nan), ("s2", not_unit)] {
        db.conn
            .execute(
                "UPDATE ai_embedding_vectors SET vector=?1 WHERE input_sha256=?2",
                params![blob, sha],
            )
            .unwrap();
    }
    let loaded = search::load_active_index(&db.conn).unwrap();
    assert_eq!(loaded.index.as_ref().unwrap().len(), 1);
    assert_eq!(loaded.status.corrupt, 2);
    assert_eq!(loaded.status.progress.indexed, 1);
    assert_eq!(loaded.status.progress.pending, 2); // 再生成の対象
    // ゼロベクトルも壊れた行として扱う
    let _ = db.conn.execute(
        "UPDATE ai_embedding_vectors SET vector=?1 WHERE input_sha256='s3'",
        [zero],
    );
    let loaded = search::load_active_index(&db.conn).unwrap();
    assert_eq!(loaded.index.unwrap().len(), 0);
    assert_eq!(loaded.status.corrupt, 3);
}

#[test]
fn search_orders_by_cosine_and_refuses_a_different_identity() {
    let mut db = db("search");
    let a = ident("a", 3, Normalization::L2);
    db.register(&a);
    let vectors = [
        ("s1", unit3(1.0, 0.0, 0.0)),
        ("s2", unit3(1.0, 1.0, 0.0)),
        ("s3", unit3(0.0, 1.0, 0.0)),
        ("s4", unit3(-1.0, 0.0, 0.0)),
    ];
    for (i, (sha, v)) in vectors.iter().enumerate() {
        db.photo(i as i64 + 1, sha);
        db.store(&a, sha, v);
    }
    db.set_pointer("a");
    let index = search::load_active_index(&db.conn).unwrap().index.unwrap();
    let query = QueryVector::new(&a, &[2.0, 0.5, 0.0]).unwrap(); // 長さが違っても向きだけを見る
    let hits = index.search(&query, 10).unwrap();
    let order: Vec<i64> = hits.iter().map(|h| h.photo_id).collect();
    assert_eq!(order, [1, 2, 3, 4]);
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
    assert!((hits[0].score - 0.970_142_5).abs() < 1e-5);
    assert_eq!(index.search(&query, 2).unwrap().len(), 2);
    assert!(index.search(&query, 0).unwrap().is_empty());
    // 同点は代表の写真 ID の昇順
    let tie = QueryVector::new(&a, &[0.0, 0.0, 1.0]).unwrap();
    let order: Vec<i64> = index
        .search(&tie, 4)
        .unwrap()
        .iter()
        .map(|h| h.photo_id)
        .collect();
    assert_eq!(order, [1, 2, 3, 4]);
    // 識別が違う検索語（全項目）は拒否。次元違いは作れない。NaN・ゼロも拒否
    for other in variants(&a) {
        let values = vec![1.0; other.dimension() as usize];
        let q = QueryVector::new(&other, &values).unwrap();
        assert_eq!(
            index.search(&q, 4).unwrap_err(),
            SearchError::IdentityMismatch
        );
    }
    assert_eq!(
        QueryVector::new(&a, &[1.0, 0.0]).unwrap_err(),
        VectorError::Dimension
    );
    assert_eq!(
        QueryVector::new(&a, &[f32::NAN, 0.0, 1.0]).unwrap_err(),
        VectorError::NotFinite
    );
    assert_eq!(
        QueryVector::new(&a, &[0.0, 0.0, 0.0]).unwrap_err(),
        VectorError::ZeroNorm
    );
}

#[test]
fn raw_spaces_compare_by_direction() {
    let mut db = db("search-raw");
    let raw = SpaceIdentity::new(
        "r",
        "shared-1",
        "model-a",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::Raw,
    )
    .unwrap();
    db.register(&raw);
    db.photo(1, "s1");
    db.photo(2, "s2");
    db.store(&raw, "s1", &[100.0, 0.0, 0.0]);
    db.store(&raw, "s2", &[0.0, 0.01, 0.0]);
    db.set_pointer("r");
    let index = search::load_active_index(&db.conn).unwrap().index.unwrap();
    let hits = index
        .search(&QueryVector::new(&raw, &[0.0, 3.0, 0.1]).unwrap(), 2)
        .unwrap();
    assert_eq!(hits[0].photo_id, 2);
    assert!(hits[0].score > 0.99);
}

#[test]
fn a_reader_sees_the_pointer_and_its_vectors_from_the_same_snapshot_while_the_pointer_switches() {
    let mut db = db("read-concurrent");
    let a = ident("a", 3, Normalization::L2);
    let b = SpaceIdentity::new(
        "b",
        "shared-2",
        "model-b",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    db.register(&a);
    db.register(&b);
    for i in 1..=4 {
        db.photo(i, &format!("s{i}"));
        db.store(&a, &format!("s{i}"), &unit3(1.0, i as f32, 0.0));
    }
    for i in 1..=2 {
        db.store(&b, &format!("s{i}"), &unit3(0.0, i as f32, 1.0));
    }
    db.set_pointer("a");

    // 読み取りの途中で別の接続がポインタを b に切り替えても、読み取りの中は a のポインタと a のベクトルのまま
    let reader = crate::db::open_read_only(&db.path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let first = search::load_in(&reader).unwrap();
    db.set_pointer("b");
    let second = search::load_in(&reader).unwrap();
    reader.execute_batch("COMMIT").unwrap();
    assert_eq!(first.status.active, ActiveState::Active("a".into()));
    assert_eq!(second.status.active, ActiveState::Active("a".into()));
    assert_eq!(first.index.unwrap().len(), 4);
    assert_eq!(second.index.unwrap().len(), 4);
    // 次の読み取りは b と b のベクトル
    let after = search::load_active_index(&reader).unwrap();
    assert_eq!(after.status.active, ActiveState::Active("b".into()));
    assert_eq!(after.index.unwrap().len(), 2);
}

#[test]
fn switching_in_a_loop_never_pairs_one_pointer_with_the_other_spaces_vectors() {
    let mut db = db("read-loop");
    let a = ident("a", 3, Normalization::L2);
    let b = SpaceIdentity::new(
        "b",
        "shared-2",
        "model-b",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    db.register(&a);
    db.register(&b);
    for i in 1..=4 {
        db.photo(i, &format!("s{i}"));
        db.store(&a, &format!("s{i}"), &unit3(1.0, i as f32, 0.0));
    }
    for i in 1..=2 {
        db.store(&b, &format!("s{i}"), &unit3(0.0, i as f32, 1.0));
    }
    db.set_pointer("a");
    let path = db.path.clone();
    let writer = std::thread::spawn(move || {
        let conn = Connection::open(path).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        for i in 0..300 {
            let target = if i % 2 == 0 { "b" } else { "a" };
            conn.execute(
                "UPDATE settings SET value=?1 WHERE key='ai_active_space'",
                [target],
            )
            .unwrap();
        }
    });
    let reader = crate::db::open_read_only(&db.path).unwrap();
    reader
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let (mut seen_a, mut seen_b) = (0, 0);
    for _ in 0..300 {
        let loaded = search::load_active_index(&reader).unwrap();
        let index = loaded.index.unwrap();
        match loaded.status.active {
            ActiveState::Active(ref id) if id == "a" => {
                seen_a += 1;
                assert_eq!((index.identity().space_id(), index.len()), ("a", 4));
            }
            ActiveState::Active(ref id) if id == "b" => {
                seen_b += 1;
                assert_eq!((index.identity().space_id(), index.len()), ("b", 2));
            }
            other => panic!("{other:?}"),
        }
    }
    writer.join().unwrap();
    assert!(seen_a + seen_b == 300);
}

#[test]
fn a_dangling_or_foreign_pointer_is_treated_as_no_space() {
    let db = db("read-dangling");
    db.photo(1, "s1");
    db.set_pointer("never-registered");
    let loaded = search::load_active_index(&db.conn).unwrap();
    assert!(loaded.index.is_none());
    assert_eq!(loaded.status.active, ActiveState::Unusable);
    db.conn
        .execute(
            "UPDATE settings SET value=X'0102' WHERE key='ai_active_space'",
            [],
        )
        .unwrap();
    assert_eq!(
        search::load_active_index(&db.conn).unwrap().status.active,
        ActiveState::Unusable
    );
    db.conn
        .execute("DELETE FROM settings WHERE key='ai_active_space'", [])
        .unwrap();
    assert_eq!(
        search::load_active_index(&db.conn).unwrap().status.active,
        ActiveState::Unset
    );
}

// ---------------------------------------------------------------- ジョブのキー

#[test]
fn the_image_embedding_key_depends_only_on_the_space_and_the_input() {
    let a = ident("a", 3, Normalization::L2);
    let key = index::image_embedding_key(&a, "sha-1");
    assert_eq!(key, index::image_embedding_key(&a.clone(), "sha-1"));
    assert_eq!(key.kind, JobKind::ImageEmbedding);
    assert_eq!(key.input_hash, "sha-1");
    assert!(key.target_key.contains("a") && key.target_key.ends_with("sha-1"));
    assert_ne!(key, index::image_embedding_key(&a, "sha-2"));
    for other in variants(&a) {
        assert_ne!(key, index::image_embedding_key(&other, "sha-1"));
    }
    // 別の空間 ID は target_key が違う（同じ入力のジョブを空間ごとに持つ）
    let b = ident("b", 3, Normalization::L2);
    assert_ne!(
        key.target_key,
        index::image_embedding_key(&b, "sha-1").target_key
    );
    // 冪等: 同じキーを積み直しても 1 件
    let mut db = db("key-idem");
    for _ in 0..3 {
        jobs::enqueue(&mut db.conn, &key, NOW).unwrap();
    }
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM ai_jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

// ---------------------------------------------------------------- 旧表・主データの不変

#[test]
fn building_and_switching_never_touch_legacy_or_primary_rows() {
    let mut db = db("untouched");
    let a = ident("a", 3, Normalization::L2);
    db.register(&a);
    db.photo(1, "s1");
    db.conn
        .execute(
            "UPDATE photos SET favorite=1, hidden=1, rotation=90, loc_source='manual', lat=1.5, lon=2.5 WHERE id=1",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,
               embedding_model,embedding_dim,embedding,analyzed_at,keep_score)
             VALUES(1,'derivatives/p1.jpg','m1',1.0,'s1','old-model',3,X'0102','t',77)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO photo_embeddings VALUES('s1','old-model',3,X'0102','t')",
            [],
        )
        .unwrap();
    let before = super::tests::dump(&db.conn);
    db.store(&a, "s1", &unit3(1.0, 0.0, 0.0));
    assert!(db.try_activate(&a, 0.95).published());
    activation::deactivate(&mut db.conn).unwrap();
    assert_eq!(super::tests::dump(&db.conn), before);
}
