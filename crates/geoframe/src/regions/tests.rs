//! 同梱の境界データの検査（manifest の SHA256・件数・名称表・47 都道府県・形式の不正の拒否）。
//! 実データの試験は展開と復号を伴うので、デバッグビルドでも数秒かかる。

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use super::data::{self, DataError, PolygonSet, testing};

/// JIS X 0401 の順の 47 都道府県（web/static/js/library.js の `LIB_PREF_READINGS` のキーと同じ表記）。
const PREFECTURES: [&str; 47] = [
    "北海道",
    "青森県",
    "岩手県",
    "宮城県",
    "秋田県",
    "山形県",
    "福島県",
    "茨城県",
    "栃木県",
    "群馬県",
    "埼玉県",
    "千葉県",
    "東京都",
    "神奈川県",
    "新潟県",
    "富山県",
    "石川県",
    "福井県",
    "山梨県",
    "長野県",
    "岐阜県",
    "静岡県",
    "愛知県",
    "三重県",
    "滋賀県",
    "京都府",
    "大阪府",
    "兵庫県",
    "奈良県",
    "和歌山県",
    "鳥取県",
    "島根県",
    "岡山県",
    "広島県",
    "山口県",
    "徳島県",
    "香川県",
    "愛媛県",
    "高知県",
    "福岡県",
    "佐賀県",
    "長崎県",
    "熊本県",
    "大分県",
    "宮崎県",
    "鹿児島県",
    "沖縄県",
];

fn polygons() -> &'static PolygonSet {
    static CELL: OnceLock<PolygonSet> = OnceLock::new();
    CELL.get_or_init(|| data::load_polygons().expect("同梱のポリゴンを読める"))
}

fn data_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/data/regions"))
}

#[test]
fn embedded_artifacts_match_the_manifest() {
    data::verify_artifacts().expect("成果物の SHA256・サイズ・上限が manifest と一致する");
    let m = data::manifest().unwrap();
    // コミットされたファイルそのものも、埋め込んだ内容と一致する（include_bytes! の取り違え防止）
    for art in [&m.artifacts.polygons, &m.artifacts.names] {
        let on_disk = std::fs::read(data_dir().join(&art.file)).expect("成果物のファイル");
        assert_eq!(testing::sha256_hex(&on_disk), art.sha256, "{}", art.file);
        assert_eq!(on_disk.len() as u64, art.bytes, "{}", art.file);
    }
    assert_eq!(
        testing::manifest_json().as_bytes(),
        std::fs::read(data_dir().join("manifest.json")).unwrap()
    );
}

#[test]
fn artifact_limits_are_inside_the_ac10_target() {
    let m = data::manifest().unwrap();
    let total = m.artifacts.polygons.bytes + m.artifacts.names.bytes;
    // PRD AC10: リリースバイナリの増加 15MB 以下（成果物の合計はその一部）
    assert!(total <= m.max_total_bytes);
    assert!(m.max_total_bytes <= 15_000_000);
    assert!(m.artifacts.polygons.bytes <= m.artifacts.polygons.max_bytes);
    assert!(m.artifacts.names.bytes <= m.artifacts.names.max_bytes);
}

