//! The `fennec` people install. It runs the version of Fennec a kit asks for, `fennec = "0.4.2"`
//! in its `kit.toml`, from `~/.fennec/toolchains/0.4.2`, and hands the command to it. Outside a
//! kit, it runs the default toolchain.
//!
//! It goes to the network only when the version asked for isn't installed, and never with
//! `--offline`. It fetches from the release page, or from `FENNEC_DIST`, a mirror. What it fetches
//! has to be a release's manifest that the release keys built in have signed (see `fennec-dist`),
//! and an archive with the size and hash the manifest gives. A kit's `kit.lock` pins the hash of
//! its toolchain's manifest, the first time it runs, and from then on nothing else is run for it:
//! not a release made again under the same version, and not one installed by hand. Nothing here
//! updates itself, or asks what's newest, unless asked.

mod kit;
mod unpack;
mod web;

use std::path::{Path, PathBuf};

use fennec_dist::{Kind, PublicKey};

/// The platform this launcher runs on, as a release's manifest names it.
pub const TARGET: &str = env!("FENNEC_TARGET");

/// Where releases are fetched from, unless `FENNEC_DIST` says otherwise: each one's files are in
/// `<dist>/v<version>/`.
pub const RELEASES: &str = "https://github.com/fennec-language/fennec/releases/download";

/// What a launcher knows: where toolchains go, where releases come from, and whom it trusts.
pub struct Launcher {
    /// Fennec's folder, `~/.fennec` unless `FENNEC_HOME` says otherwise.
    pub home: PathBuf,
    /// Where releases come from: a folder, or the address of one.
    pub dist: String,
    /// Whether it may fetch what isn't installed.
    pub offline: bool,
    /// The keys whose signatures make a release one to install.
    pub trusted: Vec<PublicKey>,
    /// The platform whose archive it installs.
    pub target: String,
}

impl Launcher {
    fn toolchains(&self) -> PathBuf {
        self.home.join("toolchains")
    }

    fn folder(&self, version: &str) -> PathBuf {
        self.toolchains().join(version)
    }

    /// The `fennec` of a toolchain.
    #[must_use]
    pub fn command(&self, version: &str) -> PathBuf {
        self.folder(version).join(format!("fennec{}", std::env::consts::EXE_SUFFIX))
    }

