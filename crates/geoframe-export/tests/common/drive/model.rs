//! 試験用の偽の Drive の模型（本物の Drive には触れない）。
//!
//! 偽の Drive の HTTP サーバー（`tests/common/fake_drive.rs`）と、試験が Drive の状態を作る・読むための部品
//! （`tests/common/drive.rs`）が、同じこのファイルを使う。状態は JSON のファイル 1 つ（`drive.json`）で、
//! ノードの表（ID・親・名前・種類・大きさ）として持つ:
//! 同じ名前のフォルダが 2 つあること、ショートカット、移動しても変わらない ID、移動の遅れた反映を、ふつうのディレクトリの
//! 木では表せないため。ID は架空の値（`fid0001` …）。
#![allow(dead_code)]

use serde_json::{Map, Value, json};

pub const ROOT: &str = "root";
pub const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
pub const GDOC_MIME: &str = "application/vnd.google-apps.document";
pub const FORM_MIME: &str = "application/vnd.google-apps.form";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Shortcut,
    /// ネイティブの Google 文書（エクスポートできる。大きさも MD5 も持たない）。
    Gdoc,
    /// エクスポートできない Google のファイル（フォームなど）。
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
    /// ファイルの MD5（`md5Checksum`）。`None` なら、中身（`content`、無ければ `x` を `size` 個）の MD5。送って作った
    /// ファイルは、送った中身の MD5 を入れる。
    pub md5: Option<String>,
    /// ショートカットの指す先の ID。
    pub target: Option<String>,
    /// ゴミ箱に入っている（偽の Drive の HTTP サーバーの `trash` が立てる。`Drive::trashed` にも ID を残す）。
    pub trashed: bool,
}

/// 実行が遅れて反映される移動（`late_then_fail` の効果）。
#[derive(Clone, Debug)]
pub struct Pending {
    pub src: String,
    pub dst: Vec<String>,
    /// あと何回の一覧が、古い状態を見せるか。
    pub remaining: u64,
}

/// 一覧（[`Drive::ls`]）の 1 行。
#[derive(Clone, Debug)]
pub struct Row {
    /// 起点からの相対パス（`/` 区切り）。
    pub path: String,
    pub size: i64,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Drive {
    pub nodes: Vec<Node>,
    pub next: u64,
    /// ゴミ箱へ移した ID。
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
    /// ファイルの MD5（`md5` があればそれ、無ければ中身の MD5）。
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

    /// 親の下の、名前が一致する最初のフォルダ（ゴミ箱へ移したものは数えない。偽の Drive の HTTP サーバーは `trashed` の印を付けて
    /// 残す）。
    fn find_dir(&self, parent: &str, name: &str) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|n| n.parent == parent && n.name == name && n.kind == Kind::Dir && !n.trashed)
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
            trashed: false,
        });
        id
    }

    /// ID を指定してノードを足す（偽の Drive の HTTP サーバーが、`generateIds` で渡した ID のフォルダ・ファイルを作る）。
    pub fn push_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    /// そのノードか、祖先のどれかがゴミ箱に入っているか（Drive の `trashed` は、親がゴミ箱の中でも真）。
    pub fn is_trashed(&self, id: &str) -> bool {
        let mut cur = id.to_owned();
        for _ in 0..64 {
            let Some(n) = self.node(&cur) else {
                return false;
            };
            if n.trashed {
                return true;
            }
            cur = n.parent.clone();
        }
        false
    }

    /// 同じ名前のフォルダがあっても、`node_mut` で書き換えるために。
    pub fn node_mut_pub(&mut self, id: &str) -> Option<&mut Node> {
        self.node_mut(id)
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

    /// `dir` の下の、ファイルとフォルダの一覧（再帰。ショートカット・Google の文書は出さない）。
    pub fn ls(&self, dir: &str) -> Vec<Row> {
        let mut out = Vec::new();
        self.ls_into(dir, "", &mut out, 0);
        out
    }

    fn ls_into(&self, dir: &str, prefix: &str, out: &mut Vec<Row>, depth: usize) {
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
                Kind::Dir => {
                    out.push(Row {
                        path: path.clone(),
                        size: -1,
                        is_dir: true,
                    });
                    self.ls_into(&n.id, &path, out, depth + 1);
                }
                Kind::File => out.push(Row {
                    path,
                    size: n.size as i64,
                    is_dir: false,
                }),
                Kind::Shortcut | Kind::Gdoc | Kind::Form => {}
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
    /// （宛先の ID は元と別になる。API の移動は ID を保つので、本番の移動とは違う。試験が「合流した」状態を作る）。親が無ければ作る。
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
                    "trashed": n.trashed,
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
                        trashed: n["trashed"].as_bool().unwrap_or(false),
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

    /// 保存する（一時ファイルに書いて rename する。偽の Drive の HTTP サーバーが読んでいる最中に、半端な内容を見せない）。
    pub fn save(&self, dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("fake drive dir");
        write_atomic(
            &dir.join("drive.json"),
            &serde_json::to_vec(&self.to_json()).unwrap(),
        );
    }
}

/// 呼び出しごとの挙動の差し替え。試験が作り、偽の Drive の HTTP サーバーが読む（`ctl.json`）。
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
    /// 効果（`tests/common/fake_drive.rs` の関数で作る）。
    pub effect: Value,
}

#[derive(Clone, Debug, Default)]
pub struct Ctl {
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
        Self { rules }
    }

    pub fn load(dir: &std::path::Path) -> Self {
        std::fs::read(dir.join("ctl.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| Self::from_json(&v))
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &std::path::Path) {
        std::fs::create_dir_all(dir).expect("fake ctl dir");
        write_atomic(
            &dir.join("ctl.json"),
            &serde_json::to_vec(&self.to_json()).unwrap(),
        );
    }
}

/// 一時ファイルに書いて rename する（読む側が、書きかけを見ない）。
fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes).expect("fake state write");
    std::fs::rename(&tmp, path).expect("fake state rename");
}
