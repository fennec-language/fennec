//! The `fennec` people install. It runs the version of Fennec a kit asks for, `fennec = "0.4.2"`
//! in its `kit.toml`, from `~/.fennec/toolchains/0.4.2`, and hands the command to it. Outside a
//! kit, it runs the version `--version` names, or else the one `fennec default` set. `+0.4.2` runs
//! that version anywhere, once, and changes no pin.
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

/// What to say when a script outside a kit has no version to run with.
pub const NO_DEFAULT: &str = "this isn't in a kit, and no version of Fennec is set for scripts outside one: `fennec default 0.4.2` sets one, with the version you want, or `fennec --version 0.4.2 <command>` runs one for this command";

/// A version named on the command line, ahead of the command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// `+0.4.2`: this version, anywhere, once.
    JustThisOnce(String),
    /// `--version 0.4.2`: the version for a script outside a kit.
    OutsideAKit(String),
}

/// What the words on the command line ask of the launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// Whether a leading `--offline` said not to fetch.
    pub offline: bool,
    pub pick: Option<Pick>,
    /// The rest, which go to the toolchain as they are.
    pub command: Vec<String>,
}

impl Asked {
    /// Reads the launcher's own options, which come before the command, so that a program's own
    /// arguments are never taken for them. `--version` with no version after it is the toolchain's,
    /// which says what it is.
    #[must_use]
    pub fn read(args: &[String]) -> Self {
        let mut asked = Asked { offline: false, pick: None, command: Vec::new() };
        let mut rest = args;
        loop {
            match rest {
                [first, others @ ..] if first == "--offline" => {
                    asked.offline = true;
                    rest = others;
                }
                [first, others @ ..]
                    if asked.pick.is_none() && first.strip_prefix('+').is_some_and(names_one) =>
                {
                    asked.pick =
                        first.strip_prefix('+').map(|version| Pick::JustThisOnce(version.to_owned()));
                    rest = others;
                }
                [first, version, others @ ..]
                    if asked.pick.is_none() && first == "--version" && names_one(version) =>
                {
                    asked.pick = Some(Pick::OutsideAKit(version.clone()));
                    rest = others;
                }
                _ => break,
            }
        }
        asked.command = rest.to_vec();
        asked
    }
}

