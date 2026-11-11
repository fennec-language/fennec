//! Which releases the launcher takes. The signatures in `fixtures` were made by `ssh-keygen -Y
//! sign`, with three Ed25519 keys made for these tests and then thrown away. A hardware key's
//! signatures are made here, as OpenSSH lays them out, since making one takes the hardware.

// The helpers unwrap and index as freely as the tests themselves.
#[cfg(test)]
mod release {
    use fennec_dist::{Kind, Manifest, PublicKey, verified_by};
    use ring::digest;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    }

    fn key(name: &str) -> PublicKey {
        PublicKey::parse(fixture(&format!("{name}.pub")).trim()).unwrap()
    }

    fn keys(names: &[&str]) -> Vec<PublicKey> {
        names.iter().map(|name| key(name)).collect()
    }

    fn signatures(names: &[&str]) -> String {
        names.iter().map(|name| fixture(&format!("{name}.sig"))).collect()
    }

    const ALL: [&str; 3] = ["first", "second", "third"];

    #[test]
    fn a_toolchain_signed_by_a_key_it_trusts_is_taken() {
        let text = fixture("toolchain.manifest");
        let manifest =
            verified_by(text.as_bytes(), &signatures(&["toolchain.first"]), Kind::Toolchain, &keys(&ALL));
        let manifest = manifest.unwrap();
        assert_eq!((manifest.kind, manifest.version.as_str()), (Kind::Toolchain, "0.4.2"));
        let archive = manifest.archive("aarch64-apple-darwin").unwrap();
        assert_eq!((archive.file.as_str(), archive.size), ("fennec-0.4.2-aarch64-apple-darwin.tar.gz", 11));
        assert!(manifest.archive("x86_64-unknown-linux-gnu").is_none());
    }

    #[test]
    fn a_toolchain_signed_by_no_key_it_trusts_is_refused() {
        let text = fixture("toolchain.manifest");
        let refused = verified_by(
            text.as_bytes(),
            &signatures(&["toolchain.first"]),
            Kind::Toolchain,
            &keys(&["second", "third"]),
        );
        assert_eq!(
            refused.unwrap_err(),
            "the toolchain release is signed by 0 of the 2 keys this build of Fennec trusts, and it needs 1"
        );
    }

    #[test]
    fn a_manifest_changed_after_it_was_signed_is_refused() {
        let text = fixture("toolchain.manifest").replace(" 11 ", " 12 ");
        let refused =
            verified_by(text.as_bytes(), &signatures(&["toolchain.first"]), Kind::Toolchain, &keys(&ALL));
        assert!(refused.unwrap_err().contains("signed by 0 of the 3 keys"));
    }

    #[test]
    fn a_signature_made_for_the_other_kind_of_release_never_counts() {
        // The second key signed the toolchain's manifest, but as a launcher.
        let text = fixture("toolchain.manifest");
        let refused = verified_by(
            text.as_bytes(),
            &signatures(&["toolchain.wrong-namespace"]),
            Kind::Toolchain,
            &keys(&ALL),
        );
        assert!(refused.unwrap_err().contains("signed by 0 of the 3 keys"));
        let as_launcher = verified_by(
            text.as_bytes(),
            &signatures(&["toolchain.wrong-namespace", "launcher.first"]),
            Kind::Launcher,
            &keys(&ALL),
        );
        assert!(
            as_launcher.unwrap_err().contains("signed by 1 of the 3 keys"),
            "the launcher's own signature is of another file"
        );
    }

    #[test]
    fn a_launcher_needs_two_of_its_keys() {
        let text = fixture("launcher.manifest");
        let check =
            |names: &[&str]| verified_by(text.as_bytes(), &signatures(names), Kind::Launcher, &keys(&ALL));
        assert_eq!(
            check(&["launcher.first"]).unwrap_err(),
            "the launcher release is signed by 1 of the 3 keys this build of Fennec trusts, and it needs 2"
        );
        assert!(
            check(&["launcher.first", "launcher.first"]).unwrap_err().contains("signed by 1 of"),
            "one key twice is one key"
        );
        for two in [
            ["launcher.first", "launcher.second"],
            ["launcher.second", "launcher.third"],
            ["launcher.third", "launcher.first"],
        ] {
            assert_eq!(check(&two).unwrap().version, "1.0.0", "{two:?}");
        }
        assert_eq!(check(&["launcher.first", "launcher.second", "launcher.third"]).unwrap().version, "1.0.0");
        let listed_twice = keys(&["first", "first", "second"]);
        let refused =
            verified_by(text.as_bytes(), &signatures(&["launcher.first"]), Kind::Launcher, &listed_twice);
        assert!(refused.unwrap_err().contains("signed by 1 of"), "a key listed twice counts once");
    }

    #[test]
    fn a_launcher_signed_as_one_is_still_not_a_toolchain() {
        let text = fixture("launcher.manifest");
        let refused = verified_by(
            text.as_bytes(),
            &signatures(&["launcher.first", "launcher.second"]),
            Kind::Toolchain,
            &keys(&ALL),
        );
        assert!(refused.unwrap_err().contains("signed by 0 of"));
    }

    #[test]
    fn what_isnt_a_signature_says_so() {
        let refused = verified_by(b"fennec toolchain 1\n", "no signatures", Kind::Toolchain, &keys(&ALL));
        assert_eq!(refused.unwrap_err(), "there are no SSH signatures here");
        let cut = fixture("toolchain.first.sig").replace("-----END SSH SIGNATURE-----", "");
        let refused = verified_by(b"fennec toolchain 1\n", &cut, Kind::Toolchain, &keys(&ALL));
        assert_eq!(refused.unwrap_err(), "an SSH signature has no end line");
    }

    #[test]
    fn only_ed25519_keys_are_taken() {
        assert!(PublicKey::parse("ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC7 someone").is_err());
        assert!(PublicKey::parse("ssh-ed25519").is_err());
        let line = fixture("first.pub");
        let mislabeled = line.replacen("ssh-ed25519", "sk-ssh-ed25519@openssh.com", 1);
        assert!(PublicKey::parse(mislabeled.trim()).unwrap_err().contains("holds ssh-ed25519"));
    }

    #[test]
    fn no_release_key_is_built_in_until_there_are_real_ones() {
        assert_eq!(fennec_dist::release_keys().unwrap(), Vec::new());
        let text = fixture("toolchain.manifest");
        let refused =
            fennec_dist::verified(text.as_bytes(), &signatures(&["toolchain.first"]), Kind::Toolchain);
        assert!(refused.unwrap_err().contains("signed by 0 of the 0 keys"));
    }

    /// OpenSSH's wire format for a string.
    fn put(to: &mut Vec<u8>, part: &[u8]) {
        to.extend_from_slice(&u32::try_from(part.len()).unwrap().to_be_bytes());
        to.extend_from_slice(part);
    }

    fn base64(bytes: &[u8]) -> String {
        const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut text = String::new();
        for chunk in bytes.chunks(3) {
            let word = chunk
                .iter()
                .enumerate()
                .fold(0u32, |word, (at, byte)| word | u32::from(*byte) << (16 - 8 * at));
            for at in 0..4 {
                if at <= chunk.len() {
                    text.push(char::from(LETTERS[usize::try_from(word >> (18 - 6 * at) & 63).unwrap()]));
                } else {
                    text.push('=');
                }
            }
        }
        text
    }

    /// A hardware key made for a test: its public key line, and a signature of `message` in
    /// `namespace` as the key would make it, with `flags` saying whether it was touched.
    fn hardware_signature(namespace: &str, message: &[u8], flags: u8) -> (String, String) {
        let pair = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap().as_ref(),
        )
        .unwrap();
        let (kind, application) = (b"sk-ssh-ed25519@openssh.com", b"ssh:");
        let mut public = Vec::new();
        put(&mut public, kind);
        put(&mut public, pair.public_key().as_ref());
        put(&mut public, application);
        let mut signed = b"SSHSIG".to_vec();
        for part in [namespace.as_bytes(), b"", b"sha512", digest::digest(&digest::SHA512, message).as_ref()]
        {
            put(&mut signed, part);
        }
        let counter = 7u32.to_be_bytes();
        let mut over = digest::digest(&digest::SHA256, application).as_ref().to_vec();
        over.push(flags);
        over.extend_from_slice(&counter);
        over.extend_from_slice(digest::digest(&digest::SHA256, &signed).as_ref());
        let mut signature = Vec::new();
        put(&mut signature, kind);
        put(&mut signature, pair.sign(&over).as_ref());
        signature.push(flags);
        signature.extend_from_slice(&counter);
        let mut blob = b"SSHSIG".to_vec();
        blob.extend_from_slice(&1u32.to_be_bytes());
        for part in [&public[..], namespace.as_bytes(), b"", b"sha512", &signature] {
            put(&mut blob, part);
        }
        let armored =
            format!("-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n", base64(&blob));
        (format!("sk-ssh-ed25519@openssh.com {} hardware test key", base64(&public)), armored)
    }

    #[test]
    fn a_hardware_key_signs_only_when_someone_touched_it() {
        let text = fixture("toolchain.manifest");
        let (line, touched) = hardware_signature("fennec-toolchain", text.as_bytes(), 0x01);
        let hardware = PublicKey::parse(&line).unwrap();
        let taken = verified_by(text.as_bytes(), &touched, Kind::Toolchain, std::slice::from_ref(&hardware));
        assert_eq!(taken.unwrap().version, "0.4.2");
        let (line, untouched) = hardware_signature("fennec-toolchain", text.as_bytes(), 0x00);
        let refused =
            verified_by(text.as_bytes(), &untouched, Kind::Toolchain, &[PublicKey::parse(&line).unwrap()]);
        assert!(refused.unwrap_err().contains("signed by 0 of"));
    }

    #[test]
    fn a_launcher_takes_a_hardware_key_and_the_paper_one_together() {
        let text = fixture("launcher.manifest");
        let (line, hardware) = hardware_signature("fennec-launcher", text.as_bytes(), 0x01);
        let mut trusted = keys(&["first"]);
        trusted.push(PublicKey::parse(&line).unwrap());
        let both = format!("{hardware}{}", fixture("launcher.first.sig"));
        assert_eq!(verified_by(text.as_bytes(), &both, Kind::Launcher, &trusted).unwrap().version, "1.0.0");
    }

    fn manifest_error(text: &str) -> String {
        Manifest::parse(text).unwrap_err()
    }

    #[test]
    fn a_manifest_is_written_one_way_only() {
        let text = fixture("toolchain.manifest");
        assert_eq!(Manifest::parse(&text).unwrap().write(), text);
        let hash = "ab".repeat(32);
        for (bad, said) in [
            ("", "the manifest is empty"),
            ("fennec toolchains 1\n", "not `fennec toolchain`"),
            ("fennec toolchain v1\nx y.tar.gz 1 HASH\n", "isn't a version"),
            ("fennec toolchain 1\n", "has no archives"),
            ("fennec toolchain 1\nx y.tar.gz 1\n", "isn't a platform, a file"),
            ("fennec toolchain 1\nx y.tar.gz -1 HASH\n", "no size or hash"),
            ("fennec toolchain 1\nx y.tar.gz +1 HASH\n", "no size or hash"),
            ("fennec toolchain 1\nx y.tar.gz 1 abcd\n", "no size or hash"),
            ("fennec toolchain 1\nx ../y.tar.gz 1 HASH\n", "isn't plain"),
            ("fennec toolchain 1\nx .y 1 HASH\n", "isn't plain"),
            ("fennec toolchain 1\nx a/y 1 HASH\n", "isn't plain"),
            ("fennec toolchain 1\nx y.tar.gz 1 HASH\nx z.tar.gz 1 HASH\n", "twice"),
            ("fennec toolchain 1\nx y.tar.gz 1 HASH\nz y.tar.gz 1 HASH\n", "twice"),
            ("fennec toolchain 1\nx y.tar.gz 01 HASH\n", "the one way"),
            ("fennec toolchain 1\nx y.tar.gz 1 HASH", "the one way"),
            ("fennec toolchain 1\r\nx y.tar.gz 1 HASH\n", "isn't a version"),
        ] {
            let bad = bad.replace("HASH", &hash);
            assert!(manifest_error(&bad).contains(said), "{bad:?}: {}", manifest_error(&bad));
        }
        let upper = format!("fennec toolchain 1\nx y.tar.gz 1 {}\n", hash.to_uppercase());
        assert!(manifest_error(&upper).contains("the one way"));
    }

    #[test]
    fn an_archive_is_taken_only_with_the_size_and_hash_its_release_says() {
        let bytes = b"hello world";
        let manifest = Manifest::parse(&format!(
            "fennec toolchain 1\nx y.tar.gz 11 {}\n",
            fennec_blake3::hash(bytes).to_hex()
        ))
        .unwrap();
        let archive = manifest.archive("x").unwrap();
        assert_eq!(archive.check(bytes), Ok(()));
        assert_eq!(archive.check(b"hello"), Err("y.tar.gz is 5 bytes, and the release says 11".to_owned()));
        assert!(archive.check(b"hello_world").unwrap_err().contains("has the hash"));
    }

    #[test]
    fn ci_writes_a_manifest_from_the_archives_it_named() {
        let archives = vec![
            ("fennec-v0.4.2-aarch64-apple-darwin.tar.gz".to_owned(), b"mac".to_vec()),
            ("fennec-0.4.2-x86_64-pc-windows-gnullvm.zip".to_owned(), b"windows".to_vec()),
        ];
        let manifest = Manifest::of_archives(Kind::Toolchain, "0.4.2", &archives).unwrap();
        let text = manifest.write();
        assert_eq!(Manifest::parse(&text).unwrap(), manifest);
        assert!(text.starts_with(
            "fennec toolchain 0.4.2\naarch64-apple-darwin fennec-v0.4.2-aarch64-apple-darwin.tar.gz 3 "
        ));
        assert_eq!(manifest.archive("x86_64-pc-windows-gnullvm").unwrap().check(b"windows"), Ok(()));
        let misnamed = vec![("fennec-v0.4.3-aarch64-apple-darwin.tar.gz".to_owned(), Vec::new())];
        assert!(
            Manifest::of_archives(Kind::Toolchain, "0.4.2", &misnamed).unwrap_err().contains("isn't named")
        );
        let launcher = vec![("fennec-launcher-v1.0.0-aarch64-apple-darwin.tar.gz".to_owned(), Vec::new())];
        assert_eq!(
            Manifest::of_archives(Kind::Launcher, "1.0.0", &launcher).unwrap().archives[0].target,
            "aarch64-apple-darwin"
        );
    }

    fn fennec_dist(args: &[&str]) -> (bool, String, String) {
        let output =
            std::process::Command::new(env!("CARGO_BIN_EXE_fennec-dist")).args(args).output().unwrap();
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        (output.status.success(), text(&output.stdout), text(&output.stderr))
    }

    #[test]
    fn the_command_writes_manifests_and_checks_them() {
        let dir =
            fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-dist-{}", std::process::id())));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("fennec-v0.4.2-aarch64-apple-darwin.tar.gz");
        std::fs::write(&archive, b"archive").unwrap();
        let (ok, written, _) = fennec_dist(&["manifest", "toolchain", "0.4.2", archive.to_str().unwrap()]);
        let expected = Manifest::of_archives(
            Kind::Toolchain,
            "0.4.2",
            &[("fennec-v0.4.2-aarch64-apple-darwin.tar.gz".to_owned(), b"archive".to_vec())],
        );
        assert!(ok);
        assert_eq!(written, expected.unwrap().write());
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let at = |name: &str| fixtures.join(name).display().to_string();
        let (manifest, signature) = (at("toolchain.manifest"), at("toolchain.first.sig"));
        let (ok, said, _) = fennec_dist(&["verify", "toolchain", &manifest, &signature, &at("first.pub")]);
        assert_eq!((ok, said.as_str()), (true, "fennec toolchain 0.4.2: signed as it needs to be\n"));
        let (ok, _, why) = fennec_dist(&["verify", "toolchain", &manifest, &signature]);
        assert_eq!(
            (ok, why.as_str()),
            (
                false,
                "error: the toolchain release is signed by 0 of the 0 keys this build of Fennec trusts, and it needs 1\n"
            )
        );
        let (ok, _, why) = fennec_dist(&["verify", "launcher", &manifest, &signature, &at("first.pub")]);
        assert!(!ok && why.contains("signed by 0 of the 1 key "), "{why}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