#[test]
fn manifest_records_the_source_and_converter() {
    let m = data::manifest().unwrap();
    assert_eq!(m.format, data::FORMAT_VERSION);
    assert_eq!(m.source.name, "Natural Earth");
    assert_eq!(m.source.license, "Public Domain");
    assert!(
        m.source
            .terms_url
            .starts_with("https://www.naturalearthdata.com/")
    );
    assert!(!m.source.data_version.is_empty());
    assert!(m.source.git_repo.starts_with("https://github.com/"));
    assert_eq!(m.source.git_commit.len(), 40);
    assert!(m.source.git_commit.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(m.converter.script.starts_with("tools/regions/"));
    assert!(!m.converter.version.is_empty() && !m.converter.pyshp.is_empty());

    let layers: BTreeSet<&str> = m.source.layers.keys().map(String::as_str).collect();
    assert_eq!(layers, BTreeSet::from(["admin0", "admin1"]));
    // 日本視点版の Admin0 と既定版の Admin1
    assert_eq!(
        m.source.layers["admin0"].stem,
        "ne_10m_admin_0_countries_jpn"
    );
    assert_eq!(
        m.source.layers["admin1"].stem,
        "ne_10m_admin_1_states_provinces"
    );
    for layer in m.source.layers.values() {
        for key in ["shp", "shx", "dbf", "prj"] {
            assert!(layer.files.contains_key(key), "{} の {key}", layer.stem);
        }
        for (ext, f) in &layer.files {
            // 取得元は固定コミットの https の URL だけ。SHA256 は 64 桁の 16 進
            assert!(f.url.starts_with("https://"), "{ext}");
            assert!(
                f.url.contains(&m.source.git_commit),
                "{ext}: 取得元がコミットで固定されている"
            );
            assert!(f.url.ends_with(&format!("{}.{ext}", layer.stem)));
            assert_eq!(f.sha256.len(), 64, "{ext}");
            assert!(
                f.sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{ext}"
            );
            assert!(f.bytes > 0);
        }
    }
    // データセット全体のハッシュ = 展開後の 2 成果物のハッシュの連結の SHA256
    let joined = format!(
        "{}{}",
        m.artifacts.polygons.raw_sha256, m.artifacts.names.raw_sha256
    );
    assert_eq!(testing::sha256_hex(joined.as_bytes()), m.dataset_sha256);
}

#[test]
fn sources_document_names_the_origin_and_license() {
    let text = std::fs::read_to_string(data_dir().join("SOURCES.md")).expect("出典説明");
    let m = data::manifest().unwrap();
    for needle in [
        "Natural Earth",
        "パブリックドメイン",
        &m.source.data_version,
        &m.source.git_commit,
        "tools/regions",
    ] {
        assert!(text.contains(needle), "SOURCES.md に {needle} が無い");
    }
    // 成果物だけを置く（取得物・作業ファイルを混ぜない）
    let mut names: Vec<String> = std::fs::read_dir(data_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "SOURCES.md",
            "manifest.json",
            "names.json.gz",
            "polygons.bin.gz"
        ]
    );
}

#[test]
fn name_table_is_consistent_with_the_manifest() {
    let m = data::manifest().unwrap();
    let t = data::names().expect("名称表");
    assert_eq!(t.countries.len(), m.counts.countries.features);
    assert_eq!(t.regions.len(), m.counts.regions.features);
    assert_eq!(t.countries.len(), 250);
    // ID の昇順（変換器が並べた順。ポリゴンも同じ並び）
    assert!(t.countries.windows(2).all(|w| w[0].id < w[1].id));
    assert!(t.regions.windows(2).all(|w| w[0].id < w[1].id));
    for p in t.countries.iter().chain(&t.regions) {
        assert!(data::valid_id(&p.id), "{}", p.id);
        assert!(!p.name.is_empty());
        assert!(p.search.contains(&p.name));
    }
    // 変換で除外した名称の無い Admin1 は載っていない
    assert!(!m.counts.excluded_admin1.is_empty());
    for code in &m.counts.excluded_admin1 {
        assert!(t.region(&format!("ne:{code}")).is_none());
    }
}