/// Whether a word names a toolchain: a release's version, or a linked build's name.
fn names_one(word: &str) -> bool {
    kit::plain_version(word) || kit::linked_name(word)
}

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

    /// The `fennec` of a toolchain installed from a release.
    #[must_use]
    pub fn command(&self, version: &str) -> PathBuf {
        self.folder(version).join(fennec_file())
    }

    /// Where the file naming a linked build's folder is kept.
    fn link_file(&self, name: &str) -> PathBuf {
        self.toolchains().join(format!("{name}.link"))
    }

    /// The folder a linked build's `fennec` is in.
    #[must_use]
    pub fn linked(&self, name: &str) -> Option<PathBuf> {
        if !kit::linked_name(name) {
            return None;
        }
        let text = std::fs::read_to_string(self.link_file(name)).ok()?;
        Some(PathBuf::from(text.trim_end_matches(['\r', '\n'])))
    }

    /// Every linked build, by name, in order.
    #[must_use]
    pub fn links(&self) -> Vec<(String, PathBuf)> {
        let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(self.toolchains())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".link").map(str::to_owned))
            .filter_map(|name| Some((name.clone(), self.linked(&name)?)))
            .collect();
        found.sort();
        found
    }

    /// Links a build of Fennec of your own, in `path` or the folder `path`'s `fennec` is in, as
    /// `name`. It's run as it is, with whatever LLVM it finds itself, and nothing is fetched, signed
    /// or pinned for it.
    ///
    /// # Errors
    ///
    /// When `name` can't name a link, or there's no `fennec` there.
    pub fn link(&self, name: &str, path: &Path) -> Result<(), String> {
        if !kit::linked_name(name) {
            return Err(format!(
                "`{name}` can't name a linked build: use lowercase letters, digits and `-`, starting with a letter, like `dev`"
            ));
        }
        let path = path.canonicalize().map_err(|err| format!("couldn't find {}: {err}", path.display()))?;
        let folder = if path.is_dir() { path } else { path.parent().map(Path::to_path_buf).unwrap_or(path) };
        if !folder.join(fennec_file()).is_file() {
            return Err(format!(
                "there's no `{}` in {}: build it first, like `cargo build --release -p fennec -p fennec-runtime`, and link its `target/release`",
                fennec_file(),
                folder.display()
            ));
        }
        std::fs::create_dir_all(self.toolchains())
            .and_then(|()| std::fs::write(self.link_file(name), format!("{}\n", folder.display())))
            .map_err(|err| format!("couldn't write the link: {err}"))
    }

    /// The `fennec` of a linked build.
    fn linked_command(&self, name: &str) -> Result<PathBuf, String> {
        let folder = self.linked(name).ok_or_else(|| {
            format!("no build of Fennec is linked as `{name}`: `fennec toolchain link {name} <path>` links one, like a checkout's `target/release`")
        })?;
        let command = folder.join(fennec_file());
        if !command.is_file() {
            return Err(format!(
                "the build linked as `{name}` has no `{}` in {} any more: build it again, or link it again",
                fennec_file(),
                folder.display()
            ));
        }
        Ok(command)
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
        names_one(version).then(|| version.to_owned())
    }

    /// Makes a toolchain the one run outside a kit, installing it if it has to be.
    ///
    /// # Errors
    ///
    /// When it isn't there and can't or mayn't be installed, or the choice can't be written.
    pub fn set_default(&self, version: &str) -> Result<(), String> {
        self.exactly(version)?;
        std::fs::write(self.toolchains().join("default"), format!("{version}\n"))
            .map_err(|err| format!("couldn't write the default toolchain: {err}"))
    }

    /// Removes an installed toolchain.
    ///
    /// # Errors
    ///
    /// When it isn't installed, or can't be removed.
    pub fn remove(&self, version: &str) -> Result<(), String> {
        if self.linked(version).is_some() {
            return std::fs::remove_file(self.link_file(version))
                .map_err(|err| format!("couldn't unlink `{version}`: {err}"));
        }
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
        let Some(root) = kit::root(from)? else {
            let version = self.default_version().ok_or(NO_DEFAULT)?;
            return self.exactly(&version);
        };
        let read = |name: &str| std::fs::read_to_string(root.join(name));
        let manifest = read("kit.toml").map_err(|err| format!("couldn't read kit.toml: {err}"))?;
        let version = kit::version(&manifest)?;
        // A linked build is whatever was built last, so there's nothing to pin.
        if kit::linked_name(&version) {
            return self.linked_command(&version);
        }
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
            fennec_dist::write_kit_file(&root.join("kit.lock"), &kit::with_pin(&lock, &version, hash))?;
        }
        Ok(self.command(&version))
    }

    /// The `fennec` of exactly this version, installing it if it has to be, whatever kit it's run
    /// in. No pin is read or written.
    ///
    /// # Errors
    ///
    /// When it isn't there and can't or mayn't be installed.
    pub fn exactly(&self, version: &str) -> Result<PathBuf, String> {
        if kit::linked_name(version) {
            return self.linked_command(version);
        }
        self.ensure(version, None)?;
        Ok(self.command(version))
    }

    /// The `fennec` of the version `--version` names, run from the folder `from`.
    ///
    /// # Errors
    ///
    /// When `from` is in a kit, whose `kit.toml` says which version it builds with, or the version
    /// isn't there and can't or mayn't be installed.
    pub fn outside_a_kit(&self, from: &Path, version: &str) -> Result<PathBuf, String> {
        if let Some(root) = kit::root(from)? {
            return Err(format!(
                "`--version` is for scripts outside a kit, and {} says which version this kit builds with: change `fennec = ...` there, or `fennec +{version} <command>` runs that version this once",
                root.join("kit.toml").display()
            ));
        }
        self.exactly(version)
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

    /// The newest release there is, checked as one to install would be: what `--latest` asks, and
    /// the only thing that asks. The release page says by where its latest release redirects to,
    /// and a mirror by a file `latest` holding the version.
    ///
    /// # Errors
    ///
    /// When it can't be found out, or the newest release isn't signed as it has to be.
    pub fn latest(&self) -> Result<String, String> {
        if self.offline {
            return Err("--offline says not to ask what's newest".to_owned());
        }
        let version = if self.dist == RELEASES {
            let page = RELEASES.trim_end_matches("/download");
            let tag = web::redirect(&format!("{page}/latest"))?;
            let (_, version) = tag.rsplit_once("/tag/v").ok_or("the release page has no release yet")?;
            version.to_owned()
        } else {
            let dist = self.dist.strip_prefix("file://").unwrap_or(&self.dist).trim_end_matches('/');
            let latest = if dist.starts_with("https://") || dist.starts_with("http://") {
                web::fetch(&format!("{dist}/latest"))?
            } else {
                std::fs::read(Path::new(dist).join("latest"))
                    .map_err(|err| format!("couldn't read {dist}/latest: {err}"))?
            };
            String::from_utf8_lossy(&latest).trim().to_owned()
        };
        if !kit::plain_version(&version) {
            return Err(format!("couldn't tell the newest release from `{version}`"));
        }
        let name = format!("fennec-v{version}.manifest");
        let text = self.fetch(&version, &name)?;
        let signatures = String::from_utf8(self.fetch(&version, &format!("{name}.sig"))?)
            .map_err(|_| "the release's signatures aren't text".to_owned())?;
        fennec_dist::verified_by(&text, &signatures, Kind::Toolchain, &self.trusted)?;
        Ok(version)
    }

    /// Moves the kit around `from` to a toolchain, pinning it in `kit.lock`: the version its
    /// `kit.toml` asks for, or with `latest` the newest release, which `kit.toml` then asks for.
    /// It's the only thing that moves a pin. Gives back what changed.
    ///
    /// # Errors
    ///
    /// When it isn't in a kit, or the toolchain can't be installed.
    pub fn update(&self, from: &Path, latest: bool) -> Result<String, String> {
        let root = kit::root(from)?.ok_or("this isn't in a kit, so there's no pin to move")?;
        let manifest = std::fs::read_to_string(root.join("kit.toml"))
            .map_err(|err| format!("couldn't read kit.toml: {err}"))?;
        let mut version = kit::version(&manifest)?;
        if kit::linked_name(&version) && !latest {
            self.linked_command(&version)?;
            return Ok(format!("kit.toml asks for the build linked as `{version}`, which isn't pinned\n"));
        }
        let lock = std::fs::read_to_string(root.join("kit.lock")).unwrap_or_default();
        let pin = kit::pinned(&lock)?;
        if latest {
            let newest = self.latest()?;
            if newest != version {
                fennec_dist::write_kit_file(&root.join("kit.toml"), &kit::asking_for(&manifest, &newest))?;
                version = newest;
            }
        }
        let hash = self.ensure(&version, None)?;
        if pin.as_ref() == Some(&(version.clone(), hash)) {
            return Ok(format!("kit.lock pins Fennec {version} already\n"));
        }
        fennec_dist::write_kit_file(&root.join("kit.lock"), &kit::with_pin(&lock, &version, hash))?;
        Ok(match pin {
            Some((before, _)) if before != version => format!("kit.lock: Fennec {before} → {version}\n"),
            Some(_) => format!("kit.lock: Fennec {version}, as its release is now\n"),
            None => format!("kit.lock: Fennec {version}\n"),
        })
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
        let bytes = self.fetch(version, &archive.file)?;
        archive.check(&bytes)?;
        let entries = if let Some(top) = archive.file.strip_suffix(".tar.gz") {
            unpack::tar_gz(&bytes, top)?
        } else if let Some(top) = archive.file.strip_suffix(".zip") {
            unpack::zip(&bytes, top)?
        } else {
            return Err(format!("{} isn't a .tar.gz or a .zip", archive.file));
        };
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

/// The name of the `fennec` command on this platform.
fn fennec_file() -> String {
    format!("fennec{}", std::env::consts::EXE_SUFFIX)
}
