//! 試験用の偽の rclone（本物の rclone でも Drive でもない。`geoframe-export` の試験だけが使う）。
//!
//! - 状態は `--config` のあるディレクトリの隣の `fake-drive/` に持つ（`drive.json`＝偽の Drive、`ctl.json`＝挙動の差し替え、
//!   `calls.log`＝呼ばれた操作名（`op`。中立の名前）と引数と、渡された環境変数を 1 行 1 件の JSON で追記）。
//! - 対応するのは `lsjson`・`cat`・`mkdir`・`copy`・`moveto`・`purge` だけ。ほかは終了コード 2。
//! - 挙動の差し替え（`ctl.json` の `rules`）: 失敗、遅延、「移動は済ませて失敗を返す」、「移動が遅れて反映される」、
//!   ファイル単位の移動への切り替え、壊れた出力、標準エラーの内容。

mod model;

use std::io::Write;
use std::path::{Path, PathBuf};

use model::{Ctl, Drive, Kind, ROOT, split_loc};
use serde_json::{Value, json};

const VALUE_FLAGS: [&str; 10] = [
    "--config",
    "--retries",
    "--low-level-retries",
    "--transfers",
    "--checkers",
    "--cache-dir",
    "--count",
    "--exclude",
    "--include",
    "--hash-type",
];

struct Parsed {
    sub: String,
    flags: Vec<(String, Option<String>)>,
    positional: Vec<String>,
}

fn parse(args: &[String]) -> Option<Parsed> {
    let sub = args.first()?.clone();
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            positional.extend(args[i + 1..].iter().cloned());
            break;
        }
        if VALUE_FLAGS.contains(&a.as_str()) {
            flags.push((a.clone(), args.get(i + 1).cloned()));
            i += 2;
        } else {
            flags.push((a.clone(), None));
            i += 1;
        }
    }
    Some(Parsed {
        sub,
        flags,
        positional,
    })
}

impl Parsed {
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == flag)
    }
    fn values(&self, flag: &str) -> Vec<String> {
        self.flags
            .iter()
            .filter(|(f, _)| f == flag)
            .filter_map(|(_, v)| v.clone())
            .collect()
    }
}

/// `lsjson` の 1 項目。`--hash`（または `--hash-type`）があれば、ファイルには `Hashes: {"md5": "<小文字の 16 進>"}` を付ける
/// （rclone v1.76.0 の `ListJSONItem.Hashes`。キーは `hash.MD5.String()` の小文字。MD5 を持たない項目は省略）。
fn row_json(r: &model::Row, hashes: bool) -> Value {
    let mut v = json!({
        "Path": r.path, "Name": r.name, "Size": r.size, "MimeType": r.mime,
        "ModTime": "2030-01-01T00:00:00.000000000Z", "IsDir": r.is_dir, "ID": r.id,
    });
    if let (true, Some(md5)) = (hashes, &r.md5) {
        v["Hashes"] = json!({ "md5": md5 });
    }
    v
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    names.sort_by_key(std::fs::DirEntry::file_name);
    for e in names {
        let name = e.file_name().to_string_lossy().into_owned();
        let rel_path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let path = e.path();
        if path.is_dir() {
            walk(&path, &rel_path, out);
        } else {
            out.push((rel_path, path));
        }
    }
}

/// rclone のコマンド名 → 中立の操作名（`remote::Remote` の操作と同じ）。
fn op_name(cmd: &str) -> &'static str {
    match cmd {
        "lsjson" => "list",
        "cat" => "read",
        "mkdir" => "create_folder",
        "copy" => "upload",
        "moveto" => "move",
        "purge" => "trash",
        _ => "unknown",
    }
}

fn stderr(text: &str) {
    let mut e = std::io::stderr();
    let _ = writeln!(e, "{text}");
}

