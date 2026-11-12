<!-- fennec:start. `fennec init` writes this block and replaces it when it runs again. -->
## Fennec

This project uses Fennec. It reads like Python, but it's statically typed, and everything is a value.

### Commands

- `fennec run`: run the tests a change affects, then the kit's `main`, or `fennec run file.fen` for one file. A failing test stops it, and `--no-test` skips them.
- `fennec test`: run the tests a change affects. It prints only failures and one line of totals, and `--verbose` lists the rest.
- `fennec fmt`: format the code. There's one style, so don't format by hand.
- `fennec check --json`: type-check without running anything. It prints one JSON object: each finding's rule, place, message, help and the fixes `fennec fix` would make.
- `fennec lint --json`: the same, with the linter's findings and hints about what may copy or allocate, and how often each runs. Hints never fail it.
- `fennec fix`: make the fixes the type checker and linter suggest. It only makes fixes that keep what the program does, so read each error's help for the rest.

### How the code looks

```fennec
from std.math import PI

struct Circle:
    radius: float

    def area(self) -> float:
        return PI * self.radius * self.radius

    def grow(ref self, by: float):
        self.radius += by

error TooSmall:
    radius: float

MINIMUM = 0.5

def checked(circle: Circle) -> Circle fails TooSmall:
    if circle.radius < MINIMUM: fail TooSmall(radius=circle.radius)
    return circle

def total_area(circles: list[Circle]) -> float:
    return sum(circle.area() for circle in circles)

def described(circles: list[Circle]) -> str:
    # A block after `x =` ends with its value.
    size = match len(circles):
        case 0:
            "no circles"
        case 1:
            "one circle"
        case _:
            "several circles"
    return f"{size}, {total_area(circles)} in all"

test "a circle has an area":
    assert Circle(radius=1.0).area() > 3.0

test "a total is never negative" for radius: float:
    assert total_area([Circle(radius=radius * radius)]) >= 0.0

def main(stdout: Stdout):
    mut circle = checked(Circle(radius=0.2)) except why:
        stdout.print(f"using the smallest circle instead: {why}")
        Circle(radius=MINIMUM)
    circle.grow(1.0)
    stdout.print(described([circle]))
```

- Write the types of parameters and results. Everything inside a function is inferred.
- A `def` that gives back a value says `return`, on its last line too.
- A block after `x =`, as `x = match ...:` and `x = f() except why:` are, ends with its value. A `return` inside one leaves the whole function.
- `len(x)` and `abs(x)` are builtins, as in Python. There's no `x.len()`.
- A name can't change unless it's declared `mut`: `mut total = 0`. A function changes what it's given only when it takes it as `ref`: `def heal(ref unit: Unit)`, and a method that changes its value takes `ref self`.
- Assigning or passing a value gives the receiver its own copy.
- `main` receives what the program may touch, like `stdout: Stdout`. There's no global `print`.
- A function that can fail says `fails` with its error and uses `fail`. Its caller passes the failure on with `?`, or handles it where the call is with `except why:`, using `why` on every way out.
- `else` is only for a value that may be `None`: `name = names.get(id) else "nobody"`.
- A kit is a `kit.toml` and code under `src`: `src/main.fen` holds `main`, and `src/enemies/ai.fen` is the module `enemies.ai`.

### Common mistakes

- A `def` ending in a bare value doesn't give it back. Write `return`.
- `else` after a call that can fail is a compile error. Write `except why:` and use `why`, or `?` to pass it on.
- An interface isn't a type you can take. Take a type parameter that fits it: `def size_of[T: Sized](item: T) -> int`.
- There's no `const`. A constant is a name at the top level of a file: `LIMIT = 10`.
- A property test's inputs follow `for`: `test "stays in range" for value: int:`.
- Indent with spaces. Tabs aren't allowed.
<!-- fennec:end -->
