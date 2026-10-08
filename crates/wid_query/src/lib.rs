//! The introspection engine behind `wid query` and, later, `wid lsp`: it
//! answers questions about a checked package as plain data.
//!
//! An [`Analysis`] is a loaded and checked program: its sources, its
//! diagnostics, the symbol index the checker builds ([`wid_sema::index`])
//! and the [`Extents`] of its declarations. [`Analysis::check`] makes one
//! from a loaded program; `wid_driver::analyze` loads a package from disk
//! and calls it. Both are pure functions of their inputs, so a long-lived
//! caller such as the LSP reruns them whenever a file changes.
//!
//! A [`Query`] is answered by [`answer`], or directly by [`outline`],
//! [`def`] and [`methods`]. Answers are [`Item`]s, the model `wid doc`
//! renders too, and [`json::document`] turns one into the stable JSON
//! document `wid query` prints (SPEC "Toolchain and CLI"). Nothing here
//! prints, exits or reads files; a request that fails returns a
//! [`Failure`], which the caller turns into a diagnostic.
//!
//! `refs` and `type` come next. They need what the checker resolves inside
//! bodies, which the index doesn't hold: a table, recorded by the checker
//! and kept in the [`Analysis`] next to the index, from the span of every
//! name, call and expression to the declaration it resolves to and its
//! type. They will be two more [`Query`] and [`Answer`] variants that read
//! it.

mod extent;
pub mod item;
pub mod json;

use wid_diagnostics::{Diagnostics, SourceMap, did_you_mean};
use wid_sema::index::{Index, PathError, SymbolKind, Target};
use wid_sema::{PackageId, ProgramInput};

pub use extent::Extents;
pub use item::{Item, ItemBuilder, Location, OriginInfo, PackageInfo, Style};

/// A loaded and checked program, ready for queries.
#[derive(Debug)]
pub struct Analysis {
    /// Every file read, with the macro expansions of the check.
    pub sources: SourceMap,
    /// The diagnostics of loading and checking, sorted.
    pub diags: Diagnostics,
    /// Every declaration the checker collected; empty when loading failed.
    pub index: Index,
    /// Where those declarations are, whole.
    pub extents: Extents,
}

impl Analysis {
    /// Checks a loaded program and indexes its declarations, never
    /// generating code. `sources` and `diags` are what loading made.
    pub fn check(input: &ProgramInput, mut sources: SourceMap, mut diags: Diagnostics) -> Analysis {
        let (mut program, sema_diags, index) = wid_sema::check_program_indexed(input);
        // Spans in code macros generated name these expansions.
        sources.set_expansions(std::mem::take(&mut program.expansions));
        diags.extend(sema_diags);
        diags.sort();
        Analysis { sources, diags, index, extents: Extents::of_program(input) }
    }

    /// A program that couldn't be loaded: its diagnostics, and nothing to
    /// query.
    pub fn unloaded(sources: SourceMap, mut diags: Diagnostics) -> Analysis {
        diags.sort();
        Analysis { sources, diags, index: Index::default(), extents: Extents::default() }
    }

    /// The package that was asked for, unless loading failed.
    pub fn root(&self) -> Option<PackageId> {
        (!self.index.packages.is_empty()).then_some(PackageId(0))
    }

    /// Builds items from this analysis; with `private`, types list their
    /// private members too.
    pub fn items(&self, private: bool) -> ItemBuilder<'_> {
        ItemBuilder { index: &self.index, sources: &self.sources, extents: &self.extents, private }
    }
}

/// A question `wid query` answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    /// `outline`: every declaration of the package, members under their
    /// type.
    Outline,
    /// `def <sym>`: the declarations a symbol path names.
    Def(String),
    /// `methods <Type>`: every method callable on a type, by origin.
    Methods(String),
}

/// The queries, as `wid query` names them.
pub const QUERIES: &[&str] = &["outline", "def", "methods"];

/// Queries SPEC.md lists that aren't implemented yet.
const PLANNED: &[&str] = &["refs", "type"];

