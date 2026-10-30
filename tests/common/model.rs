use rbx_dom_weak::types::Variant;
use rbx_dom_weak::{WeakDom, ustr};
use std::path::Path;

/// One instance of a built model: what the tests compare, nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inst {
    pub name: String,
    pub class: String,
    pub source: Option<String>,
    pub children: Vec<Inst>,
}

impl Inst {
    /// The descendant reached by following `path`'s names from this instance's children.
    pub fn find(&self, path: &[&str]) -> Option<&Inst> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };

        self.children.iter().find(|c| c.name == *first)?.find(rest)
    }

    /// The same tree without any Source and with children sorted by name then class, for
    /// comparing structure alone (Rojo orders project children and directory children apart).
    pub fn shape(&self) -> Inst {
        let mut children: Vec<Inst> = self.children.iter().map(Inst::shape).collect();
        children.sort_by(|a, b| (&a.name, &a.class).cmp(&(&b.name, &b.class)));

        Inst {
            name: self.name.clone(),
            class: self.class.clone(),
            source: None,
            children,
        }
    }
}

/// The roots of the `.rbxm` at `path`.
pub fn load(path: &Path) -> Vec<Inst> {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let dom = rbx_binary::from_reader(bytes.as_slice())
        .unwrap_or_else(|e| panic!("cannot decode {}: {e}", path.display()));

    dom.root()
        .children()
        .iter()
        .map(|r| convert(&dom, *r))
        .collect()
}

fn convert(dom: &WeakDom, referent: rbx_dom_weak::types::Ref) -> Inst {
    let inst = dom.get_by_ref(referent).expect("a child referent resolves");
    let source = match inst.properties.get(&ustr("Source")) {
        Some(Variant::String(s)) => Some(s.clone()),
        Some(Variant::BinaryString(b)) => Some(String::from_utf8_lossy(b.as_ref()).into_owned()),
        _ => None,
    };

    Inst {
        name: inst.name.clone(),
        class: inst.class.to_string(),
        source,
        children: inst.children().iter().map(|r| convert(dom, *r)).collect(),
    }
}

/// Every Source in the tree, keyed by its `/`-joined instance path from a root, in tree order.
pub fn sources(roots: &[Inst]) -> Vec<(String, String)> {
    fn walk(inst: &Inst, prefix: &str, out: &mut Vec<(String, String)>) {
        let path = if prefix.is_empty() {
            inst.name.clone()
        } else {
            format!("{prefix}/{}", inst.name)
        };

        if let Some(source) = &inst.source {
            out.push((path.clone(), source.clone()));
        }

        for child in &inst.children {
            walk(child, &path, out);
        }
    }

    let mut out = Vec::new();

    for root in roots {
        walk(root, "", &mut out);
    }

    out
}
