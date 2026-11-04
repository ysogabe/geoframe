//! 分類器の試験。合成ポリゴンで判定手順 1〜4 の各分岐を確かめ、公開の地点で同梱データのスモークを行う。
//! 実際の撮影座標は使わない。座標は (緯度, 経度) の順で渡す（`RegionIndex::classify` と同じ）。

use super::data::{self, DataError, Layer, PolygonSet};
use super::index::{Classification, InvalidCoordinate, Method, RegionIndex};
use super::search;

type Pt = (f64, f64); // (経度 x, 緯度 y)

#[derive(Clone)]
struct Poly {
    outer: Vec<Pt>,
    holes: Vec<Vec<Pt>>,
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Pt> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
}

fn solid(ring: Vec<Pt>) -> Poly {
    Poly {
        outer: ring,
        holes: vec![],
    }
}

fn add_feature(layer: &mut Layer, id: &str, parent: Option<usize>, polys: &[Poly]) {
    layer.begin_feature(id.to_owned(), parent);
    for p in polys {
        for ring in std::iter::once(&p.outer).chain(&p.holes) {
            for &(x, y) in ring {
                layer.push_point(x, y);
            }
            layer.end_ring().unwrap();
        }
        layer.end_polygon().unwrap();
    }
    layer.end_feature().unwrap();
}

/// `countries`: (ID, 多角形)、`regions`: (ID, 所属する国の ID, 多角形)。並びは与えたまま。
fn synth(
    countries: &[(&str, Vec<Poly>)],
    regions: &[(&str, &str, Vec<Poly>)],
) -> Result<RegionIndex, DataError> {
    let mut c = Layer::empty();
    for (id, polys) in countries {
        add_feature(&mut c, id, None, polys);
    }
    let mut r = Layer::empty();
    for (id, parent, polys) in regions {
        let idx = countries.iter().position(|(cid, _)| cid == parent).unwrap();
        add_feature(&mut r, id, Some(idx), polys);
    }
    RegionIndex::from_polygons(PolygonSet {
        countries: c,
        regions: r,
    })
}

