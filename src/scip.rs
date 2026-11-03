//! Reads a SCIP index and turns it into the facts the rules check.
//!
//! An indexer (`scip-go`, `scip-typescript`, …) resolves every name in the
//! project the way the compiler does. Two kinds of references matter here:
//!
//! - a reference to a *module* symbol (a Go package, a TypeScript file) stands
//!   on the lines that import or use that module, so it is a dependency;
//! - a reference to a *member* of a module (`db.Client`) is a use of a name.
//!
//! References to symbols that no document of the index defines point outside
//! the project: they are external modules.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use protobuf::Message;
use scip::types::{Index, Occurrence, PositionEncoding, occurrence::Typed_range};

use crate::check::SymbolRef;
use crate::lang::Target;
use crate::lang::registry::Manifest;
use crate::scan::Dep;

/// `SymbolRole::Definition` in the bitset of an occurrence.
const DEFINITION: i32 = 1;

/// One reference to a symbol in a document.
#[derive(Debug, Clone)]
pub struct Reference {
    /// 1-based line.
    pub line: usize,
    pub symbol: String,
    /// For a reference into an unnamed npm package (see `UNNAMED_NPM`) that
    /// stands on the string of an import: the module the string spells.
    pub spelled: Option<String>,
}

/// How scip-typescript begins a symbol defined under a `package.json` with no
/// `name`, such as the `{"type": "module"}` that marks `esm/` in a published
/// package: which package that is, only the import in the source still says.
const UNNAMED_NPM: &str = "scip-typescript npm . . ";

/// A source file of the index, with the paths relative to the project root.
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub path: String,
    pub references: Vec<Reference>,
    /// Symbols this file defines.
    pub definitions: Vec<String>,
    /// The types this file defines (classes, structs, interfaces, traits),
    /// by name and 1-based line.
    pub types: Vec<(String, usize)>,
}

/// The documents of one or more indexes, merged.
#[derive(Debug, Default)]
pub struct Documents(pub Vec<Document>);

/// Parses an index written by an indexer that ran in `base` (a directory
/// relative to the project `root`, `""` for the root itself). Documents outside
/// the project, such as generated files in a build cache, are dropped. The
/// source of a document is read only for a reference into an unnamed package.
pub fn parse(bytes: &[u8], root: &Path, base: &Path) -> Result<Vec<Document>> {
    let index = Index::parse_from_bytes(bytes).context("not a SCIP index")?;
    let prefix = crate::scan::slash(base);
    let mut documents = Vec::new();
    for document in &index.documents {
        // Indexers on Windows may write `\` separators.
        let normalized = document.relative_path.replace('\\', "/");
        let relative = normalized.as_str();
        if relative.starts_with("..")
            || relative.starts_with('/')
            || relative.as_bytes().get(1) == Some(&b':')
        {
            continue;
        }
        let path = match prefix.as_str() {
            "" => relative.to_string(),
            base => format!("{base}/{relative}"),
        };
        let encoding = document.position_encoding.enum_value_or_default();
        let mut source: Option<Vec<String>> = None;
        let mut parsed = Document {
            path,
            ..Document::default()
        };
        for occurrence in &document.occurrences {
            if occurrence.symbol.is_empty() || occurrence.symbol.starts_with("local ") {
                continue;
            }
            if occurrence.symbol_roles & DEFINITION != 0 {
                if let Some(parsed_symbol) = split(&occurrence.symbol)
                    && let Some((name, Kind::Type)) = parsed_symbol.descriptors.last()
                {
                    parsed.types.push((name.clone(), line_of(occurrence) + 1));
                }
                parsed.definitions.push(occurrence.symbol.clone());
            } else {
                let spelled = match occurrence.symbol.starts_with(UNNAMED_NPM) {
                    true => {
                        let lines = source.get_or_insert_with(|| {
                            std::fs::read_to_string(root.join(&parsed.path))
                                .unwrap_or_default()
                                .lines()
                                .map(str::to_string)
                                .collect()
                        });
                        spelled_module(lines, occurrence, encoding)
                            .filter(|module| installed(&root.join(&parsed.path), module))
                    }
                    false => None,
                };
                parsed.references.push(Reference {
                    line: line_of(occurrence) + 1,
                    symbol: occurrence.symbol.clone(),
                    spelled,
                });
            }
        }
        documents.push(parsed);
    }
    Ok(documents)
}

/// Whether the npm package of `module` is in a `node_modules` above `file`,
/// where TypeScript finds a package. A project without `name` in its
/// `package.json` is unnamed too, and an alias into it (`@/gen/api` through
/// `paths`) spells no package.
fn installed(file: &Path, module: &str) -> bool {
    let package = Manifest::Npm.package(module);
    file.ancestors()
        .skip(1)
        .any(|dir| dir.join("node_modules").join(&package).is_dir())
}

