# The store

Everything Fennec keeps between commands is in one place per machine, the store, in `~/.fennec` (or
`FENNEC_HOME`). A project's folder holds only its own files: no `target/` folder, nothing a run, a
test or a build leaves behind. `fennec build` writes the program you ask for where `-o` says, or in
the current folder, and that's all it writes outside the store.

This page is the store's format, so that the launcher and a shared cache can read it as well as
`fennec` does.

## Where things are

```
~/.fennec/
  cache/
    <version>-<build>/    one folder per build of Fennec, like 0.0.1-1f0c…: its version and a hash of all its source
      <kind>/<name>       one entry
    cleanup.lock          held while a cleanup runs
    cleaned               when a command last started a cleanup
  work/                   commands' scratch work, like a build's objects before they're linked
  daemon/<version>-<build>.sock  the build daemon's socket, one per build, named by its build's first 16 hex digits,
                          or one in the temporary folder when this path is too long
  std/<version>/          the standard library's files, for editors to open
  llvm-tools              where LLVM's tools were last found
  llvm-version            which version of LLVM they are
```

An entry is a file. Its kind is the folder it's in, and its name is a hash. Each build keeps entries
of its own, so two builds never read each other's, even of one version: a key that left out something
the compiler does would otherwise bring back what an older compiler made.

## Two sorts of entry

**An entry named by its content** is named by a hash of everything it was made from, so the same
inputs on any machine name the same entry, and an entry never changes once it's written. These are
what a shared cache can hold.

| kind | holds | named by |
|---|---|---|
| `llvm` | one function compiled to an object file | the build of Fennec, the compiler pipeline, and the function's text as LLVM reads it |
| `file-rows` | a file's definitions and the hash of each, for telling what an edit changed | the file's text |

**An entry about one program on this machine** is named by a hash of the program's absolute path, so
it's found again from where the program is. These are this machine's own and are never shared.

| kind | holds |
|---|---|
| `runs` | the hashes of each file and definition when the program last ran |
| `checked` | the text of the program when it last passed the checker |
| `run-image` | the program as last cut down to what `main` reaches, while none of its files change |
| `tests` | each test's result, with the hash of everything it reached, so an unchanged test that passed doesn't run again. A kit's are kept under its name in `kit.toml` too, so a kit that moves keeps them; a result only counts when its hash matches the test as it is, so another kit of that name can't pass a test it hasn't run |
| `tested` | the text of the program when all its tests last passed |
| `kit-state` | a kit's files when its tests last passed, by size and when they changed, so a run can tell what changed without reading them |
| `bench` | each `bench` block's numbers from the last `fennec bench` |

Hashes are BLAKE3, written as 64 lowercase hex digits, and an entry about a program adds what it is:
`<hash>.bin`, `<hash>.txt`, `<hash>.state`, `<hash>.image` or `<hash>.rows`.

## Writing, reading and cleaning up

- An entry is written to a temporary file beside it and renamed into place, so a reader sees all of
  an entry or none of it. Reading and writing take no locks.
- Reading an entry sets its modified time, at most once every ten minutes, and that's how cleanup
  knows what was used least recently. Access times aren't reliable enough on every system.
- The store has one size cap for every build's folder together: 1 GiB, or `FENNEC_CACHE_CAP` in
  bytes. A cleanup first removes, whole, any other build's folder that nothing has used for a day,
  under the cap or over it. Then, if the cache is over, it removes entries until it's at four fifths
  of the cap, the least recently used first whichever build wrote them, since another build may be
  in use elsewhere, and removes other builds' folders it empties.
- The build daemon runs cleanup when it starts and every five minutes after, while it's idle. Any
  command, with the daemon or without it, starts `fennec cache clean` in the background once no
  cleanup has run for an hour, and doesn't wait for it; `cleaned` marks when. `fennec cache clean`
  runs one when asked. Only cleanup takes a lock, so two never run at once, and one that died leaves
  a lock that's taken over after ten minutes.
- A command's scratch work goes in `work`, in a folder or file of its own named by its process and
  when it started, and the command removes it when it's done. What a command that was stopped leaves
  there, a cleanup removes once it hasn't changed for an hour.
- Nothing in the store is needed for correctness. An entry that's missing, unreadable or removed by
  cleanup only means the work is done again.

## A build's root hash (planned)

Every build gets a root hash: one hash of everything the finished program depends on.

- each of the program's files, by its module and the hash of its text;
- the Fennec version and the build of `fennec`, and so the standard library;
- the target, as the LLVM triple and the CPU it's built for;
- the flags and switches that change what's built: a release build, a static one, `--cpu baseline`, and `FENNEC_SANITIZE`, `FENNEC_CHECK_PROOFS` and `FENNEC_STRIP_FACTS`, so a program built one way is never handed back for another;
- the runtime library linked in, by the hash of its file.

The finished program is kept in the store under its root hash, with the list of its functions'
hashes beside it, so that an unchanged build reuses it whole, and a build that changed reuses every
function it can.

## A shared cache (planned, not built)

A hosted cache holds only entries named by their content, and a root hash is enough to ask for a
whole program:

1. A build asks for its root hash. If the cache has it, the finished program comes back in one round
   trip.
2. If it doesn't, the build sends the whole list of its functions' hashes in one request, and gets back
   every one the cache has. It compiles the rest itself.

Only trusted builds write to a shared cache, and **every shared entry is signed by whoever wrote it**.
A reader checks the signature against the keys it trusts before it uses an entry, and CI rebuilds from
clean and compares what it gets, since a wrong entry in a shared cache would be the worst bug a build
system can have. Entries about one program, and anything not named by its content, never leave the
machine.