type Countries = Vec<(&'static str, Vec<Poly>)>;
type Regions = Vec<(&'static str, &'static str, Vec<Poly>)>;

/// 判定手順の各分岐を含む世界（経度 x, 緯度 y）。
///
/// ```text
/// A : [0,10]x[0,10] に穴 [4,6]x[4,6]、島 [20,22]x[0,2]
/// C : 穴にぴったり入る飛び地 [4,6]x[4,6]
/// B : [10,20]x[0,10]（A と辺 x=10 を共有）
/// D : [30,40]x[0,10]（地域なし）
/// E : [50,60]x[0,10] と [55,65]x[0,10]（重なる 2 片）
/// F : 三角形 (80,0)(90,0)(80,10)
/// G : [100,110]x[0,10] に穴 [103,107]x[3,7]
/// 地域 A1: [0,5]x[0,8] と島、A2: [5,10]x[0,8]（辺 x=5 を共有。A の y>8 は地域なし）
/// 地域 B1: B 全体
/// 地域 C1: 飛び地 C の中に A1 が張り出して見える範囲 … は作らない（C には地域なし）
/// ```
fn world() -> (Countries, Regions) {
    let a = vec![
        Poly {
            outer: rect(0.0, 0.0, 10.0, 10.0),
            holes: vec![rect(4.0, 4.0, 6.0, 6.0)],
        },
        solid(rect(20.0, 0.0, 22.0, 2.0)),
    ];
    let countries = vec![
        ("ne:AAA", a),
        ("ne:CCC", vec![solid(rect(4.0, 4.0, 6.0, 6.0))]),
        ("ne:BBB", vec![solid(rect(10.0, 0.0, 20.0, 10.0))]),
        ("ne:DDD", vec![solid(rect(30.0, 0.0, 40.0, 10.0))]),
        (
            "ne:EEE",
            vec![
                solid(rect(50.0, 0.0, 60.0, 10.0)),
                solid(rect(55.0, 0.0, 65.0, 10.0)),
            ],
        ),
        (
            "ne:FFF",
            vec![solid(vec![
                (80.0, 0.0),
                (90.0, 0.0),
                (80.0, 10.0),
                (80.0, 0.0),
            ])],
        ),
        (
            "ne:GGG",
            vec![Poly {
                outer: rect(100.0, 0.0, 110.0, 10.0),
                holes: vec![rect(103.0, 3.0, 107.0, 7.0)],
            }],
        ),
    ];
    let regions = vec![
        (
            "ne:A1",
            "ne:AAA",
            vec![
                solid(rect(0.0, 0.0, 5.0, 8.0)),
                solid(rect(20.0, 0.0, 22.0, 2.0)),
            ],
        ),
        ("ne:A2", "ne:AAA", vec![solid(rect(5.0, 0.0, 10.0, 8.0))]),
        ("ne:B1", "ne:BBB", vec![solid(rect(10.0, 0.0, 20.0, 10.0))]),
        // 国 C の飛び地の上に重なる地域。所属は国 A なので、C の中の点には使われない
        ("ne:A3", "ne:AAA", vec![solid(rect(4.0, 4.0, 6.0, 6.0))]),
    ];
    (countries, regions)
}

fn world_index() -> RegionIndex {
    let (c, r) = world();
    synth(&c, &r).unwrap()
}

/// (country_id, region_id, country_method, region_method)
type Brief = (Option<String>, Option<String>, Method, Method);

fn brief(c: Classification<'_>) -> Brief {
    (
        c.country_id.map(str::to_owned),
        c.region_id.map(str::to_owned),
        c.country_method,
        c.region_method,
    )
}

fn at(idx: &RegionIndex, lat: f64, lon: f64) -> Brief {
    brief(idx.classify(lat, lon).unwrap())
}

fn expect(got: Brief, country: Option<&str>, region: Option<&str>, cm: Method, rm: Method) {
    assert_eq!(
        got,
        (
            country.map(str::to_owned),
            region.map(str::to_owned),
            cm,
            rm
        )
    );
}

use Method::{Ambiguous, Inside, Outside};

#[test]
fn step1_invalid_coordinates_are_not_classified() {
    let idx = world_index();
    for (lat, lon) in [
        (f64::NAN, 1.0),
        (1.0, f64::NAN),
        (f64::INFINITY, 1.0),
        (1.0, f64::NEG_INFINITY),
        (90.000001, 0.0),
        (-90.000001, 0.0),
        (0.0, 180.000001),
        (0.0, -180.000001),
        (1e300, 1e300),
    ] {
        assert_eq!(
            idx.classify(lat, lon),
            Err(InvalidCoordinate),
            "{lat} {lon}"
        );
    }
    // 端は有効（国の外なので outside）
    for (lat, lon) in [(90.0, 180.0), (-90.0, -180.0), (-20.0, -20.0), (-0.0, -1.0)] {
        let r = idx.classify(lat, lon).unwrap();
        assert_eq!((r.country_id, r.country_method), (None, Outside));
    }
}

#[test]
fn step2_sea_is_outside_for_both() {
    let idx = world_index();
    expect(at(&idx, 50.0, 100.0), None, None, Outside, Outside);
    expect(at(&idx, 5.0, 15.0 + 100.0), None, None, Outside, Outside);
    // 国の包絡矩形の中の、三角形の外
    expect(at(&idx, 9.0, 89.0), None, None, Outside, Outside);
}

#[test]
fn step3_one_country_one_region_is_inside() {
    let idx = world_index();
    expect(
        at(&idx, 1.0, 1.0),
        Some("ne:AAA"),
        Some("ne:A1"),
        Inside,
        Inside,
    );
    expect(
        at(&idx, 1.0, 8.0),
        Some("ne:AAA"),
        Some("ne:A2"),
        Inside,
        Inside,
    );
    expect(
        at(&idx, 5.0, 15.0),
        Some("ne:BBB"),
        Some("ne:B1"),
        Inside,
        Inside,
    );
}

#[test]
fn step3_country_only_when_no_region_contains_the_point() {
    let idx = world_index();
    // 国 A の中だが、地域は y ≤ 8 まで
    expect(at(&idx, 9.0, 2.0), Some("ne:AAA"), None, Inside, Outside);
    // 地域の無い国
    expect(at(&idx, 5.0, 35.0), Some("ne:DDD"), None, Inside, Outside);
}

#[test]
fn step3_islands_and_multi_polygon_pieces() {
    let idx = world_index();
    // 島（国 A の 2 つ目の多角形、地域 A1 の 2 つ目の多角形）
    expect(
        at(&idx, 1.0, 21.0),
        Some("ne:AAA"),
        Some("ne:A1"),
        Inside,
        Inside,
    );
    // 重なる 2 片の内側の点は 1 候補
    expect(at(&idx, 5.0, 57.0), Some("ne:EEE"), None, Inside, Outside);
    // 片 1 だけ・片 2 だけ
    expect(at(&idx, 5.0, 52.0), Some("ne:EEE"), None, Inside, Outside);
    expect(at(&idx, 5.0, 63.0), Some("ne:EEE"), None, Inside, Outside);
}

#[test]
fn step3_holes_are_outside_and_their_edges_are_inside() {
    let idx = world_index();
    // 穴 [103,107]x[3,7] の中は国の外
    expect(at(&idx, 5.0, 105.0), None, None, Outside, Outside);
    // 穴の縁・頂点は内部として扱う（境界上）
    expect(at(&idx, 5.0, 103.0), Some("ne:GGG"), None, Inside, Outside);
    expect(at(&idx, 3.0, 103.0), Some("ne:GGG"), None, Inside, Outside);
    expect(at(&idx, 7.0, 107.0), Some("ne:GGG"), None, Inside, Outside);
    // 穴の外側
    expect(at(&idx, 5.0, 102.0), Some("ne:GGG"), None, Inside, Outside);
}

#[test]
fn step3_an_enclave_in_a_hole_belongs_to_the_enclave_only() {
    let idx = world_index();
    // 穴の中（国 A ではなく C）。地域 A3 は C の範囲に重なるが、所属が違うので使わない
    expect(at(&idx, 5.0, 5.0), Some("ne:CCC"), None, Inside, Outside);
    expect(at(&idx, 4.5, 5.5), Some("ne:CCC"), None, Inside, Outside);
    // 飛び地と本国の境界（穴の縁）は両方の境界上なので国が ambiguous
    expect(at(&idx, 5.0, 4.0), None, None, Ambiguous, Ambiguous);
    expect(at(&idx, 4.0, 4.0), None, None, Ambiguous, Ambiguous);
}

#[test]
fn step4_shared_country_borders_are_ambiguous() {
    let idx = world_index();
    // 共有辺 x=10（A と B）
    expect(at(&idx, 5.0, 10.0), None, None, Ambiguous, Ambiguous);
    // 共有辺の端の頂点
    expect(at(&idx, 0.0, 10.0), None, None, Ambiguous, Ambiguous);
    expect(at(&idx, 10.0, 10.0), None, None, Ambiguous, Ambiguous);
    // 辺の両側の近く
    expect(
        at(&idx, 5.0, 9.999999),
        Some("ne:AAA"),
        Some("ne:A2"),
        Inside,
        Inside,
    );
    expect(
        at(&idx, 5.0, 10.000001),
        Some("ne:BBB"),
        Some("ne:B1"),
        Inside,
        Inside,
    );
}

#[test]
fn step3_shared_region_edge_is_ambiguous_for_the_region_only() {
    let idx = world_index();
    // 地域 A1 と A2 の共有辺 x=5（国は A だけ）
    expect(at(&idx, 1.0, 5.0), Some("ne:AAA"), None, Inside, Ambiguous);
    expect(at(&idx, 8.0, 5.0), Some("ne:AAA"), None, Inside, Ambiguous);
    // 国の外周と地域の外周が重なる点（x=0）は 1 地域
    expect(
        at(&idx, 1.0, 0.0),
        Some("ne:AAA"),
        Some("ne:A1"),
        Inside,
        Inside,
    );
}

#[test]
fn boundaries_count_as_inside() {
    let idx = world_index();
    // 外周の辺・頂点
    expect(at(&idx, 0.0, 30.0), Some("ne:DDD"), None, Inside, Outside);
    expect(at(&idx, 10.0, 40.0), Some("ne:DDD"), None, Inside, Outside);
    expect(at(&idx, 5.0, 30.0), Some("ne:DDD"), None, Inside, Outside);
    // 斜めの辺 (90,0)-(80,10) の上（外積がちょうど 0）
    expect(at(&idx, 5.0, 85.0), Some("ne:FFF"), None, Inside, Outside);
    expect(at(&idx, 0.0, 90.0), Some("ne:FFF"), None, Inside, Outside);
    // 斜めの辺の内側・外側
    expect(at(&idx, 4.0, 85.0), Some("ne:FFF"), None, Inside, Outside);
    expect(at(&idx, 6.0, 85.0), None, None, Outside, Outside);
    // 日付変更線・極の端（経度 ±180 を含む国）
    let wrap = synth(
        &[("ne:EDGE", vec![solid(rect(170.0, 80.0, 180.0, 90.0))])],
        &[],
    )
    .unwrap();
    expect(
        at(&wrap, 90.0, 180.0),
        Some("ne:EDGE"),
        None,
        Inside,
        Outside,
    );
    expect(
        at(&wrap, 85.0, 180.0),
        Some("ne:EDGE"),
        None,
        Inside,
        Outside,
    );
}

#[test]
fn results_do_not_depend_on_the_order_of_countries_regions_or_pieces() {
    let (countries, regions) = world();
    let base = synth(&countries, &regions).unwrap();

    let mut rev_c: Countries = countries.clone();
    rev_c.reverse();
    for (_, polys) in &mut rev_c {
        polys.reverse();
        for p in polys {
            // 輪の向きと始点も変える
            p.outer.pop();
            p.outer.rotate_left(1);
            p.outer.push(p.outer[0]);
            p.outer.reverse();
        }
    }
    let mut rev_r: Regions = regions.clone();
    rev_r.reverse();
    for (_, _, polys) in &mut rev_r {
        polys.reverse();
    }
    let reversed = synth(&rev_c, &rev_r).unwrap();

    let mut rot_c = countries.clone();
    rot_c.rotate_left(3);
    let mut rot_r = regions.clone();
    rot_r.rotate_left(1);
    let rotated = synth(&rot_c, &rot_r).unwrap();

    // 格子点（共有辺・頂点・穴の縁を含む 0.5 刻み）で一致する
    let mut n = 0;
    let mut y = -2.0;
    while y <= 12.0 {
        let mut x = -2.0;
        while x <= 115.0 {
            let b = at(&base, y, x);
            assert_eq!(b, at(&reversed, y, x), "reversed {y} {x}");
            assert_eq!(b, at(&rotated, y, x), "rotated {y} {x}");
            n += 1;
            x += 0.5;
        }
        y += 0.5;
    }
    assert!(n > 1000);
}

#[test]
fn region_ids_always_belong_to_the_country_id() {
    // 地域の所属は、国の ID の添字ではなく所属の宣言で決まる。国 B の範囲に別の国の地域を重ねても使われない
    let idx = synth(
        &[
            ("ne:AAA", vec![solid(rect(0.0, 0.0, 10.0, 10.0))]),
            ("ne:BBB", vec![solid(rect(20.0, 0.0, 30.0, 10.0))]),
        ],
        &[
            ("ne:A1", "ne:AAA", vec![solid(rect(0.0, 0.0, 30.0, 10.0))]),
            ("ne:B1", "ne:BBB", vec![solid(rect(18.0, 0.0, 32.0, 10.0))]),
        ],
    )
    .unwrap();
    expect(
        at(&idx, 5.0, 5.0),
        Some("ne:AAA"),
        Some("ne:A1"),
        Inside,
        Inside,
    );
    expect(
        at(&idx, 5.0, 25.0),
        Some("ne:BBB"),
        Some("ne:B1"),
        Inside,
        Inside,
    );
    // 国 A の地域 A1 は国 B の範囲にも広がるが、国 B の点では B1 だけが対象（地域 ambiguous にならない）
    for lon in [21.0, 29.0] {
        let r = idx.classify(5.0, lon).unwrap();
        assert_eq!(r.region_id, Some("ne:B1"));
    }
}

#[test]
fn a_region_pointing_outside_the_country_list_is_rejected() {
    let mut c = Layer::empty();
    add_feature(&mut c, "ne:AAA", None, &[solid(rect(0.0, 0.0, 1.0, 1.0))]);
    let mut r = Layer::empty();
    add_feature(&mut r, "ne:A1", Some(7), &[solid(rect(0.0, 0.0, 1.0, 1.0))]);
    let e = RegionIndex::from_polygons(PolygonSet {
        countries: c,
        regions: r,
    })
    .unwrap_err();
    assert!(e.0.contains("国参照"));
}

#[test]
fn builder_rejects_degenerate_shapes() {
    let mut l = Layer::empty();
    l.begin_feature("ne:X".into(), None);
    for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 0.0)] {
        l.push_point(x, y);
    }
    assert!(l.end_ring().is_err(), "3 点の輪");
    let mut l = Layer::empty();
    l.begin_feature("ne:X".into(), None);
    for (x, y) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
        l.push_point(x, y);
    }
    assert!(l.end_ring().is_err(), "閉じていない輪");
    assert!(l.end_polygon().is_err());
    assert!(l.end_feature().is_err());
}

