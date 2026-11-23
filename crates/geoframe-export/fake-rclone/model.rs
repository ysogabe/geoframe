//! 試験用の偽の Drive の模型（本物の Drive にも rclone にも触れない）。
//!
//! 偽の rclone（`main.rs`）と、試験が Drive の状態を作る・読むための部品（`tests/common/drive.rs`）が、同じこのファイルを
//! 使う。状態は JSON のファイル 1 つ（`drive.json`）で、ノードの表（ID・親・名前・種類・大きさ）として持つ:
//! 同じ名前のフォルダが 2 つあること、ショートカット、移動しても変わらない ID、移動の遅れた反映を、ふつうのディレクトリの
//! 木では表せないため。ID は架空の値（`fid0001` …）。
#![allow(dead_code)]

use serde_json::{Map, Value, json};

pub const ROOT: &str = "root";
pub const DIR_MIME: &str = "inode/directory";
pub const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
/// リンク切れのショートカットに、rclone が付ける MIME タイプ。
pub const SHORTCUT_DANGLING_MIME: &str = "application/vnd.google-apps.shortcut.dangling";
pub const GDOC_MIME: &str = "application/vnd.google-apps.document";
pub const FORM_MIME: &str = "application/vnd.google-apps.form";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Shortcut,
    /// ネイティブの Google 文書（エクスポートできる。`Size: -1`）。
    Gdoc,
    /// エクスポートできない Google のファイル（フォームなど）。`--drive-show-all-gdocs` が無ければ一覧に出ない。出れば `Size: -1`。
    Form,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::File => "file",
            Self::Shortcut => "shortcut",
            Self::Gdoc => "gdoc",
            Self::Form => "form",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "dir" => Self::Dir,
            "shortcut" => Self::Shortcut,
            "gdoc" => Self::Gdoc,
            "form" => Self::Form,
            _ => Self::File,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub parent: String,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub content: Option<String>,
    /// ファイルの MD5（`lsjson --hash` の `Hashes.md5`）。`None` なら、中身（`cat` が返すもの: `content`、無ければ `x` を
    /// `size` 個）の MD5。`copy` で作ったファイルは、送った中身の MD5 を入れる。
    pub md5: Option<String>,
    /// ショートカットの指す先の ID。
    pub target: Option<String>,
}

/// 実行が遅れて反映される移動（`late_then_fail` の効果）。
#[derive(Clone, Debug)]
pub struct Pending {
    pub src: String,
    pub dst: Vec<String>,
    /// あと何回の `lsjson` が、古い状態を見せるか。
    pub remaining: u64,
}

/// `lsjson` の 1 行。
#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub name: String,
    pub size: i64,
    pub mime: String,
    pub is_dir: bool,
    pub id: String,
    /// ファイルの MD5（フォルダ・ショートカット・Google の文書は `None`）。出すかどうかは `--hash` の有無で偽の rclone が決める。
    pub md5: Option<String>,
}

/// `lsjson` の呼び出しごとの設定（偽の rclone が引数から作る）。
#[derive(Clone, Debug)]
pub struct LsOpts {
    pub skip_shortcuts: bool,
    /// `mime`（ショートカットとして見える）か `resolved`（指す先として見える）。
    pub mode: String,
    /// `--drive-show-all-gdocs`。
    pub show_all_gdocs: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Drive {
    pub nodes: Vec<Node>,
    pub next: u64,
    /// ゴミ箱へ移した（`--drive-use-trash=true` の purge）フォルダの ID。
    pub trashed: Vec<String>,
    /// ゴミ箱を通さずに消した ID。
    pub deleted: Vec<String>,
    pub pending: Vec<Pending>,
}

/// バイト列の MD5（小文字の 16 進 32 文字）。
pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    Md5::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Node {
    /// ファイルの MD5（`md5` があればそれ、無ければ `cat` が返す中身の MD5）。
    pub fn effective_md5(&self) -> String {
        self.md5.clone().unwrap_or_else(|| {
            let body = self
                .content
                .clone()
                .unwrap_or_else(|| "x".repeat(self.size as usize));
            md5_hex(body.as_bytes())
        })
    }
}

/// `remote:a/b` を部品に分ける（remote 名は見ない）。
pub fn split_loc(loc: &str) -> Vec<String> {
    let path = loc.split_once(':').map_or(loc, |(_, p)| p);
    path.split('/')
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .collect()
}

fn mime_of(name: &str, kind: Kind) -> &'static str {
    match kind {
        Kind::Dir => DIR_MIME,
        Kind::Shortcut => SHORTCUT_MIME,
        Kind::Gdoc => GDOC_MIME,
        Kind::Form => FORM_MIME,
        Kind::File if name.ends_with(".jpg") => "image/jpeg",
        Kind::File if name.ends_with(".json") => "application/json",
        Kind::File => "application/octet-stream",
    }
}

