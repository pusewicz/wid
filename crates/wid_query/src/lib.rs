//! The introspection engine behind `wid query` and `wid lsp`: it answers
//! questions about a checked package as plain data.
//!
//! An [`Analysis`] is a loaded and checked program: its sources, its
//! diagnostics, the symbol index the checker builds ([`wid_sema::index`])
//! and the [`Extents`] of its declarations. [`Analysis::check`] makes one
//! from a loaded program; `wid_driver::analyze` loads a package (from disk,
//! or from the LSP's unsaved buffers) and calls it. Both are pure functions
//! of their inputs, so a long-lived caller such as the LSP reruns them
//! whenever a file changes.
//!
//! A [`Query`] is answered by [`answer`], or directly by [`outline`],
//! [`def`], [`methods`], [`refs`], [`calls`] and [`type_at`]. Answers are
//! [`Item`]s, the model `wid doc` renders too, or the uses and types the
//! checker recorded ([`RefItem`], [`TypeItem`]), and [`json::document`]
//! turns one into the stable JSON document `wid query` prints (SPEC
//! "Toolchain and CLI"). Nothing here prints, exits or reads files; a
//! request that fails returns a [`Failure`], which the caller turns into a
//! diagnostic.
//!
//! `outline`, `def` and `methods` read the symbol index. `refs`, `calls`
//! and `type` read what the checker resolved inside bodies and types, the
//! [`Uses`] kept next to the index: for every name, call and written type,
//! what it refers to, and for every expression and binding, its type.

pub mod complete;
mod extent;
pub mod item;
pub mod json;
mod uses;

use wid_diagnostics::{Diagnostics, SourceMap, did_you_mean};
use wid_sema::index::{Index, PathError, SymbolKind, Target};
use wid_sema::uses::Uses;
use wid_sema::{PackageId, ProgramInput};

pub use extent::Extents;
pub use item::{Item, ItemBuilder, Location, OriginInfo, PackageInfo, Style};
pub use uses::{
    Nearby, Position, PositionError, RefItem, TypeItem, calls, declaration_at, find_file, ref_at, refs, target_name,
    type_at, type_at_offset, uses_of, written,
};

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
    /// What the names and expressions in the code the checker lowered
    /// refer to, and their types.
    pub uses: Uses,
}

impl Analysis {
    /// Checks a loaded program and indexes its declarations, never
    /// generating code. `sources` and `diags` are what loading made.
    pub fn check(input: &ProgramInput, mut sources: SourceMap, mut diags: Diagnostics) -> Analysis {
        let (mut program, sema_diags, index, uses) = wid_sema::check_program_indexed(input);
        // Spans in code macros generated name these expansions.
        sources.set_expansions(std::mem::take(&mut program.expansions));
        diags.extend(sema_diags);
        diags.sort();
        Analysis { sources, diags, index, extents: Extents::of_program(input), uses }
    }

    /// A program that couldn't be loaded: its diagnostics, and nothing to
    /// query.
    pub fn unloaded(sources: SourceMap, mut diags: Diagnostics) -> Analysis {
        diags.sort();
        Analysis { sources, diags, index: Index::default(), extents: Extents::default(), uses: Uses::default() }
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
    /// `refs <sym>`: every use of what a symbol path names.
    Refs(String),
    /// `calls <sym>`: the uses of a symbol path that call it.
    Calls(String),
    /// `type <file:line:col>`: what is at a position, and its type.
    Type(String),
}

/// The queries, as `wid query` names them.
pub const QUERIES: &[&str] = &["outline", "def", "methods", "refs", "calls", "type"];

/// The queries with their arguments, for messages.
const SHAPES: &str =
    "`outline`, `def <symbol>`, `methods <Type>`, `refs <symbol>`, `calls <symbol>` or `type <file:line:column>`";

impl Query {
    /// Reads `wid query`'s positional arguments: the query and its
    /// argument. The error is a usage message.
    pub fn parse(args: &[String]) -> Result<Query, String> {
        let Some((name, rest)) = args.split_first() else {
            return Err(format!("`wid query` needs a query: {SHAPES}"));
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
            "refs" => arg("a symbol path", "Ball.update").map(Query::Refs),
            "calls" => arg("a symbol path", "Ball.update").map(Query::Calls),
            "type" => arg("a position", "main.wid:12:5").map(Query::Type),
            other => Err(match did_you_mean(other, QUERIES.iter().copied()) {
                Some(best) => format!("unknown query `{other}`; did you mean `{best}`?"),
                None => format!("unknown query `{other}`; the queries are {SHAPES}"),
            }),
        }
    }

