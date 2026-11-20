//! What each file of a program defines, which rewriting names across its files reads. A file's
//! rewriting reads it only through [`Looking`], which can note every question asked and its
//! answer. Then a later load can tell whether that file's rewriting would come out the same: it
//! does when the file is as it was and every question still has the same answer.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fennec_syntax::{Diagnostic, Item, Span};

use crate::load::File;

/// What one file defines.
struct Part {
    name: String,
    shown: String,
    names: BTreeSet<String>,
    /// What starts its kit's names in the merged module.
    prefix: String,
    exports: HashSet<String>,
    /// Where the file's last import ends, where another can go.
    imports_end: Option<Span>,
}

/// What each file defines, and which file defines each name, by the name the merged module has
/// for it.
pub(crate) struct Kit {
    files: Vec<Part>,
    owners: HashMap<String, usize>,
    /// The enums with each case name, and the file each is in.
    cases: HashMap<String, Vec<(String, usize)>>,
}

impl Kit {
    fn name(&self, file: usize) -> &str {
        self.files.get(file).map_or("", |part| &part.name)
    }

    fn shown(&self, file: usize) -> &str {
        self.files.get(file).map_or("", |part| &part.shown)
    }

    fn prefix(&self, file: usize) -> Option<&str> {
        self.files.get(file).map(|part| part.prefix.as_str())
    }

    fn imports_end(&self, file: usize) -> Option<Span> {
        self.files.get(file).and_then(|part| part.imports_end)
    }

    fn defines(&self, file: usize, name: &str) -> bool {
        self.files.get(file).is_some_and(|part| part.names.contains(name))
    }

    /// Whether code in `reader` can use `name` from `file`: every name inside a kit, and only
    /// what's exported between kits.
    fn provides(&self, reader: usize, file: usize, name: &str) -> bool {
        self.defines(file, name)
            && (self.prefix(reader) == self.prefix(file)
                || self.files.get(file).is_some_and(|part| part.exports.contains(name)))
    }

    /// A help line listing what `file` gives `reader`.
    fn listing(&self, reader: usize, file: usize) -> String {
        let apart = self.prefix(reader) != self.prefix(file);
        let names: Vec<&str> = self
            .files
            .get(file)
            .map(|part| part.names.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let names: Vec<&str> = names.into_iter().filter(|name| self.provides(reader, file, name)).collect();
        if apart {
            return if names.is_empty() {
                format!("`{}` exports nothing", self.name(file))
            } else {
                format!("`{}` exports {}", self.name(file), names.join(", "))
            };
        }
        if names.is_empty() {
            format!("{} defines nothing yet", self.shown(file))
        } else {
            format!("{} defines {}", self.shown(file), names.join(", "))
        }
    }

    fn owner(&self, name: &str) -> Option<usize> {
        self.owners.get(name).copied()
    }

    fn cases(&self, case: &str) -> Option<&[(String, usize)]> {
        self.cases.get(case).map(Vec::as_slice)
    }

    /// Whether a question is answered now as it was then. A help line is worked out again only
    /// when it was asked for, which is for a file with a mistake.
    fn answers(&self, read: &Read, answer: &Answer) -> bool {
        match (read, answer) {
            (Read::Name(file), Answer::Text(text)) => self.name(*file) == text,
            (Read::Shown(file), Answer::Text(text)) => self.shown(*file) == text,
            (Read::Prefix(file), Answer::Prefix(prefix)) => self.prefix(*file) == prefix.as_deref(),
            (Read::ImportsEnd(file), Answer::At(at)) => self.imports_end(*file) == *at,
            (Read::Defines(file, name), Answer::Yes(yes)) => self.defines(*file, name) == *yes,
            (Read::Provides(reader, file, name), Answer::Yes(yes)) => {
                self.provides(*reader, *file, name) == *yes
            }
            (Read::Listing(reader, file), Answer::Text(text)) => self.listing(*reader, *file) == *text,
            (Read::Owner(name), Answer::File(file)) => self.owner(name) == *file,
            (Read::Cases(case), Answer::Cases(cases)) => self.cases(case) == cases.as_deref(),
            _ => false,
        }
    }
}

/// Finds what each file defines, reporting names defined in two files.
pub(crate) fn gather(files: &[File], errors: &mut Vec<Diagnostic>) -> Kit {
    let mut kit = Kit { files: Vec::new(), owners: HashMap::new(), cases: HashMap::new() };
    for (at, file) in files.iter().enumerate() {
        let mut names = BTreeSet::new();
        for item in &file.module.items {
            if let Item::Enum(def) = item {
                for case in &def.cases {
                    kit.cases.entry(case.name.clone()).or_default().push((def.name.clone(), at));
                }
            }
            let Some((name, span)) = defined(item) else { continue };
            let merged = format!("{}{name}", file.prefix);
            match kit.owners.get(&merged) {
                Some(&owner) if owner == at => errors.push(
                    Diagnostic::new(format!("`{name}` is already defined in this file"), span)
                        .with_help("rename one of them"),
                ),
                Some(&owner) => errors.push(
                    Diagnostic::new(format!("`{name}` is already defined in {}", kit.shown(owner)), span)
                        .with_help(
                            "the files of a kit share one set of top-level names, so rename one of them",
                        ),
                ),
                None => {
                    kit.owners.insert(merged, at);
                }
            }
            names.insert(name.to_owned());
        }
        kit.files.push(Part {
            name: file.name.clone(),
            shown: file.shown.clone(),
            names,
            prefix: file.prefix.clone(),
            exports: file.exports.clone(),
            imports_end: file
                .module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Import(import) => Some(Span { start: import.span.end, end: import.span.end }),
                    _ => None,
                })
                .max_by_key(|at| at.start),
        });
    }
    kit
}

