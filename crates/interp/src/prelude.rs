//! The standard library so far. Most of it is written in Fennec itself; the rest is builtins.

use fennec_syntax::{Diagnostic, Import};

/// Fennec source loaded before every program.
pub(crate) const SOURCES: [&str; 2] = [ERRORS, MATH];

const ERRORS: &str = r"
error IoError:
    reason: str

error InvalidValue:
    message: str
";

const MATH: &str = r"
struct Vec3:
    x = 0.0
    y = 0.0
    z = 0.0

    def zero():
        Vec3()

    def +(self, other: Vec3):
        Vec3(x=self.x + other.x, y=self.y + other.y, z=self.z + other.z)

    def -(self, other: Vec3):
        Vec3(x=self.x - other.x, y=self.y - other.y, z=self.z - other.z)

    def *(self, scale: float):
        Vec3(x=self.x * scale, y=self.y * scale, z=self.z * scale)

    def dot(self, other: Vec3):
        self.x * other.x + self.y * other.y + self.z * other.z

    def length(self):
        sqrt(self.dot(self))

    def distance_to(self, other: Vec3):
        (self - other).length()
";

/// The modules that exist so far, and the names each one provides.
const MODULES: [(&str, &[&str]); 3] = [
    ("std.math", &["Vec3", "PI", "sqrt", "floor", "ceil", "sin", "cos"]),
    ("std.game", &["Id", "IdMap"]),
    ("std.layout", &["SoaList"]),
];

/// Checks an import names a module and names that exist. Standard library names are
/// available everywhere for now, so importing only has to be correct, not complete.
pub(crate) fn check_import(import: &Import) -> Result<(), Diagnostic> {
    let module = import.module.join(".");
    let Some((_, names)) = MODULES.iter().find(|(name, _)| *name == module) else {
        return Err(Diagnostic::new(format!("there's no module called `{module}`"), import.span)
            .with_help("the standard library so far has std.math, std.game and std.layout"));
    };
    match import.names.iter().find(|name| !names.contains(&name.as_str())) {
        Some(missing) => {
            Err(Diagnostic::new(format!("`{module}` has nothing called `{missing}`"), import.span)
                .with_help(format!("`{module}` provides {}", names.join(", "))))
        }
        None => Ok(()),
    }
}