// ---------------------------------------------------------------- 同梱データ（公開の地点）

fn real_index() -> &'static RegionIndex {
    use std::sync::OnceLock;
    static CELL: OnceLock<RegionIndex> = OnceLock::new();
    CELL.get_or_init(|| RegionIndex::load().expect("同梱データから索引を作れる"))
}

fn real(lat: f64, lon: f64) -> Classification<'static> {
    real_index().classify(lat, lon).unwrap()
}

fn region_name(id: &str) -> String {
    data::names().unwrap().region(id).unwrap().name.clone()
}

fn country_name(id: &str) -> String {
    data::names().unwrap().country(id).unwrap().name.clone()
}

#[test]
fn smoke_public_landmarks_resolve_to_the_expected_country_and_region() {
    // 公開の観光地・都市の座標だけ（私的な撮影座標ではない）
    let tokyo = real(35.6762, 139.6503);
    assert_eq!(tokyo.country_id, Some("ne:JPN"));
    let t = data::names().unwrap().region_of_pref("東京都").unwrap();
    assert_eq!(tokyo.region_id, Some(t.id.as_str()));
    assert_eq!(
        (tokyo.country_method, tokyo.region_method),
        (Inside, Inside)
    );

    for (lat, lon, country, region_en) in [
        (48.8566, 2.3522, "ne:FRA", "Paris"),
        (-33.8688, 151.2093, "ne:AUS", "New South Wales"),
        (-36.8485, 174.7633, "ne:NZL", "Auckland"),
        (21.3069, -157.8583, "ne:USA", "Hawaii"),
        (51.5074, -0.1278, "ne:GBR", "Westminster"),
        (40.7128, -74.0060, "ne:USA", "New York"),
    ] {
        let r = real(lat, lon);
        assert_eq!(r.country_id, Some(country), "{country}");
        assert_eq!(r.country_method, Inside);
        let region = r.region_id.unwrap_or_else(|| panic!("{country} の地域"));
        let p = data::names().unwrap().region(region).unwrap();
        assert!(
            p.search.iter().any(|s| s == region_en),
            "{country}: {region} ({:?}) は {region_en} のはず",
            p.search
        );
    }
}

