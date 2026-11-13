//! A log cruncher, the twin of `log_crunch.fen`: the log's lines split into borrowed parts,
//! counted into `HashMap`s of structs and counters keyed by those parts or by owned routes, each
//! route's times in a `HashMap` of `Vec`s, each minute's counts cleared as it ends, and the report
//! written into a `String`.

use std::collections::HashMap;
use std::fmt::Write;
use std::hint::black_box;

const USERS: i64 = 3_000;
const SERVICES: [&str; 4] = ["api", "auth", "store", "search"];
const SECTIONS: [&str; 7] = ["users", "orders", "items", "carts", "search", "login", "health"];
const METHODS: [&str; 6] = ["GET", "GET", "GET", "POST", "PUT", "DELETE"];
const SLOW: i64 = 400;

/// What's been seen of one route.
#[derive(Default)]
struct RouteStats {
    requests: i64,
    errors: i64,
    bytes: i64,
}

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A number from 0 up to `top` taken from a generator's state.
fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

/// The log: one line per request, a few of them cut short as a crash would leave them.
fn log(seed: i64, count: i64) -> String {
    let mut state = seed;
    let mut clock = 9 * 3_600_000;
    let mut lines = Vec::new();
    for _ in 0..count {
        state = next(state);
        clock += below(state, 30);
        state = next(state);
        let roll = below(state, 1_000);
        state = next(state);
        let user = below(state, USERS);
        state = next(state);
        let took = below(state, 40) + if roll % 9 == 0 { below(state, 7) * 97 } else { 0 };
        let stamp = format!(
            "2026-11-12T{:02}:{:02}:{:02}.{:03}Z",
            clock / 3_600_000,
            clock / 60_000 % 60,
            clock / 1_000 % 60,
            clock % 1_000
        );
        if roll < 4 {
            lines.push(format!("{stamp} ERROR api GET /users/"));
            continue;
        }
        let section = SECTIONS[roll as usize % SECTIONS.len()];
        let mut path = format!("/{section}");
        if section == "users" || section == "orders" || section == "items" {
            let _ = write!(path, "/{}", user * 7 % 1_000);
            if roll % 5 == 0 {
                path.push_str("/history");
            }
        }
        let status = if roll < 25 {
            500
        } else if roll < 70 {
            404
        } else if roll % 6 == 3 {
            201
        } else {
            200
        };
        let level = if status == 500 {
            "ERROR"
        } else if status == 404 || took > SLOW {
            "WARN"
        } else {
            "INFO"
        };
        let method = METHODS[roll as usize % METHODS.len()];
        let service = SERVICES[(user % SERVICES.len() as i64) as usize];
        let sent = 200 + roll * 3 + user % 50;
        lines.push(format!("{stamp} {level} {service} {method} {path} {status} {took}ms bytes={sent} user=fox-{user}"));
    }
    lines.join("\n")
}

/// The route a path is for: each part that's a number becomes `:id`.
fn route_of(method: &str, path: &str) -> String {
    let mut route = format!("{method} ");
    for part in path.split('/') {
        if part.is_empty() {
            continue;
        }
        if part.bytes().all(|byte| byte.is_ascii_digit()) {
            route.push_str("/:id");
        } else {
            route.push('/');
            route.push_str(part);
        }
    }
    route
}

/// Which item of sorted `times` is at `percent`.
fn percentile(times: &[i64], percent: usize) -> i64 {
    times[(times.len() - 1) * percent / 100]
}

/// The minute's busiest route, the first by name among equals.
fn busiest(counts: &HashMap<String, i64>) -> String {
    let mut names: Vec<&String> = counts.keys().collect();
    names.sort();
    let mut best = "";
    let mut most = 0;
    for name in names {
        if counts[name] > most {
            best = name;
            most = counts[name];
        }
    }
    format!("{best} {most}")
}