/// 通常の動作。`Ok` は標準出力、`Err` は終了コード。
fn operate(p: &Parsed, drive: &mut Drive, ctl: &Ctl, filewise: bool) -> Result<Vec<u8>, i32> {
    let loc = |i: usize| split_loc(p.positional.get(i).map_or("", String::as_str));
    match p.sub.as_str() {
        "lsjson" => {
            drive.apply_pending();
            let Some(dir) = drive.resolve(&loc(0)) else {
                stderr("directory not found");
                return Err(3);
            };
            // `--drive-skip-shortcuts` は値なしで真、`=true`・`=false` も読む（rclone の真偽の flag と同じ）
            let flag_on = |name: &str| {
                if p.has(&format!("{name}=false")) {
                    false
                } else {
                    p.has(name) || p.has(&format!("{name}=true"))
                }
            };
            let rows = drive.ls_opts(
                &dir,
                p.has("-R"),
                &model::LsOpts {
                    skip_shortcuts: flag_on("--drive-skip-shortcuts"),
                    mode: ctl.shortcut_mode.clone(),
                    show_all_gdocs: flag_on("--drive-show-all-gdocs"),
                },
            );
            // `--hash-type MD5` だけ（`--hash` なし）でも `--hash` を含意する。MD5 以外だけを頼まれたら、MD5 は出さない
            let hash_types = p.values("--hash-type");
            let hashes = (p.has("--hash") || !hash_types.is_empty())
                && (hash_types.is_empty()
                    || hash_types.iter().any(|t| t.eq_ignore_ascii_case("md5")));
            let v = Value::Array(rows.iter().map(|r| row_json(r, hashes)).collect());
            Ok(serde_json::to_vec_pretty(&v).unwrap())
        }
        "cat" => {
            let path = loc(0);
            let Some((name, parent)) = path.split_last() else {
                return Err(3);
            };
            let Some(dir) = drive.resolve(parent) else {
                return Err(3);
            };
            let Some(node) = drive
                .children(&dir)
                .into_iter()
                .find(|n| n.name == *name && n.kind == Kind::File)
            else {
                return Err(3);
            };
            let mut bytes = node
                .content
                .clone()
                .unwrap_or_else(|| "x".repeat(node.size as usize))
                .into_bytes();
            if let Some(n) = p
                .values("--count")
                .first()
                .and_then(|c| c.parse::<usize>().ok())
            {
                bytes.truncate(n);
            }
            Ok(bytes)
        }
        "mkdir" => {
            drive.mkdir_p(&loc(0));
            Ok(Vec::new())
        }
        "copy" => {
            let src = PathBuf::from(p.positional.first().cloned().unwrap_or_default());
            let dst = loc(1);
            let includes = p.values("--include");
            let excludes = p.values("--exclude");
            let mut files = Vec::new();
            walk(&src, "", &mut files);
            let dst_id = drive.mkdir_p(&dst);
            for (rel, path) in files {
                let pat = format!("/{rel}");
                if !includes.is_empty() && !includes.contains(&pat) {
                    continue;
                }
                if excludes.contains(&pat) {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(|_| 1)?;
                let mut parent = dst_id.clone();
                let comps: Vec<&str> = rel.split('/').collect();
                let (file, dirs) = comps.split_last().unwrap();
                for d in dirs {
                    parent = match drive
                        .children(&parent)
                        .into_iter()
                        .find(|n| n.name == *d && n.kind == Kind::Dir)
                    {
                        Some(n) => n.id.clone(),
                        None => drive.new_node(&parent, d, Kind::Dir, 0, None, None),
                    };
                }
                let existing = drive
                    .children(&parent)
                    .into_iter()
                    .find(|n| n.name == *file && n.kind == Kind::File)
                    .map(|n| n.size);
                match existing {
                    Some(size) if p.has("--immutable") && size != bytes.len() as u64 => {
                        stderr("immutable file modified");
                        return Err(1);
                    }
                    Some(_) => continue,
                    None => {}
                }
                let content = (*file == "manifest.json")
                    .then(|| String::from_utf8_lossy(&bytes).into_owned());
                let id =
                    drive.new_node(&parent, file, Kind::File, bytes.len() as u64, content, None);
                // 送った中身の MD5（`cat` で返す内容とは別。写真は中身を持たないので、実際に送ったバイトの MD5 を持つ）
                drive.set_md5(&id, &model::md5_hex(&bytes));
            }
            Ok(Vec::new())
        }
        "moveto" => {
            let Some(src) = drive.resolve(&loc(0)) else {
                stderr("directory not found");
                return Err(3);
            };
            let ok = if filewise {
                drive.move_dir_filewise(&src, &loc(1))
            } else {
                drive.move_dir(&src, &loc(1))
            };
            if ok { Ok(Vec::new()) } else { Err(1) }
        }
        "purge" => {
            let Some(id) = drive.resolve(&loc(0)) else {
                stderr("directory not found");
                return Err(3);
            };
            if id == ROOT {
                return Err(1);
            }
            drive.purge(&id, p.has("--drive-use-trash=true"));
            Ok(Vec::new())
        }
        _ => Err(2),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(p) = parse(&args) else {
        std::process::exit(2);
    };
    let Some(config) = p.values("--config").first().cloned() else {
        stderr("--config がありません");
        std::process::exit(2);
    };
    let dir = Path::new(&config)
        .parent()
        .map(|d| d.join("fake-drive"))
        .unwrap_or_default();
    std::fs::create_dir_all(&dir).expect("fake dir");

    // 呼び出しの記録（引数と、渡された環境）。効果より先に書く（止まる呼び出しも残る）
    let env: serde_json::Map<String, Value> =
        std::env::vars().map(|(k, v)| (k, json!(v))).collect();
    // `op` は中立の操作名（試験が、実装に依らない形で呼び出しを数える・並べるための名前。後の Drive API 版の偽のサーバーも
    // 同じ名前で記録する）。`cmd` と `args` と `env` は rclone 固有（rclone 版の引数・環境変数の試験だけが見る）
    let line = json!({"kind": "rclone", "op": op_name(&p.sub), "cmd": p.sub, "args": args, "env": env, "pid": std::process::id()});
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("calls.log"))
        .expect("calls.log");
    writeln!(log, "{line}").expect("log");
    drop(log);

    let mut drive = Drive::load(&dir);
    let mut ctl = Ctl::load(&dir);

    // 挙動の差し替え（最初に当たった規則）
    let mut effect: Option<Value> = None;
    for rule in &mut ctl.rules {
        let hit = rule.op == op_name(&p.sub)
            && rule
                .contains
                .as_ref()
                .is_none_or(|c| args.iter().any(|a| a.contains(c.as_str())));
        if !hit {
            continue;
        }
        rule.seen += 1;
        if effect.is_none()
            && rule.seen > rule.skip
            && rule.seen <= rule.skip.saturating_add(rule.times)
        {
            effect = Some(rule.effect.clone());
        }
    }
    ctl.save(&dir);

    let kind = effect
        .as_ref()
        .and_then(|e| e["kind"].as_str())
        .unwrap_or("");
    let code = effect
        .as_ref()
        .and_then(|e| e["code"].as_i64())
        .map_or(1, |c| c as i32);
    let err_text = effect
        .as_ref()
        .and_then(|e| e["stderr"].as_str())
        .unwrap_or("fake failure");

    let mut filewise = false;
    match kind {
        "fail" => {
            stderr(err_text);
            std::process::exit(code);
        }
        "sleep" => {
            let ms = effect.as_ref().and_then(|e| e["ms"].as_u64()).unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        "garbage" => {
            let text = effect
                .as_ref()
                .and_then(|e| e["stdout"].as_str())
                .unwrap_or("");
            print!("{text}");
            return;
        }
        "late_then_fail" => {
            // 移動を、数回の一覧のあとに反映させる。呼び出し自体は失敗で終わる
            if let (Some(src), dst) = (
                drive.resolve(&split_loc(p.positional.first().map_or("", String::as_str))),
                split_loc(p.positional.get(1).map_or("", String::as_str)),
            ) {
                let after = effect
                    .as_ref()
                    .and_then(|e| e["after_lists"].as_u64())
                    .unwrap_or(1);
                drive.pending.push(model::Pending {
                    src,
                    dst,
                    remaining: after,
                });
                drive.save(&dir);
            }
            stderr(err_text);
            std::process::exit(code);
        }
        "filewise" => filewise = true,
        _ => {}
    }

    let result = operate(&p, &mut drive, &ctl, filewise);
    drive.save(&dir);
    match result {
        Ok(out) => {
            if kind == "do_then_fail" {
                stderr(err_text);
                std::process::exit(code);
            }
            let mut so = std::io::stdout();
            let _ = so.write_all(&out);
            let _ = so.flush();
        }
        Err(c) => std::process::exit(c),
    }
}
