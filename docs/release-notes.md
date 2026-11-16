# Release notes

What changes for programs between releases, newest first. A line goes in with the change that makes it.

## Next release

- **`std.pack`: a generic type's shape now holds its fields.** The shape a value of `Pair[int]` is saved with was its name and type arguments alone, so moving or changing a field of `Pair[T]` didn't change it. Data saved before such a change then loaded into the wrong fields, with no error. The shape is now its fields' with the arguments filled in, as `Pair[int]{first: int, second: int}`, as a plain struct's always was. This changes the shape hash of every saved value that holds a generic type, and its `pack.lock` entries. Data saved by an earlier release that carries its layout still loads, by its fields' names, as data from another version of a type does.
