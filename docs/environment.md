# Environment variables

Every `FENNEC_*` name the code uses is listed here, one line each. A test, `environment::every_fennec_name_in_the_code_is_listed`, fails when the code uses a name this page doesn't list, or this page lists a name the code no longer uses. A new switch therefore comes with its line here.

A switch is for something permanent: an option a user or CI sets, or the way `fennec` talks to a program it built. A knob added to debug or measure something comes out once that work is done.

## For people using Fennec

- `FENNEC_HOME`: Fennec's own folder, in place of `~/.fennec`. See [store.md](store.md).
- `FENNEC_CACHE_CAP`: the most bytes the store's cache holds. By default it's 20 GiB, or a tenth of the space free on its disk if that's less. See [store.md](store.md).
- `FENNEC_OFFLINE`: never fetch anything. The launcher passes it on to the toolchain.
- `FENNEC_TRUST`: folders, listed as `PATH` lists them, whose kits are trusted whoever owns them, as a project mounted into a container is the host user's. A folder anyone can write to still isn't. Without it, a kit is taken only through folders this user or the system owns.
- `FENNEC_DIST`: the mirror the launcher fetches releases from.
- `FENNEC_LAUNCHER_DIST`: the mirror `fennec self update` fetches launcher releases from.
- `FENNEC_VERSION`: which release `install.sh` installs.
- `FENNEC_DOWNLOAD`: where `install.sh` downloads from.
- `FENNEC_LINKED_AS`: the name the launcher runs a linked build as, which it tells that build when it hands it `fennec new`. Set to `dev`, the new kit asks for `dev` rather than the build's version, of which there's no release to run.
- `FENNEC_STATIC`: `0` links a built program against the system's libc in place of musl.
- `FENNEC_THREADS`: how many workers a program's pool has; the number of cores by default. At most 256: asking for more makes 256 and says so on standard error, once. A pool the system won't give the program, as when it has run out of open files, is said once too, and `parallel` then runs its ranges one after another. A worker past the first that the system won't start stops the program, saying how many started and to ask for no more than that.
- `FENNEC_THREADS_RUNTIME`: `0` or `1` picks the plain runtime or the one with threads, in place of what the program needs.
- `FENNEC_NO_CACHE`: build without the cache, and run every test rather than reusing results.
- `FENNEC_TOOLCHAIN_STORE`: the toolchain's store, which every home reads the standard library's parse and the runtime libraries' stamps from, in place of `fennec-store` beside `fennec`; empty turns it off. See [store.md](store.md).
- `FENNEC_NO_DAEMON`: never start or use the daemon. `fennec build`, `fennec run` and `fennec test` then do their work in their own process, as they do whenever `CI` is set.
- `FENNEC_DAEMON_IDLE`: how many seconds the daemon stays up with nothing to do; 1800 by default. One that has served only one request goes after 60, or this if it's less.
- `FENNEC_BUILD_CHILD`: `1` has a build's lowering, code generation and link run in a process of its own (`fennec build-child`), whose memory goes when it ends, and fall back to the command's own process if that one can't start, crashes or doesn't start in time, saying why. The build daemon's builds always go to one, unless this is `0`: the daemon holds what each build kept of its analyses, as bytes, and hands them to the next. Otherwise a build is made in the command's process.
- `FENNEC_DAEMONS`: the folder where build daemons note themselves, for the cap of four a user runs at once; by default one of the user's own in the temp folder. Tests set it to keep their daemons apart, which also lets a daemon start for a `FENNEC_HOME` in the temp folder, where commands otherwise never start one.
- `FENNEC_COMPARE_KEPT`: the build daemon checks all of a kit again after every build that carried over what its last build's check found, and fails the build, as the compiler's mistake, on any difference. Without it, one build in 8 of a dev compiler does, picked by a hash of the files' texts, and a release compiler none. Set to `0`, none does, and only the daemon's check in the background after the build compares.
- `FENNEC_KEPT_RULE_OFF`: a dev compiler leaves out the rule it names for carrying a check over, so a test can see a wrong carried check caught. `bounds` is the one: a callee whose type parameters' bounds changed has its callers checked again. The dev compiler's own check, at emitting, of what the build's check handed on is left out too, so a wrong carried check gets as far as in a release compiler. A release compiler ignores it.
- `FENNEC_KEPT_BUDGET`: for tests: how many bytes the build daemon may hold once no build is under way, counted as they're allocated, in place of 4 GB. Past it, the kits built longest ago are let go of, and their next builds check all of them; if that isn't enough, so is what it holds of each kit's runs.
- `FENNEC_LLVM_BIN`: the folder holding LLVM's `opt` and `llc`.
- `FENNEC_LLVM_KEEP`: keeps the folder of LLVM text a build writes, to read what was emitted.
- `FENNEC_BUILD_JOBS`: how many functions a dev build compiles at once; the number of cores by default. `.cargo/config.toml` sets it to 2 for everything Cargo runs, the tests among them, since they run many builds at once.