#[test]
fn country_names_follow_the_miharashi_overrides() {
    let t = data::names().unwrap();
    // miharashi の tools/world_geojson_to_txt.py の NAME_OVERRIDES（PRD R13）
    for (id, name) in [
        ("ne:TWN", "台湾"),
        ("ne:USA", "アメリカ"),
        ("ne:GBR", "イギリス"),
        ("ne:KOR", "韓国"),
        ("ne:PRK", "北朝鮮"),
        ("ne:JPN", "日本"),
    ] {
        let c = t.country(id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(c.name, name, "{id}");
    }
    // 言い換え前の表記も検索では当たる
    assert!(
        t.country("ne:TWN")
            .unwrap()
            .search
            .iter()
            .any(|s| s == "中華民国")
    );
    assert!(
        t.country("ne:JPN")
            .unwrap()
            .search
            .iter()
            .any(|s| s == "Japan")
    );
    assert_eq!(t.country("ne:JPN").unwrap().iso2.as_deref(), Some("JP"));
}

#[test]
fn all_47_prefectures_map_to_distinct_japanese_regions() {
    let t = data::names().unwrap();
    let japan: Vec<_> = t
        .regions
        .iter()
        .filter(|r| r.country.as_deref() == Some(data::JAPAN_ID))
        .collect();
    assert_eq!(japan.len(), 47, "日本の Admin1 は 47 件");
    assert_eq!(t.prefectures().count(), 47);
    let mut seen = BTreeSet::new();
    for pref in PREFECTURES {
        let r = t
            .region_of_pref(pref)
            .unwrap_or_else(|| panic!("{pref} に対応する地域が無い"));
        assert_eq!(r.country.as_deref(), Some(data::JAPAN_ID));
        assert_eq!(r.pref.as_deref(), Some(pref));
        // 旅のカードは regions[].region と places[].pref の名前の一致に頼る（design.md「写真詳細・旅」）: 47 件すべてで地域名 = pref
        assert_eq!(r.name, pref, "{pref}: 地域名が pref と違う");
        assert!(
            r.iso.as_deref().is_some_and(|iso| iso.starts_with("JP-")),
            "{pref}"
        );
        assert!(seen.insert(r.id.clone()), "{pref}: 地域が重複している");
    }
    // 日本語名はそのまま都道府県名（東京都・大阪府・京都府・北海道）
    for (pref, en) in [
        ("東京都", "Tokyo"),
        ("大阪府", "Ōsaka"),
        ("京都府", "Kyōto"),
        ("北海道", "Hokkaidō"),
    ] {
        let r = t.region_of_pref(pref).unwrap();
        assert_eq!(r.name, pref);
        assert!(r.search.iter().any(|s| s == en), "{pref} の英語表記 {en}");
    }
    assert!(t.region_of_pref("日本").is_none());
}

#[test]
fn ids_are_namespaced_and_resolved_explicitly() {
    let t = data::names().unwrap();
    // ISO ではなく Natural Earth の ID（ne:<ADM0_A3> と ne:<adm1_code>）
    assert!(t.country("ne:USA").is_some());
    assert!(
        t.region("ne:USA-3514")
            .is_some_and(|r| r.country.as_deref() == Some("ne:USA"))
    );
    // 使えない文字（'+'・'?'）を含む adm1_code は、対応表で別の ID にしてある
    assert!(t.regions.iter().all(|r| !r.id.contains(['+', '?'])));
    assert!(t.region("ne:MCO-X00").is_some());
    // Admin0（日本視点版）に無い国を指していた Admin1 は、付け替え先の国に属する
    for (id, country) in [
        ("ne:SOL-X00", "ne:SOM"),
        ("ne:CYN-X00", "ne:CYP"),
        ("ne:KAS-X00", "ne:IND"),
        ("ne:KAB-X00", "ne:KAZ"),
    ] {
        assert_eq!(
            t.region(id).and_then(|r| r.country.as_deref()),
            Some(country),
            "{id}"
        );
    }
}

#[test]
fn polygons_match_the_manifest_and_names() {
    let m = data::manifest().unwrap();
    let set = polygons();
    let t = data::names().unwrap();
    // load_polygons が検査済みだが、独立に数え直す
    assert_eq!(set.countries.len(), m.counts.countries.features);
    assert_eq!(set.regions.len(), m.counts.regions.features);
    let vertices: usize = set
        .countries
        .rings()
        .chain(set.regions.rings())
        .map(<[_]>::len)
        .sum();
    assert_eq!(
        vertices,
        m.counts.countries.vertices + m.counts.regions.vertices
    );
    assert!(vertices > 1_800_000);
    data::check_polygon_set(set, m, t).unwrap();
    // 穴と島が保持されている（Admin0 の穴 18 件、Admin1 の穴 111 件）
    assert_eq!(m.counts.countries.holes, 18);
    assert_eq!(m.counts.regions.holes, 111);
    let japan = set
        .countries
        .ids()
        .iter()
        .position(|id| id == data::JAPAN_ID)
        .unwrap();
    assert!(
        set.countries.polygon_range(japan).len() > 100,
        "日本は多数の島を持つ"
    );
}

#[test]
fn polygon_coordinates_are_finite_closed_and_in_range() {
    for layer in [&polygons().countries, &polygons().regions] {
        for f in 0..layer.len() {
            assert!(!layer.polygon_range(f).is_empty(), "{}", layer.id(f));
            for ring in layer.feature_rings(f) {
                assert!(ring.len() >= 4);
                assert_eq!(ring.first(), ring.last());
                for [x, y] in ring {
                    assert!(x.is_finite() && y.is_finite());
                    assert!((-180.0..=180.0).contains(x) && (-90.0..=90.0).contains(y));
                }
            }
        }
    }
}

#[test]
fn the_dateline_is_never_crossed_by_an_edge() {
    let mut on_the_edge = 0;
    for layer in [&polygons().countries, &polygons().regions] {
        for f in 0..layer.len() {
            for ring in layer.feature_rings(f) {
                for w in ring.windows(2) {
                    let dx = (w[1][0] - w[0][0]).abs();
                    if dx > 180.0 {
                        // 許すのは、両端がちょうど ±180 の辺（南極の縁）だけ
                        assert!(
                            w[0][0].abs() == 180.0 && w[1][0].abs() == 180.0,
                            "{}",
                            layer.id(f)
                        );
                        on_the_edge += 1;
                    }
                }
            }
        }
    }
    assert!(
        on_the_edge <= 4,
        "図の端に沿う辺は南極の縁だけ: {on_the_edge}"
    );
}

#[test]
fn regions_refer_to_the_country_polygons_listed_in_the_names() {
    let set = polygons();
    for r in 0..set.regions.len() {
        let idx = set.regions.parent(r).expect("地域は国を持つ");
        assert!(set.countries.id(idx).starts_with("ne:"));
    }
    // 国のみで地域を持たない国がある（Admin1 の無い国は『国のみ判定』）
    let with_regions: BTreeSet<usize> = (0..set.regions.len())
        .filter_map(|r| set.regions.parent(r))
        .collect();
    assert!(with_regions.len() < set.countries.len());
    assert!(with_regions.len() > 200);
}

// ---------------------------------------------------------------- 形式の不正を拒否する

fn limits() -> data::DecodeLimits {
    data::DecodeLimits::from_manifest(data::manifest().unwrap())
}

/// 手で組んだ小さなデータ用の上限（件数に余裕を持たせる）。
fn small() -> data::DecodeLimits {
    let l = data::LayerLimits {
        features: 8,
        polygons: 8,
        rings: 8,
        vertices: 64,
    };
    data::DecodeLimits {
        countries: l,
        regions: l,
    }
}

fn raw_polygons() -> Vec<u8> {
    let m = data::manifest().unwrap();
    let a = &m.artifacts.polygons;
    testing::gunzip(testing::polygons_gz(), a.raw_bytes, a.max_raw_bytes).unwrap()
}

fn err(r: Result<PolygonSet, DataError>) -> String {
    r.expect_err("拒否されるはず").0
}

#[test]
fn decoder_rejects_malformed_polygons() {
    let raw = raw_polygons();
    assert!(data::decode_polygons(&raw, &limits()).is_ok());
    // magic（形式の版を含む）
    let mut bad = raw.clone();
    bad[7] = b'9';
    assert!(err(data::decode_polygons(&bad, &limits())).contains("magic"));
    // 途中で切れる
    assert!(data::decode_polygons(&raw[..raw.len() / 2], &limits()).is_err());
    assert!(data::decode_polygons(&raw[..4], &limits()).is_err());
    assert!(data::decode_polygons(&[], &limits()).is_err());
    // 末尾の余り
    let mut extra = raw.clone();
    extra.push(0);
    assert!(err(data::decode_polygons(&extra, &limits())).contains("余り"));
}

#[test]
fn decoder_rejects_hostile_counts_without_allocating() {
    // 国の件数が 2^60 と宣言された短いデータ
    let mut hostile = data::MAGIC.to_vec();
    hostile.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x10, 0x00]);
    assert!(err(data::decode_polygons(&hostile, &limits())).contains("個数"));
    // 終わらない可変長整数
    let mut endless = data::MAGIC.to_vec();
    endless.extend_from_slice(&[0xff; 16]);
    assert!(data::decode_polygons(&endless, &limits()).is_err());
}

