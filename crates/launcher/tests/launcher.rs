//! What the launcher installs and runs. Each test makes a release in a folder, as CI would, with
//! a toolchain whose `fennec` is a script saying what it was asked, and signs it with keys made
//! for the test.

#[cfg(test)]
#[cfg(unix)]
#[path = "common/signing.rs"]
mod signing;

// The helpers unwrap and index as freely as the tests themselves.
#[cfg(test)]
#[cfg(unix)]
mod launcher {
    use std::path::{Path, PathBuf};

    use fennec_dist::{Kind, Manifest, PublicKey};
    use fennec_launcher::Launcher;
    use ring::signature::Ed25519KeyPair;

    const TARGET: &str = "aarch64-apple-darwin";

    /// A folder of its own for a test, empty.
    fn scratch(name: &str) -> Scratch {
        // Linux won't start a file while anything holds it open to write, and a process another
        // test thread starts holds a copy of every open file until it runs its program. So a test
        // that writes a toolchain and starts it could find it busy: there, these tests take
        // turns. macOS has no such rule, and checks each new program the first time it starts,
        // one at a time, so taking turns there would only make them slow.
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let turn =
            cfg!(target_os = "linux").then(|| TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        let dir = std::env::temp_dir().join(format!("fennec-launcher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch { dir: fennec_temp::Temp::at(std::fs::canonicalize(dir).unwrap()), _turn: turn }
    }

    /// A test's folder, held with its turn.
    struct Scratch {
        dir: fennec_temp::Temp,
        _turn: Option<std::sync::MutexGuard<'static, ()>>,
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.dir
        }
    }

    impl AsRef<Path> for Scratch {
        fn as_ref(&self) -> &Path {
            &self.dir
        }
    }

    use super::signing::sign;

    /// A key made for a test, and its public key as the launcher trusts one.
    fn key() -> (Ed25519KeyPair, PublicKey) {
        super::signing::made_key().unwrap()
    }

    /// A tar's header for an entry.
    fn header(name: &str, size: usize, mode: u32, kind: u8) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(format!("{mode:07o}").as_bytes());
        header[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        header
    }

    /// A `.tar.gz` of entries: a path, a mode and bytes, or a kind of entry with no bytes.
    fn tar_gz(entries: &[(&str, u32, &[u8], u8)]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, mode, bytes, kind) in entries {
            tar.extend(header(name, bytes.len(), *mode, *kind));
            tar.extend_from_slice(bytes);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.resize(tar.len() + 1024, 0);
        let mut gzip = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        gzip.extend(miniz_oxide::deflate::compress_to_vec(&tar, 6));
        gzip.extend_from_slice(&[0; 8]);
        gzip
    }

    /// The `fennec` of a test's toolchain: a script that says its version and what it was asked.
    fn script(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho \"fennec {version}: $*\"\nexit 3\n").into_bytes()
    }

    /// A release of `version` in `dist`, of the archive given, signed by `pair`.
    fn release_of(dist: &Path, version: &str, archive: &[u8], pair: &Ed25519KeyPair) {
        release_as(dist, version, archive, "tar.gz", pair);
    }

    /// Like `release_of`, with an archive of the kind `extension` names.
    fn release_as(dist: &Path, version: &str, archive: &[u8], extension: &str, pair: &Ed25519KeyPair) {
        let folder = dist.join(format!("v{version}"));
        std::fs::create_dir_all(&folder).unwrap();
        let file = format!("fennec-v{version}-{TARGET}.{extension}");
        std::fs::write(folder.join(&file), archive).unwrap();
        let manifest =
            Manifest::of_archives(Kind::Toolchain, version, &[(file, archive.to_vec())]).unwrap().write();
        let name = format!("fennec-v{version}.manifest");
        std::fs::write(folder.join(&name), &manifest).unwrap();
        std::fs::write(
            folder.join(format!("{name}.sig")),
            sign(pair, "fennec-toolchain", manifest.as_bytes()),
        )
        .unwrap();
    }

    /// A release of `version` whose toolchain is a script, signed by `pair`.
    fn release(dist: &Path, version: &str, pair: &Ed25519KeyPair) {
        let top = format!("fennec-v{version}-{TARGET}");
        let archive = tar_gz(&[
            (&format!("{top}/"), 0o755, b"", b'5'),
            (&format!("{top}/fennec"), 0o755, &script(version), b'0'),
            (&format!("{top}/licenses/notice.txt"), 0o644, b"notice", b'0'),
        ]);
        release_of(dist, version, &archive, pair);
    }

    fn launcher(dir: &Path, trusted: &PublicKey) -> Launcher {
        Launcher {
            home: dir.join("home"),
            dist: dir.join("dist").display().to_string(),
            offline: false,
            trusted: vec![trusted.clone()],
            target: TARGET.to_owned(),
        }
    }

    fn kit(dir: &Path, version: &str) -> PathBuf {
        let kit = dir.join("game");
        std::fs::create_dir_all(kit.join("src")).unwrap();
        std::fs::write(
            kit.join("kit.toml"),
            format!("name = \"game\"\nversion = \"0.1.0\"\nfennec = \"{version}\"\n"),
        )
        .unwrap();
        kit
    }

    fn ran(command: &Path, arg: &str) -> (String, Option<i32>) {
        let output = std::process::Command::new(command).arg(arg).output().unwrap();
        (String::from_utf8_lossy(&output.stdout).into_owned(), output.status.code())
    }

    #[test]
    fn a_kit_gets_its_toolchain_once_pins_it_and_runs_it_from_then_on() {
        let dir = scratch("first");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        let kit = kit(&dir, "0.4.2");
        let launcher = launcher(&dir, &public);
        let command = launcher.toolchain_for(&kit.join("src")).unwrap();
        assert_eq!(ran(&command, "run"), ("fennec 0.4.2: run\n".to_owned(), Some(3)));
        let lock = std::fs::read_to_string(kit.join("kit.lock")).unwrap();
        let manifest = std::fs::read(dir.join("dist/v0.4.2/fennec-v0.4.2.manifest")).unwrap();
        assert!(
            lock.ends_with(&format!("toolchain 0.4.2 {}\n", fennec_blake3::hash(&manifest).to_hex())),
            "{lock}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("home/toolchains/0.4.2/licenses/notice.txt")).unwrap(),
            "notice"
        );
        // Nothing is fetched again: the release can be gone, and the launcher can be offline.
        std::fs::remove_dir_all(dir.join("dist")).unwrap();
        let offline = Launcher { offline: true, ..launcher };
        assert_eq!(offline.toolchain_for(&kit).unwrap(), command);
        assert_eq!(offline.installed(), ["0.4.2"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn offline_nothing_is_fetched() {
        let dir = scratch("offline");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        let kit = kit(&dir, "0.4.2");
        let offline = Launcher { offline: true, ..launcher(&dir, &public) };
        assert_eq!(
            offline.toolchain_for(&kit).unwrap_err(),
            "Fennec 0.4.2 isn't installed, and --offline says not to fetch it"
        );
        assert!(offline.installed().is_empty() && !kit.join("kit.lock").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_release_no_trusted_key_signed_is_never_installed() {
        let dir = scratch("untrusted");
        let ((pair, _), (_, trusted)) = (key(), key());
        release(&dir.join("dist"), "0.4.2", &pair);
        let kit = kit(&dir, "0.4.2");
        let launcher = launcher(&dir, &trusted);
        assert!(launcher.toolchain_for(&kit).unwrap_err().contains("signed by 0 of the 1 key "));
        assert!(launcher.installed().is_empty() && !kit.join("kit.lock").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_archive_changed_after_its_release_was_signed_is_never_installed() {
        let dir = scratch("changed");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        let archive = dir.join(format!("dist/v0.4.2/fennec-v0.4.2-{TARGET}.tar.gz"));
        let mut bytes = std::fs::read(&archive).unwrap();
        let last = bytes.len() - 9;
        bytes[last] ^= 1;
        std::fs::write(&archive, bytes).unwrap();
        let launcher = launcher(&dir, &public);
        assert!(launcher.toolchain_for(&kit(&dir, "0.4.2")).unwrap_err().contains("has the hash"));
        assert_eq!(launcher.installed(), Vec::<String>::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_kit_runs_only_the_toolchain_its_lock_pins() {
        let dir = scratch("pinned");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        let kit = kit(&dir, "0.4.2");
        let mine = launcher(&dir, &public);
        mine.toolchain_for(&kit).unwrap();
        // The same version released again, as another build: a teammate's machine fetching it
        // now gets a manifest the kit's lock doesn't pin, and refuses it.
        let elsewhere = Launcher { home: dir.join("teammate"), ..launcher(&dir, &public) };
        let top = format!("fennec-v0.4.2-{TARGET}");
        let other = tar_gz(&[(&format!("{top}/fennec"), 0o755, &script("0.4.2-other"), b'0')]);
        std::fs::remove_dir_all(dir.join("dist")).unwrap();
        release_of(&dir.join("dist"), "0.4.2", &other, &pair);
        assert!(elsewhere.toolchain_for(&kit).unwrap_err().contains("isn't the one kit.lock pins"));
        assert_eq!(elsewhere.installed(), Vec::<String>::new());
        // And one installed some other way isn't run either.
        let manifest = mine.home.join("toolchains/0.4.2/manifest");
        std::fs::write(&manifest, "fennec toolchain 0.4.2\n").unwrap();
        assert!(mine.toolchain_for(&kit).unwrap_err().contains("installed isn't the one kit.lock pins"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_fennec_update_moves_a_pin() {
        let dir = scratch("moved");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        release(&dir.join("dist"), "0.4.3", &pair);
        let kit = kit(&dir, "0.4.2");
        let launcher = launcher(&dir, &public);
        launcher.toolchain_for(&kit).unwrap();
        std::fs::write(kit.join("kit.toml"), "name = \"game\"\nversion = \"0.1.0\"\nfennec = \"0.4.3\"\n")
            .unwrap();
        assert_eq!(
            launcher.toolchain_for(&kit).unwrap_err(),
            "kit.toml asks for Fennec 0.4.3, and kit.lock pins 0.4.2: `fennec update` moves the pin"
        );
        assert_eq!(launcher.installed(), ["0.4.2"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn outside_a_kit_the_default_toolchain_runs() {
        let dir = scratch("default");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        let launcher = launcher(&dir, &public);
        let none = launcher.toolchain_for(&dir).unwrap_err();
        assert!(none.contains("`fennec default 0.4.2` sets one"), "{none}");
        let offline = Launcher { offline: true, ..self::launcher(&dir, &public) };
        assert!(offline.set_default("0.4.2").unwrap_err().contains("isn't installed"));
        // Setting one fetches it if it has to.
        launcher.set_default("0.4.2").unwrap();
        assert_eq!(launcher.default_version().as_deref(), Some("0.4.2"));
        assert_eq!(ran(&launcher.toolchain_for(&dir).unwrap(), "x").0, "fennec 0.4.2: x\n");
        launcher.remove("0.4.2").unwrap();
        assert_eq!(launcher.installed(), Vec::<String>::new());
        assert!(launcher.remove("0.4.2").is_err() && launcher.remove("../home").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_archive_reaching_outside_its_folder_is_never_unpacked() {
        let dir = scratch("outside");
        let (pair, public) = key();
        let top = format!("fennec-v0.4.2-{TARGET}");
        let fennec = (format!("{top}/fennec"), 0o755, script("0.4.2"), b'0');
        for (name, kind) in
            [(format!("{top}/../evil"), b'0'), ("evil".to_owned(), b'0'), (format!("{top}/link"), b'2')]
        {
            let archive =
                tar_gz(&[(&fennec.0, fennec.1, &fennec.2, fennec.3), (&name, 0o644, b"evil", kind)]);
            let _ = std::fs::remove_dir_all(dir.join("dist"));
            release_of(&dir.join("dist"), "0.4.2", &archive, &pair);
            let launcher = launcher(&dir, &public);
            let refused = launcher.ensure("0.4.2", None).unwrap_err();
            assert!(
                refused.contains("isn't in its folder") || refused.contains("is a link"),
                "{name}: {refused}"
            );
            assert!(
                launcher.installed().is_empty()
                    && !dir.join("evil").exists()
                    && !dir.join("home/toolchains/evil").exists()
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_installed_launcher_trusts_only_the_keys_built_in_and_hands_off_as_it_is() {
        let dir = scratch("command");
        let (pair, _) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        let kit = kit(&dir, "0.4.2");
        let fennec = |args: &[&str]| {
            std::process::Command::new(env!("CARGO_BIN_EXE_fennec-launcher"))
                .args(args)
                .current_dir(&kit)
                .env("FENNEC_HOME", dir.join("home"))
                .env("FENNEC_DIST", dir.join("dist"))
                .env_remove("FENNEC_OFFLINE")
                .output()
                .unwrap()
        };
        let refused = fennec(&["run"]);
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(!refused.status.success() && said.contains("signed by 0 of the 0 keys"), "{said}");
        // One installed and pinned another way, here by a launcher trusting a test's key, is run,
        // with the arguments and exit code as they are. Only a leading `--offline` is the launcher's.
        let (signer, trusted) = key();
        release(&dir.join("other"), "0.4.2", &signer);
        let by_hand = Launcher { dist: dir.join("other").display().to_string(), ..launcher(&dir, &trusted) };
        by_hand.toolchain_for(&kit).unwrap();
        let ran = fennec(&["--offline", "run", "--offline", "main.fen"]);
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "fennec 0.4.2: run --offline main.fen\n");
        assert_eq!(ran.status.code(), Some(3));
        let listed = fennec(&["toolchain", "list"]);
        assert_eq!(String::from_utf8_lossy(&listed.stdout), "0.4.2\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A toolchain as if installed, whose `fennec` says what it was asked.
    fn installed(home: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let folder = home.join("toolchains").join(version);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("manifest"), format!("fennec toolchain {version}\n")).unwrap();
        std::fs::write(folder.join("fennec"), script(version)).unwrap();
        std::fs::set_permissions(folder.join("fennec"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The launcher's output and exit code, run in `at` with `home` as Fennec's folder.
    fn run_in(at: &Path, home: &Path, args: &[&str]) -> (String, String, Option<i32>) {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_fennec-launcher"))
            .args(args)
            .current_dir(at)
            .env("FENNEC_HOME", home)
            .env("FENNEC_DIST", home.join("no releases here"))
            .env_remove("FENNEC_OFFLINE")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.code(),
        )
    }

    /// `+0.4.2` runs that version anywhere, and leaves what a kit pins alone.
    #[test]
    fn a_plus_runs_that_version_this_once_and_changes_no_pin() {
        let dir = scratch("plus");
        let home = dir.join("home");
        installed(&home, "0.4.2");
        installed(&home, "0.4.3");
        let kit = kit(&dir, "0.4.2");
        let (said, _, _) = run_in(&kit, &home, &["+0.4.3", "build", "--offline"]);
        assert_eq!(said, "fennec 0.4.3: build --offline\n");
        assert!(!kit.join("kit.lock").exists());
        let (said, _, _) = run_in(&dir, &home, &["--offline", "+0.4.2", "run"]);
        assert_eq!(said, "fennec 0.4.2: run\n");
        let (_, why, code) = run_in(&dir, &home, &["+0.9.9", "--offline", "run"]);
        assert_eq!(code, Some(1));
        assert!(why.contains("0.9.9 isn't installed") || why.contains("no releases here"), "{why}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `--version` chooses for a script outside a kit, and a kit's `kit.toml` is what chooses in one.
    #[test]
    fn a_version_flag_works_only_outside_a_kit() {
        let dir = scratch("version-flag");
        let home = dir.join("home");
        installed(&home, "0.4.2");
        let loose = dir.join("scripts");
        std::fs::create_dir_all(&loose).unwrap();
        let (said, _, _) = run_in(&loose, &home, &["--version", "0.4.2", "run", "x.fen", "--version", "1"]);
        assert_eq!(said, "fennec 0.4.2: run x.fen --version 1\n");
        let kit = kit(&dir, "0.4.2");
        let inside = kit.join("src");
        let (_, why, code) = run_in(&inside, &home, &["--version", "0.4.2", "run"]);
        assert_eq!(code, Some(1));
        assert!(why.contains("`--version` is for scripts outside a kit"), "{why}");
        assert!(why.contains("+0.4.2"), "{why}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The version for scripts outside a kit, set once; with none set, it says how.
    #[test]
    fn fennec_default_sets_the_version_for_scripts() {
        let dir = scratch("default-command");
        let home = dir.join("home");
        installed(&home, "0.4.2");
        let loose = dir.join("scripts");
        std::fs::create_dir_all(&loose).unwrap();
        let (_, why, code) = run_in(&loose, &home, &["run", "x.fen"]);
        assert_eq!(code, Some(1));
        assert!(why.contains("`fennec default 0.4.2` sets one"), "{why}");
        let (_, why, code) = run_in(&loose, &home, &["default"]);
        assert_eq!(code, Some(1), "{why}");
        assert_eq!(run_in(&loose, &home, &["default", "0.4.2"]).2, Some(0));
        assert_eq!(run_in(&loose, &home, &["default"]).0, "0.4.2\n");
        assert_eq!(run_in(&loose, &home, &["run", "x.fen"]).0, "fennec 0.4.2: run x.fen\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A build of your own, linked as `dev`, runs for a kit asking for it, and for `+dev`, with
    /// nothing fetched or pinned; unlinking it leaves the build where it is.
    #[test]
    fn a_linked_build_runs_as_it_is() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("link");
        let home = dir.join("home");
        let build = dir.join("checkout/target/release");
        std::fs::create_dir_all(&build).unwrap();
        let kit = kit(&dir, "dev");
        let (_, why, code) = run_in(&kit, &home, &["run"]);
        assert_eq!(code, Some(1));
        assert!(why.contains("`fennec toolchain link dev <path>`"), "{why}");
        let (_, why, code) = run_in(&dir, &home, &["toolchain", "link", "dev", &build.to_string_lossy()]);
        assert_eq!(code, Some(1));
        assert!(why.contains("there's no `fennec`"), "{why}");
        std::fs::write(build.join("fennec"), script("dev")).unwrap();
        std::fs::set_permissions(build.join("fennec"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let fennec = build.join("fennec").to_string_lossy().into_owned();
        assert_eq!(run_in(&dir, &home, &["toolchain", "link", "dev", &fennec]).2, Some(0));
        assert_eq!(run_in(&kit, &home, &["run", "x"]).0, "fennec dev: run x\n");
        assert!(!kit.join("kit.lock").exists());
        assert_eq!(run_in(&dir, &home, &["+dev", "build"]).0, "fennec dev: build\n");
        let listed = run_in(&dir, &home, &["toolchain", "list"]).0;
        assert_eq!(listed, format!("dev → {}\n", build.display()));
        assert_eq!(run_in(&dir, &home, &["toolchain", "remove", "dev"]).2, Some(0));
        assert!(build.join("fennec").is_file());
        assert_eq!(run_in(&kit, &home, &["run"]).2, Some(1));
        let (_, why, _) = run_in(&dir, &home, &["toolchain", "link", "Dev", &fennec]);
        assert!(why.contains("can't name a linked build"), "{why}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The release workflow makes its archives with `tar czf`, which on macOS adds headers for
    /// files' attributes and `._` files, and on Linux GNU's long names.
    #[test]
    fn an_archive_made_by_the_systems_tar_unpacks() {
        let dir = scratch("tar");
        let (pair, public) = key();
        let top = format!("fennec-v0.4.2-{TARGET}");
        let deep = dir.join(&top).join("llvm").join("a".repeat(60)).join("b".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.join(&top).join("fennec"), script("0.4.2")).unwrap();
        std::fs::write(deep.join("tool"), b"tool").unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir.join(&top).join("fennec"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let made = std::process::Command::new("tar")
            .arg("czf")
            .arg("archive.tar.gz")
            .arg(&top)
            .current_dir(&dir)
            .status();
        assert!(made.unwrap().success());
        let archive = std::fs::read(dir.join("archive.tar.gz")).unwrap();
        release_of(&dir.join("dist"), "0.4.2", &archive, &pair);
        let launcher = launcher(&dir, &public);
        launcher.ensure("0.4.2", None).unwrap();
        assert_eq!(ran(&launcher.command("0.4.2"), "x").0, "fennec 0.4.2: x\n");
        let installed = launcher.home.join("toolchains/0.4.2/llvm").join("a".repeat(60)).join("b".repeat(60));
        assert_eq!(std::fs::read(installed.join("tool")).unwrap(), b"tool");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Serves the files under `dir` over plain HTTP on this machine, as a release page does: each
    /// archive by a redirect to where it really is, and the rest whole, in chunks.
    fn serve(dir: PathBuf) -> String {
        use std::io::{BufRead as _, BufReader, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                while reader.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line.split(' ').nth(1).unwrap_or("/").to_owned();
                let mut stream = stream;
                if let Some(real) = path.strip_prefix("/elsewhere") {
                    let bytes = std::fs::read(dir.join(real.trim_start_matches('/'))).unwrap_or_default();
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
                    for chunk in bytes.chunks(1000) {
                        let _ = write!(stream, "{:x}\r\n", chunk.len());
                        let _ = stream.write_all(chunk);
                        let _ = stream.write_all(b"\r\n");
                    }
                    let _ = stream.write_all(b"0\r\n\r\n");
                } else if path.ends_with(".tar.gz") {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nLocation: /elsewhere{path}\r\nContent-Length: 0\r\n\r\n"
                    );
                } else if let Ok(bytes) = std::fs::read(dir.join(path.trim_start_matches('/'))) {
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", bytes.len());
                    let _ = stream.write_all(&bytes);
                } else {
                    let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                }
            }
        });
        address
    }

    #[test]
    fn a_release_is_fetched_from_a_mirror_through_its_redirects() {
        let dir = scratch("web");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        let mirror = Launcher { dist: format!("{}/", serve(dir.join("dist"))), ..launcher(&dir, &public) };
        let kit = kit(&dir, "0.4.2");
        assert_eq!(ran(&mirror.toolchain_for(&kit).unwrap(), "x").0, "fennec 0.4.2: x\n");
        assert!(mirror.ensure("0.4.3", None).unwrap_err().contains("404"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fennec_update_moves_the_pin_to_what_kit_toml_asks_for() {
        let dir = scratch("update");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        release(&dir.join("dist"), "0.4.3", &pair);
        let kit = kit(&dir, "0.4.2");
        let launcher = launcher(&dir, &public);
        assert_eq!(launcher.update(&kit, false).unwrap(), "kit.lock: Fennec 0.4.2\n");
        assert_eq!(launcher.update(&kit, false).unwrap(), "kit.lock pins Fennec 0.4.2 already\n");
        std::fs::write(kit.join("kit.toml"), "name = \"game\"\nversion = \"0.1.0\"\nfennec = \"0.4.3\"\n")
            .unwrap();
        assert!(launcher.toolchain_for(&kit).is_err());
        assert_eq!(launcher.update(&kit, false).unwrap(), "kit.lock: Fennec 0.4.2 → 0.4.3\n");
        assert_eq!(ran(&launcher.toolchain_for(&kit).unwrap(), "x").0, "fennec 0.4.3: x\n");
        assert!(launcher.update(&dir, false).unwrap_err().contains("isn't in a kit"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_latest_asks_whats_newest_and_the_answer_has_to_be_signed() {
        let dir = scratch("latest");
        let (pair, public) = key();
        release(&dir.join("dist"), "0.4.2", &pair);
        release(&dir.join("dist"), "0.4.3", &pair);
        std::fs::write(dir.join("dist/latest"), "0.4.3\n").unwrap();
        let kit = kit(&dir, "0.4.2");
        let fresh = launcher;
        let launcher = fresh(&dir, &public);
        launcher.toolchain_for(&kit).unwrap();
        assert_eq!(launcher.latest().unwrap(), "0.4.3");
        // Asking moves nothing: the kit runs what it's pinned to until `fennec update --latest`.
        assert_eq!(ran(&launcher.toolchain_for(&kit).unwrap(), "x").0, "fennec 0.4.2: x\n");
        assert_eq!(launcher.update(&kit, true).unwrap(), "kit.lock: Fennec 0.4.2 → 0.4.3\n");
        assert!(std::fs::read_to_string(kit.join("kit.toml")).unwrap().contains("fennec = \"0.4.3\"\n"));
        assert_eq!(ran(&launcher.toolchain_for(&kit).unwrap(), "x").0, "fennec 0.4.3: x\n");
        let offline = Launcher { offline: true, ..fresh(&dir, &public) };
        assert!(offline.latest().unwrap_err().contains("--offline"));
        // A newest release nobody trusted signed isn't one.
        let (stranger, _) = key();
        release(&dir.join("dist"), "0.5.0", &stranger);
        std::fs::write(dir.join("dist/latest"), "0.5.0\n").unwrap();
        assert!(launcher.latest().unwrap_err().contains("signed by 0 of"));
        let mirror = Launcher { dist: serve(dir.join("dist")), ..fresh(&dir, &public) };
        assert!(mirror.latest().unwrap_err().contains("signed by 0 of"));
        std::fs::write(dir.join("dist/latest"), "0.4.3\n").unwrap();
        assert_eq!(mirror.latest().unwrap(), "0.4.3");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A zip of stored entries, each a name, the Unix mode it was made with, and its bytes, as a
    /// zip made on Unix holds them.
    fn stored_zip(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut zip = Vec::new();
        let mut listed = Vec::new();
        for (name, mode, bytes) in entries {
            let offset = u32::try_from(zip.len()).unwrap();
            let size = u32::try_from(bytes.len()).unwrap();
            let length = u16::try_from(name.len()).unwrap();
            zip.extend_from_slice(&[0x50, 0x4b, 3, 4, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            zip.extend_from_slice(&size.to_le_bytes());
            zip.extend_from_slice(&size.to_le_bytes());
            zip.extend_from_slice(&length.to_le_bytes());
            zip.extend_from_slice(&[0, 0]);
            zip.extend_from_slice(name.as_bytes());
            zip.extend_from_slice(bytes);
            listed.extend_from_slice(&[0x50, 0x4b, 1, 2, 20, 3, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            listed.extend_from_slice(&size.to_le_bytes());
            listed.extend_from_slice(&size.to_le_bytes());
            listed.extend_from_slice(&length.to_le_bytes());
            listed.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            listed.extend_from_slice(&(mode << 16).to_le_bytes());
            listed.extend_from_slice(&offset.to_le_bytes());
            listed.extend_from_slice(name.as_bytes());
        }
        let start = u32::try_from(zip.len()).unwrap();
        let count = u16::try_from(entries.len()).unwrap();
        let listed_size = u32::try_from(listed.len()).unwrap();
        zip.extend(listed);
        zip.extend_from_slice(&[0x50, 0x4b, 5, 6, 0, 0, 0, 0]);
        zip.extend_from_slice(&count.to_le_bytes());
        zip.extend_from_slice(&count.to_le_bytes());
        zip.extend_from_slice(&listed_size.to_le_bytes());
        zip.extend_from_slice(&start.to_le_bytes());
        zip.extend_from_slice(&[0, 0]);
        zip
    }

    #[test]
    fn a_zip_made_by_the_systems_zip_unpacks() {
        let dir = scratch("zip");
        let (pair, public) = key();
        let top = format!("fennec-v0.4.2-{TARGET}");
        std::fs::create_dir_all(dir.join(&top).join("llvm/bin")).unwrap();
        std::fs::write(dir.join(&top).join("fennec"), script("0.4.2")).unwrap();
        std::fs::write(dir.join(&top).join("llvm/bin/opt"), "opt ".repeat(1000)).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir.join(&top).join("fennec"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let made = std::process::Command::new("zip")
            .args(["-q", "-r", "archive.zip"])
            .arg(&top)
            .current_dir(&dir)
            .status();
        assert!(made.unwrap().success());
        release_as(
            &dir.join("dist"),
            "0.4.2",
            &std::fs::read(dir.join("archive.zip")).unwrap(),
            "zip",
            &pair,
        );
        let launcher = launcher(&dir, &public);
        launcher.ensure("0.4.2", None).unwrap();
        assert_eq!(ran(&launcher.command("0.4.2"), "x").0, "fennec 0.4.2: x\n");
        let opt = std::fs::read_to_string(launcher.home.join("toolchains/0.4.2/llvm/bin/opt")).unwrap();
        assert_eq!(opt, "opt ".repeat(1000));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_zip_reaching_outside_its_folder_is_never_unpacked() {
        let dir = scratch("zip-outside");
        let (pair, public) = key();
        let top = format!("fennec-v0.4.2-{TARGET}");
        let fennec = format!("{top}/fennec");
        let script = script("0.4.2");
        for (name, mode) in [
            (format!("{top}/../evil"), 0o100_644),
            (format!("{top}/link"), 0o120_777),
            (format!("{top}\\evil"), 0o100_644),
        ] {
            let archive = stored_zip(&[(&fennec, 0o100_755, &script), (&name, mode, b"evil")]);
            let _ = std::fs::remove_dir_all(dir.join("dist"));
            release_as(&dir.join("dist"), "0.4.2", &archive, "zip", &pair);
            let launcher = launcher(&dir, &public);
            let refused = launcher.ensure("0.4.2", None).unwrap_err();
            assert!(
                refused.contains("isn't in its folder") || refused.contains("is a link"),
                "{name}: {refused}"
            );
            assert_eq!(launcher.installed(), Vec::<String>::new());
        }
        let archive = stored_zip(&[(&fennec, 0o100_755, &script)]);
        let _ = std::fs::remove_dir_all(dir.join("dist"));
        release_as(&dir.join("dist"), "0.4.2", &archive, "zip", &pair);
        launcher(&dir, &public).ensure("0.4.2", None).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