    /// The versions installed, in order.
    #[must_use]
    pub fn installed(&self) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(self.toolchains())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().join("manifest").is_file())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .filter(|version| kit::plain_version(version))
            .collect();
        found.sort();
        found
    }

    /// The toolchain run outside a kit, if one's been chosen.
    #[must_use]
    pub fn default_version(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.toolchains().join("default")).ok()?;
        let version = text.trim();
        kit::plain_version(version).then(|| version.to_owned())
    }

    /// Makes an installed toolchain the one run outside a kit.
    ///
    /// # Errors
    ///
    /// When it isn't installed, or the choice can't be written.
    pub fn set_default(&self, version: &str) -> Result<(), String> {
        if !self.installed().iter().any(|installed| installed == version) {
            return Err(format!(
                "Fennec {version} isn't installed: `fennec toolchain install {version}` installs it"
            ));
        }
        std::fs::write(self.toolchains().join("default"), format!("{version}\n"))
            .map_err(|err| format!("couldn't write the default toolchain: {err}"))
    }

    /// Removes an installed toolchain.
    ///
    /// # Errors
    ///
    /// When it isn't installed, or can't be removed.
    pub fn remove(&self, version: &str) -> Result<(), String> {
        if !kit::plain_version(version) || !self.folder(version).join("manifest").is_file() {
            return Err(format!("Fennec {version} isn't installed"));
        }
        std::fs::remove_dir_all(self.folder(version))
            .map_err(|err| format!("couldn't remove Fennec {version}: {err}"))
    }

    /// The `fennec` to hand a command to, run from the folder `from`: the toolchain the kit around
    /// it asks for, or the default one outside a kit. It's installed first if it has to be and may
    /// be, and a kit's `kit.lock` pins it the first time.
    ///
    /// # Errors
    ///
    /// When there's no toolchain to run, or the one there is isn't the one pinned.
    pub fn toolchain_for(&self, from: &Path) -> Result<PathBuf, String> {
        let Some(root) = kit::root(from) else {
            let version = self.default_version().ok_or(
                "this isn't in a kit, and there's no default toolchain: `fennec toolchain default <version>` chooses one",
            )?;
            self.ensure(&version, None)?;
            return Ok(self.command(&version));
        };
        let read = |name: &str| std::fs::read_to_string(root.join(name));
        let manifest = read("kit.toml").map_err(|err| format!("couldn't read kit.toml: {err}"))?;
        let version = kit::version(&manifest)?;
        let lock = match read("kit.lock") {
            Ok(lock) => lock,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(format!("couldn't read kit.lock: {err}")),
        };
        let pin = kit::pinned(&lock)?;
        if let Some((pinned, _)) = pin.as_ref().filter(|(pinned, _)| *pinned != version) {
            return Err(format!(
                "kit.toml asks for Fennec {version}, and kit.lock pins {pinned}: `fennec update` moves the pin"
            ));
        }
        let hash = self.ensure(&version, pin.as_ref().map(|(_, hash)| *hash))?;
        if pin.is_none() {
            std::fs::write(root.join("kit.lock"), kit::with_pin(&lock, &version, hash))
                .map_err(|err| format!("couldn't write kit.lock: {err}"))?;
        }
        Ok(self.command(&version))
    }

    /// The hash of the manifest of the toolchain `version`, installing it if it isn't there.
    ///
    /// # Errors
    ///
    /// When it isn't `pin`, or it isn't there and can't or mayn't be installed.
    pub fn ensure(
        &self,
        version: &str,
        pin: Option<fennec_blake3::Hash>,
    ) -> Result<fennec_blake3::Hash, String> {
        if !kit::plain_version(version) {
            return Err(format!("`{version}` isn't a version"));
        }
        let Ok(manifest) = std::fs::read(self.folder(version).join("manifest")) else {
            if self.offline {
                return Err(format!("Fennec {version} isn't installed, and --offline says not to fetch it"));
            }
            return self.install(version, pin);
        };
        let hash = fennec_blake3::hash(&manifest);
        match pin {
            Some(pin) if pin != hash => Err(format!(
                "the Fennec {version} installed isn't the one kit.lock pins: its manifest's hash is {}, and kit.lock's is {}",
                hash.to_hex(),
                pin.to_hex()
            )),
            _ => Ok(hash),
        }
    }

    /// Reads one of a release's files.
    fn fetch(&self, version: &str, file: &str) -> Result<Vec<u8>, String> {
        let dist = self.dist.strip_prefix("file://").unwrap_or(&self.dist);
        if dist.starts_with("https://") || dist.starts_with("http://") {
            return web::fetch(&format!("{}/v{version}/{file}", dist.trim_end_matches('/')));
        }
        let path = Path::new(dist).join(format!("v{version}")).join(file);
        std::fs::read(&path).map_err(|err| format!("couldn't read {}: {err}", path.display()))
    }

    /// Installs the toolchain `version`, giving back its manifest's hash.
    ///
    /// # Errors
    ///
    /// When its release isn't signed as it has to be, isn't `pin`, has no archive for this
    /// platform, or its archive isn't the one the manifest names.
    pub fn install(
        &self,
        version: &str,
        pin: Option<fennec_blake3::Hash>,
    ) -> Result<fennec_blake3::Hash, String> {
        let name = format!("fennec-v{version}.manifest");
        let text = self.fetch(version, &name)?;
        let signatures = self.fetch(version, &format!("{name}.sig"))?;
        let signatures =
            String::from_utf8(signatures).map_err(|_| "the release's signatures aren't text".to_owned())?;
        let manifest = fennec_dist::verified_by(&text, &signatures, Kind::Toolchain, &self.trusted)?;
        if manifest.version != version {
            return Err(format!("the release of Fennec {version} says it's {}", manifest.version));
        }
        let hash = fennec_blake3::hash(&text);
        if let Some(pin) = pin.filter(|pin| *pin != hash) {
            return Err(format!(
                "the release of Fennec {version} isn't the one kit.lock pins: its manifest's hash is {}, and kit.lock's is {}",
                hash.to_hex(),
                pin.to_hex()
            ));
        }
        let archive = manifest
            .archive(&self.target)
            .ok_or_else(|| format!("Fennec {version} has no release for {}", self.target))?;
        let top = archive.file.strip_suffix(".tar.gz").ok_or_else(|| {
            format!("{} isn't a .tar.gz, which is all this launcher unpacks yet", archive.file)
        })?;
        let bytes = self.fetch(version, &archive.file)?;
        archive.check(&bytes)?;
        let entries = unpack::tar_gz(&bytes, top)?;
        let has_command = entries.iter().any(|entry| {
            entry.runnable
                && entry.bytes.is_some()
                && entry.path == Path::new(&format!("fennec{}", std::env::consts::EXE_SUFFIX))
        });
        if !has_command {
            return Err(format!("{} has no `fennec` to run", archive.file));
        }
        let toolchains = self.toolchains();
        std::fs::create_dir_all(&toolchains)
            .map_err(|err| format!("couldn't make {}: {err}", toolchains.display()))?;
        // Unpacked beside where it goes and then moved there, so a toolchain is there whole or not at all.
        let partial = toolchains.join(format!(".{version}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&partial);
        let placed = unpack::write(&entries, &partial)
            .and_then(|()| {
                std::fs::write(partial.join("manifest"), &text)
                    .and_then(|()| std::fs::write(partial.join("manifest.sig"), signatures.as_bytes()))
                    .map_err(|err| format!("couldn't write the manifest: {err}"))
            })
            .and_then(|()| match std::fs::rename(&partial, self.folder(version)) {
                // Another command installed it meanwhile, which `ensure` then checks as any other.
                Err(_) if self.folder(version).join("manifest").is_file() => Ok(()),
                moved => moved.map_err(|err| format!("couldn't install Fennec {version}: {err}")),
            });
        let _ = std::fs::remove_dir_all(&partial);
        placed?;
        self.ensure(version, Some(hash))
    }
}