/// The package an import names when `occurrence` stands on its string
/// (`"hono"`, `'@scope/pkg/sub'`); `None` for a name, a relative path or a
/// range over more than one line.
fn spelled_module(
    lines: &[String],
    occurrence: &Occurrence,
    encoding: PositionEncoding,
) -> Option<String> {
    let (line, start, end) = match (&occurrence.typed_range, occurrence.range.as_slice()) {
        (Some(Typed_range::SingleLineRange(r)), _) => (r.line, r.start_character, r.end_character),
        (Some(_), _) => return None,
        (None, [line, start, end]) => (*line, *start, *end),
        (None, _) => return None,
    };
    let text = lines.get(usize::try_from(line).ok()?)?;
    // The byte offset of a column, counted in the code units of the index;
    // TypeScript counts UTF-16 ones when the index does not say.
    let offset = |column: i32| -> Option<usize> {
        let column = usize::try_from(column).ok()?;
        let mut units = 0;
        for (byte, c) in text.char_indices() {
            if units == column {
                return Some(byte);
            }
            units += match encoding {
                PositionEncoding::UTF8CodeUnitOffsetFromLineStart => c.len_utf8(),
                PositionEncoding::UTF32CodeUnitOffsetFromLineStart => 1,
                _ => c.len_utf16(),
            };
        }
        (units == column).then_some(text.len())
    };
    let quoted = text.get(offset(start)?..offset(end)?)?;
    let module = ['"', '\'', '`']
        .into_iter()
        .find_map(|quote| quoted.strip_prefix(quote)?.strip_suffix(quote))?;
    let bare = !module.is_empty()
        && !module.starts_with(['.', '/'])
        && !module.contains("${")
        && !module.contains(['"', '\'', '`']);
    bare.then(|| module.to_string())
}

/// The start line, 0-based, from the typed range or the deprecated one.
fn line_of(occurrence: &Occurrence) -> usize {
    let line = match &occurrence.typed_range {
        Some(Typed_range::SingleLineRange(range)) => range.line,
        Some(Typed_range::MultiLineRange(range)) => range.start_line,
        Some(_) | None => occurrence.range.first().copied().unwrap_or(0),
    };
    usize::try_from(line).unwrap_or(0)
}

/// How a descriptor of a symbol ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Namespace,
    /// A class, interface or other type: `Name#`.
    Type,
    Other,
}

/// A symbol split into the package that owns it and its descriptors.
#[derive(Debug)]
struct Parsed<'a> {
    scheme: &'a str,
    package: &'a str,
    descriptors: Vec<(String, Kind)>,
}

/// `scheme manager package version descriptors`.
fn split(symbol: &str) -> Option<Parsed<'_>> {
    let mut parts = symbol.splitn(5, ' ');
    let scheme = parts.next()?;
    let _manager = parts.next()?;
    let package = parts.next()?;
    let _version = parts.next()?;
    let descriptors = descriptors(parts.next()?)?;
    Some(Parsed {
        scheme,
        package,
        descriptors,
    })
}

/// The descriptors of a symbol: a name, quoted in backticks when it holds
/// anything but identifier characters, and a suffix (`/` namespace, `#` type,
/// `.` term, `().` method, `:` meta, `!` macro, `[T]` type parameter, `(p)` parameter).
fn descriptors(text: &str) -> Option<Vec<(String, Kind)>> {
    scan(text).map(|found| {
        found
            .into_iter()
            .map(|(name, kind, _)| (name, kind))
            .collect()
    })
}

/// The descriptors with the offset, in characters, at which each one ends.
fn scan(text: &str) -> Option<Vec<(String, Kind, usize)>> {
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    let mut found = Vec::new();
    while at < chars.len() {
        let mut name = String::new();
        if chars[at] == '`' {
            at += 1;
            loop {
                match chars.get(at)? {
                    '`' if chars.get(at + 1) == Some(&'`') => {
                        name.push('`');
                        at += 2;
                    }
                    '`' => {
                        at += 1;
                        break;
                    }
                    c => {
                        name.push(*c);
                        at += 1;
                    }
                }
            }
        } else if chars[at] == '[' || chars[at] == '(' {
            // A type or value parameter descriptor starts with its bracket.
        } else {
            while let Some(c) = chars.get(at)
                && (c.is_alphanumeric() || matches!(c, '_' | '+' | '-' | '$'))
            {
                name.push(*c);
                at += 1;
            }
        }
        let kind = match chars.get(at)? {
            '/' => {
                at += 1;
                Kind::Namespace
            }
            '#' => {
                at += 1;
                Kind::Type
            }
            '.' | ':' | '!' => {
                at += 1;
                Kind::Other
            }
            '(' => {
                // `name().` or `name(disambiguator).`; a parameter is `(name)`.
                let close = chars[at..].iter().position(|c| *c == ')')? + at;
                at = close + 1;
                if chars.get(at) == Some(&'.') {
                    at += 1;
                }
                Kind::Other
            }
            '[' => {
                let close = chars[at..].iter().position(|c| *c == ']')? + at;
                if name.is_empty() {
                    name = chars[at + 1..close].iter().collect();
                }
                at = close + 1;
                Kind::Other
            }
            _ => return None,
        };
        found.push((name, kind, at));
    }
    Some(found)
}

