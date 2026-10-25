//! E12・E13 の単体テスト（検査の境界・更新する列・書き込みが無いときの扱い）。Python の
//! `admin_setloc`・`admin_setflag` との突き合わせは `tests/update_api.rs`（フィクスチャ）。
//! 座標・地名は架空の値。

use serde_json::{Map, Value, json};

use super::{set_flag, set_loc};
use crate::post::ApiError;
use crate::testutil::TempDir;
use crate::writer::{OpenMode, Writer};

struct Db {
    _dir: TempDir,
    writer: Writer,
}

fn db() -> Db {
    let dir = TempDir::new("update");
    let writer = Writer::open(&dir.path().join("catalog.db"), OpenMode::CreateIfMissing).unwrap();
    writer.call(crate::schema::migrate).unwrap();
    writer
        .call(|c| {
            c.execute_batch(
                "INSERT INTO photos(id, source, name, lat, lon, loc_source, inferred_from, infer_dt_s,
                                    place, elev, muni_cd, pref, city, display, favorite, hidden, rotation)
                 VALUES(1, 'drive', 'a', 35.0, 139.0, 'inferred', 7, 600,
                        'どこか', 3.0, '01', '県', '市', 'derivatives/a.jpg', 1, 0, 90);
                 INSERT INTO photos(id, source, name) VALUES(2, 'drive', 'b');",
            )
        })
        .unwrap();
    Db { _dir: dir, writer }
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn message(e: ApiError) -> (&'static str, String) {
    match e {
        ApiError::BadRequest(m) => ("400", m),
        ApiError::NotFound(m) => ("404", m),
        ApiError::Unavailable => ("503", String::new()),
        other => panic!("{other:?}"),
    }
}

fn setloc_err(db: &Db, payload: Value) -> (&'static str, String) {
    message(set_loc(&obj(payload), Some(&db.writer)).unwrap_err())
}

fn setflag_err(db: &Db, payload: Value) -> (&'static str, String) {
    message(set_flag(&obj(payload), Some(&db.writer)).unwrap_err())
}

fn row(db: &Db, id: i64) -> Vec<Value> {
    db.writer
        .call(move |c| {
            c.query_row(
                "SELECT lat, lon, loc_source, inferred_from, infer_dt_s, place, elev, muni_cd,
                        pref, city, display, favorite, hidden, rotation FROM photos WHERE id=?1",
                [id],
                |r| {
                    (0..14)
                        .map(|i| {
                            Ok(match r.get_ref(i)? {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(n) => json!(n),
                                rusqlite::types::ValueRef::Real(f) => json!(f),
                                rusqlite::types::ValueRef::Text(t) => {
                                    json!(String::from_utf8_lossy(t))
                                }
                                rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                            })
                        })
                        .collect()
                },
            )
        })
        .unwrap()
}

#[test]
fn setlocは位置と手動の印を書き_推定と地名の列を空にする() {
    let db = db();
    let out = set_loc(
        &obj(json!({"id": 1, "lat": 43.0, "lon": 141.5})),
        Some(&db.writer),
    )
    .unwrap();
    assert_eq!(
        out,
        json!({"ok": true, "id": 1, "lat": 43.0, "lon": 141.5, "loc_source": "manual"})
    );
    // 書く列は Python の admin_setloc と同じ。display・favorite・hidden・rotation は触らない
    assert_eq!(
        row(&db, 1),
        vec![
            json!(43.0),
            json!(141.5),
            json!("manual"),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            json!("derivatives/a.jpg"),
            json!(1),
            json!(0),
            json!(90),
        ]
    );
    // 整数の座標は浮動小数で返る（Python の float(v)）
    let out = set_loc(
        &obj(json!({"id": 2, "lat": 35, "lon": -180})),
        Some(&db.writer),
    )
    .unwrap();
    assert_eq!(out["lat"].as_f64(), Some(35.0));
    assert!(out["lat"].is_f64() && out["lon"].is_f64());
}

