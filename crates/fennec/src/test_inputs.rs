//! Inputs for property tests through LLVM, as ordinary Fennec functions a test calls, so building
//! them takes nothing but what any program takes. They draw the same values from the same seed, in
//! the same order, every run, so a test fails on the same inputs each time.

use std::collections::HashMap;
use std::fmt::Write as _;

use fennec_syntax::{Item, Module, TypeDef, TypeExpr};

/// What the functions here are called until `crate::llvm_tests` gives them names no program can
/// write.
pub(crate) const PREFIX: &str = "fennec_test_";

/// The functions every type's function uses: the next random number below a limit, and how long a
/// string or list is, which is empty once it's deep inside another.
const SHARED: &str = "\
def fennec_test_random(ref state: int, limit: int) -> int:
    state = state * 48271 % 2147483647
    state % limit

def fennec_test_length(ref state: int, depth: int) -> int:
    if depth > 2:
        return 0
    fennec_test_random(ref state, 8)

";

/// The functions for what a batch of tests take.
pub(crate) struct Inputs<'m> {
    module: &'m Module,
    /// The function for each type, by how the type is written.
    made: HashMap<String, String>,
    /// The function giving the simpler values of each type, by how the type is written.
    simpler: HashMap<String, String>,
    source: String,
}

pub(crate) fn written(ty: &TypeExpr) -> String {
    let args: Vec<String> = ty.args.iter().map(written).collect();
    let plain = match (ty.name.as_str(), args.is_empty()) {
        ("tuple", _) => format!("({})", args.join(", ")),
        (name, true) => name.to_owned(),
        (name, false) => format!("{name}[{}]", args.join(", ")),
    };
    if ty.optional { format!("{plain}?") } else { plain }
}

impl<'m> Inputs<'m> {
    pub(crate) fn new(module: &'m Module) -> Self {
        Inputs { module, made: HashMap::new(), simpler: HashMap::new(), source: SHARED.to_owned() }
    }

    /// The source of every function so far.
    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    /// The function that makes a value of `ty`, written if it hasn't been, or None for a type whose
    /// values can't be made here yet.
    pub(crate) fn function_for(&mut self, ty: &TypeExpr) -> Option<String> {
        let shown = written(ty);
        if let Some(name) = self.made.get(&shown) {
            return Some(name.clone());
        }
        // Named before its body is written, for a type that holds itself.
        let name = format!("{PREFIX}input_{}", self.made.len());
        self.made.insert(shown.clone(), name.clone());
        let body = if ty.optional {
            let inner = self.function_for(&TypeExpr { optional: false, ..ty.clone() })?;
            format!(
                "    if fennec_test_random(ref state, 5) == 0:\n        return None\n    {inner}(ref state, depth)\n"
            )
        } else {
            self.body_for(ty)?
        };
        let _ = write!(self.source, "def {name}(ref state: int, depth: int) -> {shown}:\n{body}\n");
        Some(name)
    }

    /// The function giving a value's simpler values to try, simplest first, written if it hasn't
    /// been, or None for a type whose values can't be made here.
    pub(crate) fn simpler_for(&mut self, ty: &TypeExpr) -> Option<String> {
        let shown = written(ty);
        if let Some(name) = self.simpler.get(&shown) {
            return Some(name.clone());
        }
        let body = match (ty.name.as_str(), ty.args.as_slice(), ty.optional) {
            // Toward 0: 0, half of it rounded toward 0, and one step nearer.
            ("int", [], false) => "    if value == 0:\n        return []\n    mut half = value // 2\n    if value < 0 and half * 2 != value:\n        half += 1\n    if value > 0:\n        return [0, half, value - 1]\n    [0, half, value + 1]\n".to_owned(),
            ("bool", [], false) => "    if value:\n        return [False]\n    []\n".to_owned(),
            ("str", [], false) => "    if value.len() == 0:\n        return []\n    [\"\", value[0:value.len() - 1]]\n".to_owned(),
            // Empty, then each item left out in turn.
            ("list", [item], false) => {
                self.function_for(item)?;
                format!(
                    "    if value.len() == 0:\n        return []\n    mut options: list[{shown}] = [[]]\n    for skip in range(value.len()):\n        options.append(value[:skip] + value[skip + 1:])\n    options\n"
                )
            }
            // Toward 0, as a whole number first.
            ("float", [], false) => "    if value == 0.0:\n        return []\n    if value % 1.0 == 0.0:\n        return [0.0]\n    whole = value - value % 1.0\n    if value < 0.0:\n        return [0.0, whole + 1.0]\n    [0.0, whole]\n".to_owned(),
            // Tuples, structs and optionals have nothing simpler yet. Shrinking structs by their
            // fields is still to come.
            _ => {
                self.function_for(ty)?;
                "    []\n".to_owned()
            }
        };
        let name = format!("{PREFIX}simpler_{}", self.simpler.len());
        self.simpler.insert(shown.clone(), name.clone());
        let _ = write!(self.source, "def {name}(value: {shown}) -> list[{shown}]:\n{body}\n");
        Some(name)
    }

