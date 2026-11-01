//! `thumb.rs` の単体試験。生成関数を差し替えて、ロック・枠・待ちの上限・失敗の記憶・掃除を確かめる
//! （画像のデコードは `imaging` の試験と `tests/thumb.rs`）。

use std::os::unix::fs::symlink;
use std::sync::atomic::AtomicBool;

use super::*;
use crate::testutil::TempDir;

const OK_BYTES: &[u8] = b"synthetic thumbnail";

fn gen_of(n: u8) -> Gen {
    thumb_generation(Some(&format!("derivatives/{n}.jpg")), Some("m"), Some(1.0))
}

fn job_with(root: &Path, id: i64, generation: Gen, current: Option<Gen>) -> Job {
    Job {
        id,
        generation,
        src: root.join("src.jpg"),
        data_root: root.to_path_buf(),
        current_gen: Arc::new(move || current),
    }
}

fn job(root: &Path, id: i64) -> Job {
    let generation = gen_of(id as u8);
    job_with(root, id, generation, Some(generation))
}

fn root_of(tmp: &TempDir) -> PathBuf {
    fs::canonicalize(tmp.path()).unwrap()
}

fn state_with(renderer: Renderer) -> Arc<ThumbState> {
    Arc::new(ThumbState::with_renderer(renderer))
}

fn ok_renderer() -> Renderer {
    Arc::new(|_| Ok(OK_BYTES.to_vec()))
}

/// 条件が成り立つまで（上限つきで）待つ。成り立たないと試験を失敗にする。
async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("条件が成り立ちませんでした: {what}");
}

// ---------------------------------------------------------------------------
// 経路・世代

#[test]
fn 経路は数字の_idと_jpgだけを受ける() {
    for (rest, expected) in [
        ("1.jpg", Some(1)),
        ("42.jpg", Some(42)),
        ("123456789012345678.jpg", Some(123_456_789_012_345_678)),
        ("0.jpg", None),
        ("012.jpg", None),
        ("1234567890123456789.jpg", None),
        ("12.JPG", None),
        ("12.jpeg", None),
        ("12.png", None),
        ("12", None),
        (".jpg", None),
        ("a.jpg", None),
        ("-1.jpg", None),
        ("+1.jpg", None),
        ("1 .jpg", None),
        ("１.jpg", None),
        ("12.jpg/x", None),
        ("a/12.jpg", None),
        ("", None),
    ] {
        assert_eq!(parse_id(rest), expected, "{rest:?}");
    }
}

#[test]
fn 版は12桁の小文字hexだけ() {
    assert_eq!(query_v(Some("v=0123456789ab")), Some("0123456789ab"));
    assert_eq!(query_v(Some("x=1&v=aa")), Some("aa"));
    assert_eq!(query_v(None), None);
    assert_eq!(query_v(Some("vv=1")), None);
    assert!(Gen::parse("0123456789ab").is_some());
    for bad in [
        "",
        "0123456789A",
        "0123456789AB",
        "0123456789a",
        "0123456789abc",
        "0123456789ag",
    ] {
        assert!(Gen::parse(bad).is_none(), "{bad}");
    }
    let generation = gen_of(1);
    assert_eq!(Gen::parse(&generation.hex()), Some(generation));
    assert_eq!(generation.hex().len(), 12);
}

#[test]
fn 世代は_display_md5_mtimeのどれが変わっても変わる() {
    let base = thumb_generation(
        Some("derivatives/a.jpg"),
        Some("abc"),
        Some(1_700_000_000.5),
    );
    assert_eq!(
        base,
        thumb_generation(
            Some("derivatives/a.jpg"),
            Some("abc"),
            Some(1_700_000_000.5)
        )
    );
    for other in [
        thumb_generation(
            Some("derivatives/a.r90.jpg"),
            Some("abc"),
            Some(1_700_000_000.5),
        ),
        thumb_generation(
            Some("derivatives/a.jpg"),
            Some("abd"),
            Some(1_700_000_000.5),
        ),
        thumb_generation(
            Some("derivatives/a.jpg"),
            Some("abc"),
            Some(1_700_000_001.0),
        ),
        thumb_generation(Some("derivatives/a.jpg"), Some("abc"), None),
        thumb_generation(None, Some("abc"), Some(1_700_000_000.5)),
    ] {
        assert_ne!(base, other);
    }
    // NULL は空として連結する
    assert_eq!(
        thumb_generation(None, None, None),
        thumb_generation(Some(""), Some(""), None)
    );
}