#[test]
fn smoke_sea_poles_and_the_dateline() {
    // 大西洋・太平洋の真ん中、ヌル島
    for (lat, lon) in [(0.0, -30.0), (30.0, -40.0), (0.0, 0.0), (0.0, -150.0)] {
        let r = real(lat, lon);
        assert_eq!(
            (r.country_id, r.country_method),
            (None, Outside),
            "{lat} {lon}"
        );
        assert_eq!((r.region_id, r.region_method), (None, Outside));
    }
    // 南極（極点そのものは Natural Earth の輪の外。南緯 89 度は内側）
    assert_eq!(real(-89.0, 0.0).country_id, Some("ne:ATA"));
    // フィジーは日付変更線の両側（東経 178 度台・西経 179 度台）
    assert_eq!(real(-18.14, 178.44).country_id, Some("ne:FJI"));
    assert_eq!(real(-16.85, -179.95).country_id, Some("ne:FJI"));
    // 範囲の端
    for (lat, lon) in [(90.0, 180.0), (-90.0, -180.0), (90.0, 0.0)] {
        assert!(real_index().classify(lat, lon).is_ok());
    }
    assert!(real_index().classify(91.0, 0.0).is_err());
    assert!(real_index().classify(0.0, f64::NAN).is_err());
}

#[test]
fn smoke_enclaves_islands_and_japan_view_borders() {
    // 飛び地・小国
    assert_eq!(real(41.9029, 12.4534).country_id, Some("ne:VAT"));
    assert_eq!(real(-29.31, 27.48).country_id, Some("ne:LSO"));
    assert_eq!(real(43.7384, 7.4246).country_id, Some("ne:MCO"));
    // 日本の島（沖縄本島・父島・択捉島は日本視点版で日本。父島の輪は 14 点の粗い形で、岸に近い点は外れる）
    assert_eq!(real(26.2124, 127.6809).country_id, Some("ne:JPN"));
    assert_eq!(real(27.08, 142.22).country_id, Some("ne:JPN"));
    assert_eq!(real(45.0, 147.8).country_id, Some("ne:JPN"));
    // 日本の国の中で地域が決まらない点は、国のみ（地域 outside）か 1 地域
    let r = real(45.0, 147.8);
    assert_eq!(r.country_method, Inside);
    assert_ne!(r.region_method, Inside, "択捉島は Admin1 側ではロシア");
}