/// `symbol` cut after its first type descriptor: the class that a member such as a
/// method or a constructor belongs to. The symbol itself when there is no type.
fn class_symbol(symbol: &str) -> String {
    let mut parts = symbol.splitn(5, ' ');
    let header: Vec<&str> = parts.by_ref().take(4).collect();
    let Some(text) = parts.next() else {
        return symbol.to_string();
    };
    let Some(found) = scan(text) else {
        return symbol.to_string();
    };
    let chars: Vec<char> = text.chars().collect();
    let kept: String = if header.first() == Some(&"scip-ruby") {
        // Ruby nests its classes in modules, which are types too (`Shop#Web#Order#run().`):
        // the class is the last of the leading types. A class method belongs to the
        // singleton class (`Shop#Web#`<Class:Order>`#find().`), which stands for the class.
        let leading = found
            .iter()
            .take_while(|(_, kind, _)| *kind == Kind::Type)
            .count();
        let Some((name, _, end)) = leading.checked_sub(1).map(|last| &found[last]) else {
            return symbol.to_string();
        };
        match name
            .strip_prefix("<Class:")
            .and_then(|n| n.strip_suffix('>'))
        {
            Some(class) => {
                let start = leading.checked_sub(2).map_or(0, |before| found[before].2);
                format!("{}{class}#", chars[..start].iter().collect::<String>())
            }
            None => chars[..*end].iter().collect(),
        }
    } else {
        // Elsewhere the class is the first type.
        match found.iter().find(|(_, kind, _)| *kind == Kind::Type) {
            Some((_, _, end)) => chars[..*end].iter().collect(),
            None => return symbol.to_string(),
        }
    };
    format!("{} {kept}", header.join(" "))
}

/// What a reference points at.
enum Referent {
    /// A whole module: the package or file that an import names.
    Module,
    /// A name inside a module.
    Member(String),
    /// A package or namespace named on its own, which is not a dependency.
    Package,
}

impl Parsed<'_> {
    fn referent(&self) -> Referent {
        // A Python module is the term `__init__` of its namespace.
        if self.scheme == "scip-python"
            && matches!(self.descriptors.as_slice(), [(_, Kind::Namespace), (name, _)] if name == "__init__")
        {
            return Referent::Module;
        }
        // Ruby modules and classes are both types: a class is what a constant names.
        if self.scheme == "scip-ruby" {
            return match self.descriptors.iter().all(|(_, kind)| *kind == Kind::Type) {
                true => Referent::Module,
                false => Referent::Package,
            };
        }
        // PHP, Java and C# have no module symbols: a class is what an import names.
        let class_based = matches!(self.scheme, "scip-php" | "scip-java" | "scip-dotnet");
        if class_based
            && self
                .descriptors
                .iter()
                .all(|(_, kind)| *kind == Kind::Namespace)
        {
            return Referent::Package;
        }
        if class_based
            && matches!(self.descriptors.last(), Some((_, Kind::Type)))
            && self.descriptors[..self.descriptors.len() - 1]
                .iter()
                .all(|(_, kind)| *kind == Kind::Namespace)
        {
            return Referent::Module;
        }
        match self
            .descriptors
            .iter()
            .find(|(_, kind)| *kind != Kind::Namespace)
        {
            Some((name, _)) => Referent::Member(name.clone()),
            None => Referent::Module,
        }
    }

    /// The namespace descriptors joined: the import path of a Go package, the
    /// dotted name of a Python module, the path of a Rust module.
    fn namespace(&self, separator: &str) -> String {
        self.descriptors
            .iter()
            .take_while(|(_, kind)| *kind == Kind::Namespace)
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(separator)
    }

    /// All the descriptor names joined: the fully qualified name of a class.
    fn qualified(&self, separator: &str) -> String {
        self.descriptors
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(separator)
    }

    /// The package that owns the symbol, with the scheme: what tells the
    /// project's own symbols from those of its dependencies.
    fn owner(&self) -> String {
        format!("{} {}", self.scheme, self.package)
    }

    /// The external module the symbol belongs to, in the spelling of the
    /// language's import statements and of `forbid_external`.
    fn external_module(&self) -> String {
        match self.scheme {
            // Go: the package path (`fmt`, `net/http`, `github.com/lib/pq`).
            "scip-go" => self.namespace("/"),
            // PHP: the class by its qualified name (`Illuminate\\Support\\Str`).
            "scip-php" => self.qualified("\\"),
            // Java and C#: the class by its qualified name (`java.util.List`).
            "scip-java" | "scip-dotnet" => self.qualified("."),
            // Ruby: the class by its qualified name (`ActiveRecord::Base`).
            "scip-ruby" => self.qualified("::"),
            // Python: the dotted module (`os.path`, `requests`).
            "scip-python" => self.namespace("."),
            // Rust: the crate and the module path; the standard library is
            // spread over `core`, `alloc` and `std` but written `std::…`.
            "rust-analyzer" => {
                let krate = match self.package {
                    "core" | "alloc" | "std" => "std".to_string(),
                    other => other.replace('-', "_"),
                };
                std::iter::once(krate)
                    .chain(
                        self.descriptors
                            .iter()
                            .take_while(|(_, kind)| *kind == Kind::Namespace)
                            .map(|(name, _)| name.clone())
                            .filter(|name| name != "crate"),
                    )
                    .collect::<Vec<_>>()
                    .join("::")
            }
            // TypeScript: the npm package and the file inside it that was imported
            // (`lodash/fp`, `moment/locale/ru`); `@types/x` stands for `x`.
            _ => {
                let mut parts: Vec<String> = self
                    .descriptors
                    .iter()
                    .take_while(|(_, kind)| *kind == Kind::Namespace)
                    .map(|(name, _)| name.clone())
                    .collect();
                if let Some(last) = parts.last_mut() {
                    for suffix in [".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", ".js", ".mjs"] {
                        if let Some(stem) = last.strip_suffix(suffix) {
                            *last = stem.to_string();
                            break;
                        }
                    }
                    if last == "index" {
                        parts.pop();
                    }
                }
                std::iter::once(npm_name(self.package))
                    .chain(parts)
                    .collect::<Vec<_>>()
                    .join("/")
            }
        }
    }

    /// The qualifier that `forbid_symbol` names: what the code calls the module.
    fn qualifier(&self, internal: Option<&str>) -> String {
        match (self.scheme, internal) {
            ("scip-go", _) => self.namespace("/"),
            ("scip-python", _) => self.namespace("."),
            ("scip-php", _) => self.namespace("\\"),
            ("scip-ruby", _) => self.qualified("::"),
            ("scip-java" | "scip-dotnet", _) => self.namespace("."),
            ("rust-analyzer", _) => self.external_module(),
            (_, Some(path)) => path
                .rsplit_once('.')
                .map_or(path, |(stem, _)| stem)
                .to_string(),
            (_, None) => npm_name(self.package),
        }
    }
}

