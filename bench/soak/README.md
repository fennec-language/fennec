# Soak test

Shows that a long-running native Fennec server doesn't leak. A small HTTP API with `shared` state, SQLite and file writes runs under steady load for a set time while some requests crash and others fail. The test passes when:

- memory stays flat after warm-up;
- latency is steady, minute by minute;
- no file descriptors leak;
- the server never stops, and each crash ends only its own request.

It isn't a required check. Run it by hand, under the watchdog, after changes to the runtime, the scheduler or how native code lets go of what it made:

```bash
cargo build -p fennec -p fennec-runtime
./target/debug/fennec build bench/soak/server.fen -o target/soak-server
perl watch.pl 700 4096 python3 -I bench/soak/soak.py target/soak-server --minutes 10 --out soak.json
```

## What it sends

Eight clients, each on a kept-alive connection, send a weighted mix:

| Request | Share | What it does |
|---|---:|---|
| `GET /count` | 40% | counts the request in a `shared` value |
| `POST /note?n=N` | 22% | a SQLite transaction: insert note N, delete notes 2000 older |
| `POST /note?n=-1` | 3% | the same note again: fails while it's there, answered 500 |
| `POST /file?n=N` | 20% | writes the body to one of 64 files, then reads it back |
| `GET /stats` | 10% | reads the `shared` value under its lock |
| `GET /crash?n=N` | 1% | overflows an `int`: crashes its request, which closes its connection |
| not HTTP | 2% | answered 400 without the handler running |
| half a request | 2% | the client hangs up partway |

`--only count,file` sends only those kinds, to find which one leaks.

## What it measures

Every 5 seconds it takes the server's RSS and its open descriptors (from `lsof`). It also records each request's latency, the throughput, and every outcome. The report is JSON, with every sample.

**Warm-up:** each worker's caches fill up to a bound as it serves: the runtime keeps up to 8 MB of freed blocks for reuse, and the allocator keeps its pages. One worker reaches its bound after about 40,000 requests. The test therefore runs the server on 4 workers (`--threads`), so each reaches its bound within the run, and only judges memory after the first 40% of it. On every core, with 18 workers sharing the load, a worker's warm-up alone would take longer than 10 minutes.

## Results

First runs, 2026-11-08, on an 18-core M5 MacBook Pro, the native server on 4 workers:

| Run | Requests | Throughput | p50 | p99 | Descriptors | RSS |
|---|---:|---:|---:|---:|---:|---|
| 10 min, full mix | 442,317 | 737/s | 0.57 ms | 16.5 ms | 20–30 | 17 → 86 MB, still growing |
| 10 min, no crashes | 590,871 | 985/s | 0.32 ms | 12.4 ms | 20–29 | 21 → 66 MB |
| 30 min, no crashes | 1,640,645 | 912/s | 0.31 ms | 14.0 ms | 21–31 | 14 → 66 MB by minute 12, then 76 MB by minute 30 |

In every run the server never stopped, each crash ended only its own request and connection, and every other request was answered.

**What's left to fix:**
- **A crash leaks about 1.8 KB:** the frames' locals are never let go of, since native code's crash path returns without releasing them. At about 5,700 crashes a second alone it grows by 600 MB a minute. The fix is for native code to release live locals on the crash path, as a failure's early return does.
- **Without crashes, memory still creeps,** about 0.5 MB a minute by minute 30 and slowing. Part of it is the runtime's cache of freed blocks filling to its bound: a run with that cache cut to 32 KB per thread reached 38 MB at 10 minutes rather than 66. What's left isn't found yet. It isn't Fennec values: a capability call from native code leaves nothing behind in the runtime (`a_capability_call_from_native_code_leaves_nothing_behind`). HTTP serving, `shared` values and request bodies grew by nothing over 290,000 requests each.

Until both are fixed, `soak.py` reports a failure on memory, as it should.