#[test]
fn smoke_a_vertex_shared_by_two_countries_is_ambiguous() {
    let set = real_polygons();
    let find = |id: &str| set.countries.ids().iter().position(|c| c == id).unwrap();
    let (es, pt) = (find("ne:ESP"), find("ne:PRT"));
    let verts = |f: usize| -> std::collections::HashSet<(u64, u64)> {
        set.countries
            .feature_rings(f)
            .flatten()
            .map(|p| (p[0].to_bits(), p[1].to_bits()))
            .collect()
    };
    let portugal = verts(pt);
    let shared = set
        .countries
        .feature_rings(es)
        .flatten()
        .find(|p| portugal.contains(&(p[0].to_bits(), p[1].to_bits())))
        .expect("スペインとポルトガルは頂点を共有する");
    let r = real(shared[1], shared[0]);
    assert_eq!(
        (r.country_id, r.region_id, r.country_method, r.region_method),
        (None, None, Ambiguous, Ambiguous)
    );
}

fn real_polygons() -> &'static PolygonSet {
    use std::sync::OnceLock;
    static CELL: OnceLock<PolygonSet> = OnceLock::new();
    CELL.get_or_init(|| data::load_polygons().unwrap())
}

#[test]
fn every_region_found_on_a_world_grid_belongs_to_the_country_found() {
    let names = data::names().unwrap();
    let (mut inside, mut with_region) = (0, 0);
    let mut lat = -88.0;
    while lat <= 88.0 {
        let mut lon = -178.0;
        while lon <= 178.0 {
            let r = real(lat, lon);
            if let Some(region) = r.region_id {
                with_region += 1;
                assert_eq!(r.country_method, Inside);
                assert_eq!(
                    names.region(region).unwrap().country.as_deref(),
                    r.country_id,
                    "{region}"
                );
            }
            if r.country_id.is_some() {
                inside += 1;
            } else {
                assert!(r.region_id.is_none());
            }
            lon += 4.0;
        }
        lat += 4.0;
    }
    assert!(inside > 200 && with_region > 100, "{inside} {with_region}");
}

