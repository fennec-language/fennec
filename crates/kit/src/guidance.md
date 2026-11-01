<!-- fennec:start. `fennec init` writes this block and replaces it when it runs again. -->
## Fennec

This project uses Fennec. It reads like Python, but it's statically typed, and everything is a value.

### Commands

- `fennec run`: run the tests a change affects, then the kit's `main`, or `fennec run file.fen` for one file. A failing test stops it, and `--no-test` skips them.
- `fennec test`: run the tests a change affects. It prints only failures and one line of totals, and `--verbose` lists the rest.
- `fennec fmt`: format the code. There's one style, so don't format by hand.
- `fennec fix`: make the fixes the type checker suggests. Every error says what to do, so read it first.

### How the code looks

```fennec
from std.math import PI

struct Circle:
    radius: float

    def area(self) -> float:
        PI * self.radius * self.radius

error TooSmall:
    radius: float

MINIMUM = 0.5

def checked(circle: Circle) -> Circle:
    if circle.radius < MINIMUM: fail TooSmall(radius=circle.radius)
    circle

def total_area(circles: list[Circle]) -> float:
    sum(circle.area() for circle in circles)

test "a circle has an area":
    assert Circle(radius=1.0).area() > 3.0

test "a total is never negative" for radius: float:
    assert total_area([Circle(radius=radius * radius)]) >= 0.0

def main(stdout: Stdout):
    circle = checked(Circle(radius=2.0))?
    stdout.print(f"area: {circle.area()}")
```

- Write the types of parameters and results. Everything inside a function is inferred.
- A function's last expression is its value. Use `return` only to leave early.
- A name can't change unless it's declared `mut`: `mut total = 0`. The same goes for parameters and methods: `mut self`.
- Assigning or passing a value gives the receiver its own copy.
- `main` receives what the program may touch, like `stdout: Stdout`. There's no global `print`.
- A function that can fail uses `fail`, and its caller passes the failure on with `?`.
- A kit is a `kit.toml` and code under `src`: `src/main.fen` holds `main`, and `src/enemies/ai.fen` is the module `enemies.ai`.

### Common mistakes

- An interface isn't a type you can take. Take a type parameter that fits it: `def size_of[T: Sized](item: T) -> int`.
- There's no `const`. A constant is a name at the top level of a file: `LIMIT = 10`.
- A property test's inputs follow `for`: `test "stays in range" for value: int:`.
- Indent with spaces. Tabs aren't allowed.
<!-- fennec:end -->
