//! The allocator's settings fixed when it's built, read back from a process that uses it.

/// Does nothing but use the runtime, whose allocator mimalloc is: a process to start, for mimalloc to
/// say its settings.
#[test]
fn starts() {
    assert!(!fennec_runtime::SYMBOLS.is_empty());
}

/// mimalloc reserves its arenas 64 MiB at a time, as `.cargo/config.toml` builds it to.
#[test]
#[cfg(not(feature = "sanitize"))]
fn the_allocator_reserves_its_arenas_64_mib_at_a_time() -> std::io::Result<()> {
    let said = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "starts", "--test-threads=1"])
        .env("MIMALLOC_VERBOSE", "1")
        .output()?;
    let said = String::from_utf8_lossy(&said.stderr);
    assert!(said.contains("option 'arena_reserve': 65536 KiB"), "{said}");
    Ok(())
}