    /// The query's name: `outline`, `def`, `methods`, `refs`, `calls`,
    /// `type`.
    pub fn name(&self) -> &'static str {
        match self {
            Query::Outline => "outline",
            Query::Def(_) => "def",
            Query::Methods(_) => "methods",
            Query::Refs(_) => "refs",
            Query::Calls(_) => "calls",
            Query::Type(_) => "type",
        }
    }

    /// Its argument: the symbol path it asks about, or for `type`, the
    /// position.
    pub fn symbol(&self) -> Option<&str> {
        match self {
            Query::Outline => None,
            Query::Def(s) | Query::Methods(s) | Query::Refs(s) | Query::Calls(s) | Query::Type(s) => Some(s),
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
    /// The uses of a symbol (`refs`), or its calls (`calls`), sorted by
    /// file, line and column.
    Refs(Vec<RefItem>),
    /// What is at a position.
    Type(Box<TypeItem>),
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
    /// `type` was given a position where nothing is, or no position.
    Position(PositionError),
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
        Query::Refs(path) => Answer::Refs(refs(analysis, pkg, path)?),
        Query::Calls(path) => Answer::Refs(calls(analysis, pkg, path)?),
        Query::Type(position) => Answer::Type(Box::new(type_at(analysis, pkg, position)?)),
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
            struct_literals: Vec::new(),
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
        assert_eq!(Query::parse(&args("refs Ball.pos")), Ok(Query::Refs("Ball.pos".into())));
        assert_eq!(Query::parse(&args("calls clamp")), Ok(Query::Calls("clamp".into())));
        assert_eq!(Query::parse(&args("type main.wid:3:5")), Ok(Query::Type("main.wid:3:5".into())));
        for (line, message) in [
            ("", "needs a query"),
            ("defs Ball", "did you mean `def`?"),
            ("reff Ball", "did you mean `refs`?"),
            ("type", "needs a position, like `wid query type main.wid:12:5`"),
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

    const PROGRAM: &str = "\
MAX = 3

struct Ball
  pos: Int

  def move(by: Int) -> Int
    @pos += by
    @pos
  end
end

def larger(a: $T, b: $T) -> T = a > b ? a : b

def main
  ball = Ball.new(pos: MAX)
  ball.move(1)
  ball.pos = larger(1, 2)
  p larger(1.5, 2.5)
end
";

    /// The uses of a symbol path as `kind line:column`, in order.
    fn uses_of(a: &Analysis, path: &str) -> Vec<String> {
        let found = super::refs(a, ROOT, path).expect("resolves");
        found.iter().map(|r| format!("{} {}:{}", r.kind.as_str(), r.location.line, r.location.column)).collect()
    }

    #[test]
    fn refs_list_every_use_once() {
        let a = analysis(PROGRAM);
        assert_eq!(uses_of(&a, "Ball.pos"), ["declaration 4:3", "write 7:5", "read 8:5", "write 15:19", "write 17:8"]);
        assert_eq!(uses_of(&a, "Ball"), ["declaration 3:8", "type 15:10"]);
        assert_eq!(uses_of(&a, "MAX"), ["declaration 1:1", "read 15:24"]);
        // A generic method is checked once per instance; its calls count once.
        assert_eq!(uses_of(&a, "larger"), ["declaration 12:5", "call 17:14", "call 18:5"]);
        assert_eq!(uses_of(&a, "Ball.move"), ["declaration 6:7", "call 16:8"]);
        let calls = super::calls(&a, ROOT, "larger").expect("resolves");
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|c| c.context.as_deref() == Some("main")));
        let found = super::refs(&a, ROOT, "Ball.pos").expect("resolves");
        let contexts: Vec<Option<&str>> = found.iter().map(|r| r.context.as_deref()).collect();
        assert_eq!(contexts, [Some("Ball"), Some("Ball.move"), Some("Ball.move"), Some("main"), Some("main")]);
        assert!(matches!(super::refs(&a, ROOT, "Bal"), Err(Failure::Path(_))));
    }

    #[test]
    fn type_finds_the_innermost_thing_at_a_position() {
        let a = analysis(PROGRAM);
        let at = |pos: &str| super::type_at(&a, ROOT, pos).expect(pos);
        let local = at("main.wid:15:3");
        assert_eq!((local.kind, local.ty.as_deref()), ("local", Some("Ball")));
        assert_eq!(local.refers_to.as_ref().map(|i| i.kind), Some("local"));
        let call = at("main.wid:16:8");
        assert_eq!((call.kind, call.ty.as_deref()), ("call", Some("Int")));
        assert_eq!((call.location.column, call.span.column, call.span.end_column), (8, 3, 15));
        assert_eq!(call.refers_to.as_ref().map(|i| i.path.as_str()), Some("Ball.move"));
        let field = at("main.wid:17:9");
        assert_eq!((field.kind, field.ty.as_deref()), ("field", Some("Int")));
        let param = at("main.wid:12:33");
        assert_eq!((param.kind, param.ty.as_deref()), ("parameter", Some("Int")));
        assert_eq!(param.instances, ["Int", "F64"]);
        assert_eq!(param.refers_to.as_ref().map(|i| i.signature.as_str()), Some("a: $T"));
        let written = at("main.wid:6:16");
        assert_eq!((written.kind, written.ty.as_deref()), ("type", Some("Int")));
        let declared = at("main.wid:6:8");
        assert_eq!((declared.kind, declared.ty.as_deref()), ("declaration", Some("proc(Int) -> Int")));
        let constant = at("main.wid:15:25");
        assert_eq!((constant.kind, constant.ty.as_deref()), ("expression", Some("Int")));
        assert_eq!(constant.refers_to.as_ref().map(|i| i.kind), Some("constant"));
    }

    #[test]
    fn type_explains_positions_it_cannot_answer() {
        use super::PositionError as E;
        let a = analysis(PROGRAM);
        let fail = |pos: &str| match super::type_at(&a, ROOT, pos) {
            Err(Failure::Position(e)) => e,
            other => panic!("{pos}: {other:?}"),
        };
        assert_eq!(fail("main.wid"), E::Malformed);
        assert_eq!(fail("main.wid:0:1"), E::Malformed);
        assert!(matches!(fail("mian.wid:1:1"), E::UnknownFile { candidates } if candidates == ["main.wid"]));
        assert_eq!(fail("main.wid:99:1"), E::NoLine { lines: 19 });
        assert_eq!(fail("main.wid:1:20"), E::NoColumn { last: 8 });
        let E::Nothing { nearest } = fail("main.wid:2:1") else { panic!("nothing on a blank line") };
        assert_eq!(nearest.iter().map(|n| (n.line, n.column)).collect::<Vec<_>>(), [(1, 1), (3, 8), (4, 3)]);
        assert_eq!(nearest[0].text, "MAX");
        assert_eq!(super::Position::parse("a:b.wid:3:4").map(|p| p.file), Some("a:b.wid".to_string()));
    }
}