/// The name an item defines at the top level, and where.
fn defined(item: &Item) -> Option<(&str, Span)> {
    match item {
        Item::Function(def) => Some((&def.name, def.span)),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => Some((&def.name, def.span)),
        Item::Interface(interface) => Some((&interface.name, interface.span)),
        Item::TypeAlias { name, span, .. } | Item::Const { name, span, .. } => Some((name, *span)),
        Item::Extend(_) | Item::Import(_) | Item::Test(_) | Item::Stmt(_) => None,
    }
}

/// A question a file's rewriting asks of the kit's names. Files are by their place in the load,
/// which a rewriting is kept for only while the load's files are the same, in the same order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Read {
    Name(usize),
    Shown(usize),
    Prefix(usize),
    ImportsEnd(usize),
    Defines(usize, String),
    Provides(usize, usize, String),
    Listing(usize, usize),
    Owner(String),
    Cases(String),
}

/// What a question was answered.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Yes(bool),
    Text(String),
    Prefix(Option<String>),
    At(Option<Span>),
    File(Option<usize>),
    Cases(Option<Vec<(String, usize)>>),
}

/// What a file's rewriting asked of the kit's names, with each answer.
#[derive(Debug, Default)]
pub(crate) struct Reads(BTreeMap<Read, Answer>);

impl Reads {
    /// Whether `kit` answers every question as it was answered when these were noted. A
    /// rewriting reads nothing else of the other files, so it comes out the same when they do.
    pub(crate) fn hold(&self, kit: &Kit) -> bool {
        self.0.iter().all(|(read, answer)| kit.answers(read, answer))
    }
}

/// The kit's names as one file's rewriting reads them, noting each question and its answer when
/// asked to.
pub(crate) struct Looking<'a> {
    kit: &'a Kit,
    reads: Option<RefCell<Reads>>,
}

impl<'a> Looking<'a> {
    pub(crate) fn new(kit: &'a Kit, noting: bool) -> Self {
        Looking { kit, reads: noting.then(|| RefCell::new(Reads::default())) }
    }

    /// What was asked and answered, when noting.
    pub(crate) fn into_reads(self) -> Option<Reads> {
        self.reads.map(RefCell::into_inner)
    }

    fn noted(&self, read: impl FnOnce() -> Read, answer: impl FnOnce() -> Answer) {
        if let Some(reads) = &self.reads {
            reads.borrow_mut().0.entry(read()).or_insert_with(answer);
        }
    }

    pub(crate) fn name(&self, file: usize) -> &'a str {
        let name = self.kit.name(file);
        self.noted(|| Read::Name(file), || Answer::Text(name.to_owned()));
        name
    }

    pub(crate) fn shown(&self, file: usize) -> &'a str {
        let shown = self.kit.shown(file);
        self.noted(|| Read::Shown(file), || Answer::Text(shown.to_owned()));
        shown
    }

    pub(crate) fn prefix(&self, file: usize) -> Option<&'a str> {
        let prefix = self.kit.prefix(file);
        self.noted(|| Read::Prefix(file), || Answer::Prefix(prefix.map(str::to_owned)));
        prefix
    }

    pub(crate) fn imports_end(&self, file: usize) -> Option<Span> {
        let end = self.kit.imports_end(file);
        self.noted(|| Read::ImportsEnd(file), || Answer::At(end));
        end
    }

    pub(crate) fn defines(&self, file: usize, name: &str) -> bool {
        let defines = self.kit.defines(file, name);
        self.noted(|| Read::Defines(file, name.to_owned()), || Answer::Yes(defines));
        defines
    }

    /// Whether code in `reader` can use `name` from `file`: every name inside a kit, and only
    /// what's exported between kits.
    pub(crate) fn provides(&self, reader: usize, file: usize, name: &str) -> bool {
        let provides = self.kit.provides(reader, file, name);
        self.noted(|| Read::Provides(reader, file, name.to_owned()), || Answer::Yes(provides));
        provides
    }

    /// A help line listing what `file` gives `reader`.
    pub(crate) fn listing(&self, reader: usize, file: usize) -> String {
        let listing = self.kit.listing(reader, file);
        self.noted(|| Read::Listing(reader, file), || Answer::Text(listing.clone()));
        listing
    }

    /// The file that defines `name`, as the merged module calls it.
    pub(crate) fn owner(&self, name: &str) -> Option<usize> {
        let owner = self.kit.owner(name);
        self.noted(|| Read::Owner(name.to_owned()), || Answer::File(owner));
        owner
    }

    /// Each enum with a case called `case`, and the file it's in.
    pub(crate) fn cases(&self, case: &str) -> Option<&'a [(String, usize)]> {
        let cases = self.kit.cases(case);
        self.noted(|| Read::Cases(case.to_owned()), || Answer::Cases(cases.map(<[(String, usize)]>::to_vec)));
        cases
    }

    /// What the merged module calls a name `file` defines.
    pub(crate) fn merged(&self, file: usize, name: &str) -> String {
        format!("{}{name}", self.prefix(file).unwrap_or(""))
    }

    /// Whether `file` is of a dependency rather than the program's own kit.
    pub(crate) fn is_dependency(&self, file: usize) -> bool {
        self.prefix(file).is_some_and(|prefix| !prefix.is_empty())
    }

    /// Whether the files `reader` and `file` are of different kits.
    pub(crate) fn apart(&self, reader: usize, file: usize) -> bool {
        self.prefix(reader) != self.prefix(file)
    }
}