#[test]
fn 元ファイルは_displayか_drive_idからだけ決める() {
    let row = |display: Option<&str>, drive_id: Option<&str>| PhotoRow {
        display: display.map(str::to_owned),
        drive_id: drive_id.map(str::to_owned),
        md5: None,
        mtime: None,
    };
    let source = |r: PhotoRow| source_of(&r);
    assert_eq!(
        source(row(Some("derivatives/a.jpg"), None)),
        Some(Source {
            area: Area::Derivatives,
            rel: vec!["a.jpg".into()]
        })
    );
    assert_eq!(
        source(row(Some("photos/2024/x.PNG"), Some("ignored"))),
        Some(Source {
            area: Area::Photos,
            rel: vec!["2024".into(), "x.PNG".into()]
        })
    );
    // display が無い・空なら drive_id から
    for display in [None, Some("")] {
        assert_eq!(
            source(row(display, Some("abc_-1"))),
            Some(Source {
                area: Area::Derivatives,
                rel: vec!["abc_-1.jpg".into()]
            })
        );
    }
    // 外を指す・領域でない・字面が不正・ドット始まり・drive_id が不正
    for display in [
        "catalog.db",
        "../x",
        "tmp/x.jpg",
        "derivatives/../catalog.db",
        "derivatives/../../x",
        "/etc/passwd",
        "/derivatives/a.jpg",
        "derivatives",
        "derivatives/",
        "photos/.hidden/a.jpg",
        "derivatives/.dl_x",
        "blog_exports/a.jpg",
        "thumbs/1-0123456789ab.jpg",
        "derivatives/a\0.jpg",
    ] {
        assert_eq!(source(row(Some(display), Some("abc"))), None, "{display:?}");
    }
    for drive_id in ["../x", "a/b", "a.b", "", "a b"] {
        assert_eq!(source(row(None, Some(drive_id))), None, "{drive_id:?}");
    }
    assert_eq!(source(row(None, None)), None);
}

#[test]
fn 失敗の記憶は上限で古いものから捨てる() {
    let mut failed = Failed::new(3);
    let keys: Vec<(i64, Gen)> = (1..=5).map(|n| (n, gen_of(n as u8))).collect();
    for key in &keys {
        assert!(failed.insert(*key));
    }
    assert!(!failed.insert(keys[4]), "既にあるものは新しくない");
    assert!(!failed.contains(&keys[0]) && !failed.contains(&keys[1]));
    assert!(keys[2..].iter().all(|k| failed.contains(k)));
    assert_eq!(failed.order.len(), 3);
    assert_eq!(failed.set.len(), 3);
}

// ---------------------------------------------------------------------------
// 同時に作る数・同じ写真は 1 回

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 同じ写真への同時の要求で生成は1回() {
    let tmp = TempDir::new("thumb-single");
    let root = root_of(&tmp);
    let state = state_with(Arc::new(|_| {
        std::thread::sleep(Duration::from_millis(80));
        Ok(OK_BYTES.to_vec())
    }));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let state = Arc::clone(&state);
            let j = job(&root, 7);
            tokio::spawn(async move { state.produce(j).await })
        })
        .collect();
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.unwrap());
    }
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == Outcome::Generated)
            .count(),
        1
    );
    assert_eq!(
        outcomes.iter().filter(|o| **o == Outcome::Cached).count(),
        7
    );
    assert_eq!(state.renders(), 1);
    assert_eq!(state.lock_table_len(), 0, "ロック表は空に戻る");
    assert_eq!(state.waiting.load(Ordering::SeqCst), 0);
    let name = cache_name(7, gen_of(7));
    assert_eq!(fs::read(root.join("thumbs").join(name)).unwrap(), OK_BYTES);
}

