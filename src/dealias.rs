use crate::sourcemap::NodeIndex;
use crate::{log, paths};
use regex::bytes::{Captures, Regex};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    /// the file's instance path joined by `/` (orphans: its workspace-relative path)
    pub label: String,
    /// the original require, always written with double quotes
    pub from: String,
    pub to: String,
}

/// One regex for a whole alias table with the reach of v1's two Lua patterns per alias:
/// `require("@a")` and `require("@a/sub")`, either quote on either side, no whitespace,
/// comments and strings included. Group 1 is the alias, group 2 the subpath.
pub fn matcher<'a>(aliases: impl IntoIterator<Item = &'a str>) -> Option<Regex> {
    // longest first so an alias that prefixes another never shadows it
    let mut names: Vec<&str> = aliases.into_iter().collect();

    if names.is_empty() {
        return None;
    }

    names.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    names.dedup();

    let alt = names
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|");

    // the subpath class matches any byte but a quote, as Lua's `[^"']` does
    let pattern = format!(r#"require\(["']@({alt})(?:["']\)|/((?-u:[^"'])+)["']\))"#);

    Some(Regex::new(&pattern).expect("escaped alias alternatives always compile"))
}

/// The instance path a require is resolved from (v1.2.4 `replaceAliases.luau:401-412`).
/// Luau resolves `init.luau` from its parent folder, so the root init gets an empty context and
/// `./<Root>/...` collapses to `@self/...`; wax's pre-RFC shim treats a nested init as its own
/// folder, so `wax` appends an empty sentinel that costs one extra `..`.
pub fn context_path(file: &Path, instance: &[String], wax: bool) -> Vec<String> {
    if !paths::is_init(file) {
        return instance.to_vec();
    }

    if instance.len() == 1 {
        return Vec::new();
    }

    let mut out = instance.to_vec();

    if wax {
        out.push(String::new());
    }

    out
}

/// `util.getRelativePath` (v1.2.4 `util.luau:309-341`).
pub fn relative_path(from: &[String], to: &[String]) -> Vec<String> {
    let common = from.iter().zip(to).take_while(|(a, b)| a == b).count();
    let ups = from.len().saturating_sub(common + 1);

    let mut out = if ups == 0 {
        vec![".".to_string()]
    } else {
        vec!["..".to_string(); ups]
    };

    out.extend(to[common..].iter().cloned());
    out
}

/// `convertToSelfPath` (v1.2.4 `replaceAliases.luau:116-154`): `./<Root>/rest` and
/// `../../<Root>/rest` become `@self/rest`, so a Wally rename of the package folder is harmless.
pub fn to_self_path(rel: &str, root_name: &str) -> String {
    let out = self_path_bytes(rel.as_bytes(), root_name.as_bytes());

    String::from_utf8(out).expect("whole segments of UTF-8 text stay UTF-8")
}

/// [`to_self_path`] on bytes, so a subpath that is not UTF-8 passes through untouched.
fn self_path_bytes(rel: &[u8], root_name: &[u8]) -> Vec<u8> {
    let segments: Vec<&[u8]> = rel.split(|b| *b == b'/').collect();

    let rest = |from: usize| {
        let mut out = b"@self".to_vec();

        for segment in &segments[from..] {
            out.push(b'/');
            out.extend_from_slice(segment);
        }

        out
    };

    if segments.len() >= 2 && segments[0] == b"." && segments[1] == root_name {
        return rest(2);
    }

    for (i, segment) in segments.iter().enumerate() {
        if i > 0 && *segment == root_name && segments[..i].iter().all(|s| *s == b"..") {
            return rest(i + 1);
        }
    }

    rel.to_vec()
}

/// Each alias's target instance path: `<scope_dir>/<value>` normalized, looked up as is, then
/// with `/init.luau`, then `/init.lua`. A miss logs v1's two warnings and the alias stays raw.
pub fn resolve_aliases(
    aliases: &BTreeMap<String, String>,
    scope_dir: &Path,
    index: &NodeIndex,
    luaurc: &Path,
) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();

    for (alias, value) in aliases {
        let key = paths::normalize(&scope_dir.join(value));
        let candidates = [key.clone(), key.join("init.luau"), key.join("init.lua")];
        let hit = candidates
            .iter()
            .find_map(|c| index.paths.get(c).map(|instance| (c, instance)));

        let Some((path, instance)) = hit else {
            log::warn(&format!(
                "Alias '{alias}' with path '{value}' in {} was not found in the sourcemap.",
                luaurc.display()
            ));
            log::warn(&format!("Looking for: {}", key.display()));
            continue;
        };

        if log::verbose() > 0 {
            log::info(&format!(
                "Alias '{alias}' -> fs:{} -> instance:{}",
                path.display(),
                instance.join("/")
            ));
        }

        out.insert(alias.clone(), instance.clone());
    }

    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Rewrites every match of `re` through `to_path(alias, subpath)`, recording one row per match.
fn rewrite_with(
    re: Option<&Regex>,
    src: &[u8],
    label: &str,
    to_path: impl Fn(&str, Option<&[u8]>) -> Vec<u8>,
) -> (Vec<u8>, Vec<Replacement>) {
    let Some(re) = re else {
        return (src.to_vec(), Vec::new());
    };

    let mut rows = Vec::new();

    let out = re.replace_all(src, |caps: &Captures| {
        let alias = text(&caps[1]);
        let sub = caps.get(2).map(|m| m.as_bytes());

        let from = match sub {
            Some(sub) => format!(r#"require("@{alias}/{}")"#, text(sub)),
            None => format!(r#"require("@{alias}")"#),
        };

        let mut rep = b"require(\"".to_vec();
        rep.extend(to_path(&alias, sub));
        rep.extend(b"\")");

        rows.push(Replacement {
            label: label.to_string(),
            from,
            to: text(&rep),
        });

        rep
    });

    (out.into_owned(), rows)
}

fn join_sub(base: &str, sub: Option<&[u8]>) -> Vec<u8> {
    let mut out = base.as_bytes().to_vec();

    if let Some(sub) = sub {
        out.push(b'/');
        out.extend(sub);
    }

    out
}

/// The rules of a project scope: aliases resolved to instance paths, rewritten relative to the
/// requiring file's context path.
#[derive(Clone, Debug)]
pub struct ProjectRules {
    pub root_name: String,
    pub targets: BTreeMap<String, Vec<String>>,
    re: Option<Regex>,
}

impl ProjectRules {
    pub fn new(root_name: String, targets: BTreeMap<String, Vec<String>>) -> ProjectRules {
        let re = matcher(targets.keys().map(String::as_str));

        ProjectRules {
            root_name,
            targets,
            re,
        }
    }

    pub fn rewrite(
        &self,
        src: &[u8],
        context: &[String],
        label: &str,
    ) -> (Vec<u8>, Vec<Replacement>) {
        rewrite_with(self.re.as_ref(), src, label, |alias, sub| {
            let target = &self.targets[alias];
            let relative = relative_path(context, target).join("/");

            self_path_bytes(&join_sub(&relative, sub), self.root_name.as_bytes())
        })
    }
}

/// The rules of an orphan scope (a `.luaurc` with no project file): literal text replacement
/// with each value exactly as written.
#[derive(Clone, Debug)]
pub struct OrphanRules {
    pub aliases: BTreeMap<String, String>,
    re: Option<Regex>,
}

impl OrphanRules {
    pub fn new(aliases: BTreeMap<String, String>) -> OrphanRules {
        let re = matcher(aliases.keys().map(String::as_str));

        OrphanRules { aliases, re }
    }

    pub fn rewrite(&self, src: &[u8], label: &str) -> (Vec<u8>, Vec<Replacement>) {
        rewrite_with(self.re.as_ref(), src, label, |alias, sub| {
            join_sub(&self.aliases[alias], sub)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sourcemap;
    use std::path::PathBuf;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_path(&v(&["R", "mod", "helpers"]), &v(&["R", "utils", "config"])),
            v(&["..", "utils", "config"])
        );
        assert_eq!(relative_path(&v(&[]), &v(&["R"])), v(&[".", "R"]));
        assert_eq!(
            relative_path(&v(&["R", "a"]), &v(&["R", "a", "b"])),
            v(&[".", "b"])
        );
        assert_eq!(
            relative_path(&v(&["R", "lib", "Foo"]), &v(&["R", "pkg", "Logger"])),
            v(&["..", "pkg", "Logger"])
        );
        assert_eq!(
            relative_path(&v(&["R", "lib", "Foo", ""]), &v(&["R", "pkg", "Logger"])),
            v(&["..", "..", "pkg", "Logger"])
        );
    }

    #[test]
    fn relative_path_to_an_ancestor() {
        assert_eq!(
            relative_path(&v(&["R", "a", "b"]), &v(&["R", "a"])),
            v(&["."])
        );
        assert_eq!(
            relative_path(&v(&["R", "a", "b", "c"]), &v(&["R", "a"])),
            v(&[".."])
        );
    }

    #[test]
    fn self_paths() {
        assert_eq!(to_self_path("./R/mod/helpers", "R"), "@self/mod/helpers");
        assert_eq!(to_self_path("../../R/x", "R"), "@self/x");
        assert_eq!(to_self_path("../R2/x", "R"), "../R2/x");
        assert_eq!(to_self_path("../mod/R/x", "R"), "../mod/R/x");
    }

    #[test]
    fn self_path_edges() {
        assert_eq!(to_self_path("./R", "R"), "@self");
        assert_eq!(to_self_path("../R", "R"), "@self");
        assert_eq!(to_self_path("./X/R/y", "R"), "./X/R/y");
        assert_eq!(to_self_path("R/x", "R"), "R/x");
        // the first root name with only `..` before it wins
        assert_eq!(to_self_path("../R/R/x", "R"), "@self/R/x");
    }

    #[test]
    fn context_paths() {
        let root = v(&["R"]);
        let nested = v(&["R", "lib", "Foo"]);
        assert_eq!(
            context_path(Path::new("/p/src/init.luau"), &root, false),
            v(&[])
        );
        assert_eq!(
            context_path(Path::new("/p/src/lib/Foo/init.luau"), &nested, false),
            nested
        );
        assert_eq!(
            context_path(Path::new("/p/src/lib/Foo/init.lua"), &nested, true),
            v(&["R", "lib", "Foo", ""])
        );
        assert_eq!(
            context_path(
                Path::new("/p/src/lib/Foo/Bar.luau"),
                &v(&["R", "lib", "Foo", "Bar"]),
                true
            ),
            v(&["R", "lib", "Foo", "Bar"])
        );
    }

    #[test]
    fn context_path_root_init_under_wax_is_still_empty() {
        assert_eq!(
            context_path(Path::new("/p/src/init.lua"), &v(&["R"]), true),
            v(&[])
        );
        assert_eq!(
            context_path(Path::new("/p/src/reinit.luau"), &v(&["R", "reinit"]), true),
            v(&["R", "reinit"])
        );
    }

    #[test]
    fn matcher_reach() {
        let m = matcher(["vfx", "vfx-pkg"]).unwrap();
        let alias = |s: &str| {
            m.captures(s.as_bytes())
                .map(|c| String::from_utf8(c[1].to_vec()).unwrap())
        };
        assert_eq!(
            alias(r#"require("@vfx-pkg/Z")"#).as_deref(),
            Some("vfx-pkg")
        );
        assert_eq!(alias(r#"require('@vfx")"#).as_deref(), Some("vfx"));
        assert_eq!(alias(r#"-- require("@vfx/x")"#).as_deref(), Some("vfx"));
        assert_eq!(alias(r#"require ("@vfx")"#), None);
        assert_eq!(alias(r#"require("@vfxx")"#), None);
        assert_eq!(alias(r#"require("@vfx/")"#), None);
        assert!(matcher(std::iter::empty::<&str>()).is_none());
    }

    #[test]
    fn matcher_is_literal() {
        // v1 escaped only - + * [ ] ( ), so a `.` matched any character
        let m = matcher(["a.b"]).unwrap();
        assert!(m.is_match(br#"require("@a.b")"#));
        assert!(!m.is_match(br#"require("@aXb")"#));
    }

    #[test]
    fn project_rewrite_matches_basic_alias_fixture() {
        let targets = BTreeMap::from([
            ("mod".to_string(), v(&["BasicAlias", "mod"])),
            ("utils".to_string(), v(&["BasicAlias", "utils"])),
        ]);
        let rules = ProjectRules::new("BasicAlias".into(), targets);
        let (root, rows) = rules.rewrite(
            b"local h = require(\"@mod/helpers\")\nlocal c = require('@utils/config')",
            &v(&[]),
            "BasicAlias",
        );
        assert_eq!(
            String::from_utf8(root).unwrap(),
            "local h = require(\"@self/mod/helpers\")\nlocal c = require(\"@self/utils/config\")"
        );
        assert_eq!(rows[0].from, r#"require("@mod/helpers")"#);
        let (h, _) = rules.rewrite(
            b"require(\"@utils/config\")",
            &v(&["BasicAlias", "mod", "helpers"]),
            "BasicAlias/mod/helpers",
        );
        assert_eq!(h, b"require(\"../utils/config\")");
    }

    #[test]
    fn project_rewrite_rows_and_bare_alias() {
        let targets = BTreeMap::from([("pkg".to_string(), v(&["R", "pkg"]))]);
        let rules = ProjectRules::new("R".into(), targets);
        let (out, rows) = rules.rewrite(
            b"require('@pkg') require(\"@pkg/A/B\") require(\"@other/x\")",
            &v(&["R", "mod", "m"]),
            "R/mod/m",
        );
        assert_eq!(
            out,
            b"require(\"../pkg\") require(\"../pkg/A/B\") require(\"@other/x\")"
        );
        assert_eq!(
            rows,
            vec![
                Replacement {
                    label: "R/mod/m".into(),
                    from: r#"require("@pkg")"#.into(),
                    to: r#"require("../pkg")"#.into()
                },
                Replacement {
                    label: "R/mod/m".into(),
                    from: r#"require("@pkg/A/B")"#.into(),
                    to: r#"require("../pkg/A/B")"#.into()
                },
            ]
        );
    }

    #[test]
    fn project_rewrite_keeps_non_utf8_bytes() {
        let targets = BTreeMap::from([("pkg".to_string(), v(&["R", "pkg"]))]);
        let rules = ProjectRules::new("R".into(), targets);
        let (out, rows) =
            rules.rewrite(b"\xff\xfe require(\"@pkg/x\") \x80", &v(&["R", "m"]), "R/m");
        assert_eq!(out, b"\xff\xfe require(\"./pkg/x\") \x80");
        assert_eq!(rows.len(), 1);

        let (same, none) = ProjectRules::new("R".into(), BTreeMap::new()).rewrite(
            b"require(\"@pkg/x\")",
            &v(&[]),
            "R",
        );
        assert_eq!(same, b"require(\"@pkg/x\")");
        assert!(none.is_empty());
    }

    #[test]
    fn project_rewrite_self_converts_through_a_non_utf8_subpath() {
        let targets = BTreeMap::from([("up".to_string(), v(&["R"]))]);
        let rules = ProjectRules::new("R".into(), targets);
        let (out, rows) = rules.rewrite(b"require(\"@up/R/\xff\")", &v(&["R", "a", "b"]), "R/a/b");
        assert_eq!(out, b"require(\"@self/\xff\")");
        assert_eq!(rows[0].from, "require(\"@up/R/\u{fffd}\")");
    }

    #[test]
    fn project_rewrite_self_alias_fixture() {
        // self-alias: @self = ./src (the root), so the root init keeps @self and a leaf goes relative
        let targets = BTreeMap::from([
            ("mod".to_string(), v(&["SelfAlias", "mod"])),
            ("self".to_string(), v(&["SelfAlias"])),
        ]);
        let rules = ProjectRules::new("SelfAlias".into(), targets);
        let (init, _) = rules.rewrite(b"require(\"@self/mod/helpers\")", &v(&[]), "SelfAlias");
        assert_eq!(init, b"require(\"@self/mod/helpers\")");
        let (helpers, _) = rules.rewrite(
            b"require(\"@self/mod/deep/nested\")",
            &v(&["SelfAlias", "mod", "helpers"]),
            "SelfAlias/mod/helpers",
        );
        assert_eq!(helpers, b"require(\"../mod/deep/nested\")");
    }

    #[test]
    fn orphan_rewrite_is_literal() {
        let rules = OrphanRules::new(BTreeMap::from([
            ("packages".to_string(), "./packages".to_string()),
            ("vfx".to_string(), "../BundledAssets/ForgeVFX".to_string()),
        ]));
        let (out, _) = rules.rewrite(
            b"require(\"@packages/Promise\") require('@vfx')",
            "src/main.luau",
        );
        assert_eq!(
            out,
            b"require(\"./packages/Promise\") require(\"../BundledAssets/ForgeVFX\")"
        );
    }

    #[test]
    fn orphan_rows() {
        let rules = OrphanRules::new(BTreeMap::from([("lib".to_string(), "./lib".to_string())]));
        let (_, rows) = rules.rewrite(b"require('@lib/utils')", "src/main.luau");
        assert_eq!(
            rows,
            vec![Replacement {
                label: "src/main.luau".into(),
                from: r#"require("@lib/utils")"#.into(),
                to: r#"require("./lib/utils")"#.into()
            }]
        );
    }

    #[test]
    fn resolve_aliases_against_the_fixture_index() {
        let sm = sourcemap::parse(r#"{"name":"NestedInit","className":"ModuleScript","filePaths":["/p/src/init.luau","/p/default.project.json"],"children":[{"name":"lib","className":"Folder","children":[{"name":"Foo","className":"ModuleScript","filePaths":["/p/src/lib/Foo/init.luau"],"children":[{"name":"Bar","className":"ModuleScript","filePaths":["/p/src/lib/Foo/Bar.luau"]}]}]},{"name":"pkg","className":"Folder","children":[{"name":"Logger","className":"ModuleScript","filePaths":["/p/src/pkg/Logger.luau"]}]}]}"#).unwrap();
        let idx = sourcemap::index(&sm, Some(Path::new("/p/src")), &|p| {
            p == Path::new("/p/src/init.luau")
        });
        let aliases = BTreeMap::from([
            ("pkg".to_string(), "./src/pkg".to_string()),
            ("lib".to_string(), "./src/lib/".to_string()),
            ("foo".to_string(), "src/./lib/Foo".to_string()),
            ("root".to_string(), "./src".to_string()),
            ("gone".to_string(), "./src/nope".to_string()),
            ("up".to_string(), "../p/src/pkg/Logger.luau".to_string()),
        ]);
        let got = resolve_aliases(
            &aliases,
            Path::new("/p"),
            &idx,
            &PathBuf::from("/p/.luaurc"),
        );
        assert_eq!(
            got,
            BTreeMap::from([
                ("foo".to_string(), v(&["NestedInit", "lib", "Foo"])),
                ("lib".to_string(), v(&["NestedInit", "lib"])),
                ("pkg".to_string(), v(&["NestedInit", "pkg"])),
                ("root".to_string(), v(&["NestedInit"])),
                ("up".to_string(), v(&["NestedInit", "pkg", "Logger"])),
            ])
        );
    }
}