#[test]
fn setlocの境界は閉区間で_bool_文字列_範囲外_idの不正は400() {
    let db = db();
    for (lat, lon) in [(90.0, 180.0), (-90.0, -180.0), (0.0, 0.0)] {
        let payload = json!({"id": 2, "lat": lat, "lon": lon});
        assert!(
            set_loc(&obj(payload), Some(&db.writer)).is_ok(),
            "{lat},{lon}"
        );
    }
    let bad = |payload: Value| setloc_err(&db, payload);
    assert_eq!(
        bad(json!({"id": 2, "lat": 90.000001, "lon": 0})),
        ("400", "lat が範囲外です（-90.0〜90.0）".into())
    );
    assert_eq!(bad(json!({"id": 2, "lat": -90.000001, "lon": 0})).0, "400");
    assert_eq!(
        bad(json!({"id": 2, "lat": 0, "lon": 180.000001})),
        ("400", "lon が範囲外です（-180.0〜180.0）".into())
    );
    assert_eq!(bad(json!({"id": 2, "lat": 0, "lon": -181})).0, "400");
    // bool・文字列・null・欠落・配列は数値ではない
    for value in [
        json!(true),
        json!("35.0"),
        Value::Null,
        json!([1]),
        json!({"a": 1}),
    ] {
        assert_eq!(
            bad(json!({"id": 2, "lat": value, "lon": 0})),
            ("400", "lat は数値で指定してください".into())
        );
        assert_eq!(
            bad(json!({"id": 2, "lat": 0, "lon": value})),
            ("400", "lon は数値で指定してください".into())
        );
    }
    assert_eq!(
        bad(json!({"id": 2, "lon": 0})),
        ("400", "lat は数値で指定してください".into())
    );
    // id: bool・文字列・小数・欠落は整数ではない。0 と負数は範囲外
    for value in [json!(true), json!("2"), json!(2.0), json!(1.5), Value::Null] {
        assert_eq!(
            bad(json!({"id": value, "lat": 0, "lon": 0})),
            ("400", "id は整数で指定してください".into())
        );
    }
    assert_eq!(
        bad(json!({"lat": 0, "lon": 0})),
        ("400", "id は整数で指定してください".into())
    );
    for id in [0, -1, i64::MIN] {
        assert_eq!(
            bad(json!({"id": id, "lat": 0, "lon": 0})),
            ("400", "id が範囲外です".into())
        );
    }
    // 検査の順序は id → lat → lon（最初の違反だけを返す）
    assert_eq!(
        bad(json!({"id": 0, "lat": "x", "lon": "y"})),
        ("400", "id が範囲外です".into())
    );
    assert_eq!(
        bad(json!({"id": 2, "lat": "x", "lon": "y"})),
        ("400", "lat は数値で指定してください".into())
    );
}