#[test]
fn vertices_of_real_regions_are_classified_without_panicking() {
    // 全地域の最初の頂点（境界上）。国は必ず当たる（地域の頂点は国の境界上か内側）か、日本視点の食い違いで外
    let set = real_polygons();
    let (mut hit, mut total) = (0, 0);
    for f in 0..set.regions.len() {
        let p = set.regions.feature_rings(f).next().unwrap()[0];
        let r = real(p[1], p[0]);
        total += 1;
        if r.country_id.is_some() || r.country_method == Ambiguous {
            hit += 1;
        }
    }
    assert!(hit * 100 > total * 90, "{hit}/{total}");
}

// ---------------------------------------------------------------- 分類版・検索用文字列

#[test]
fn classifier_version_is_stable_and_covers_the_data_and_the_algorithm() {
    let v = search::classifier_version().unwrap();
    assert_eq!(v, search::classifier_version().unwrap());
    assert!(v.starts_with("gf1-") && v.len() == 4 + 24, "{v}");
    assert!(v[4..].bytes().all(|b| b.is_ascii_hexdigit()));
    // 同梱データを入れ替えたときに気づくための固定値（データ・名称表・アルゴリズムのどれかを変えたら更新する）
    assert_eq!(v, "gf1-86b7f6320ea368e1df274276");
}