impl Query {
    /// Reads `wid query`'s positional arguments: the query and its
    /// argument. The error is a usage message.
    pub fn parse(args: &[String]) -> Result<Query, String> {
        let Some((name, rest)) = args.split_first() else {
            return Err("`wid query` needs a query: `outline`, `def <symbol>` or `methods <Type>`".to_string());
        };
        let arg = |what: &str, example: &str| match rest {
            [] => Err(format!("`wid query {name}` needs {what}, like `wid query {name} {example}`")),
            [one] => Ok(one.clone()),
            [_, extra, ..] => Err(format!(
                "unexpected argument `{extra}`; `wid query {name}` takes {what}, like `wid query {name} {example}`"
            )),
        };
        match name.as_str() {
            "outline" => match rest {
                [] => Ok(Query::Outline),
                [extra, ..] => Err(format!(
                    "unexpected argument `{extra}`; `wid query outline` takes no arguments (name a package with `-in:`, like `wid query outline -in:{extra}`)"
                )),
            },
            "def" => arg("a symbol path", "Ball.update").map(Query::Def),
            "methods" => arg("a type", "Ball").map(Query::Methods),
            planned if PLANNED.contains(&planned) => Err(format!(
                "`wid query {planned}` isn't implemented yet; the queries so far are `outline`, `def` and `methods`"
            )),
            other => {
                let all: Vec<&str> = QUERIES.iter().chain(PLANNED).copied().collect();
                Err(match did_you_mean(other, all) {
                    Some(best) => format!("unknown query `{other}`; did you mean `{best}`?"),
                    None => format!("unknown query `{other}`; the queries are `outline`, `def` and `methods`"),
                })
            }
        }
    }

    /// The query's name: `outline`, `def`, `methods`.
    pub fn name(&self) -> &'static str {
        match self {
            Query::Outline => "outline",
            Query::Def(_) => "def",
            Query::Methods(_) => "methods",
        }
    }

    /// The symbol path it asks about, if it takes one.
    pub fn symbol(&self) -> Option<&str> {
        match self {
            Query::Outline => None,
            Query::Def(s) | Query::Methods(s) => Some(s),
        }
    }
}

/// What a query found.
#[derive(Clone, Debug)]
pub enum Answer {
    /// The package's declarations in source order, each with what it
    /// declares itself (fields, enum members, methods) in its lists.
    Outline(Vec<Item>),
    /// The declaration a symbol path names; for an overload set, the set
    /// and then each of its members.
    Def(Vec<Item>),
    /// The methods and overload sets callable on a type, by origin.
    Methods(Vec<MethodGroup>),
}

/// Methods a type answers to that come from the same place.
#[derive(Clone, Debug)]
pub struct MethodGroup {
    /// Where they come from: the type itself, an `include`, an `extend` or
    /// a `using` field.
    pub origin: OriginInfo,
    /// The methods and overload sets, in lookup order.
    pub methods: Vec<Item>,
}

/// Why a query failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The symbol path has an empty segment (`Ball.`, `.x`, ``).
    Malformed,
    /// The symbol path doesn't resolve.
    Path(PathError),
    /// `methods` was asked of something that isn't a type: what the path
    /// named.
    NotAType(Target),
}

/// Resolves a symbol path written with dots (`Ball.update`,
/// `rl.draw_circle_v`) in `pkg`, through [`Index::resolve`].
pub fn resolve(index: &Index, pkg: PackageId, path: &str, private: bool) -> Result<Target, Failure> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(Failure::Malformed);
    }
    index.resolve(pkg, &segments, private).map_err(Failure::Path)
}

/// Answers a query about the package `pkg`.
pub fn answer(analysis: &Analysis, pkg: PackageId, query: &Query) -> Result<Answer, Failure> {
    Ok(match query {
        Query::Outline => Answer::Outline(outline(analysis, pkg)),
        Query::Def(path) => Answer::Def(def(analysis, pkg, path)?),
        Query::Methods(ty) => Answer::Methods(methods(analysis, pkg, ty)?),
    })
}

/// Every declaration of a package, private ones too, in source order. A
/// struct lists its fields, an enum its members, and a type, module or
/// extension the methods, constants and overload sets written in it.
pub fn outline(analysis: &Analysis, pkg: PackageId) -> Vec<Item> {
    let items = analysis.items(true);
    analysis.index.package(pkg).items.iter().map(|&id| items.overview_item(id)).collect()
}

/// The declarations a symbol path names in `pkg`, private ones too: one,
/// or for an overload set the set and then each of its members. An import
/// name gives a `package` item.
pub fn def(analysis: &Analysis, pkg: PackageId, path: &str) -> Result<Vec<Item>, Failure> {
    let target = resolve(&analysis.index, pkg, path, true)?;
    let items = analysis.items(true);
    let mut out = vec![items.target_item(&target, path)];
    if let Target::Symbol(id) = target {
        let s = analysis.index.symbol(id);
        if s.kind == SymbolKind::Overload {
            out.extend(s.links.iter().filter_map(|l| l.symbol).map(|m| items.full_item(m)));
        }
    }
    Ok(out)
}