#[test]
fn setlocとsetflagは存在しないidを404にし_何も書かない() {
    let db = db();
    assert_eq!(
        setloc_err(&db, json!({"id": 99, "lat": 1, "lon": 1})),
        ("404", "id=99 は存在しません".into())
    );
    assert_eq!(
        setflag_err(&db, json!({"id": 99, "favorite": true})),
        ("404", "id=99 は存在しません".into())
    );
    // i64 を超える id は、Python では SQLite に渡せず 500。Rust は存在しない id として 404
    let (status, text) = setloc_err(
        &db,
        serde_json::from_str(r#"{"id": 9223372036854775808, "lat": 1, "lon": 1}"#).unwrap(),
    );
    assert_eq!(
        (status, text.as_str()),
        ("404", "id=9223372036854775808 は存在しません")
    );
    // ただし、ほかの検査が先（id 以外の違反は 400 のまま）
    let (status, _) = setloc_err(
        &db,
        serde_json::from_str(r#"{"id": 9223372036854775808, "lat": 99, "lon": 1}"#).unwrap(),
    );
    assert_eq!(status, "400");
}

/// rowid は `i64::MAX` まで実在しうる。それより大きい `id` を `i64::MAX` に丸めると、その行が更新されてしまう。
#[test]
fn i64を超えるidは最大のidの行があっても_その行を更新せず404() {
    let db = db();
    db.writer
        .call(|c| {
            c.execute(
                "INSERT INTO photos(id, source, name, lat, lon, loc_source, favorite, hidden)
                 VALUES(9223372036854775807, 'drive', 'max', 35.0, 139.0, 'exif', 0, 0)",
                [],
            )
        })
        .unwrap();
    let before = row(&db, i64::MAX);
    let before_all = (row(&db, 1), row(&db, 2));
    let too_big = r#"9223372036854775808"#;
    let setloc = format!(r#"{{"id": {too_big}, "lat": 1, "lon": 1}}"#);
    let setflag = format!(r#"{{"id": {too_big}, "favorite": true, "hidden": true}}"#);
    assert_eq!(
        setloc_err(&db, serde_json::from_str(&setloc).unwrap()),
        ("404", format!("id={too_big} は存在しません"))
    );
    assert_eq!(
        setflag_err(&db, serde_json::from_str(&setflag).unwrap()),
        ("404", format!("id={too_big} は存在しません"))
    );
    // u64 の最大値でも同じ
    let setflag = r#"{"id": 18446744073709551615, "hidden": true}"#;
    assert_eq!(
        setflag_err(&db, serde_json::from_str(setflag).unwrap()),
        ("404", "id=18446744073709551615 は存在しません".into())
    );
    assert_eq!(row(&db, i64::MAX), before, "最大の id の行は変わらない");
    assert_eq!((row(&db, 1), row(&db, 2)), before_all);
    // 入力の検査（400）と writer の確認（503）は、範囲外の id でも今まで通りその順で先
    assert_eq!(
        setloc_err(
            &db,
            serde_json::from_str(&format!(r#"{{"id": {too_big}, "lat": 1}}"#)).unwrap()
        )
        .0,
        "400"
    );
    assert_eq!(
        setflag_err(
            &db,
            serde_json::from_str(&format!(r#"{{"id": {too_big}}}"#)).unwrap()
        )
        .0,
        "400"
    );
    let err = set_loc(&obj(serde_json::from_str(&setloc).unwrap()), None).unwrap_err();
    assert_eq!(message(err).0, "503");
    let err = set_flag(
        &obj(serde_json::from_str(&format!(r#"{{"id": {too_big}, "favorite": true}}"#)).unwrap()),
        None,
    )
    .unwrap_err();
    assert_eq!(message(err).0, "503");
    // 実在する最大の id そのものは、普通に更新できる
    let out = set_flag(
        &obj(json!({"id": i64::MAX, "favorite": true})),
        Some(&db.writer),
    )
    .unwrap();
    assert_eq!(
        out,
        json!({"ok": true, "id": i64::MAX, "favorite": true, "hidden": false})
    );
}

#[test]
fn setflagは指定した列だけ書き_更新後の両方の値を返す() {
    let db = db();
    // 1 は favorite=1, hidden=0。hidden だけ指定すると favorite はそのまま
    let out = set_flag(&obj(json!({"id": 1, "hidden": true})), Some(&db.writer)).unwrap();
    assert_eq!(
        out,
        json!({"ok": true, "id": 1, "favorite": true, "hidden": true})
    );
    let out = set_flag(&obj(json!({"id": 1, "favorite": false})), Some(&db.writer)).unwrap();
    assert_eq!(
        out,
        json!({"ok": true, "id": 1, "favorite": false, "hidden": true})
    );
    let out = set_flag(
        &obj(json!({"id": 1, "favorite": true, "hidden": false})),
        Some(&db.writer),
    )
    .unwrap();
    assert_eq!(
        out,
        json!({"ok": true, "id": 1, "favorite": true, "hidden": false})
    );
    // 位置などには触らない
    let r = row(&db, 1);
    assert_eq!(r[2], json!("inferred"));
    assert_eq!(r[10], json!("derivatives/a.jpg"));
    assert_eq!(r[13], json!(90));
}

#[test]
fn setflagの不正な入力は400() {
    let db = db();
    assert_eq!(
        setflag_err(&db, json!({"id": 1})),
        (
            "400",
            "favorite か hidden のいずれかを指定してください".into()
        )
    );
    assert_eq!(setflag_err(&db, json!({"id": 1, "other": true})).0, "400");
    // bool だけ。null・数値・文字列は拒否（1 は bool ではない）
    for value in [
        Value::Null,
        json!(1),
        json!(0),
        json!("true"),
        json!([true]),
    ] {
        assert_eq!(
            setflag_err(&db, json!({"id": 1, "favorite": value})),
            ("400", "favorite は true/false で指定してください".into())
        );
        assert_eq!(
            setflag_err(&db, json!({"id": 1, "hidden": value})),
            ("400", "hidden は true/false で指定してください".into())
        );
    }
    // favorite → hidden の順に見る。片方が不正なら、もう片方が正しくても書かない
    assert_eq!(
        setflag_err(&db, json!({"id": 1, "favorite": false, "hidden": 1})).1,
        "hidden は true/false で指定してください"
    );
    assert_eq!(row(&db, 1)[11], json!(1), "不正な入力では何も書かない");
    // id の検査が先
    assert_eq!(
        setflag_err(&db, json!({"id": true, "favorite": "x"})).1,
        "id は整数で指定してください"
    );
    assert_eq!(setflag_err(&db, json!({"id": 0})).1, "id が範囲外です");
}

#[test]
fn writerが無ければ検査のあとで書かずに503() {
    let db = db();
    let before = (row(&db, 1), row(&db, 2));
    let payload = obj(json!({"id": 1, "lat": 1, "lon": 1}));
    assert!(matches!(
        set_loc(&payload, None),
        Err(ApiError::Unavailable)
    ));
    let payload = obj(json!({"id": 1, "favorite": true}));
    assert!(matches!(
        set_flag(&payload, None),
        Err(ApiError::Unavailable)
    ));
    // 入力の違反は 503 より先（取り込みの有無によらず Python と同じ 400）
    let payload = obj(json!({"id": 1, "lat": 100, "lon": 1}));
    assert!(matches!(
        set_loc(&payload, None),
        Err(ApiError::BadRequest(_))
    ));
    let payload = obj(json!({"id": 1}));
    assert!(matches!(
        set_flag(&payload, None),
        Err(ApiError::BadRequest(_))
    ));
    assert_eq!(before, (row(&db, 1), row(&db, 2)));
}