## For CI and the tests

- `FENNEC_CHECK_PROOFS`: `1` builds an assertion for every check a proof left out, so a wrong proof stops the program. It also checks, rather than hands to LLVM, every fact it can check where it runs: what's assumed, that a length is in range, and that a `ref` parameter's address is no other parameter's.
- `FENNEC_BOXED_TYPES`: `1` lays types out as before every type that doesn't contain itself was held in place: a struct or enum holding a dict, an optional, a set, a closure or a type that's a block is a block too. It's there to tell the two apart if a type held in place is ever wrong, and it keys the build cache. `fennec-fuzz run programs` builds a quarter of its random programs a third time with it, which must do what the dev build laid out in place does, and the edit fuzzer builds some versions with it.
- `FENNEC_BLOCK_MESSAGES`: `1` sends each message on a channel as one word, a block made for a string or a struct, as before flat messages crossed as their words through each thread's buffer. It's there to tell the two apart if a message sent as its words is ever wrong, and it keys the build cache.
- `FENNEC_NAMED_CONN_CALLS`: `1` has a build call the methods std.net calls on a served connection for each request, like `_read_head` and `write`, by name through `fennec_net_call`, as before they had typed exports of their own (`fennec_conn_*`). It's there for the test that compares the two byte for byte, and it keys the build cache. A release compiler ignores it.
- `FENNEC_BOXED_DICT_VALUES`: `1` holds each struct a dict has as a value in a block of its own, with its address in the dict's entry, as before values were held in the entries themselves, each entry as wide as its key and its value. It's there to tell the two apart if a value held in an entry is ever wrong, and it keys the build cache.
- `FENNEC_BOXED_RESULTS`: `1` builds every result as a block of its own, as before results were kept in place: the case's number, then the value or the error, in the caller's slot. It's there to tell the two apart if a result kept in place is ever wrong.
- `FENNEC_STRIP_FACTS`: `1` builds without the facts the compiler hands LLVM beside the code: `!range`, `!tbaa`, `!invariant.load`, `inbounds`, `nsw` and `nuw`, `noalias`, `captures(none)` and `llvm.assume`. The program runs the same, only slower. One that prints otherwise was handed a wrong fact, which is the only way to catch a wrong TBAA tag. The nightly proofs run builds everything both ways and compares.
- `FENNEC_SANITIZE`: `address` or `thread` builds programs with that sanitizer.
- `FENNEC_AUDIT`: `1` builds a program for a heap audit and has it check its heap as it ends: each live block's owner count against the holders found for it, and that a root leads to it. What's wrong is said on stderr, a line for each block, and the program exits with 87. The build keeps no native blocks, which the check couldn't see into, is told apart in a build's cache key, and links the runtime libraries built with their `audit` feature, `libfennec_runtime_audit.a` and `libfennec_runtime_threads_audit.a`, which no release has: without them the build is refused. A program built without it and run with it set says that it checked nothing and exits with 87 too, so a run that was meant to be audited can't pass unaudited. A worker that leaves its last server is checked the same way, there and then: a served round's blocks end with it and are handed to no one, so one left over is in that worker's table and nowhere else. It's checked whatever else is parked on it, as when the server was run from inside a launched task: a task's own blocks are told apart and left out. It doesn't check blocks behind a `shared` value's lock. `.github/scripts/audit.sh` runs every example and kit this way, in CI's run on main every four hours and every night. `fennec-fuzz run programs` with it set builds and runs its random programs this way, and a line of the audit's fails the program that made it, as the nightly does for a quarter of an hour. `fennec-fuzz run replay` with it set does the same for its recorded and replayed tasks, as the nightly does for another quarter of an hour. `fennec-fuzz run http-audited` builds its HTTP server this way whatever it's run with, and a line of the server's audit, once it's stopped with `SIGTERM`, fails the input.
- `FENNEC_AUDIT_SELFTEST`: with `FENNEC_AUDIT`, the runtime leaves one block nothing holds just before the audit, which then has to name it and exit with 87. It checks that the audit runs at all. `rounds` leaves one for every round a server serves instead, on the worker that served it, to check that a worker's own blocks are looked at.
- `FENNEC_FAULTS`: exists only for the fuzzer and the tests. Installed builds ignore it: only a runtime library built with its `faults` cargo feature reads it, which no release or install has, so a program built by an installed `fennec` writes every file as it always does with it set, whatever it says. `llvm::a_shipped_runtime_never_reads_fennec_faults` checks that. In a runtime built with `faults`, it fails `std.fs`'s work on the disk where it says, as a full disk would, to fuzz writes that fail partway. Each rule is `work:n:error`, or `write:n:error:bytes`, split by commas, as `write:3:nospace:10,rename:1:io`: the nth `create`, `write`, `flush`, `sync` or `rename` of the run fails with `nospace`, `quota`, `readonly`, `denied`, `toolarge` or `io`, said in the same words on every system, or `kill` ends the process there. `crates/rt/src/stdlib/faults.rs` says what each counts. Rules that can't be read stop the run before `main`, with exit code 2.
- `FENNEC_FAULTED`: for `fennec-fuzz run fault-writes`, the `fennec` it builds with: a copy beside a runtime library built with the `faults` feature, plain and with `threads`, and no bitcode, so a `--release` build links that library too. Left out, the fuzzer uses `fennec` itself, and its canary run stops the target, since a runtime that ships reads no `FENNEC_FAULTS`. The fuzz workflow's Replay job builds the copy and sets it.
- `FENNEC_FAULTS_KILL_RULE`, `FENNEC_FAULTS_KILL_IN` and `FENNEC_FAULTS_KILL_WHOLE`: set by `fennec-rt`'s test of a run killed partway through a write for the copy of itself it starts: the rule that kills it, the folder it writes in, and whether it writes the file whole.
- `FENNEC_EXPECT_LLVM`: LLVM's tests fail rather than skip themselves when its tools are missing.
- `FENNEC_EXPECT_STATIC`: the static-link test fails rather than skips when musl is missing.
- `FENNEC_PARALLEL_STRESS`: every parallel loop runs on the pool, split one item a range.
- `FENNEC_STREAM_RUN_AHEAD`: set, a parallel stream's reader waits for the workers to run every chunk in its window before it takes one, so chunks always run ahead of it, as a test of what runs past a `break` needs. Off on a worker and in a replay.
- `FENNEC_TEST_SLOW_WAKE_MS`: a debug build's runtime holds a woken idle worker this many milliseconds before its slot says it's awake, so a test finds every worker asleep while a `parallel` call's first job is queued: the moment the deadlock check's look at the pools' queues is for. A release build never reads it.
- `FENNEC_TEST_BUILD_CHILD`: in a dev compiler, what a build's own process (`FENNEC_BUILD_CHILD`) does wrong, for the tests of its fallback and of what ends it: `crash` kills itself at once, `hang` never says it started, and `hang-building:<file>` starts what it builds, as a link stopped partway leaves it, writes its process id to the file and never ends, unless the file is there already. A release compiler never reads it.
- `FENNEC_UNDER_MUTANTS`: slow tests sample fewer inputs under cargo-mutants.
- `FENNEC_TEST_POSTGRES`: the URL of a real Postgres for `std.postgres`'s tests.
- `FENNEC_TEST_WORDS_ROLE`, `FENNEC_TEST_WORDS_LOG`, `FENNEC_TEST_WORDS_WORKERS`, `FENNEC_TEST_WORDS_WAY` and `FENNEC_TEST_WORDS_SAW`: what the runtime's test of messages sent as words gives a run of itself, recorded or replayed in a process of its own. They say: record or replay, the log, how many workers, words or today's calls, and the file to say what it saw in. Nothing else reads them.
- `FENNEC_TEST_REPORTING`: tells a run of the runtime's own test binary to be the one where two threads call `intrinsics.report` at once, for the test that reads its standard error and checks each line came out whole. Nothing else reads it.
- `FENNEC_TEST_CONN_ROLE`, `FENNEC_TEST_CONN_LOG`, `FENNEC_TEST_CONN_WAY` and `FENNEC_TEST_CONN_SAW`: what the runtime's test of a served connection's typed calls gives a run of itself, recorded or replayed in a process of its own. They say: record or replay, the log, the named calls or the typed ones, and the file to say what it saw in. Nothing else reads them.
- `FENNEC_TEST_LOOKUP_ROLE`, `FENNEC_TEST_LOOKUP_LOG` and `FENNEC_TEST_LOOKUP_SAW`: what the runtime's test of a name's lookup that timed out gives a run of itself, recorded or replayed in a process of its own. They say: record or replay, the log, and the file to say what it saw in. Nothing else reads them.
- `FENNEC_PIN_OUTPUTS`: rewrites `crates/fennec/tests/fixtures/pinned` from this build's results.
- `FENNEC_UPDATE_UNCHECKED`: `1` rewrites `crates/llvm/unchecked_runtime_calls.txt`, the ratchet of runtime calls emitted code doesn't check yet, from what the lint finds now.
- `FENNEC_RUN_WAITS`: `fennec run` waits for the program rather than becoming it, so its time can be measured.
- `FENNEC_NEW_EXECUTABLES`: a file every executable a build makes as a new file is added to, a line each, so the gate can count them: macOS can hold a new file's first launch. One linked out of the store, the same file as one launched before, isn't added.
- `FENNEC_GATE_LOCK_HOLDER`: set by `open-pr.sh` for the gate it runs, to its own pid: the process holding the worktree's gate lock, which `gate.sh` goes on under rather than taking it again. One gate runs in a worktree at a time (`.github/scripts/gate-lock.sh`).
- `FENNEC_KEEP_FAILED`: `1` has a test that fails keep the temporary folder it made, and say on standard error where, rather than remove it: the gate's tests step sets it, and keeps each such folder, with the failed step's whole log and every panic, in `~/.fennec/gate-logs`.
- `FENNEC_LINK_KEEP`: a build keeps the objects it links, whole, in the folder this names, for the loader's tests to link them their own way and compare with the system linker's.
- `FENNEC_LOSE_KEPT_OBJECTS`: `1` has a dev build remove every kept object it found just before it links, as a cleanup another command runs can, for a test to check that each is compiled again and the build still makes the same program.
- `FENNEC_CHECK_UNITS`: `1` has a dev build check the index of kept units it brought up to date in place against one made afresh from what it kept, and stop if they differ. A dev compiler does this on one build in eight, picked by the program's functions as they are, so each edit draws afresh; this asks for it on every build.
- `FENNEC_KEPT_ANALYSIS_RULE_OFF`: for tests: the parts of the rule for using a kept analysis again that a build leaves out, by name, comma-separated (`ranges`: whether its parameters' ranges are as they were), so a wrong one is used, for the daemon's check after the build to find. A release compiler ignores it.
- `FENNEC_CHECK_WHOLE`: `1` has a build analyze every function again when it used any function's analysis kept from the build before in the same process, as the build daemon keeps them, and stop if one isn't what analyzing it again gives. The build then says how many it used again, of how many. A dev compiler does this on one build in eight, picked by the program; this asks for it on every build, with any compiler. `0` has no build do so, for a test of the check the daemon makes after a build that used analyses again.
- `FENNEC_CHECK_SIGNS`: `1` has a build work out which ints can't be negative a second way, walking every function round after round as it did before the worklist, and stop if the two differ. What's proven there leaves bounds checks out, so this checks the worklist on a program a test doesn't have.
- `FENNEC_CACHE_AUDIT`: `1` has a dev build write each unit it uses again from the last build anyway, and fail if it doesn't come out as it was kept: its text, what it shares, every kind it asks the layouts for, in order, and every read it makes of the whole program. For a test to check that a unit is only used again when it would come out the same.
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
- `FENNEC_TRACE_FILE`: where the program writes what each site cost on each kind of thread (main, a task, a parallel worker or a server worker), for `fennec dev trace`.

## Set while building Fennec

- `FENNEC_BUILD`: a hash of the sources, stamped on the cache, the daemon and the runtime so a stale one is never used.
- `FENNEC_TARGET`: the target the launcher is built for.

## Names that aren't switches

- `FENNEC_DEADLINE`: a constant in the tests, how long a child process may take.
- `FENNEC_DATABASE_URL`, `FENNEC_FOX`, `FENNEC_NO_SUCH_VARIABLE`: names that Fennec programs in tests and examples read or set, to test `env`.
- `FENNEC_STD_PRIVATE`: read by nothing. The test that `fennec` refuses to import std's private names sets it, to show that no variable lets a program in: only `fennec-std-tests`, a command only the repository's test builds have, imports them.

## Decided, not built yet

- `FENNEC_LOG`: per-part log levels for `Log.from_env`. See [decisions.md](decisions.md).