/// `@types/lodash` is `lodash`, `@types/scope__name` is `@scope/name`.
fn npm_name(package: &str) -> String {
    match package.strip_prefix("@types/") {
        Some(rest) => match rest.split_once("__") {
            Some((scope, name)) => format!("@{scope}/{name}"),
            None => rest.to_string(),
        },
        None => package.to_string(),
    }
}

/// The use a Ruby reference makes: of a constant (`Shop::Db::Client` is `Shop::Db.Client`)
/// or of a method, which is also a use of its class (`Shop::Db::Client.query`). Of the
/// constants a line spells out only the deepest one counts, as for dependencies.
fn ruby_use(
    reference: &Reference,
    path: &str,
    defined: &HashMap<&str, &str>,
    reopened: &HashSet<&str>,
    mine: &HashSet<&str>,
    shadowed: impl Fn(&Parsed, usize) -> bool,
) -> Option<SymbolRef> {
    let class = class_symbol(&reference.symbol);
    let parsed = split(&class)?;
    let names: Vec<&str> = parsed.descriptors.iter().map(|(n, _)| n.as_str()).collect();
    let (name, outer) = names.split_last()?;
    if !parsed
        .descriptors
        .iter()
        .all(|(_, kind)| *kind == Kind::Type)
    {
        return None;
    }
    let method = match class == reference.symbol {
        true => None,
        false => Some(
            split(&reference.symbol)?
                .descriptors
                .get(names.len())?
                .0
                .clone(),
        ),
    };
    if method.is_none() && shadowed(&parsed, reference.line) {
        return None;
    }
    // A module reopened in several files is no one file's.
    let home = |symbol: &str| {
        (!reopened.contains(symbol))
            .then(|| defined.get(symbol))
            .flatten()
    };
    let home = method
        .is_some()
        .then(|| home(&reference.symbol))
        .flatten()
        .or_else(|| home(&class));
    // A constant that this file opens itself (`Shop` around its classes) is no use of it.
    if home == Some(&path) || (method.is_none() && mine.contains(class.as_str())) {
        return None;
    }
    let qualified = names.join("::");
    let target = match home {
        Some(file) => Some(Target::Internal(PathBuf::from(file))),
        None if defined.contains_key(class.as_str()) => None,
        None => Some(Target::External(qualified.clone())),
    };
    Some(SymbolRef {
        file: PathBuf::from(path),
        line: reference.line,
        specifier: outer.join("::"),
        name: name.to_string(),
        method: method.map(|method| (qualified, method)),
        target,
    })
}

/// Dependencies and symbol uses of the documents.
pub struct Facts {
    pub deps: Vec<Dep>,
    pub symbols: Vec<SymbolRef>,
    /// With `symbols`, the files that use a name of an unnamed package, whose
    /// uses are left out of `symbols`.
    pub unnamed: BTreeSet<String>,
}

