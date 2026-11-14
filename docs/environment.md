# Environment variables

Every `FENNEC_*` name the code uses is listed here, one line each. A test, `environment::every_fennec_name_in_the_code_is_listed`, fails when the code uses a name this page doesn't list, or this page lists a name the code no longer uses. A new switch therefore comes with its line here.

A switch is for something permanent: an option a user or CI sets, or the way `fennec` talks to a program it built. A knob added to debug or measure something comes out once that work is done.

## For people using Fennec

- `FENNEC_HOME`: Fennec's own folder, in place of `~/.fennec`. See [store.md](store.md).
- `FENNEC_CACHE_CAP`: the most bytes the store's cache holds; 1 GiB by default. See [store.md](store.md).
- `FENNEC_OFFLINE`: never fetch anything. The launcher passes it on to the toolchain.
- `FENNEC_TRUST`: folders, listed as `PATH` lists them, whose kits are trusted whoever owns them, as a project mounted into a container is the host user's. A folder anyone can write to still isn't. Without it, a kit is taken only through folders this user or the system owns.
- `FENNEC_DIST`: the mirror the launcher fetches releases from.
- `FENNEC_VERSION`: which release `install.sh` installs.
- `FENNEC_DOWNLOAD`: where `install.sh` downloads from.
- `FENNEC_STATIC`: `0` links a built program against the system's libc in place of musl.
- `FENNEC_THREADS`: how many workers a program's pool has; the number of cores by default.
- `FENNEC_THREADS_RUNTIME`: `0` or `1` picks the plain runtime or the one with threads, in place of what the program needs.
- `FENNEC_NO_CACHE`: build without the cache, and run every test rather than reusing results.
- `FENNEC_NO_DAEMON`: never start or use the daemon.
- `FENNEC_DAEMON_IDLE`: how many seconds the daemon stays up with nothing to do; 600 by default.
- `FENNEC_LLVM_BIN`: the folder holding LLVM's `opt` and `llc`.
- `FENNEC_LLVM_KEEP`: keeps the folder of LLVM text a build writes, to read what was emitted.

## For CI and the tests

- `FENNEC_CHECK_PROOFS`: `1` builds an assertion for every check a proof left out, so a wrong proof stops the program. It also checks, rather than hands to LLVM, every fact it can check where it runs: what's assumed, that a length is in range, and that a `ref` parameter's address is no other parameter's.
- `FENNEC_BOXED_RESULTS`: `1` builds every result as a block of its own, as before results were kept in place: the case's number, then the value or the error, in the caller's slot. It's there to tell the two apart if a result kept in place is ever wrong.
- `FENNEC_STRIP_FACTS`: `1` builds without the facts the compiler hands LLVM beside the code: `!range`, `!tbaa`, `!invariant.load`, `inbounds`, `nsw` and `nuw`, `noalias`, `captures(none)` and `llvm.assume`. The program runs the same, only slower. One that prints otherwise was handed a wrong fact, which is the only way to catch a wrong TBAA tag. The nightly proofs run builds everything both ways and compares.
- `FENNEC_SANITIZE`: `address` or `thread` builds programs with that sanitizer.
- `FENNEC_EXPECT_LLVM`: LLVM's tests fail rather than skip themselves when its tools are missing.
- `FENNEC_EXPECT_STATIC`: the static-link test fails rather than skips when musl is missing.
- `FENNEC_PARALLEL_STRESS`: every parallel loop runs on the pool, split one item a range.
- `FENNEC_STREAM_RUN_AHEAD`: set, a parallel stream's reader waits for the workers to run every chunk in its window before it takes one, so chunks always run ahead of it, as a test of what runs past a `break` needs. Off on a worker and in a replay.
- `FENNEC_UNDER_MUTANTS`: slow tests sample fewer inputs under cargo-mutants.
- `FENNEC_TEST_POSTGRES`: the URL of a real Postgres for `std.postgres`'s tests.
- `FENNEC_PIN_OUTPUTS`: rewrites `crates/fennec/tests/fixtures/pinned` from this build's results.
- `FENNEC_UPDATE_UNCHECKED`: `1` rewrites `crates/llvm/unchecked_runtime_calls.txt`, the ratchet of runtime calls emitted code doesn't check yet, from what the lint finds now.
- `FENNEC_RUN_WAITS`: `fennec run` waits for the program rather than becoming it, so its time can be measured.
- `FENNEC_NEW_EXECUTABLES`: a file every executable a build makes as a new file is added to, a line each, so the gate can count them: macOS can hold a new file's first launch. One linked out of the store, the same file as one launched before, isn't added.
- `FENNEC_LINK_KEEP`: a build keeps the objects it links, whole, in the folder this names, for the loader's tests to link them their own way and compare with the system linker's.
- `FENNEC_LIVE_REPORT`: a program writes the runtime's live counts to this file as it ends.
- `FENNEC_HASH_SEED`: a fixed seed for dict hashing, so a run's order repeats. `fennec test` sets it.
- `FENNEC_GATE_LOCK`: the lock file the pre-push hook takes, so the landing script's test can use its own.
- `FENNEC_DOCKER_CONTEXT`: the Docker context the gate's Linux check runs in, in place of `colima-fennec`, Fennec's own VM. See [landing.md](landing.md).
- `FENNEC_LINUX_CHECK_HELD`: set by the gate's Linux check for itself, run again under its lock: the lock's file descriptor.
- `FENNEC_PARKED_STOP`: a parked-code test runs itself again with this set, to crash on purpose.
- `FENNEC_CONNECTIONS`: how many connections an ignored serve memory test opens, run by hand.
- `FENNEC_AI_TEST_BASE_URL`: the host `tools/ai-test` sends its requests to.
- `FENNEC_AI_TEST_MODEL`: the model `tools/ai-test` uses when `--model` isn't given.

## Between `fennec` and the programs it builds

- `FENNEC_COUNTS`: the program says, as it ends, what it allocated, copied and wrote. `fennec dev copies` sets it.
- `FENNEC_RECORD`: the program records its inputs to this file, for `fennec run --record`.
- `FENNEC_REPLAY`: the program replays the inputs recorded in this file, for `fennec replay`.
- `FENNEC_RUN_AS`: the name `fennec run` gives the program it runs.
- `FENNEC_TESTS_REAL`: a test binary uses real capabilities, for `fennec test --real`.
- `FENNEC_TRACE_FILE`: where the program writes what each site cost, for `fennec dev trace`.

## Set while building Fennec

- `FENNEC_BUILD`: a hash of the sources, stamped on the cache, the daemon and the runtime so a stale one is never used.
- `FENNEC_TARGET`: the target the launcher is built for.

## Names that aren't switches

- `FENNEC_DEADLINE`: a constant in the tests, how long a child process may take.
- `FENNEC_DATABASE_URL`, `FENNEC_FOX`, `FENNEC_NO_SUCH_VARIABLE`: names that Fennec programs in tests and examples read or set, to test `env`.

## Decided, not built yet

- `FENNEC_LOG`: per-part log levels for `Log.from_env`. See [decisions.md](decisions.md).
