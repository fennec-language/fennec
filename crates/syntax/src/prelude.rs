//! The part of the standard library written in Fennec itself.

/// The standard library written in Fennec, loaded before every program by both the
/// interpreter and the type checker.
pub const PRELUDE: [&str; 2] = [ERRORS, MATH];

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
