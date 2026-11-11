//! Kits from git and from paths: pinned in kit.lock, kept in the store, and fetched without running
//! anything, in repositories made for each test.

#[cfg(test)]
mod deps {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use fennec_deps::{Kits, pins};

    /// A fresh folder for one test.
    fn folder(name: &str) -> fennec_temp::Temp {
        let dir = std::env::temp_dir().join(format!("fennec-deps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // As `git` takes it: on Windows, not in the verbatim form `canonicalize` gives.
        fennec_temp::Temp::at(fennec_deps::plain(&dir.canonicalize().unwrap()))
    }

    fn git(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args([
                "-c",
                "user.name=Fennec",
                "-c",
                "user.email=fennec@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A repository with the files given, committed, and the commit's hash.
    fn committed(dir: &Path, files: &[(&str, &str)]) -> String {
        std::fs::create_dir_all(dir).unwrap();
        if !dir.join(".git").exists() {
            git(dir, &["init", "--quiet", "--initial-branch=main"]);
        }
        commit(dir, files)
    }

    fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
        for (path, text) in files {
            write(&dir.join(path), text);
        }
        git(dir, &["add", "--all"]);
        git(dir, &["commit", "--quiet", "--allow-empty", "-m", "a change"]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn kit_toml(name: &str, dependencies: &str) -> String {
        format!(
            "name = \"{name}\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n\n[dependencies]\n{dependencies}"
        )
    }

    /// A folder's address for git: `file:///C:/...` on Windows, `file:///...` elsewhere.
    fn file_url(path: &Path) -> String {
        let path = path.display().to_string().replace('\\', "/");
        let path = path.strip_prefix("//?/").unwrap_or(&path);
        if path.starts_with('/') { format!("file://{path}") } else { format!("file:///{path}") }
    }

    fn from_git(name: &str, repository: &Path, commit: &str) -> String {
        format!("{name} = {{ git = \"{}\", commit = \"{commit}\" }}\n", file_url(repository))
    }

    /// A kit named `game` depending on what's given.
    fn game(dir: &Path, dependencies: &str) -> PathBuf {
        let root = dir.join("game");
        write(&root.join("kit.toml"), &kit_toml("game", dependencies));
        write(&root.join("src/main.fen"), "def main():\n    pass\n");
        root
    }

    /// The physics kit, with a file beside it that a kit doesn't take.
    fn physics(dir: &Path) -> (PathBuf, String) {
        let repository = dir.join("physics");
        let commit = committed(
            &repository,
            &[
                ("kit.toml", &kit_toml("physics", "")),
                ("src/lib.fen", "export def gravity() -> float:\n    return 9.8\n"),
                ("README.md", "not part of the kit\n"),
            ],
        );
        (repository, commit)
    }

    #[test]
    fn a_kit_from_git_is_pinned_by_hash_and_kept_whole_in_the_store() {
        let dir = folder("pinned");
        let (repository, commit) = physics(&dir);
        let root = game(&dir, &from_git("physics", &repository, &commit));
        let kits = Kits::new(&dir.join("home"), false);

        let unpinned = kits.ensure(&root).unwrap_err();
        assert!(unpinned.contains("kit.lock doesn't pin yet: `fennec update` fetches it"), "{unpinned}");

        let said = kits.update(&root).unwrap();
        assert!(said.starts_with("kit.lock: `physics` pinned to file://"), "{said}");
        assert!(said.ends_with(", 2 files\n"), "only kit.toml and src: {said}");
        let lock = std::fs::read_to_string(root.join("kit.lock")).unwrap();
        let pinned = pins(&lock).unwrap();
        assert_eq!(pinned.len(), 1);
        let pin = pinned.first().unwrap();
        assert_eq!((pin.name.as_str(), pin.commit.as_str()), ("physics", commit.as_str()));
        assert!(lock.starts_with("# What this kit builds with, by hash."), "{lock}");

        let stored = kits.stored(&pin.hash);
        assert!(stored.join("kit.toml").is_file() && stored.join("src/lib.fen").is_file());
        assert!(!stored.join("README.md").exists());

        let found = kits.ensure(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found.first().map(|found| &found.folder), Some(&stored));
        assert_eq!(kits.update(&root).unwrap(), "kit.lock pins every dependency already\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn moving_a_commit_needs_fennec_update_which_says_what_changed() {
        let dir = folder("moved");
        let (repository, first) = physics(&dir);
        let root = game(&dir, &from_git("physics", &repository, &first));
        let kits = Kits::new(&dir.join("home"), false);
        kits.update(&root).unwrap();

        let second = commit(
            &repository,
            &[
                ("src/lib.fen", "export def gravity() -> float:\n    return 9.81\n"),
                ("src/more.fen", "export def drag() -> float:\n    return 0.1\n"),
            ],
        );
        write(&root.join("kit.toml"), &kit_toml("game", &from_git("physics", &repository, &second)));
        let stale = kits.ensure(&root).unwrap_err();
        assert!(stale.contains("and kit.lock pins"), "{stale}");
        assert!(stale.ends_with("`fennec update` moves the pin"), "{stale}");

        let said = kits.update(&root).unwrap();
        let short = |commit: &str| commit.get(..12).unwrap().to_owned();
        assert!(said.contains(&format!("at {} → ", short(&first))), "{said}");
        assert!(said.contains(&format!("at {}\n", short(&second))), "{said}");
        assert!(
            said.contains("  changed src/lib.fen\n") && said.contains("  added src/more.fen\n"),
            "{said}"
        );
        assert_eq!(said.lines().count(), 3, "{said}");
        assert_eq!(kits.ensure(&root).unwrap().len(), 1);

        // Taken out of kit.toml, it's taken out of kit.lock.
        write(&root.join("kit.toml"), &kit_toml("game", ""));
        assert_eq!(
            kits.update(&root).unwrap(),
            "kit.lock: `physics` isn't a dependency any more, so it's no longer pinned\n"
        );
        assert_eq!(pins(&std::fs::read_to_string(root.join("kit.lock")).unwrap()).unwrap(), []);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn offline_a_kit_in_the_store_is_used_and_a_missing_one_is_an_error() {
        let dir = folder("offline");
        let (repository, commit) = physics(&dir);
        let root = game(&dir, &from_git("physics", &repository, &commit));
        let home = dir.join("home");
        Kits::new(&home, false).update(&root).unwrap();
        let offline = Kits::new(&home, true);
        assert_eq!(offline.ensure(&root).unwrap().len(), 1);

        std::fs::remove_dir_all(home.join("kits")).unwrap();
        let missing = offline.ensure(&root).unwrap_err();
        assert!(missing.contains("isn't on this machine yet, and fetching is off"), "{missing}");
        // Online, it's fetched again, and it's the same kit.
        assert_eq!(Kits::new(&home, false).ensure(&root).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_kit_fetched_with_other_files_than_kit_lock_pinned_is_refused() {
        let dir = folder("tampered");
        let (repository, commit) = physics(&dir);
        let root = game(&dir, &from_git("physics", &repository, &commit));
        let home = dir.join("home");
        let kits = Kits::new(&home, false);
        kits.update(&root).unwrap();
        let lock = std::fs::read_to_string(root.join("kit.lock")).unwrap();
        let pin = pins(&lock).unwrap().into_iter().next().unwrap();
        let other = fennec_blake3::hash(b"other files").to_hex();
        std::fs::write(root.join("kit.lock"), lock.replace(&pin.hash.to_hex(), &other)).unwrap();
        std::fs::remove_dir_all(home.join("kits")).unwrap();

        let refused = kits.ensure(&root).unwrap_err();
        assert!(refused.contains("isn't what kit.lock pinned"), "{refused}");
        assert!(!Kits::new(&home, false).stored(&fennec_blake3::Hash::from_hex(&other).unwrap()).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Asking for one commit by its hash works where the host allows it; elsewhere its branches
    /// are fetched and the commit found among them. A commit that isn't there says so.
    #[test]
    fn a_commit_is_fetched_by_hash_or_found_among_branches() {
        let dir = folder("by-hash");
        let (repository, commit) = physics(&dir);
        git(&repository, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
        let root = game(&dir, &from_git("physics", &repository, &commit));
        let kits = Kits::new(&dir.join("home"), false);
        assert!(kits.update(&root).is_ok());

        let missing = "0123456789abcdef0123456789abcdef01234567";
        write(&root.join("kit.toml"), &kit_toml("game", &from_git("physics", &repository, missing)));
        let said = kits.update(&root).unwrap_err();
        assert!(said.contains(&format!("has no commit {missing}")), "{said}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_among_a_kits_files_is_refused() {
        let dir = folder("link");
        let (repository, _) = physics(&dir);
        std::os::unix::fs::symlink("/etc/passwd", repository.join("src/secret.fen")).unwrap();
        let commit = commit(&repository, &[]);
        let root = game(&dir, &from_git("physics", &repository, &commit));
        let said = Kits::new(&dir.join("home"), false).update(&root).unwrap_err();
        assert!(said.contains("has a link at src/secret.fen, and a kit's files can't be links"), "{said}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_kit_inside_a_repository_is_found_by_its_folder() {
        let dir = folder("folder");
        let repository = dir.join("fennec");
        let commit = committed(
            &repository,
            &[
                ("kits/postgres/kit.toml", &kit_toml("postgres", "")),
                ("kits/postgres/src/lib.fen", "export def port() -> int:\n    return 5432\n"),
                ("kit.toml", "not this one\n"),
            ],
        );
        let line = format!(
            "postgres = {{ git = \"{}\", commit = \"{commit}\", folder = \"kits/postgres\" }}\n",
            file_url(&repository)
        );
        let root = game(&dir, &line);
        let kits = Kits::new(&dir.join("home"), false);
        let said = kits.update(&root).unwrap();
        assert!(said.contains(" kits/postgres at "), "{said}");
        let found = kits.ensure(&root).unwrap();
        let folder = &found.first().unwrap().folder;
        assert!(std::fs::read_to_string(folder.join("kit.toml")).unwrap().contains("name = \"postgres\""));
        assert!(folder.join("src/lib.fen").is_file());

        let elsewhere = line.replace("kits/postgres", "kits/mysql");
        write(&root.join("kit.toml"), &kit_toml("game", &elsewhere));
        let said = kits.update(&root).unwrap_err();
        assert!(said.contains("has no kit.toml in `kits/mysql`"), "{said}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A kit's dependencies' dependencies are pinned in its kit.lock too, and a kit from a path
    /// isn't pinned at all. Two different kits under one name are an error.
    #[test]
    fn dependencies_of_dependencies_are_pinned_and_a_name_means_one_kit() {
        let dir = folder("deeper");
        let (physics, first) = physics(&dir);
        let shapes = dir.join("shapes");
        let shapes_commit = committed(
            &shapes,
            &[("kit.toml", &kit_toml("shapes", &from_git("physics", &physics, &first))), ("src/lib.fen", "")],
        );
        let tools = dir.join("tools");
        write(&tools.join("kit.toml"), &kit_toml("tools", &from_git("physics", &physics, &first)));
        let root = game(
            &dir,
            &format!("{}tools = {{ path = \"../tools\" }}\n", from_git("shapes", &shapes, &shapes_commit)),
        );
        let kits = Kits::new(&dir.join("home"), false);
        kits.update(&root).unwrap();
        let pinned = pins(&std::fs::read_to_string(root.join("kit.lock")).unwrap()).unwrap();
        let names: Vec<&str> = pinned.iter().map(|pin| pin.name.as_str()).collect();
        assert_eq!(names, ["shapes", "physics"]);
        let found = kits.ensure(&root).unwrap();
        let tools_found = found.iter().find(|found| found.name == "tools").unwrap();
        assert_eq!((tools_found.folder.clone(), tools_found.pin.clone()), (tools.clone(), None));

        let second = commit(&physics, &[("src/lib.fen", "")]);
        write(&tools.join("kit.toml"), &kit_toml("tools", &from_git("physics", &physics, &second)));
        let said = kits.update(&root).unwrap_err();
        assert!(said.contains("two kits are named `physics`"), "{said}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_kit_from_git_cant_name_a_path() {
        let dir = folder("git-path");
        let repository = dir.join("engine");
        let commit =
            committed(&repository, &[("kit.toml", &kit_toml("engine", "tools = { path = \"../tools\" }\n"))]);
        let root = game(&dir, &from_git("engine", &repository, &commit));
        let said = Kits::new(&dir.join("home"), false).update(&root).unwrap_err();
        assert!(
            said.contains("`tools` comes from the path `../tools`, and `engine` that names it is from git"),
            "{said}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