    fn body_for(&mut self, ty: &TypeExpr) -> Option<String> {
        Some(match (ty.name.as_str(), ty.args.as_slice()) {
            ("int", []) => "    kind = fennec_test_random(ref state, 10)\n    if kind == 0:\n        return 0\n    if kind == 1:\n        return 1\n    if kind == 2:\n        return -1\n    fennec_test_random(ref state, 2001) - 1000\n".to_owned(),
            ("float", []) => {
                let int = TypeExpr { name: "int".to_owned(), args: Vec::new(), optional: false, span: ty.span };
                let int = self.function_for(&int)?;
                format!("    float({int}(ref state, depth)) / 8.0\n")
            }
            ("bool", []) => "    fennec_test_random(ref state, 2) == 0\n".to_owned(),
            ("str", []) => "    letters = \"abcdefghijklmnopqrstuvwxyz\"\n    mut text = \"\"\n    for _ in range(fennec_test_length(ref state, depth)):\n        text += letters[fennec_test_random(ref state, 26)]\n    text\n".to_owned(),
            ("list", [item]) => {
                let inner = self.function_for(item)?;
                format!(
                    "    mut items: {} = []\n    for _ in range(fennec_test_length(ref state, depth)):\n        items.append({inner}(ref state, depth + 1))\n    items\n",
                    written(ty)
                )
            }
            ("tuple", items) => {
                let mut body = String::new();
                let mut names = Vec::with_capacity(items.len());
                for (at, item) in items.iter().enumerate() {
                    let inner = self.function_for(item)?;
                    let _ = writeln!(body, "    item{at} = {inner}(ref state, depth + 1)");
                    names.push(format!("item{at}"));
                }
                let _ = writeln!(body, "    ({})", names.join(", "));
                body
            }
            (name, []) => self.body_of_type(name)?,
            _ => return None,
        })
    }

    /// A struct's fields, or one of an enum's cases with its fields. A `where` rule can refuse what
    /// comes out, which isn't tried again here.
    fn body_of_type(&mut self, name: &str) -> Option<String> {
        let module = self.module;
        let def: &TypeDef = module.items.iter().find_map(|item| match item {
            Item::Struct(def) | Item::Enum(def) if def.name == name && def.generics.is_empty() => Some(def),
            _ => None,
        })?;
        if def.cases.is_empty() {
            let (lines, made) = self.fields(&def.fields, "    ")?;
            return Some(format!("{lines}    {name}({made})\n"));
        }
        let mut body = format!("    kind = fennec_test_random(ref state, {})\n", def.cases.len());
        for (at, case) in def.cases.iter().enumerate() {
            let (lines, made) = self.fields(&case.fields, "        ")?;
            let made = if case.fields.is_empty() {
                format!(".{}", case.name)
            } else {
                format!(".{}({made})", case.name)
            };
            let _ = write!(body, "    if kind == {at}:\n{lines}        return {made}\n");
        }
        // Never reached, but a function has to give back a value at its end.
        let first = def.cases.first()?;
        if first.fields.is_empty() {
            let _ = writeln!(body, "    .{}", first.name);
        } else {
            let (lines, made) = self.fields(&first.fields, "    ")?;
            let _ = writeln!(body, "{lines}    .{}({made})", first.name);
        }
        Some(body)
    }

    /// Lines that make a value for each field with a type, and the arguments that pass them on.
    fn fields(&mut self, fields: &[fennec_syntax::Field], indent: &str) -> Option<(String, String)> {
        let mut lines = String::new();
        let mut args = Vec::with_capacity(fields.len());
        for field in fields {
            if field.rule.is_some() {
                return None;
            }
            let Some(ty) = &field.ty else { continue };
            let inner = self.function_for(ty)?;
            let _ = writeln!(lines, "{indent}field_{} = {inner}(ref state, depth + 1)", field.name);
            args.push(format!("{0}=field_{0}", field.name));
        }
        Some((lines, args.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::Inputs;

    #[test]
    fn it_leaves_what_it_cannot_make() {
        let source = "\
struct Ruled:
    x: int where x > 0

struct Boxed[T]:
    value: T

enum Holds:
    One(inner: Ruled)

test \"t\" for a: Ruled:
    pass
";
        let module = fennec_syntax::parse(source).unwrap();
        for ty in ["Ruled", "Boxed[int]", "Holds", "i8", "Unknown", "dict[int, int]"] {
            let test = format!("test \"t\" for a: {ty}:\n    pass\n");
            let parsed = fennec_syntax::parse(&test).unwrap();
            let Some(fennec_syntax::Item::Test(test)) = parsed.items.first() else { panic!("no test") };
            let wanted = test.inputs.first().and_then(|input| input.ty.as_ref()).unwrap();
            assert_eq!(Inputs::new(&module).function_for(wanted), None, "{ty}");
        }
    }
}