fn main() {
    let started = std::time::Instant::now();
    let text = log(black_box(11), black_box(50_000));
    let mut levels: HashMap<&str, i64> = HashMap::new();
    let mut statuses: HashMap<i64, i64> = HashMap::new();
    let mut routes: HashMap<String, RouteStats> = HashMap::new();
    let mut times: HashMap<String, Vec<i64>> = HashMap::new();
    let mut users: HashMap<&str, i64> = HashMap::new();
    let mut minute: HashMap<String, i64> = HashMap::new();
    let mut minutes: Vec<String> = Vec::new();
    let mut current = "";
    let mut bad = 0;
    for line in text.split('\n') {
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 9 {
            bad += 1;
            continue;
        }
        let Ok(status) = parts[5].parse::<i64>() else {
            bad += 1;
            continue;
        };
        let Ok(took) = parts[6].strip_suffix("ms").unwrap_or(parts[6]).parse::<i64>() else {
            bad += 1;
            continue;
        };
        let Ok(sent) = parts[7].strip_prefix("bytes=").unwrap_or(parts[7]).parse::<i64>() else {
            bad += 1;
            continue;
        };
        let at = &parts[0][11..16];
        if at != current {
            if !minute.is_empty() {
                minutes.push(format!("{current} {} routes, busiest {}", minute.len(), busiest(&minute)));
            }
            minute.clear();
            current = at;
        }
        *levels.entry(parts[1]).or_insert(0) += 1;
        *statuses.entry(status).or_insert(0) += 1;
        let route = route_of(parts[3], parts[4]);
        if !routes.contains_key(&route) {
            routes.insert(route.clone(), RouteStats::default());
            times.insert(route.clone(), Vec::new());
        }
        let stats = routes.get_mut(&route).expect("added above");
        stats.requests += 1;
        stats.bytes += sent;
        if status >= 500 {
            stats.errors += 1;
        }
        times.get_mut(&route).expect("added above").push(took);
        let user = parts[8].strip_prefix("user=").unwrap_or(parts[8]);
        *users.entry(user).or_insert(0) += sent;
        *minute.entry(route).or_insert(0) += 1;
    }
    minutes.push(format!("{current} {} routes, busiest {}", minute.len(), busiest(&minute)));

    let total: i64 = routes.values().map(|stats| stats.requests).sum();
    let volume: i64 = routes.values().map(|stats| stats.bytes).sum();
    let mut report = format!("{total} requests, {bad} bad lines, {volume} bytes, {} users\n", users.len());
    let mut names: Vec<&str> = levels.keys().copied().collect();
    names.sort();
    for level in names {
        let _ = writeln!(report, "{level} {}", levels[level]);
    }
    let mut codes: Vec<i64> = statuses.keys().copied().collect();
    codes.sort();
    for status in codes {
        let _ = writeln!(report, "status {status} {}", statuses[&status]);
    }
    let mut names: Vec<&String> = routes.keys().collect();
    names.sort();
    for route in names {
        let stats = &routes[route];
        let ordered = times.get_mut(route).expect("every route has times");
        ordered.sort();
        let share = stats.errors as f64 * 100.0 / stats.requests as f64;
        let mut row = format!("{route}: {} requests, {share:.2}% errors, {} bytes", stats.requests, stats.bytes);
        let _ = write!(row, ", p50 {}ms", percentile(ordered, 50));
        let _ = write!(row, ", p90 {}ms", percentile(ordered, 90));
        let _ = write!(row, ", p99 {}ms", percentile(ordered, 99));
        report.push_str(&row);
        report.push('\n');
    }
    let slow: Vec<&String> =
        times.iter().filter(|(_, ordered)| percentile(ordered, 99) > SLOW).map(|(route, _)| route).collect();
    let _ = writeln!(report, "{} routes slower than {SLOW}ms at p99", slow.len());
    let mut top: Vec<(i64, &str)> = users.iter().map(|(user, amount)| (*amount, *user)).collect();
    top.sort();
    for (amount, user) in top.iter().rev().take(5) {
        let _ = writeln!(report, "top {user} {amount}");
    }
    for line in &minutes {
        report.push_str(line);
        report.push('\n');
    }
    print!("{report}");
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