/// One `Dep` per file and module it refers to, at the first line that does:
/// the import. With `symbols`, also one `SymbolRef` per use of a name.
///
/// `excluded` are documents of files that `exclude` hides: never read as
/// sources, they only resolve the names that no document of `documents` defines.
pub fn facts(documents: &[Document], excluded: &[Document], symbols: bool) -> Facts {
    let mut defined: HashMap<&str, &str> = HashMap::new();
    // Symbols that more than one file defines: a Ruby module reopened in each file under it.
    let mut reopened: HashSet<&str> = HashSet::new();
    for document in documents {
        for symbol in &document.definitions {
            let entry = defined.entry(symbol).or_insert(&document.path);
            if *entry != document.path.as_str() {
                reopened.insert(symbol);
            }
            if document.path.as_str() < *entry {
                *entry = &document.path;
            }
        }
    }
    let mut hidden: HashMap<&str, &str> = HashMap::new();
    for document in excluded {
        for symbol in &document.definitions {
            if !defined.contains_key(symbol.as_str()) {
                let entry = hidden.entry(symbol).or_insert(&document.path);
                if document.path.as_str() < *entry {
                    *entry = &document.path;
                }
            }
        }
    }
    // Packages that define something are the project's own: a reference into
    // one that no definition matches (a relative import in Python) is not external.
    let own: BTreeSet<String> = defined
        .keys()
        .chain(hidden.keys())
        .filter_map(|symbol| split(symbol).map(|parsed| parsed.owner()))
        .collect();
    // Python names a module by its path from the working directory in its
    // definition (`src.shop.infra`) but as imported in a reference (`shop.infra`,
    // or just `views` for a relative import), so match those by their ending.
    let modules_of = |defined: &HashMap<&str, &'_ str>| -> Vec<(String, String)> {
        defined
            .iter()
            .filter_map(|(symbol, path)| {
                let parsed = split(symbol)?;
                (parsed.scheme == "scip-python" && matches!(parsed.referent(), Referent::Module))
                    .then(|| (parsed.namespace("."), path.to_string()))
            })
            .collect()
    };
    let python_modules = [modules_of(&defined), modules_of(&hidden)];
    // An excluded module is the answer only when no checked one matches.
    let python_module = |name: &str| -> Option<&str> {
        let ending = format!(".{name}");
        python_modules.iter().find_map(|modules| {
            let mut found = modules
                .iter()
                .filter(|(module, _)| module == name || module.ends_with(&ending))
                .map(|(_, path)| path.as_str());
            let first = found.next()?;
            found.all(|path| path == first).then_some(first)
        })
    };
    let mut deps = Vec::new();
    let mut uses = Vec::new();
    let mut unnamed = BTreeSet::new();
    for document in documents {
        // Rust writes `crate::domain::order::Order` as a reference to every module on
        // the path, and Ruby `Shop::Domain::Order` as one to every module and the class;
        // only the deepest one is what the line depends on.
        let mut paths: HashMap<usize, Vec<Vec<String>>> = HashMap::new();
        for reference in &document.references {
            if let Some(parsed) = split(&reference.symbol)
                && matches!(parsed.scheme, "rust-analyzer" | "scip-ruby")
                && matches!(parsed.referent(), Referent::Module)
            {
                paths
                    .entry(reference.line)
                    .or_default()
                    .push(parsed.descriptors.iter().map(|(n, _)| n.clone()).collect());
            }
        }
        let shadowed = |parsed: &Parsed, line: usize| {
            matches!(parsed.scheme, "rust-analyzer" | "scip-ruby")
                && paths.get(&line).is_some_and(|all| {
                    let mine: Vec<String> =
                        parsed.descriptors.iter().map(|(n, _)| n.clone()).collect();
                    all.iter().any(|other| {
                        other.len() > mine.len()
                            && (other.starts_with(&mine)
                                || (parsed.scheme == "rust-analyzer" && mine == ["crate"]))
                    })
                })
        };
        // (target, module) -> first line
        let mut first: BTreeMap<(Option<&str>, String), usize> = BTreeMap::new();
        let mine: HashSet<&str> = document.definitions.iter().map(String::as_str).collect();
        for reference in &document.references {
            if symbols && reference.symbol.starts_with("scip-ruby ") {
                uses.extend(ruby_use(
                    reference,
                    &document.path,
                    &defined,
                    &reopened,
                    &mine,
                    shadowed,
                ));
            }
            // For PHP, Java, C# and Ruby a member (a method, a constructor) is a use of its class.
            let symbol = if reference.symbol.starts_with("scip-php ")
                || reference.symbol.starts_with("scip-java ")
                || reference.symbol.starts_with("scip-dotnet ")
                || reference.symbol.starts_with("scip-ruby ")
            {
                class_symbol(&reference.symbol)
            } else {
                reference.symbol.clone()
            };
            let Some(parsed) = split(&symbol) else {
                continue;
            };
            let mut internal = defined
                .get(symbol.as_str())
                .or_else(|| hidden.get(symbol.as_str()))
                .copied();
            match parsed.referent() {
                Referent::Module => {
                    if shadowed(&parsed, reference.line) {
                        continue;
                    }
                    if internal.is_none() && parsed.scheme == "scip-python" {
                        internal = python_module(&parsed.namespace("."));
                    }
                    if internal == Some(document.path.as_str()) {
                        continue;
                    }
                    let unnamed = internal.is_none() && reference.symbol.starts_with(UNNAMED_NPM);
                    // C# gives its own types and the framework's the same header, so there
                    // an undefined type is always external.
                    if internal.is_none()
                        && !unnamed
                        && parsed.scheme != "scip-dotnet"
                        && own.contains(&parsed.owner())
                    {
                        continue;
                    }
                    // Only the string of the import names an unnamed package; a name
                    // bound to one of its modules (`z` of `import { z } from "zod"`)
                    // stands on a line whose string already did.
                    let module = match (internal, unnamed) {
                        (Some(_), _) => String::new(),
                        (None, true) => reference.spelled.clone().unwrap_or_default(),
                        (None, false) => parsed.external_module(),
                    };
                    if internal.is_none() && module.is_empty() {
                        continue;
                    }
                    let line = first.entry((internal, module)).or_insert(reference.line);
                    *line = (*line).min(reference.line);
                }
                Referent::Package => {}
                Referent::Member(name) => {
                    if !symbols {
                        continue;
                    }
                    let home = internal;
                    if home == Some(document.path.as_str()) {
                        continue;
                    }
                    // Which package an unnamed name belongs to the index does not say.
                    if home.is_none() && reference.symbol.starts_with(UNNAMED_NPM) {
                        unnamed.insert(document.path.clone());
                        continue;
                    }
                    let target = match home {
                        Some(path) => Target::Internal(PathBuf::from(path)),
                        None => Target::External(parsed.external_module()),
                    };
                    uses.push(SymbolRef {
                        file: PathBuf::from(&document.path),
                        line: reference.line,
                        specifier: parsed.qualifier(home),
                        name,
                        method: None,
                        target: Some(target),
                    });
                }
            }
        }
        for ((internal, module), line) in first {
            let (specifier, target) = match internal {
                Some(path) => (path.to_string(), Target::Internal(PathBuf::from(path))),
                None => (module.clone(), Target::External(module)),
            };
            deps.push(Dep {
                file: PathBuf::from(&document.path),
                line,
                specifier,
                target,
            });
        }
    }
    Facts {
        deps,
        symbols: uses,
        unnamed,
    }
}