/// 同時に走っている生成の数を数える生成関数。`gate` が立つまで抜けない。
fn gated(gate: &Arc<AtomicBool>, now: &Arc<AtomicUsize>, peak: &Arc<AtomicUsize>) -> Renderer {
    let (gate, now, peak) = (Arc::clone(gate), Arc::clone(now), Arc::clone(peak));
    Arc::new(move |_| {
        let running = now.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(running, Ordering::SeqCst);
        while !gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        now.fetch_sub(1, Ordering::SeqCst);
        Ok(OK_BYTES.to_vec())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 別の写真でも同時の生成は2を超えない() {
    let tmp = TempDir::new("thumb-slots");
    let root = root_of(&tmp);
    let (gate, now, peak) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let state = state_with(gated(&gate, &now, &peak));
    let tasks: Vec<_> = (1..=6)
        .map(|id| {
            let state = Arc::clone(&state);
            let j = job(&root, id);
            tokio::spawn(async move { state.produce(j).await })
        })
        .collect();
    until("2 つが走り、残りが待つ", || {
        now.load(Ordering::SeqCst) == 2 && state.waiting.load(Ordering::SeqCst) == 4
    })
    .await;
    gate.store(true, Ordering::SeqCst);
    for task in tasks {
        assert_eq!(task.await.unwrap(), Outcome::Generated);
    }
    assert_eq!(peak.load(Ordering::SeqCst), THUMB_CONCURRENCY);
    assert_eq!(state.renders(), 6);
    assert_eq!(state.lock_table_len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 枠を待っている要求を捨てても同時の生成は2を超えない() {
    let tmp = TempDir::new("thumb-drop-wait");
    let root = root_of(&tmp);
    let (gate, now, peak) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let state = state_with(gated(&gate, &now, &peak));
    let spawn_for = |id: i64| {
        let state = Arc::clone(&state);
        let j = job(&root, id);
        tokio::spawn(async move { state.produce(j).await })
    };
    let (a, b) = (spawn_for(1), spawn_for(2));
    until("枠が埋まる", || now.load(Ordering::SeqCst) == 2).await;
    let waiter = spawn_for(3);
    until("3 つ目が枠を待つ", || {
        state.waiting.load(Ordering::SeqCst) == 1
    })
    .await;
    // 待っている要求を捨てる（クライアントが切った）。待ち数もロック表も元に戻る
    waiter.abort();
    let _ = waiter.await;
    until("待ち数が戻る", || {
        state.waiting.load(Ordering::SeqCst) == 0
    })
    .await;
    assert_eq!(state.lock_table_len(), 2, "走っている 2 つの分だけ");
    // 走っている生成は続く。後から来た要求は、2 つが終わってから走る
    let late = spawn_for(4);
    until("後続が枠を待つ", || {
        state.waiting.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(now.load(Ordering::SeqCst), 2);
    gate.store(true, Ordering::SeqCst);
    for task in [a, b, late] {
        assert_eq!(task.await.unwrap(), Outcome::Generated);
    }
    assert_eq!(peak.load(Ordering::SeqCst), THUMB_CONCURRENCY);
    assert_eq!(state.renders(), 3, "捨てた要求の分は作らない");
    assert_eq!(state.lock_table_len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 生成の途中で要求を捨てても次の要求は二重に生成しない() {
    let tmp = TempDir::new("thumb-drop-run");
    let root = root_of(&tmp);
    let (gate, now, peak) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let state = state_with(gated(&gate, &now, &peak));
    let first = {
        let (state, j) = (Arc::clone(&state), job(&root, 9));
        tokio::spawn(async move { state.produce(j).await })
    };
    until("生成が始まる", || now.load(Ordering::SeqCst) == 1).await;
    // 生成の途中で、要求の future を捨てる（切断）。生成はロックを持ったまま続く
    first.abort();
    let _ = first.await;
    assert_eq!(
        state.lock_table_len(),
        1,
        "ロックは blocking 側が持っている"
    );
    let second = {
        let (state, j) = (Arc::clone(&state), job(&root, 9));
        tokio::spawn(async move { state.produce(j).await })
    };
    until("次の要求が同じ ID のロックを待つ", || {
        state.waiting.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(now.load(Ordering::SeqCst), 1, "二重に走らない");
    gate.store(true, Ordering::SeqCst);
    // 先の生成が書いたファイルを、次の要求は再確認で見つける
    assert_eq!(second.await.unwrap(), Outcome::Cached);
    assert_eq!(state.renders(), 1);
    assert_eq!(peak.load(Ordering::SeqCst), 1);
    until("後始末", || state.lock_table_len() == 0).await;
    assert!(root.join("thumbs").join(cache_name(9, gen_of(9))).is_file());
}

// ---------------------------------------------------------------------------
// 待ちの上限

#[tokio::test(start_paused = true)]
async fn ロックを待つ時間には上限がある() {
    let tmp = TempDir::new("thumb-wait-time");
    let root = root_of(&tmp);
    let state = state_with(ok_renderer());
    // 別の要求が同じ ID のロックを持っている
    let mut held = IdLock::new(&state.locks, 5);
    held.acquire().await;
    let started = Instant::now();
    assert_eq!(state.produce(job(&root, 5)).await, Outcome::Busy);
    assert!(started.elapsed() >= THUMB_WAIT_MAX);
    assert_eq!(state.waiting.load(Ordering::SeqCst), 0);
    assert_eq!(state.renders(), 0);
    drop(held);
    assert_eq!(state.lock_table_len(), 0);
}

#[tokio::test(start_paused = true)]
async fn 待つ数には上限がある() {
    let tmp = TempDir::new("thumb-queue");
    let root = root_of(&tmp);
    let mut state = ThumbState::with_renderer(ok_renderer());
    state.queue_max = 2;
    let state = Arc::new(state);
    let mut held = IdLock::new(&state.locks, 5);
    held.acquire().await;
    let waiters: Vec<_> = (0..2)
        .map(|_| {
            let (state, j) = (Arc::clone(&state), job(&root, 5));
            tokio::spawn(async move { state.produce(j).await })
        })
        .collect();
    tokio::task::yield_now().await;
    until("2 つが待つ", || {
        state.waiting.load(Ordering::SeqCst) == 2
    })
    .await;
    // 3 つ目は待たずに断る
    assert_eq!(state.produce(job(&root, 5)).await, Outcome::Busy);
    assert_eq!(state.waiting.load(Ordering::SeqCst), 2);
    drop(held);
    for waiter in waiters {
        // ロックが空いたので、先の 1 つが作り、残りは再確認で見つける
        let outcome = waiter.await.unwrap();
        assert!(
            matches!(outcome, Outcome::Generated | Outcome::Cached),
            "{outcome:?}"
        );
    }
    assert_eq!(state.renders(), 1);
    assert_eq!(state.waiting.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// 失敗

#[tokio::test(flavor = "multi_thread")]
async fn 復号できない写真は覚えて2回目は生成を試みない() {
    type Case = (fn() -> ImageError, &'static str);
    let cases: [Case; 4] = [
        (
            || ImageError::Decode {
                format: "jpeg",
                class: crate::imaging::DecodeClass::Corrupt,
            },
            "decode",
        ),
        (|| ImageError::FileTooLarge, "limit"),
        (
            || ImageError::TooManyPixels {
                width: 9,
                height: 9,
            },
            "limit",
        ),
        (|| ImageError::NotAnImage, "decode"),
    ];
    for (error, label) in cases {
        let tmp = TempDir::new("thumb-failed");
        let root = root_of(&tmp);
        let state = state_with(Arc::new(move |_| Err(error())));
        assert_eq!(
            state.produce(job(&root, 3)).await,
            Outcome::Rejected,
            "{label}"
        );
        assert_eq!(
            state.produce(job(&root, 3)).await,
            Outcome::Rejected,
            "{label}"
        );
        assert_eq!(state.renders(), 1, "{label}: 2 回目は復号しない");
        assert!(state.is_failed((3, gen_of(3))));
        assert!(!root.join("thumbs").exists(), "失敗では thumbs/ も作らない");
        assert_eq!(state.lock_table_len(), 0);
        // 世代が変われば（写真が変われば）もう一度試す
        let other = thumb_generation(Some("derivatives/other.jpg"), None, None);
        assert_eq!(
            state.produce(job_with(&root, 3, other, Some(other))).await,
            Outcome::Rejected
        );
        assert_eq!(state.renders(), 2);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn 元ファイルを読めない入出力の失敗は覚えない() {
    let tmp = TempDir::new("thumb-io");
    let root = root_of(&tmp);
    let state = state_with(Arc::new(|_| {
        Err(ImageError::Io(std::io::ErrorKind::NotFound))
    }));
    assert_eq!(state.produce(job(&root, 3)).await, Outcome::Unreadable);
    assert_eq!(state.produce(job(&root, 3)).await, Outcome::Unreadable);
    assert_eq!(state.renders(), 2);
    assert!(!state.is_failed((3, gen_of(3))));
}

#[tokio::test(flavor = "multi_thread")]
async fn 書けないときは元の画像に回し60秒は全体で生成を試みない() {
    let tmp = TempDir::new("thumb-write");
    let root = root_of(&tmp);
    // thumbs/ がリンク: 書かない
    let elsewhere = root.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, root.join("thumbs")).unwrap();
    let state = state_with(ok_renderer());
    assert_eq!(state.produce(job(&root, 1)).await, Outcome::WriteFailed);
    assert_eq!(
        fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "リンク先に書かない"
    );
    // 別の写真も、しばらくは生成を試みない
    assert_eq!(state.produce(job(&root, 2)).await, Outcome::WriteFailed);
    assert_eq!(state.renders(), 1);
    assert!(state.write_blocked());
    // 時間が過ぎたらもう一度試す
    *plain(&state.write_failed_until) = Some(Instant::now() - Duration::from_secs(1));
    assert!(!state.write_blocked());
    assert_eq!(state.produce(job(&root, 3)).await, Outcome::WriteFailed);
    assert_eq!(state.renders(), 2);
    // 書けない失敗は記憶に入れない
    assert!(!state.is_failed((1, gen_of(1))));
}

#[tokio::test(flavor = "multi_thread")]
async fn 生成関数がパニックしても元の画像に回しロックを放す() {
    let tmp = TempDir::new("thumb-panic");
    let root = root_of(&tmp);
    let state = state_with(Arc::new(|_| panic!("synthetic")));
    assert_eq!(state.produce(job(&root, 1)).await, Outcome::WriteFailed);
    assert_eq!(state.lock_table_len(), 0);
    assert_eq!(state.slots.available_permits(), THUMB_CONCURRENCY);
}

// ---------------------------------------------------------------------------
// 古い世代の掃除

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"old").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn 古い世代だけを消す() {
    let tmp = TempDir::new("thumb-sweep");
    let root = root_of(&tmp);
    let thumbs = root.join("thumbs");
    let old_a = thumbs.join("1-0123456789ab.jpg");
    let old_b = thumbs.join("1-ffffffffffff.jpg");
    let other_id = thumbs.join("2-0123456789ab.jpg");
    let other_id_prefix = thumbs.join("11-0123456789ab.jpg");
    let bad_name = thumbs.join("1-0123456789AB.jpg");
    let short = thumbs.join("1-abc.jpg");
    let note = thumbs.join("1-0123456789ab.jpg.bak");
    for path in [
        &old_a,
        &old_b,
        &other_id,
        &other_id_prefix,
        &bad_name,
        &short,
        &note,
    ] {
        touch(path);
    }
    // 通常のファイルでないもの（リンク・ディレクトリ）は消さない
    let outside = root.join("outside.txt");
    fs::write(&outside, b"keep").unwrap();
    let link = thumbs.join("1-aaaaaaaaaaaa.jpg");
    symlink(&outside, &link).unwrap();
    let dir = thumbs.join("1-bbbbbbbbbbbb.jpg");
    fs::create_dir(&dir).unwrap();

    let state = state_with(ok_renderer());
    assert_eq!(state.produce(job(&root, 1)).await, Outcome::Generated);
    let current = thumbs.join(cache_name(1, gen_of(1)));
    assert!(current.is_file());
    assert!(!old_a.exists() && !old_b.exists(), "古い世代は消える");
    for kept in [
        &other_id,
        &other_id_prefix,
        &bad_name,
        &short,
        &note,
        &outside,
    ] {
        assert!(kept.exists(), "{kept:?}");
    }
    assert!(
        fs::symlink_metadata(&link).unwrap().is_symlink(),
        "リンクは消さない"
    );
    assert!(dir.is_dir());
    assert_eq!(fs::read(&outside).unwrap(), b"keep");
}

#[tokio::test(flavor = "multi_thread")]
async fn 消す直前に世代が変わっていたら消さない() {
    let tmp = TempDir::new("thumb-sweep-rotated");
    let root = root_of(&tmp);
    let old = root.join("thumbs").join("1-0123456789ab.jpg");
    touch(&old);
    let state = state_with(ok_renderer());
    // 作っているあいだに回転され、DB の世代は別のものになった
    let rotated = thumb_generation(Some("derivatives/a.r90.jpg"), None, None);
    let started = job_with(&root, 1, gen_of(1), Some(rotated));
    assert_eq!(state.produce(started).await, Outcome::Generated);
    assert!(old.exists(), "回転後の世代を消さない");
    // DB を引けなかったときも消さない
    let unknown = job_with(&root, 1, gen_of(1), None);
    let again = ThumbState::with_renderer(ok_renderer());
    fs::remove_file(root.join("thumbs").join(cache_name(1, gen_of(1)))).unwrap();
    assert_eq!(Arc::new(again).produce(unknown).await, Outcome::Generated);
    assert!(old.exists());
}