/// The methods and overload sets callable on a type (or module), grouped
/// by origin in lookup order: its own, `include`d modules, `extend`
/// blocks and `using` fields. For a builtin type, the extensions'.
/// Private ones are included.
pub fn methods(analysis: &Analysis, pkg: PackageId, ty: &str) -> Result<Vec<MethodGroup>, Failure> {
    let index = &analysis.index;
    let target = resolve(index, pkg, ty, true)?;
    let groups = match &target {
        Target::Symbol(id) if index.symbol(*id).kind.has_members() => index.member_groups(index.alias_target(*id)),
        Target::Builtin(name) => index.builtin_groups(name),
        _ => return Err(Failure::NotAType(target)),
    };
    let items = analysis.items(true);
    Ok(groups
        .iter()
        .filter_map(|group| {
            let methods: Vec<Item> = group
                .members
                .iter()
                .filter(|&&m| matches!(index.symbol(m).kind, SymbolKind::Method | SymbolKind::Overload))
                .map(|&m| items.entry(m))
                .collect();
            (!methods.is_empty()).then(|| MethodGroup { origin: items.origin(&group.origin), methods })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use wid_diagnostics::{Diagnostics, SourceMap};
    use wid_sema::index::{PathErrorKind, Target};
    use wid_sema::{CheckOptions, FileInput, PackageId, PackageInput, ProgramInput};

    use super::{Analysis, Answer, Failure, Query, answer, def, methods, outline};

    const SRC: &str = "\
# Says things.
module Greeter
  def greet -> Int = 1
end

struct Entity
  hp: Int
  def move
  end
end

# The player.
#
# More about the player.
struct Player
  using base: Entity
  # The name.
  name: Int
  include Greeter

  # Heals.
  def heal(amount: Int)
  end

  private def secret
  end
end

extend Player
  def boost
  end
end

enum Dir
  north
  east = 4
end

def clamp_int(x: Int) -> Int = x
def clamp_both(x: Int, y: Int) -> Int = x
overload :clamp, :clamp_int, :clamp_both

extend Int
  def twice -> Int = self * 2
end

private LIMIT = 3
";

    /// The analysis of a one-file library package.
    fn analysis(src: &str) -> Analysis {
        let mut sources = SourceMap::new();
        let file = sources.add("main.wid".into(), "main.wid".into(), src);
        let (ast, diags) = wid_syntax::parse_file(file, src);
        assert!(diags.is_empty(), "{diags:?}");
        let file = FileInput {
            ast,
            imports: HashMap::new(),
            text: Arc::from(src),
            display: "main.wid".into(),
            deferred: HashMap::new(),
        };
        let package = PackageInput {
            name: "main".into(),
            path: ".".into(),
            dir: ".".into(),
            files: vec![file],
            c_sources: Vec::new(),
            cimport: None,
        };
        let options = CheckOptions { library: true, ..CheckOptions::default() };
        let input = ProgramInput { packages: vec![package], options, prelude: None };
        let analysis = Analysis::check(&input, sources, Diagnostics::new());
        assert!(!analysis.diags.has_errors(), "{:?}", analysis.diags);
        analysis
    }

    const ROOT: PackageId = PackageId(0);

    #[test]
    fn outline_nests_members_and_marks_private_ones() {
        let a = analysis(SRC);
        let items = outline(&a, ROOT);
        let names: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "Greeter",
                "Entity",
                "Player",
                "extend Player",
                "Dir",
                "clamp_int",
                "clamp_both",
                "clamp",
                "extend Int",
                "LIMIT"
            ]
        );
        let player = &items[2];
        let fields: Vec<&str> = player.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fields, ["base", "name"]);
        let methods: Vec<(&str, bool)> = player.methods.iter().map(|m| (m.path.as_str(), m.private)).collect();
        assert_eq!(methods, [("Player.heal", false), ("Player.secret", true)]);
        assert!(items[9].private);
        let dir: Vec<&str> = items[4].members.iter().map(|m| m.signature.as_str()).collect();
        assert_eq!(dir, ["north", "east = 4"]);
        let heal = &player.methods[0];
        let (at, whole) = (heal.location.as_ref().expect("located"), heal.span.as_ref().expect("spanned"));
        assert_eq!((at.line, at.column, at.end_line, at.end_column), (22, 7, 22, 11));
        assert_eq!((whole.line, whole.column, whole.end_line, whole.end_column), (22, 3, 23, 6));
    }

    #[test]
    fn def_finds_every_kind_of_symbol() {
        let a = analysis(SRC);
        let one = |path: &str| {
            let items = def(&a, ROOT, path).expect("resolves");
            assert_eq!(items.len(), 1, "{path}");
            items.into_iter().next().expect("one item")
        };
        assert_eq!(one("Player").kind, "struct");
        assert_eq!(one("Player").doc.as_deref(), Some("The player.\n\nMore about the player."));
        assert_eq!(one("Player.greet").path, "Greeter.greet");
        assert!(one("Player.secret").private);
        let hp = one("Player.hp");
        assert_eq!((hp.kind, hp.promoted_into.as_deref()), ("field", Some("Player")));
        assert_eq!(one("Dir.east").signature, "east = 4");
        assert_eq!(one("Int").kind, "builtin_type");
        assert_eq!(one("Int").methods[0].name, "twice");
        let set = def(&a, ROOT, "clamp").expect("an overload set");
        let paths: Vec<(&str, &str)> = set.iter().map(|i| (i.kind, i.path.as_str())).collect();
        assert_eq!(paths, [("overload", "clamp"), ("method", "clamp_int"), ("method", "clamp_both")]);
    }

    #[test]
    fn methods_are_grouped_by_origin() {
        let a = analysis(SRC);
        let groups = methods(&a, ROOT, "Player").expect("a struct");
        let shape: Vec<(&str, Vec<&str>)> =
            groups.iter().map(|g| (g.origin.kind, g.methods.iter().map(|m| m.path.as_str()).collect())).collect();
        assert_eq!(
            shape,
            [
                ("own", vec!["Player.heal", "Player.secret"]),
                ("include", vec!["Greeter.greet"]),
                ("extend", vec!["boost"]),
                ("using", vec!["Entity.move"]),
            ]
        );
        assert_eq!(groups[1].origin.via, "include Greeter");
        let int = methods(&a, ROOT, "Int").expect("a builtin type");
        assert_eq!((int.len(), int[0].origin.via.as_str()), (1, "extend Int"));
        assert!(matches!(methods(&a, ROOT, "clamp_int"), Err(Failure::NotAType(Target::Symbol(_)))));
    }

    #[test]
    fn failures_say_what_went_wrong() {
        let a = analysis(SRC);
        assert_eq!(def(&a, ROOT, "Player.").map(|_| ()), Err(Failure::Malformed));
        let Err(Failure::Path(err)) = def(&a, ROOT, "Plyer") else { panic!("an unknown name") };
        let PathErrorKind::UnknownName { candidates, .. } = err.kind else { panic!("{err:?}") };
        assert!(candidates.iter().any(|c| c == "Player"));
        let Err(Failure::Path(err)) = methods(&a, ROOT, "Player.hepl") else { panic!("a missing member") };
        assert_eq!(err.segment, 1);
    }

    #[test]
    fn queries_parse_with_usage_errors() {
        let args = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(Query::parse(&args("outline")), Ok(Query::Outline));
        assert_eq!(Query::parse(&args("def Ball.update")), Ok(Query::Def("Ball.update".into())));
        assert_eq!(Query::parse(&args("methods Ball")), Ok(Query::Methods("Ball".into())));
        for (line, message) in [
            ("", "needs a query"),
            ("defs Ball", "did you mean `def`?"),
            ("refs Ball", "isn't implemented yet"),
            ("def", "needs a symbol path"),
            ("methods Ball extra", "unexpected argument `extra`"),
            ("outline shapes", "-in:shapes"),
        ] {
            let err = Query::parse(&args(line)).expect_err(line);
            assert!(err.contains(message), "{line}: {err}");
        }
    }

    #[test]
    fn answers_render_as_one_stable_document() {
        let a = analysis(SRC);
        let query = Query::Outline;
        let found = answer(&a, ROOT, &query).expect("an outline");
        assert!(matches!(found, Answer::Outline(_)));
        let package = a.items(true).package_info(ROOT);
        let doc = super::json::document(&query, Some(&package), Some(&found));
        assert_eq!(doc["query"], "outline");
        assert_eq!(doc["symbol"], serde_json::Value::Null);
        assert_eq!(doc["package"]["name"], "main");
        let player = &doc["results"][2];
        assert_eq!(player["summary"], "The player.");
        assert_eq!(player["children"][0]["kind"], "field");
        assert_eq!(player["children"][3]["private"], true);
        assert_eq!(player["span"]["end_line"], 27);
        let text = super::json::render(&doc);
        assert_eq!(text, super::json::render(&doc), "rendering is deterministic");
        let def = Query::Def("clamp".into());
        let found = answer(&a, ROOT, &def).expect("an overload set");
        let doc = super::json::document(&def, Some(&package), Some(&found));
        assert_eq!(doc["results"][0]["methods"][1]["name"], "clamp_both");
        assert_eq!(doc["results"][2]["span"]["line"], 40);
        let none = super::json::document(&def, None, None);
        assert_eq!((none["package"].is_null(), none["results"].as_array().map(Vec::len)), (true, Some(0)));
    }
}