/// 先頭から「国 1 件・地域 0 件・ID `ne:X`」までを組む。
fn one_country_prefix(id: &[u8]) -> Vec<u8> {
    let mut out = data::MAGIC.to_vec();
    out.extend_from_slice(&[1, 0]);
    out.push(u8::try_from(id.len()).unwrap());
    out.extend_from_slice(id);
    out
}

#[test]
fn decoder_rejects_counts_that_fit_the_input_but_exceed_the_manifest() {
    let m = data::manifest().unwrap();
    let c = &m.counts.countries;
    let put = |out: &mut Vec<u8>, mut n: u64| loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    };
    // 入力長（ゼロ埋め）には収まるが manifest の件数を超える個数を、層ごと・階層ごとに宣言したデータ
    let polygons = {
        let mut d = one_country_prefix(b"ne:X");
        put(&mut d, c.polygons as u64 + 1);
        d.resize(d.len() + c.polygons + 8, 0);
        d
    };
    let rings = {
        let mut d = one_country_prefix(b"ne:X");
        put(&mut d, 1); // 多角形 1
        put(&mut d, c.rings as u64 + 1);
        d.resize(d.len() + c.rings + 8, 0);
        d
    };
    let vertices = {
        let mut d = one_country_prefix(b"ne:X");
        put(&mut d, 1);
        put(&mut d, 1);
        put(&mut d, c.vertices as u64 + 1);
        d.resize(d.len() + 2 * c.vertices + 16, 0);
        d
    };
    let features = {
        let mut d = data::MAGIC.to_vec();
        put(&mut d, c.features as u64 + 1);
        put(&mut d, 0);
        d.resize(d.len() + 3 * c.features + 16, 0);
        d
    };
    for (label, d) in [
        ("polygons", polygons),
        ("rings", rings),
        ("vertices", vertices),
        ("features", features),
    ] {
        let e = err(data::decode_polygons(&d, &limits()));
        assert!(e.contains("manifest の件数"), "{label}: {e}");
    }
    // 同じ個数でも、上限の中なら読み進められる（ここでは先の検査で別の理由で止まる）
    let mut ok = one_country_prefix(b"ne:X");
    put(&mut ok, 1);
    ok.resize(ok.len() + 16, 0);
    assert!(!err(data::decode_polygons(&ok, &limits())).contains("manifest の件数"));
}