impl Drive {
    pub fn new_id(&mut self) -> String {
        self.next += 1;
        format!("fid{:04}", self.next)
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn children(&self, parent: &str) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.parent == parent).collect()
    }

    /// 親の下の、名前が一致する最初のフォルダ。
    fn find_dir(&self, parent: &str, name: &str) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|n| n.parent == parent && n.name == name && n.kind == Kind::Dir)
    }

    pub fn resolve<S: AsRef<str>>(&self, path: &[S]) -> Option<String> {
        let mut cur = ROOT.to_owned();
        for c in path {
            cur = self.find_dir(&cur, c.as_ref())?.id.clone();
        }
        Some(cur)
    }

    pub fn mkdir_p<S: AsRef<str>>(&mut self, path: &[S]) -> String {
        let mut cur = ROOT.to_owned();
        for c in path {
            cur = match self.find_dir(&cur, c.as_ref()) {
                Some(n) => n.id.clone(),
                None => self.new_node(&cur, c.as_ref(), Kind::Dir, 0, None, None),
            };
        }
        cur
    }

    /// 同じ名前のフォルダがあっても、もう 1 つ作る（Drive は同じ名前を許す）。
    pub fn mkdir_dup<S: AsRef<str>>(&mut self, parent: &[S], name: &str) -> String {
        let p = self.mkdir_p(parent);
        self.new_node(&p, name, Kind::Dir, 0, None, None)
    }

    pub fn new_node(
        &mut self,
        parent: &str,
        name: &str,
        kind: Kind,
        size: u64,
        content: Option<String>,
        target: Option<String>,
    ) -> String {
        let id = self.new_id();
        self.nodes.push(Node {
            id: id.clone(),
            parent: parent.to_owned(),
            name: name.to_owned(),
            kind,
            size,
            content,
            md5: None,
            target,
        });
        id
    }

    /// ファイルの MD5 を指定する（中身を書き換えたが同じ名前・同じ大きさのまま、を作る）。
    pub fn set_md5(&mut self, id: &str, md5: &str) {
        if let Some(n) = self.node_mut(id) {
            n.md5 = Some(md5.to_owned());
        }
    }

    pub fn add_file<S: AsRef<str>>(
        &mut self,
        parent: &[S],
        name: &str,
        size: u64,
        content: Option<&str>,
    ) -> String {
        let p = self.mkdir_p(parent);
        self.new_node(&p, name, Kind::File, size, content.map(str::to_owned), None)
    }

    /// MD5 を指定して足す（公開する一式と同じ MD5 のファイルを作るとき）。
    pub fn add_file_md5<S: AsRef<str>>(
        &mut self,
        parent: &[S],
        name: &str,
        size: u64,
        content: Option<&str>,
        md5: &str,
    ) -> String {
        let id = self.add_file(parent, name, size, content);
        self.set_md5(&id, md5);
        id
    }

    pub fn add_shortcut<S: AsRef<str>>(
        &mut self,
        parent: &[S],
        name: &str,
        target: &str,
    ) -> String {
        let p = self.mkdir_p(parent);
        self.new_node(&p, name, Kind::Shortcut, 0, None, Some(target.to_owned()))
    }

    /// ネイティブの Google 文書（`Size: -1` で一覧に出る）を足す。
    pub fn add_gdoc<S: AsRef<str>>(&mut self, parent: &[S], name: &str) -> String {
        let p = self.mkdir_p(parent);
        self.new_node(&p, name, Kind::Gdoc, 0, None, None)
    }

    /// エクスポートできない Google のファイル（フォーム）を足す。`--drive-show-all-gdocs` のときだけ一覧に出る。
    pub fn add_form<S: AsRef<str>>(&mut self, parent: &[S], name: &str) -> String {
        let p = self.mkdir_p(parent);
        self.new_node(&p, name, Kind::Form, 0, None, None)
    }

    /// ルートからのパス（`a/b/c`）。
    pub fn path_of(&self, id: &str) -> Option<String> {
        let mut parts = Vec::new();
        let mut cur = id.to_owned();
        while cur != ROOT {
            let n = self.node(&cur)?;
            parts.push(n.name.clone());
            cur = n.parent.clone();
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// `lsjson`。`shortcut_mode` は `mime`（ショートカットとして見える）か `resolved`（指す先として見える）。
    /// エクスポートできない Google のファイルも出す（`--drive-show-all-gdocs` 付きの一覧と同じ）。
    pub fn ls(
        &self,
        dir: &str,
        recursive: bool,
        skip_shortcuts: bool,
        shortcut_mode: &str,
    ) -> Vec<Row> {
        self.ls_opts(
            dir,
            recursive,
            &LsOpts {
                skip_shortcuts,
                mode: shortcut_mode.to_owned(),
                show_all_gdocs: true,
            },
        )
    }

    pub fn ls_opts(&self, dir: &str, recursive: bool, opts: &LsOpts) -> Vec<Row> {
        let mut out = Vec::new();
        self.ls_into(dir, "", recursive, opts, &mut out, 0);
        out
    }

    fn ls_into(
        &self,
        dir: &str,
        prefix: &str,
        recursive: bool,
        opts: &LsOpts,
        out: &mut Vec<Row>,
        depth: usize,
    ) {
        if depth > 8 {
            return;
        }
        for n in self.children(dir) {
            let path = if prefix.is_empty() {
                n.name.clone()
            } else {
                format!("{prefix}/{}", n.name)
            };
            match n.kind {
                Kind::Shortcut => {
                    if opts.skip_shortcuts {
                        continue;
                    }
                    if opts.mode == "resolved" {
                        // 解決された項目の ID は、リンク先の ID とショートカットの ID をタブで結んだ複合 ID（rclone の drive.go の
                        // joinID）。リンク先が無い（リンク切れ）ときは、MIME タイプが `.dangling` で、ショートカット自身の ID
                        if let Some(t) = n.target.as_deref().and_then(|t| self.node(t)) {
                            let is_dir = t.kind == Kind::Dir;
                            out.push(Row {
                                path: path.clone(),
                                name: n.name.clone(),
                                size: if is_dir || matches!(t.kind, Kind::Gdoc | Kind::Form) {
                                    -1
                                } else {
                                    t.size as i64
                                },
                                mime: mime_of(&t.name, t.kind).to_owned(),
                                is_dir,
                                id: format!("{}\t{}", t.id, n.id),
                                md5: (t.kind == Kind::File).then(|| t.effective_md5()),
                            });
                            if is_dir && recursive {
                                self.ls_into(&t.id, &path, recursive, opts, out, depth + 1);
                            }
                        } else {
                            out.push(Row {
                                path,
                                name: n.name.clone(),
                                size: 0,
                                mime: SHORTCUT_DANGLING_MIME.to_owned(),
                                is_dir: false,
                                id: n.id.clone(),
                                md5: None,
                            });
                        }
                    } else {
                        out.push(Row {
                            path,
                            name: n.name.clone(),
                            size: 0,
                            mime: SHORTCUT_MIME.to_owned(),
                            is_dir: false,
                            id: n.id.clone(),
                            md5: None,
                        });
                    }
                }
                Kind::Dir => {
                    out.push(Row {
                        path: path.clone(),
                        name: n.name.clone(),
                        size: -1,
                        mime: DIR_MIME.to_owned(),
                        is_dir: true,
                        id: n.id.clone(),
                        md5: None,
                    });
                    if recursive {
                        self.ls_into(&n.id, &path, recursive, opts, out, depth + 1);
                    }
                }
                Kind::File => out.push(Row {
                    path,
                    name: n.name.clone(),
                    size: n.size as i64,
                    mime: mime_of(&n.name, Kind::File).to_owned(),
                    is_dir: false,
                    id: n.id.clone(),
                    md5: Some(n.effective_md5()),
                }),
                Kind::Gdoc | Kind::Form => {
                    if n.kind == Kind::Form && !opts.show_all_gdocs {
                        continue;
                    }
                    out.push(Row {
                        path,
                        name: n.name.clone(),
                        size: -1,
                        mime: mime_of(&n.name, n.kind).to_owned(),
                        is_dir: false,
                        id: n.id.clone(),
                        md5: None,
                    });
                }
            }
        }
    }

    fn remove_subtree(&mut self, id: &str) {
        let kids: Vec<String> = self.children(id).iter().map(|n| n.id.clone()).collect();
        for k in kids {
            self.remove_subtree(&k);
        }
        self.nodes.retain(|n| n.id != id);
    }

    /// フォルダの移動（親と名前の変更。ID は変わらない）。宛先が既にあるフォルダなら、中身を足して（merge）元を消す
    /// （本物の rclone の `moveto` と同じ。宛先の ID は元と別になる）。親が無ければ作る。
    pub fn move_dir(&mut self, src: &str, dst: &[String]) -> bool {
        let Some((leaf, parent_path)) = dst.split_last() else {
            return false;
        };
        if self.node(src).is_none() {
            return false;
        }
        let parent = self.mkdir_p(parent_path);
        if let Some(existing) = self.find_dir(&parent, leaf).map(|n| n.id.clone()) {
            if existing == src {
                return true;
            }
            let kids: Vec<String> = self.children(src).iter().map(|n| n.id.clone()).collect();
            for k in kids {
                if let Some(n) = self.node_mut(&k) {
                    n.parent = existing.clone();
                }
            }
            self.nodes.retain(|n| n.id != src);
            return true;
        }
        if let Some(n) = self.node_mut(src) {
            n.parent = parent;
            n.name = leaf.clone();
        }
        true
    }

    /// ファイル単位の移動に切り替わった場合（宛先に新しい ID のフォルダができ、中身が移り、元は消える）。
    pub fn move_dir_filewise(&mut self, src: &str, dst: &[String]) -> bool {
        let Some((leaf, parent_path)) = dst.split_last() else {
            return false;
        };
        if self.node(src).is_none() {
            return false;
        }
        let parent = self.mkdir_p(parent_path);
        let fresh = self.new_node(&parent, leaf, Kind::Dir, 0, None, None);
        let kids: Vec<String> = self.children(src).iter().map(|n| n.id.clone()).collect();
        for k in kids {
            if let Some(n) = self.node_mut(&k) {
                n.parent = fresh.clone();
            }
        }
        self.nodes.retain(|n| n.id != src);
        true
    }

    /// `purge`。`trash` が真ならゴミ箱へ（ID を `trashed` に残す）、偽なら `deleted` に残す。
    pub fn purge(&mut self, id: &str, trash: bool) {
        self.remove_subtree(id);
        if trash {
            self.trashed.push(id.to_owned());
        } else {
            self.deleted.push(id.to_owned());
        }
    }

    pub fn apply_pending(&mut self) {
        let mut pending = std::mem::take(&mut self.pending);
        pending.retain_mut(|p| {
            if p.remaining == 0 {
                self.move_dir(&p.src.clone(), &p.dst.clone());
                false
            } else {
                p.remaining -= 1;
                true
            }
        });
        self.pending = pending;
    }

    // ---- 保存 ----

    pub fn to_json(&self) -> Value {
        let nodes: Vec<Value> = self
            .nodes
            .iter()
            .map(|n| {
                json!({
                    "id": n.id, "parent": n.parent, "name": n.name, "kind": n.kind.as_str(),
                    "size": n.size, "content": n.content, "target": n.target, "md5": n.md5,
                })
            })
            .collect();
        let pending: Vec<Value> = self
            .pending
            .iter()
            .map(|p| json!({"src": p.src, "dst": p.dst, "remaining": p.remaining}))
            .collect();
        json!({
            "nodes": nodes, "next": self.next, "trashed": self.trashed,
            "deleted": self.deleted, "pending": pending,
        })
    }

    pub fn from_json(v: &Value) -> Self {
        let strs = |v: &Value| -> Vec<String> {
            v.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let nodes = v["nodes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|n| Node {
                        id: n["id"].as_str().unwrap_or_default().to_owned(),
                        parent: n["parent"].as_str().unwrap_or_default().to_owned(),
                        name: n["name"].as_str().unwrap_or_default().to_owned(),
                        kind: Kind::parse(n["kind"].as_str().unwrap_or("file")),
                        size: n["size"].as_u64().unwrap_or(0),
                        content: n["content"].as_str().map(str::to_owned),
                        md5: n["md5"].as_str().map(str::to_owned),
                        target: n["target"].as_str().map(str::to_owned),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let pending = v["pending"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| Pending {
                        src: p["src"].as_str().unwrap_or_default().to_owned(),
                        dst: strs(&p["dst"]),
                        remaining: p["remaining"].as_u64().unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            nodes,
            next: v["next"].as_u64().unwrap_or(0),
            trashed: strs(&v["trashed"]),
            deleted: strs(&v["deleted"]),
            pending,
        }
    }

    pub fn load(dir: &std::path::Path) -> Self {
        std::fs::read(dir.join("drive.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| Self::from_json(&v))
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("fake drive dir");
        std::fs::write(
            dir.join("drive.json"),
            serde_json::to_vec(&self.to_json()).unwrap(),
        )
        .expect("fake drive save");
    }
}

/// 呼び出しごとの挙動の差し替え。試験が作り、偽の rclone が読む（`ctl.json`）。
#[derive(Clone, Debug)]
pub struct Rule {
    /// 中立の操作名（`list`・`read`・`create_folder`・`upload`・`move`・`trash`）。
    pub op: String,
    /// 引数のどこかにこの文字列があるときだけ（`None` は常に）。
    pub contains: Option<String>,
    /// 一致した呼び出しのうち、最初の `skip` 回は通常どおりにする。
    pub skip: u64,
    /// その次の `times` 回に効果を掛ける。
    pub times: u64,
    pub seen: u64,
    /// `{"kind": "fail"|"sleep"|"do_then_fail"|"late_then_fail"|"filewise"|"garbage"|"stderr_fail", ...}`
    pub effect: Value,
}

#[derive(Clone, Debug, Default)]
pub struct Ctl {
    /// `mime`（既定）か `resolved`。
    pub shortcut_mode: String,
    pub rules: Vec<Rule>,
}

impl Ctl {
    pub fn to_json(&self) -> Value {
        let rules: Vec<Value> = self
            .rules
            .iter()
            .map(|r| {
                json!({"op": r.op, "contains": r.contains, "skip": r.skip, "times": r.times,
                       "seen": r.seen, "effect": r.effect})
            })
            .collect();
        let mut m = Map::new();
        m.insert("shortcut_mode".into(), json!(self.shortcut_mode));
        m.insert("rules".into(), Value::Array(rules));
        Value::Object(m)
    }

    pub fn from_json(v: &Value) -> Self {
        let rules = v["rules"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| Rule {
                        op: r["op"].as_str().unwrap_or_default().to_owned(),
                        contains: r["contains"].as_str().map(str::to_owned),
                        skip: r["skip"].as_u64().unwrap_or(0),
                        times: r["times"].as_u64().unwrap_or(1),
                        seen: r["seen"].as_u64().unwrap_or(0),
                        effect: r["effect"].clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            shortcut_mode: v["shortcut_mode"].as_str().unwrap_or("mime").to_owned(),
            rules,
        }
    }

    pub fn load(dir: &std::path::Path) -> Self {
        std::fs::read(dir.join("ctl.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| Self::from_json(&v))
            .unwrap_or_else(|| Self {
                shortcut_mode: "mime".into(),
                rules: Vec::new(),
            })
    }

    pub fn save(&self, dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("fake ctl dir");
        std::fs::write(
            dir.join("ctl.json"),
            serde_json::to_vec(&self.to_json()).unwrap(),
        )
        .expect("fake ctl save");
    }
}
