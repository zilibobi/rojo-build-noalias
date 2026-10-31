//! `cargo run --example rbxm_diff -- <a.rbxm> <b.rbxm>`: walks both models in child order and
//! prints up to 20 differences as `<instance path>: <what>`. Exits 0 when there are none, 1 when
//! there are some, 2 when a file cannot be read or decoded.

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{Instance, WeakDom};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::ExitCode;

const LIMIT: usize = 20;
const EXCERPT: usize = 120;

struct Side {
    dom: WeakDom,
    paths: HashMap<Ref, String>,
}

impl Side {
    fn load(path: &Path) -> Result<Side, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let dom = rbx_binary::from_reader(bytes.as_slice())
            .map_err(|e| format!("cannot decode {}: {e}", path.display()))?;
        let mut paths = HashMap::new();
        name_children(&dom, dom.root(), "", &mut paths);

        Ok(Side { dom, paths })
    }

    fn get(&self, referent: Ref) -> &Instance {
        self.dom
            .get_by_ref(referent)
            .expect("a child referent resolves")
    }

    /// A Ref property is compared by the path of the instance it points at, since referents are
    /// assigned afresh on every load.
    fn target(&self, referent: Ref) -> String {
        if referent.is_none() {
            return "nil".to_string();
        }

        self.paths
            .get(&referent)
            .cloned()
            .unwrap_or_else(|| "<outside the model>".to_string())
    }
}

/// Each child's path is its parent's plus its name, with `[index]` added when a sibling shares it.
fn name_children(dom: &WeakDom, parent: &Instance, prefix: &str, out: &mut HashMap<Ref, String>) {
    let children = parent.children();

    for (index, referent) in children.iter().enumerate() {
        let child = dom
            .get_by_ref(*referent)
            .expect("a child referent resolves");
        let shared = children
            .iter()
            .filter(|r| dom.get_by_ref(**r).is_some_and(|c| c.name == child.name))
            .count()
            > 1;
        let label = if shared {
            format!("{}[{index}]", child.name)
        } else {
            child.name.clone()
        };
        let path = if prefix.is_empty() {
            label
        } else {
            format!("{prefix}/{label}")
        };

        name_children(dom, child, &path, out);
        out.insert(*referent, path);
    }
}

struct Diff<'a> {
    a: &'a Side,
    b: &'a Side,
    found: Vec<String>,
}

impl Diff<'_> {
    fn note(&mut self, path: &str, what: String) {
        let path = if path.is_empty() { "<root>" } else { path };
        self.found.push(format!("{path}: {what}"));
    }

    fn full(&self) -> bool {
        self.found.len() >= LIMIT
    }

    fn children(&mut self, a: &Instance, b: &Instance, path: &str) {
        let (ca, cb) = (a.children(), b.children());

        for index in 0..ca.len().max(cb.len()) {
            if self.full() {
                return;
            }

            match (ca.get(index), cb.get(index)) {
                (Some(ra), Some(rb)) => self.instance(*ra, *rb),
                (Some(ra), None) => {
                    let child = self.a.get(*ra);
                    let what = format!("missing child in b: {} ({})", child.name, child.class);
                    self.note(path, what);
                }
                (None, Some(rb)) => {
                    let child = self.b.get(*rb);
                    let what = format!("missing child in a: {} ({})", child.name, child.class);
                    self.note(path, what);
                }
                (None, None) => unreachable!("index is below the longer length"),
            }
        }
    }

    fn instance(&mut self, ra: Ref, rb: Ref) {
        let (a, b) = (self.a.get(ra), self.b.get(rb));
        let path = self.a.paths[&ra].clone();

        if a.name != b.name {
            self.note(&path, format!("name {:?} vs {:?}", a.name, b.name));
        }

        if a.class != b.class {
            self.note(&path, format!("class {} vs {}", a.class, b.class));
        }

        let keys: BTreeSet<&str> = a
            .properties
            .keys()
            .chain(b.properties.keys())
            .map(|k| k.as_str())
            .collect();

        for key in keys {
            if self.full() {
                return;
            }

            let va = a.properties.get(&rbx_dom_weak::ustr(key));
            let vb = b.properties.get(&rbx_dom_weak::ustr(key));

            match (va, vb) {
                (Some(va), Some(vb)) => {
                    if let Some(what) = self.value(va, vb) {
                        self.note(&path, format!("property {key}: {what}"));
                    }
                }
                (Some(_), None) => self.note(&path, format!("property {key} missing in b")),
                (None, Some(_)) => self.note(&path, format!("property {key} missing in a")),
                (None, None) => unreachable!("the key came from one of the maps"),
            }
        }

        self.children(a, b, &path);
    }

    fn value(&self, va: &Variant, vb: &Variant) -> Option<String> {
        match (va, vb) {
            (Variant::Ref(ra), Variant::Ref(rb)) => {
                let (ta, tb) = (self.a.target(*ra), self.b.target(*rb));
                (ta != tb).then(|| format!("ref {ta} vs {tb}"))
            }
            (Variant::String(sa), Variant::String(sb)) => {
                (sa != sb).then(|| text(sa.as_bytes(), sb.as_bytes()))
            }
            (Variant::BinaryString(sa), Variant::BinaryString(sb)) => {
                (sa != sb).then(|| text(sa.as_ref(), sb.as_ref()))
            }
            _ if va == vb => None,
            _ => Some(format!(
                "{} vs {}",
                excerpt(&format!("{va:?}")),
                excerpt(&format!("{vb:?}"))
            )),
        }
    }
}

/// Where two strings first differ: the line number and both lines, shortened.
fn text(a: &[u8], b: &[u8]) -> String {
    let (a, b) = (String::from_utf8_lossy(a), String::from_utf8_lossy(b));
    let mut la = a.split('\n');
    let mut lb = b.split('\n');
    let mut line = 1;

    loop {
        match (la.next(), lb.next()) {
            (Some(x), Some(y)) if x == y => line += 1,
            (x, y) => {
                return format!(
                    "text differs at line {line} ({} vs {} bytes): {} vs {}",
                    a.len(),
                    b.len(),
                    excerpt(&format!("{:?}", x.unwrap_or("<end>"))),
                    excerpt(&format!("{:?}", y.unwrap_or("<end>")))
                );
            }
        }
    }
}

fn excerpt(s: &str) -> String {
    if s.chars().count() <= EXCERPT {
        return s.to_string();
    }

    let cut: String = s.chars().take(EXCERPT).collect();
    format!("{cut}...")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let [a, b] = args.as_slice() else {
        eprintln!("usage: rbxm_diff <a.rbxm> <b.rbxm>");
        return ExitCode::from(2);
    };

    let (a, b) = match (Side::load(Path::new(a)), Side::load(Path::new(b))) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let mut diff = Diff {
        a: &a,
        b: &b,
        found: Vec::new(),
    };
    diff.children(a.dom.root(), b.dom.root(), "");

    for line in &diff.found {
        println!("{line}");
    }

    if diff.found.is_empty() {
        ExitCode::SUCCESS
    } else {
        if diff.full() {
            println!("(stopped after {LIMIT} differences)");
        }

        ExitCode::from(1)
    }
}