#[test]
fn decoder_rejects_bad_ids_and_empty_polygon_lists() {
    // 長さ 0・65・使えない文字・UTF-8 でない・日本語
    for (label, id) in [
        ("空", Vec::new()),
        ("長すぎる", vec![b'a'; 65]),
        ("空白", b"ne:a b".to_vec()),
        ("制御文字", b"ne:a\n".to_vec()),
        ("UTF-8 でない", vec![b'n', b'e', b':', 0xff]),
        ("日本語", "ne:日本".as_bytes().to_vec()),
    ] {
        let mut d = one_country_prefix(&id);
        d.extend_from_slice(&[1, 1, 4]);
        let e = err(data::decode_polygons(&d, &small()));
        assert!(e.contains("ID"), "{label}: {e}");
    }
    // 長さ 64 ちょうどは ID としては通る（先の検査で別の理由で止まる）
    let mut d = one_country_prefix(&[b'a'; 64]);
    d.push(0);
    assert!(err(data::decode_polygons(&d, &small())).contains("多角形が無い"));
    // 多角形 0 件の地物
    let mut d = one_country_prefix(b"ne:X");
    d.push(0);
    assert!(err(data::decode_polygons(&d, &small())).contains("多角形が無い"));
    // 輪が 0 本の多角形
    let mut d = one_country_prefix(b"ne:X");
    d.extend_from_slice(&[1, 0]);
    assert!(err(data::decode_polygons(&d, &small())).contains("輪が無い"));
}