impl Documents {
    pub fn merged(parts: Vec<Vec<Document>>) -> Result<Self> {
        let mut by_path: BTreeMap<String, Document> = BTreeMap::new();
        for document in parts.into_iter().flatten() {
            match by_path.get_mut(&document.path) {
                Some(seen) => {
                    seen.references.extend(document.references);
                    seen.definitions.extend(document.definitions);
                }
                None => {
                    by_path.insert(document.path.clone(), document);
                }
            }
        }
        if by_path.is_empty() {
            bail!("the indexers produced no documents");
        }
        Ok(Self(by_path.into_values().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str, defs: &[&str], refs: &[(usize, &str)]) -> Document {
        Document {
            path: path.to_string(),
            definitions: defs.iter().map(|s| s.to_string()).collect(),
            types: Vec::new(),
            references: refs
                .iter()
                .map(|(line, symbol)| Reference {
                    line: *line,
                    symbol: symbol.to_string(),
                    spelled: None,
                })
                .collect(),
        }
    }

    const DB: &str = "scip-go gomod example.com/app . `example.com/app/internal/db`/";
    const FMT: &str = "scip-go gomod github.com/golang/go/src go1.25 fmt/";

    #[test]
    fn ruby_depends_on_the_deepest_constant_of_a_line() {
        let order = "scip-ruby gem project 0 Shop#Domain#Order#";
        let docs = [
            doc("lib/order.rb", &[order], &[]),
            doc(
                "lib/web.rb",
                &[],
                &[
                    (3, "scip-ruby gem project 0 Shop#"),
                    (3, "scip-ruby gem project 0 Shop#Domain#"),
                    (3, order),
                    (3, "scip-ruby gem project 0 Class#new()."),
                    (4, "scip-ruby gem project 0 Shop#Domain#Order#`@id`."),
                ],
            ),
        ];
        let deps = facts(&docs, &[], false).deps;
        let found: Vec<_> = deps
            .iter()
            .map(|d| (d.line, d.specifier.as_str()))
            .collect();
        assert_eq!(found, [(3, "lib/order.rb")]);
    }

    #[test]
    fn a_ruby_class_method_is_a_use_of_its_class() {
        let order = "scip-ruby gem project 0 Shop#Domain#Order#";
        let find = "scip-ruby gem project 0 Shop#Domain#`<Class:Order>`#find().";
        let docs = [
            doc("lib/order.rb", &[order, find], &[]),
            doc("lib/web.rb", &[], &[(5, find)]),
        ];
        let deps = facts(&docs, &[], false).deps;
        let found: Vec<_> = deps
            .iter()
            .map(|d| (d.line, d.specifier.as_str()))
            .collect();
        assert_eq!(found, [(5, "lib/order.rb")]);
    }

    #[test]
    fn ruby_constants_and_methods_are_symbol_uses() {
        let ruby = "scip-ruby gem project 0 ";
        let client = format!("{ruby}Shop#Db#Client#");
        let query = format!("{ruby}Shop#Db#Client#query().");
        let connect = format!("{ruby}Shop#Db#`<Class:Client>`#connect().");
        let helper = format!("{ruby}Shop#`<Class:Db>`#helper().");
        let config = format!("{ruby}`<Class:Shop>`#config().");
        let docs = [
            doc(
                "lib/shop/db/client.rb",
                &[
                    &format!("{ruby}Shop#"),
                    &format!("{ruby}Shop#Db#"),
                    &client,
                    &query,
                    &connect,
                    &helper,
                ],
                &[],
            ),
            doc(
                "lib/shop/db/rollback.rb",
                &[&format!("{ruby}Shop#"), &format!("{ruby}Shop#Db#")],
                &[],
            ),
            doc("lib/shop.rb", &[&format!("{ruby}Shop#"), &config], &[]),
            doc(
                "lib/shop/web/controller.rb",
                &[&format!("{ruby}Shop#"), &format!("{ruby}Shop#Web#")],
                &[
                    (7, &format!("{ruby}Shop#")),
                    (7, &format!("{ruby}Shop#Db#")),
                    (7, &client),
                    (7, &connect),
                    (8, &query),
                    (9, &format!("{ruby}Kernel#raise().")),
                    (9, &format!("{ruby}ActiveRecord#")),
                    (9, &format!("{ruby}ActiveRecord#Rollback#")),
                    (12, &format!("{ruby}Shop#")),
                    (12, &format!("{ruby}Shop#Db#")),
                    (12, &helper),
                    (14, &config),
                ],
            ),
        ];
        let uses: Vec<_> = facts(&docs, &[], true)
            .symbols
            .into_iter()
            .map(|u| {
                let method = u.method.map(|(owner, name)| format!("{owner}.{name}"));
                (
                    u.line,
                    format!("{}.{}", u.specifier, u.name),
                    method,
                    u.target,
                )
            })
            .collect();
        let db = || Some(Target::Internal(PathBuf::from("lib/shop/db/client.rb")));
        let method = |m: &str| Some(m.to_string());
        assert_eq!(
            uses,
            [
                (7, "Shop::Db.Client".into(), None, db()),
                (
                    7,
                    "Shop::Db.Client".into(),
                    method("Shop::Db::Client.connect"),
                    db()
                ),
                (
                    8,
                    "Shop::Db.Client".into(),
                    method("Shop::Db::Client.query"),
                    db()
                ),
                (
                    9,
                    ".Kernel".into(),
                    method("Kernel.raise"),
                    Some(Target::External("Kernel".into()))
                ),
                (
                    9,
                    "ActiveRecord.Rollback".into(),
                    None,
                    Some(Target::External("ActiveRecord::Rollback".into()))
                ),
                // Shop::Db is reopened in both files of lib/shop/db: no one file owns it.
                (12, "Shop.Db".into(), None, None),
                (12, "Shop.Db".into(), method("Shop::Db.helper"), db()),
                // The file opens Shop too, but the method is defined elsewhere.
                (
                    14,
                    ".Shop".into(),
                    method("Shop.config"),
                    Some(Target::Internal(PathBuf::from("lib/shop.rb")))
                ),
            ]
        );
        assert!(facts(&docs, &[], false).symbols.is_empty());
    }

    #[test]
    fn a_type_is_the_last_descriptor_ending_in_a_hash() {
        let last = |symbol: &str| {
            split(symbol).and_then(|p| {
                p.descriptors
                    .last()
                    .map(|(n, k)| (n.clone(), matches!(k, Kind::Type)))
            })
        };
        assert_eq!(
            last("scip-go gomod ex.com/m . `ex.com/m/repo`/OrderRepository#"),
            Some(("OrderRepository".to_string(), true))
        );
        assert_eq!(
            last("semanticdb maven . . m/Outer#Inner#"),
            Some(("Inner".to_string(), true))
        );
        assert_eq!(
            last("scip-go gomod ex.com/m . `ex.com/m/repo`/OrderRepository#Find()."),
            Some(("Find".to_string(), false))
        );
    }

    #[test]
    fn parses_descriptors() {
        let d = descriptors("src/lib/`index.ts`/Client#Query().").unwrap();
        let names: Vec<_> = d.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["src", "lib", "index.ts", "Client", "Query"]);
        assert_eq!(d[2].1, Kind::Namespace);
        assert_eq!(d[3].1, Kind::Type);
        assert_eq!(d[4].1, Kind::Other);
        let go = split("scip-go gomod example.com/app . `example.com/app/internal/db`/Scoped().")
            .unwrap();
        assert_eq!(go.namespace("/"), "example.com/app/internal/db");
        assert!(matches!(go.referent(), Referent::Member(n) if n == "Scoped"));
        assert!(matches!(split(DB).unwrap().referent(), Referent::Module));
    }

    #[test]
    fn module_references_are_dependencies_at_their_first_line() {
        let documents = vec![
            doc("internal/db/db.go", &[DB, &format!("{DB}Client#")], &[]),
            doc(
                "internal/handlers/h.go",
                &[],
                &[
                    (12, DB),
                    (6, DB),
                    (6, FMT),
                    (9, &format!("{DB}Client#")),
                    (
                        9,
                        "scip-go gomod github.com/golang/go/src go1.25 fmt/Println().",
                    ),
                ],
            ),
        ];
        let facts = facts(&documents, &[], false);
        let got: Vec<_> = facts
            .deps
            .iter()
            .map(|d| (d.file.to_str().unwrap(), d.line, d.target.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("internal/handlers/h.go", 6, Target::External("fmt".into())),
                (
                    "internal/handlers/h.go",
                    6,
                    Target::Internal(PathBuf::from("internal/db/db.go"))
                ),
            ]
        );
    }

    #[test]
    fn excluded_documents_only_resolve_what_no_source_defines() {
        let generated = "scip-go gomod example.com/app . `example.com/app/gen`/";
        let sources = vec![
            doc("internal/db/db.go", &[DB], &[]),
            doc("internal/handlers/h.go", &[], &[(3, DB), (4, generated)]),
        ];
        // `a_gen` sorts first and also defines DB; it must not win.
        let excluded = vec![
            doc("a_gen/db.go", &[DB], &[(1, DB)]),
            doc("gen/gen.go", &[generated], &[]),
        ];
        let got: Vec<_> = facts(&sources, &excluded, false)
            .deps
            .iter()
            .map(|d| (d.file.to_str().unwrap().to_string(), d.target.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "internal/handlers/h.go".to_string(),
                    Target::Internal(PathBuf::from("gen/gen.go"))
                ),
                (
                    "internal/handlers/h.go".to_string(),
                    Target::Internal(PathBuf::from("internal/db/db.go"))
                ),
            ]
        );
    }