#[test]
fn search_text_joins_country_then_region_names_without_duplicates() {
    let t = search::search_text("ne:JPN", Some("ne:JPN-TOKYO")).or_else(|| {
        let r = data::names().unwrap().region_of_pref("東京都")?;
        search::search_text("ne:JPN", Some(&r.id))
    });
    let t = t.unwrap();
    assert!(t.contains("日本") && t.contains("Japan"), "{t}");
    assert!(t.contains("東京都"), "{t}");
    assert!(t.contains("Tokyo"), "{t}");
    // 重複なし・空白は 1 つ
    let words: Vec<&str> = t.split(' ').collect();
    assert!(!t.contains("  ") && !t.starts_with(' ') && !t.ends_with(' '));
    assert!(words.len() >= 4);
    // 国だけ
    let c = search::search_text("ne:FRA", None).unwrap();
    assert!(c.contains("フランス") && c.contains("France"), "{c}");
    // 他の国の地域・未知の ID は None
    let tokyo = data::names().unwrap().region_of_pref("東京都").unwrap();
    assert_eq!(search::search_text("ne:FRA", Some(&tokyo.id)), None);
    assert_eq!(search::search_text("ne:ZZZ", None), None);
    assert_eq!(search::search_text("ne:JPN", Some("ne:nothing")), None);
}

#[test]
fn version_and_search_text_do_not_expand_the_polygons() {
    // このスレッドの展開回数（thread_local）が増えないこと。読み取り専用の起動が使う経路
    let before = data::POLYGON_LOADS.with(std::cell::Cell::get);
    let _ = data::manifest().unwrap();
    let _ = data::names().unwrap();
    let _ = search::classifier_version().unwrap();
    let _ = search::search_text("ne:JPN", None).unwrap();
    assert_eq!(data::POLYGON_LOADS.with(std::cell::Cell::get), before);
    // 索引を作ると 1 回数える
    let idx = RegionIndex::load().unwrap();
    assert_eq!(data::POLYGON_LOADS.with(std::cell::Cell::get), before + 1);
    assert_eq!(idx.len(), (250, 4589));
    drop(idx);
}

#[test]
fn display_names_resolve_for_classified_ids() {
    let r = real(35.6762, 139.6503);
    assert_eq!(country_name(r.country_id.unwrap()), "日本");
    assert!(region_name(r.region_id.unwrap()).contains("東京"));
}

#[test]
fn the_index_can_be_shared_between_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<RegionIndex>();
    let idx = std::sync::Arc::new(world_index());
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let idx = std::sync::Arc::clone(&idx);
            std::thread::spawn(move || at(&idx, 1.0, 1.0))
        })
        .collect();
    for h in handles {
        expect(
            h.join().unwrap(),
            Some("ne:AAA"),
            Some("ne:A1"),
            Inside,
            Inside,
        );
    }
}