#[test]
fn decoder_rejects_non_finite_unclosed_and_tiny_rings() {
    fn feature_bytes(n_points: u8, points: &[(i64, i64)]) -> Vec<u8> {
        fn zz(n: i64) -> u64 {
            ((n << 1) ^ (n >> 63)) as u64
        }
        fn put(out: &mut Vec<u8>, mut n: u64) {
            loop {
                let b = (n & 0x7f) as u8;
                n >>= 7;
                if n == 0 {
                    out.push(b);
                    return;
                }
                out.push(b | 0x80);
            }
        }
        let mut out = data::MAGIC.to_vec();
        put(&mut out, 1); // 国 1 件
        put(&mut out, 0); // 地域 0 件
        out.extend_from_slice(&[3, b'n', b'e', b':']); // id
        put(&mut out, 1); // 多角形 1
        put(&mut out, 1); // 輪 1
        put(&mut out, u64::from(n_points));
        let (mut px, mut py) = (0i64, 0i64);
        for &(x, y) in points {
            put(&mut out, zz(x.wrapping_sub(px)));
            put(&mut out, zz(y.wrapping_sub(py)));
            (px, py) = (x, y);
        }
        out
    }
    let bits = |x: f64| x.to_bits() as i64;
    let a = (bits(1.0), bits(2.0));
    let b = (bits(3.0), bits(2.0));
    let c = (bits(3.0), bits(4.0));
    // 正しい輪（閉じた 4 点）は読める
    assert!(data::decode_polygons(&feature_bytes(4, &[a, b, c, a]), &small()).is_ok());
    // 閉じていない
    assert!(
        err(data::decode_polygons(
            &feature_bytes(4, &[a, b, c, b]),
            &small()
        ))
        .contains("閉じていない")
    );
    // 4 点未満
    assert!(
        err(data::decode_polygons(
            &feature_bytes(3, &[a, b, a]),
            &small()
        ))
        .contains("4 未満")
    );
    // 有限でない
    let nan = (bits(f64::NAN), bits(2.0));
    assert!(
        err(data::decode_polygons(
            &feature_bytes(4, &[nan, b, c, nan]),
            &small()
        ))
        .contains("有限")
    );
    let inf = (bits(f64::INFINITY), bits(2.0));
    assert!(
        err(data::decode_polygons(
            &feature_bytes(4, &[inf, b, c, inf]),
            &small()
        ))
        .contains("有限")
    );
}

#[test]
fn gunzip_refuses_oversized_and_mismatched_expansion() {
    let m = data::manifest().unwrap();
    let a = &m.artifacts.names;
    let gz = testing::names_gz();
    assert!(testing::gunzip(gz, a.raw_bytes, a.max_raw_bytes).is_ok());
    // 宣言より大きい・小さい
    assert!(testing::gunzip(gz, a.raw_bytes - 1, a.max_raw_bytes).is_err());
    assert!(testing::gunzip(gz, a.raw_bytes + 1, a.max_raw_bytes).is_err());
    // 上限を超える展開は拒否（上限 = 宣言より小さい）
    assert!(testing::gunzip(gz, a.raw_bytes, a.raw_bytes - 1).is_err());
    // 宣言が上限を超える
    assert!(testing::gunzip(gz, a.max_raw_bytes + 1, a.max_raw_bytes).is_err());
    // gzip でないバイト列
    assert!(testing::gunzip(b"not gzip", 8, 100).is_err());
}

// ---------------------------------------------------------------- 流し込みの復号

/// 1 回に `chunk` バイトずつしか返さない読み手（窓の読み直しの境界を通す）。
struct Dribble<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl std::io::Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.chunk.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

#[test]
fn streaming_decode_matches_the_slice_decode_at_any_chunk_size() {
    let raw = raw_polygons();
    let whole = data::decode_polygons(&raw, &limits()).unwrap();
    assert_eq!(&whole, polygons());
    // 窓（64KiB）をまたぐ・可変長整数の途中で切れる読み方
    for chunk in [1usize << 16, 4093, 37] {
        let src = Dribble { data: &raw, chunk };
        let got = data::decode_polygons_from(src, raw.len() as u64, &limits()).unwrap();
        assert!(got == whole, "chunk={chunk}");
    }
}

#[test]
fn streaming_decode_checks_the_declared_total() {
    let raw = raw_polygons();
    let n = raw.len() as u64;
    // 宣言より長い（余り）・短い（途中で終わる、または件数が残りに収まらない）
    let longer = data::decode_polygons_from(&raw[..], n - 1, &limits());
    assert!(longer.is_err());
    let shorter = data::decode_polygons_from(&raw[..], n + 1, &limits());
    assert!(err(shorter).contains("サイズ"));
}

#[test]
fn flat_layout_has_no_nested_allocations_per_ring() {
    // 座標は 1 本の配列に並び、点の数は manifest と一致する（入れ子の Vec に戻していないことの確認）
    let m = data::manifest().unwrap();
    let set = polygons();
    assert_eq!(set.countries.points_len(), m.counts.countries.vertices);
    assert_eq!(set.regions.points_len(), m.counts.regions.vertices);
    assert_eq!(set.countries.rings_len(), m.counts.countries.rings);
    assert_eq!(set.regions.polygons_len(), m.counts.regions.polygons);
}