    #[test]
    fn member_references_are_symbol_uses() {
        let documents = vec![
            doc("internal/db/db.go", &[DB, &format!("{DB}Client#")], &[]),
            doc(
                "internal/handlers/h.go",
                &[],
                &[
                    (9, &format!("{DB}Client#")),
                    (10, &format!("{FMT}Println().")),
                ],
            ),
        ];
        let uses = facts(&documents, &[], true).symbols;
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].name, "Client");
        assert_eq!(uses[0].specifier, "example.com/app/internal/db");
        assert_eq!(
            uses[0].target,
            Some(Target::Internal(PathBuf::from("internal/db/db.go")))
        );
        assert_eq!(uses[1].target, Some(Target::External("fmt".into())));
        assert!(facts(&documents, &[], false).symbols.is_empty());
    }

    #[test]
    fn a_class_is_the_module_in_php() {
        let documents = vec![
            doc(
                "src/Domain/Order.php",
                &["scip-php composer __root__ 1.0 App/Domain/Order#"],
                &[],
            ),
            doc(
                "src/Web/C.php",
                &[],
                &[
                    (4, "scip-php composer __root__ 1.0 App/Domain/Order#"),
                    (
                        5,
                        "scip-php composer illuminate/support 10.0 Illuminate/Support/Str#",
                    ),
                    (7, "scip-php composer __root__ 1.0 App/Domain/Order#id()."),
                ],
            ),
        ];
        let deps = facts(&documents, &[], false).deps;
        let got: Vec<_> = deps.iter().map(|d| (d.line, d.target.clone())).collect();
        assert_eq!(
            got,
            vec![
                (5, Target::External("Illuminate\\Support\\Str".into())),
                (4, Target::Internal(PathBuf::from("src/Domain/Order.php"))),
            ]
        );
    }

    #[test]
    fn typescript_externals_use_the_npm_name() {
        assert_eq!(npm_name("@types/lodash"), "lodash");
        assert_eq!(npm_name("@types/babel__core"), "@babel/core");
        assert_eq!(npm_name("react"), "react");
        let documents = vec![doc(
            "src/a.ts",
            &[],
            &[(1, "scip-typescript npm @types/lodash 4.0.0 `index.d.ts`/")],
        )];
        let facts = facts(&documents, &[], false);
        assert_eq!(facts.deps[0].target, Target::External("lodash".into()));
        let deep = split("scip-typescript npm moment 2.0.0 locale/`ru.d.ts`/").unwrap();
        assert_eq!(deep.external_module(), "moment/locale/ru");
    }

    #[test]
    fn a_name_of_an_unnamed_package_is_no_symbol_use_but_is_reported_as_unjudged() {
        // scip-typescript before 0.6.1 indexes what sits under a `package.json` without
        // `name` (`{"type": "module"}` in `esm/`) as the unnamed package `. .`.
        let documents = vec![doc(
            "src/web/a.ts",
            &[],
            &[(1, "scip-typescript npm . . esm/`index.d.ts`/Thing#")],
        )];
        let facts = facts(&documents, &[], true);
        assert!(facts.symbols.is_empty());
        assert_eq!(
            facts.unnamed.into_iter().collect::<Vec<_>>(),
            ["src/web/a.ts"]
        );
    }

    #[test]
    fn an_unnamed_package_is_what_the_string_of_its_import_spells() {
        let at = |line: i32, start: i32, end: i32| {
            let mut occurrence = Occurrence::new();
            occurrence.range = vec![line, start, end];
            occurrence
        };
        let lines: Vec<String> = [
            "import { Hono } from 'hono'",
            "import x from \"./local\"",
            // `é` is one UTF-16 unit and two UTF-8 bytes.
            "/*é*/ import a from \"@scope/pkg/sub\"",
        ]
        .map(str::to_string)
        .to_vec();
        let utf16 = PositionEncoding::UnspecifiedPositionEncoding;
        assert_eq!(
            spelled_module(&lines, &at(0, 21, 27), utf16).as_deref(),
            Some("hono")
        );
        assert_eq!(spelled_module(&lines, &at(0, 9, 13), utf16), None, "a name");
        assert_eq!(
            spelled_module(&lines, &at(1, 14, 23), utf16),
            None,
            "a path"
        );
        assert_eq!(
            spelled_module(&lines, &at(2, 20, 36), utf16).as_deref(),
            Some("@scope/pkg/sub")
        );
        let utf8 = PositionEncoding::UTF8CodeUnitOffsetFromLineStart;
        assert_eq!(
            spelled_module(&lines, &at(2, 21, 37), utf8).as_deref(),
            Some("@scope/pkg/sub")
        );
        assert_eq!(
            spelled_module(&lines, &at(9, 0, 1), utf16),
            None,
            "no such line"
        );
    }

    #[test]
    fn documents_outside_the_project_are_dropped_and_paths_rebased() {
        let mut index = Index::new();
        for path in ["a.go", "../../go-build/x.go", "C:\\cache\\y.go", "b\\c.go"] {
            let mut document = scip::types::Document::new();
            document.relative_path = path.to_string();
            let mut occurrence = Occurrence::new();
            occurrence.symbol = DB.to_string();
            occurrence.typed_range =
                Some(Typed_range::SingleLineRange(scip::types::SingleLineRange {
                    line: 4,
                    ..Default::default()
                }));
            document.occurrences.push(occurrence);
            index.documents.push(document);
        }
        let bytes = index.write_to_bytes().unwrap();
        let documents =
            parse(&bytes, Path::new("/nonexistent"), Path::new("services/api")).unwrap();
        let paths: Vec<_> = documents.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["services/api/a.go", "services/api/b/c.go"]);
        assert_eq!(documents[0].references[0].line, 5);
    }
}
