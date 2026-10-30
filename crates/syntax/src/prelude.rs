//! The part of the standard library written in Fennec itself.

/// The standard library written in Fennec, loaded before every program by both the
/// interpreter and the type checker.
pub const PRELUDE: [&str; 3] = [ERRORS, MATH, PATH];

const ERRORS: &str = r"
error IoError:
    reason: str

error InvalidValue:
    message: str

error ParseError:
    message: str
    line: int
    column: int
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

const PATH: &str = r#"
## A file path, like `saves/slot1.json`. Its parts are separated by `/`. Every method only
## looks at the text; none of them touch the file system.
struct Path:
    text: str

    ## `Path("saves") / "slot1.json"` is the same as `.join`.
    def /(self, part: str) -> Path:
        self.join(part)

    ## This path with `part` added. A `part` that starts with `/` replaces the path.
    def join(self, part: str) -> Path:
        if part.startswith("/") or self.text == "":
            return Path(part)
        if self.text.endswith("/"):
            return Path(self.text + part)
        Path(self.text + "/" + part)

    ## Whether the path starts at the root, `/`.
    def is_absolute(self) -> bool:
        self.text.startswith("/")

    ## The names between the `/`s, leaving out empty ones and `.`.
    def parts(self) -> list[str]:
        [part for part in self.text.split("/") if part != "" and part != "."]

    ## The last part: `fox.png` for `assets/fox.png`.
    def name(self) -> str:
        parts = self.parts()
        if len(parts) == 0:
            return ""
        parts[-1]

    ## What follows the name's last `.`, without the dot: `png` for `fox.png`. Empty for
    ## names like `notes` and `.hidden`.
    def extension(self) -> str:
        pieces = self.name().split(".")
        if len(pieces) < 2 or (len(pieces) == 2 and pieces[0] == ""):
            return ""
        pieces[-1]

    ## The name without its extension: `fox` for `fox.png`.
    def stem(self) -> str:
        name = self.name()
        extension = self.extension()
        if extension == "":
            return name
        name[:len(name) - len(extension) - 1]

    ## The path without its last part: `assets` for `assets/fox.png`, and `.` for `fox.png`.
    def parent(self) -> Path:
        parts = self.parts()
        if len(parts) == 0:
            return self.rebuild([])
        self.rebuild(parts[:len(parts) - 1])

    ## The same path written simply: `a/./b/../c` becomes `a/c`. A `..` that would go above
    ## the start stays in a relative path and is dropped from an absolute one.
    def normalize(self) -> Path:
        mut kept = []
        for part in self.parts():
            if part != "..":
                kept.append(part)
            elif len(kept) > 0 and kept[-1] != "..":
                kept.pop()
            elif not self.is_absolute():
                kept.append("..")
        self.rebuild(kept)

    ## `parts` joined by `/`, starting at the root when this path does.
    def rebuild(self, parts: list[str]) -> Path:
        text = "/".join(parts)
        if self.is_absolute():
            return Path("/" + text)
        if text == "":
            return Path(".")
        Path(text)
"#;
