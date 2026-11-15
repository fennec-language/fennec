//! Which top-level names code reaches by writing them bare. Each kit's code means its own names,
//! what it imports, and the language's: the standard library's code never reaches a program's or
//! a dependency's, and a dependency's never reaches the program's. So a program's own `round` is
//! its own, and std's `Duration.seconds` still rounds with the builtin.
//!
//! Whose code it is goes by the function it's in, never by where its text is: what the checker
//! writes into a program's function is the program's, though it's parsed as std's text is.

use fennec_syntax::{Diagnostic, KIT_MARK, Span};

use crate::checker::Checker;
use crate::function::FnCx;

/// Whose code a piece of code is, which decides the top-level names it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Code {
    /// The standard library's, whose own names are all std's.
    Library,
    /// A dependency's, whose own names carry its kit's mark.
    Dependency,
    /// The program's own, which reaches every name in sight.
    Program,
}

impl Code {
    /// Whose code the function `key` is, given whether it's std's.
    pub(crate) fn of(key: &str, std: bool) -> Code {
        if std {
            Code::Library
        } else if key.contains(KIT_MARK) {
            Code::Dependency
        } else {
            Code::Program
        }
    }

    /// Whether this code reaches the top-level function or constant `name`, which is std's when
    /// `prelude` says so.
    pub(crate) fn reaches(self, name: &str, prelude: bool) -> bool {
        match self {
            Code::Library => prelude,
            Code::Dependency => prelude || name.contains(KIT_MARK),
            Code::Program => true,
        }
    }
}

/// What starts the name of std's function while it's rewritten among the program's items.
pub(crate) const STD_AT: &str = "\0std:";

/// Whether the code `key` names is std's: by the function, type or constant it is, or is written
/// inside, with any type arguments put aside. `is_std` says whether one of those is std's, and
/// `None` for a name that's none of them.
pub fn std_code(key: &str, is_std: &dyn Fn(&str) -> Option<bool>) -> bool {
    if key.starts_with(STD_AT) {
        return true;
    }
    let mut at = without_type_args(key.strip_prefix("const ").unwrap_or(key));
    loop {
        if let Some(std) = is_std(&at) {
            return std;
        }
        match at.rsplit_once('.') {
            Some((outer, _)) => at = outer.to_owned(),
            None => return false,
        }
    }
}

/// A key with every `[...]` in it left out: `Box.get` for `Box[int].get`.
fn without_type_args(key: &str) -> String {
    let mut plain = String::new();
    let mut depth = 0_usize;
    for c in key.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    plain
}

impl Checker {
    /// Whether the function, type or constant `name` is std's, or `None` for one of none of those.
    fn std_item(&self, name: &str) -> Option<bool> {
        let program = &self.program;
        program
            .functions
            .get(name)
            .map(|entry| entry.prelude)
            .or_else(|| program.type_info(name).map(|info| info.prelude))
            .or_else(|| program.constants.get(name).map(|entry| entry.prelude))
    }
}

impl FnCx<'_> {
    /// Whose code is being checked here.
    pub(crate) fn code(&self) -> Code {
        let std = std_code(&self.key, &|name| self.checker.std_item(name));
        Code::of(&self.key, std)
    }

    /// Whether `name`, written bare here, names a top-level function this code reaches. What the
    /// checker wrote into the program, like a type's saving functions, is the compiler's, as
    /// std's is, and any code reaches it.
    pub(crate) fn names_function(&self, name: &str) -> bool {
        let code = self.code();
        self.checker.program.functions.get(name).is_some_and(|entry| {
            code.reaches(name, entry.prelude || crate::lend_lowering::not_written_by_hand(entry.def.span))
        })
    }

    /// The constant `name`, written bare at `span`, names for this code: one of the program's
    /// own it reaches, or one of std's the program imported, by its own name.
    pub(crate) fn named_constant(&mut self, name: &str, span: Span) -> Option<String> {
        let code = self.code();
        let compilers = |entry: &crate::program::ConstEntry| {
            entry.prelude || crate::lend_lowering::not_written_by_hand(entry.span)
        };
        if self.checker.program.constants.get(name).is_some_and(|entry| code.reaches(name, compilers(entry)))
        {
            return Some(name.to_owned());
        }
        if code == Code::Library {
            return None;
        }
        let program = &self.checker.program;
        // A module's constants listed with its builtins, like `math.INFINITY`, are imported as
        // its builtins are.
        let qualified = program
            .std_constants
            .get(name)
            .or_else(|| {
                program.imported(name, span).filter(|imported| program.constants.contains_key(*imported))
            })?
            .clone();
        self.checker.std_names.insert((span.start, span.end), qualified.clone());
        Some(qualified)
    }
}

impl FnCx<'_> {
    /// The modules or core files whose top-level function or constant is called `name`: `json`
    /// for `json.quoted`, `core.str` for str.fen's `padding`. Methods, like `Duration.seconds`,
    /// aren't among them.
    fn std_owners(&self, name: &str) -> Vec<&str> {
        let program = &self.checker.program;
        let suffix = format!(".{name}");
        let functions = program.functions.iter().filter(|(_, entry)| entry.prelude && entry.owner.is_none());
        let constants = program.constants.iter().filter(|(_, entry)| entry.prelude);
        let mut owners: Vec<&str> = functions
            .map(|(key, _)| key.as_str())
            .chain(constants.map(|(key, _)| key.as_str()))
            .filter_map(|key| key.strip_suffix(&suffix))
            .filter(|owner| !owner.contains(|c: char| c.is_ascii_uppercase()))
            .collect();
        owners.sort_unstable();
        owners.dedup();
        owners
    }

    /// The std modules that export a function or constant called `name`, written in Fennec, for
    /// the help on using one that isn't imported.
    pub(crate) fn std_exporters(&self, name: &str, span: Span) -> Vec<String> {
        let owners = self.std_owners(name);
        let exported =
            owners.into_iter().filter(|owner| fennec_syntax::std_member(owner, name, span).is_ok());
        exported.map(str::to_owned).collect()
    }

    /// Why a name only the standard library's own code has can't be named here, as str.fen's
    /// `padding` or std.net's `MOST_ROUTE_PARAMETERS`. `None` for a name std doesn't have, or one
    /// a module exports, which is imported to be used.
    pub(crate) fn std_own(&self, name: &str, span: Span) -> Option<Diagnostic> {
        if self.code() == Code::Library {
            return None;
        }
        let owners = self.std_owners(name);
        if owners.iter().any(|owner| fennec_syntax::std_member(owner, name, span).is_ok()) {
            return None;
        }
        let owner = owners.first()?;
        let (message, help) = match owner.strip_prefix("core.") {
            Some(_) => (
                format!("`{name}` is private to the standard library"),
                format!("only std's own code uses it: write a `{name}` of your own"),
            ),
            None => (
                format!("`{name}` is private to `std.{owner}`"),
                format!("only `std.{owner}`'s own code uses it: write a `{name}` of your own"),
            ),
        };
        Some(Diagnostic::new(message, span).with_help(help))
    }
}
