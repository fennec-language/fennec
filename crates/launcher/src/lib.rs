//! The `fennec` people install. It runs the version of Fennec a kit asks for, `fennec = "0.4.2"`
//! in its `kit.toml`, from `~/.fennec/toolchains/0.4.2`, and hands the command to it. Outside a
//! kit, it runs the version `--version` names, or else the one `fennec default` set, or else the
//! newest stable version installed. `+0.4.2` runs that version anywhere, once, and changes no pin.
//!
//! It goes to the network only when the version asked for isn't installed, and never with
//! `--offline`. It fetches from the release page, or from `FENNEC_DIST`, a mirror. What it fetches
//! has to be a release's manifest that the release keys built in have signed (see `fennec-dist`),
//! and an archive with the size and hash the manifest gives. A kit's `kit.lock` pins the hash of
//! its toolchain's manifest, the first time it runs, and from then on nothing else is run for it:
//! not a release made again under the same version, and not one installed by hand. Nothing here
//! updates itself, or asks what's newest, unless asked.
//!
//! `fennec self update` replaces the launcher itself, with the newest launcher release, from
//! `FENNEC_LAUNCHER_DIST` or the launcher's release folder. A launcher release has to be signed by
//! two of the release keys, not one, since it's what checks everything else.

mod kit;
mod unpack;

use std::path::{Path, PathBuf};

use fennec_dist::{Kind, PublicKey};

/// The platform this launcher runs on, as a release's manifest names it.
pub const TARGET: &str = env!("FENNEC_TARGET");

/// Where releases are fetched from, unless `FENNEC_DIST` says otherwise: each one's files are in
/// `<dist>/v<version>/`.
pub const RELEASES: &str = "https://github.com/fennec-language/fennec/releases/download";

/// Where launcher releases come from, unless `FENNEC_LAUNCHER_DIST` says otherwise: `latest`, the
/// newest one's version, and each one's files in `<dist>/v<version>/`.
pub const LAUNCHER_RELEASES: &str = "https://dl.fennec-lang.dev/launcher";

/// What `fennec default` says when none is set.
pub const NO_DEFAULT: &str = "no version of Fennec is set for scripts outside a kit, so the newest stable one installed runs: `fennec default 0.4.2` sets one, with the version you want";

/// What to say when a script outside a kit has no version to run with: none set, and no stable one
/// installed. Nothing is fetched unless asked.
pub const NO_TOOLCHAIN: &str = "this isn't in a kit, and no stable version of Fennec is installed for scripts outside one: `fennec toolchain install --latest` installs the newest, `fennec default 0.4.2` sets one, with the version you want, or `fennec --version 0.4.2 <command>` runs one for this command";

/// A version's place among others: its numbers first, as numbers, so `0.10.0` comes after `0.9.1`,
/// then a stable release after any prerelease of it, like `0.5.0-rc.1`, and the text last, so the
/// order is total. What's past a `+` is build metadata, which orders nothing but the text.
fn version_order(version: &str) -> (Vec<u64>, bool, &str) {
    let core = version.split('+').next().unwrap_or(version);
    let (numbers, prerelease) = core.split_once('-').map_or((core, false), |(numbers, _)| (numbers, true));
    let numbers = numbers.split('.').map(|part| part.parse().unwrap_or(0)).collect();
    (numbers, !prerelease, version)
}

/// Whether a version is a stable release: no prerelease part, like `-rc.1`.
fn stable(version: &str) -> bool {
    !version.split('+').next().unwrap_or(version).contains('-')
}

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

/// How a launcher reaches the network, for the few commands that fetch.
pub trait Web {
    /// The bytes at `url`, of which there are at most `most`.
    ///
    /// # Errors
    ///
    /// When they can't be fetched, or there are more, saying why.
    fn fetch(&self, url: &str, most: u64) -> Result<Vec<u8>, String>;

    /// Where `url` redirects to, without following it.
    ///
    /// # Errors
    ///
    /// When it can't be asked, or doesn't redirect.
    fn redirect(&self, url: &str) -> Result<String, String>;
}

/// The most a fetch with no size to go by takes in: a manifest, its signatures, a `latest`. An
/// archive's size comes from its signed manifest, and is all its fetch takes in.
pub const MOST_BYTES: u64 = 1 << 20;

/// Fetching through `fennec-fetch`, the program beside the launcher that does the network's
/// work, over HTTPS checked against what the operating system trusts, started only when something
/// has to be fetched. What it fetches comes back on its output. The launcher links nothing for
/// HTTPS: on macOS that alone costs every command milliseconds to start.
pub struct Helper(pub PathBuf);

impl Helper {
    /// `fennec-fetch` beside the program running.
    ///
    /// # Errors
    ///
    /// When the program running can't be found.
    pub fn beside_this() -> Result<Self, String> {
        beside_this("fennec-fetch").map(Helper)
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let ran = std::process::Command::new(&self.0)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| format!("couldn't run {} to fetch: {err}", self.0.display()))?;
        if !ran.status.success() {
            let said = String::from_utf8_lossy(&ran.stderr);
            let said = said.trim().strip_prefix("error: ").unwrap_or(said.trim());
            return Err(if said.is_empty() {
                format!("{} failed", self.0.display())
            } else {
                said.to_owned()
            });
        }
        Ok(ran.stdout)
    }
}

impl Web for Helper {
    fn fetch(&self, url: &str, most: u64) -> Result<Vec<u8>, String> {
        self.run(&["fetch", url, &most.to_string()])
    }

    fn redirect(&self, url: &str) -> Result<String, String> {
        let said = self.run(&["redirect", url])?;
        Ok(String::from_utf8_lossy(&said).trim().to_owned())
    }
}

/// The program `name` beside the one running, as the launcher's programs are installed together.
///
/// # Errors
///
/// When the program running can't be found.
pub fn beside_this(name: &str) -> Result<PathBuf, String> {
    let this = std::env::current_exe().map_err(|err| format!("couldn't find the launcher: {err}"))?;
    Ok(this.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

/// The launcher's three programs, as installed side by side in Fennec's `bin`: `fennec`, which
/// hands a command to the toolchain it's for when that's installed already; `fennec-setup`, which
/// it hands everything else to; and `fennec-fetch`, which does the network's work.
pub const PROGRAMS: [&str; 3] = ["fennec", "fennec-setup", "fennec-fetch"];

/// Runs `command` in place of this process, with the arguments, called `fennec` as the launcher
/// was: signals, the exit code and every stream go straight to it. It's told to fetch nothing when
/// the launcher was.
///
/// # Errors
///
/// When it can't be run.
#[cfg(unix)]
pub fn hand_off(command: &Path, args: &[String], offline: bool) -> Result<std::process::ExitCode, String> {
    use std::os::unix::process::CommandExt as _;
    let err = toolchain(command, args, offline).arg0("fennec").exec();
    Err(format!("couldn't run {}: {err}", command.display()))
}

/// Runs `command`, with the arguments, and exits as it does. Ctrl-C reaches every program in the
/// console, so the launcher lets it go by and waits: the program decides what it means, and the
/// launcher exits as it does, rather than leaving it running behind a prompt.
///
/// # Errors
///
/// When it can't be run.
#[cfg(not(unix))]
pub fn hand_off(command: &Path, args: &[String], offline: bool) -> Result<std::process::ExitCode, String> {
    ctrlc::set_handler(|| {})
        .map_err(|err| format!("couldn't let Ctrl-C through to the toolchain: {err}"))?;
    let status = toolchain(command, args, offline)
        .status()
        .map_err(|err| format!("couldn't run {}: {err}", command.display()))?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(std::process::ExitCode::FAILURE, std::process::ExitCode::from))
}

/// `command` with the arguments, told to fetch nothing when the launcher was.
fn toolchain(command: &Path, args: &[String], offline: bool) -> std::process::Command {
    let mut toolchain = std::process::Command::new(command);
    toolchain.args(args);
    if offline {
        toolchain.env("FENNEC_OFFLINE", "1");
    }
    toolchain
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
    /// How it fetches what it has to.
    pub web: Box<dyn Web>,
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
        self.private_toolchains()?;
        std::fs::write(self.link_file(name), format!("{}\n", folder.display()))
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
        found.sort_by(|a, b| version_order(a).cmp(&version_order(b)));
        found
    }

    /// The version a script outside a kit runs with: the one `fennec default` set, or else the
    /// newest stable one installed. `None` when there's neither; nothing is fetched to find one.
    #[must_use]
    pub fn for_scripts(&self) -> Option<String> {
        self.default_version().or_else(|| self.installed().into_iter().rfind(|version| stable(version)))
    }

    /// The version the kit around the folder `from` asks for, if it's in one that says. Only for
    /// showing: running in a kit reads it again, with every check.
    #[must_use]
    pub fn kit_version(&self, from: &Path) -> Option<String> {
        let root = kit::root(from).ok()??;
        kit::version(&std::fs::read_to_string(root.join("kit.toml")).ok()?).ok()
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
    /// it asks for, or outside a kit the one for scripts (see `for_scripts`). It's installed first if it has to be and may
    /// be, and a kit's `kit.lock` pins it the first time.
    ///
    /// # Errors
    ///
    /// When there's no toolchain to run, or the one there is isn't the one pinned.
    pub fn toolchain_for(&self, from: &Path) -> Result<PathBuf, String> {
        let Some(root) = kit::root(from)? else {
            let version = self.for_scripts().ok_or(NO_TOOLCHAIN)?;
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
        match self.found(version, pin)? {
            Some(hash) => Ok(hash),
            None if self.offline => {
                Err(format!("Fennec {version} isn't installed, and --offline says not to fetch it"))
            }
            None => self.install(version, pin),
        }
    }

    /// The hash of the manifest of the toolchain `version`, if it's installed. Nothing is
    /// fetched, installed or written.
    ///
    /// # Errors
    ///
    /// When it isn't a version, or what's installed isn't `pin` or is where others could write.
    fn found(
        &self,
        version: &str,
        pin: Option<fennec_blake3::Hash>,
    ) -> Result<Option<fennec_blake3::Hash>, String> {
        if !kit::plain_version(version) {
            return Err(format!("`{version}` isn't a version"));
        }
        let Ok(manifest) = std::fs::read(self.folder(version).join("manifest")) else {
            return Ok(None);
        };
        // A toolchain is run as it's found, as one installed another way is, and the pin in
        // kit.lock is what holds a kit to its own. But not from a folder others can write to,
        // where anyone could have put one: as Fennec's cache, it has to be its user's alone.
        let manifest_at = self.folder(version).join("manifest");
        if !fennec_dist::trusted(&self.folder(version)) || !fennec_dist::trusted(&manifest_at) {
            return Err(format!(
                "the Fennec {version} installed is where others could write to it, so it isn't run: {}",
                self.folder(version).display()
            ));
        }
        let hash = fennec_blake3::hash(&manifest);
        match pin {
            Some(pin) if pin != hash => Err(format!(
                "the Fennec {version} installed isn't the one kit.lock pins: its manifest's hash is {}, and kit.lock's is {}",
                hash.to_hex(),
                pin.to_hex()
            )),
            _ => Ok(Some(hash)),
        }
    }

    /// The `fennec` to hand a command to, run from the folder `from`, when it's there to run as it
    /// is: what `toolchain_for`, or with a version named `exactly` or `outside_a_kit`, gives, with
    /// every check, for a toolchain installed already and a kit pinned already. Nothing is fetched,
    /// installed or written, and none of the code that would is reached, so the launcher's `fennec`
    /// links none of it. None for anything else, which those then do in full and say what's wrong.
    #[must_use]
    pub fn ready(&self, from: &Path, pick: Option<&Pick>) -> Option<PathBuf> {
        let root = kit::root(from).ok()?;
        let version = match (pick, root) {
            (Some(Pick::JustThisOnce(version)), _) | (Some(Pick::OutsideAKit(version)), None) => {
                version.clone()
            }
            (Some(Pick::OutsideAKit(_)), Some(_)) => return None,
            (None, None) => self.for_scripts()?,
            (None, Some(root)) => {
                let version = kit::version(&std::fs::read_to_string(root.join("kit.toml")).ok()?).ok()?;
                if kit::linked_name(&version) {
                    return self.linked_command(&version).ok();
                }
                let (pinned, hash) =
                    kit::pinned(&std::fs::read_to_string(root.join("kit.lock")).ok()?).ok()??;
                self.found(&version, Some(hash)).ok()?.filter(|_| pinned == version)?;
                return Some(self.command(&version));
            }
        };
        if kit::linked_name(&version) {
            return self.linked_command(&version).ok();
        }
        self.found(&version, None).ok()??;
        Some(self.command(&version))
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
            let tag = self.web.redirect(&format!("{page}/latest"))?;
            let (_, version) = tag.rsplit_once("/tag/v").ok_or("the release page has no release yet")?;
            version.to_owned()
        } else {
            self.latest_in(&self.dist)?
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
            // A mirror can't roll a kit back to an older release, with bugs since fixed, by calling
            // it the latest.
            if kit::plain_version(&version) && older(&newest, &version) {
                return Err(format!(
                    "the newest release the mirror names, {newest}, is older than the {version} kit.toml asks for already"
                ));
            }
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

    /// The version a mirror's file `latest` names, as it says it.
    fn latest_in(&self, dist: &str) -> Result<String, String> {
        let latest = self.read_from(dist, "latest", MOST_BYTES)?;
        Ok(String::from_utf8_lossy(&latest).trim().to_owned())
    }

    /// Reads a file of `dist`, a folder or the address of one, taking in at most `most` bytes: an
    /// archive's size comes from its signed manifest, and anything else is small.
    fn read_from(&self, dist: &str, file: &str, most: u64) -> Result<Vec<u8>, String> {
        let dist = dist.strip_prefix("file://").unwrap_or(dist);
        if dist.starts_with("https://") || dist.starts_with("http://") {
            return self.web.fetch(&format!("{}/{file}", dist.trim_end_matches('/')), most);
        }
        let path = Path::new(dist).join(file);
        let size =
            std::fs::metadata(&path).map_err(|err| format!("couldn't read {}: {err}", path.display()))?.len();
        if size > most {
            return Err(format!("{} is bigger than the {most} bytes it can be", path.display()));
        }
        std::fs::read(&path).map_err(|err| format!("couldn't read {}: {err}", path.display()))
    }

    /// Replaces this launcher, which is version `this` and whose `fennec-setup` is `running`, with
    /// the newest launcher release in `launchers`, a folder or the address of one: each of its
    /// `PROGRAMS`. Only the launcher installed in Fennec's folder, in `bin`, replaces itself, and
    /// only with a release newer than it that two of the release keys signed. Each program is
    /// written beside where it goes and moved there, so each is the old one or the new one, never
    /// part of one; `fennec` goes last, so one stopped halfway is still the old launcher, and the
    /// next `self update` does it all again. Gives back what changed.
    ///
    /// # Errors
    ///
    /// When there aren't enough release keys to sign one, the release isn't signed as it has to
    /// be, is older, or has no archive for this platform with both programs in it, or the
    /// programs can't be replaced.
    pub fn self_update(&self, launchers: &str, running: &Path, this: &str) -> Result<String, String> {
        let needs = Kind::Launcher.needs();
        if self.trusted.len() < needs {
            let keys = if self.trusted.len() == 1 { "key" } else { "keys" };
            return Err(format!(
                "this launcher trusts {} release {keys}, and a new launcher has to be signed by {needs}, so there's none it could install yet",
                self.trusted.len()
            ));
        }
        if self.offline {
            return Err("--offline says not to fetch a new launcher".to_owned());
        }
        let bin = self.home.join("bin");
        let programs = PROGRAMS.map(|program| format!("{program}{}", std::env::consts::EXE_SUFFIX));
        let installed = bin.join(&programs[1]);
        let same = |a: &Path, b: &Path| a.canonicalize().is_ok_and(|a| b.canonicalize().ok() == Some(a));
        if !same(running, &installed) {
            return Err(format!(
                "this launcher is {}, not the one installed in {}, so it doesn't replace itself",
                running.display(),
                bin.display()
            ));
        }
        if !fennec_dist::trusted(&bin) {
            return Err(format!(
                "{} is where others could write to it, so no launcher is put there",
                bin.display()
            ));
        }
        let newest = self.latest_in(launchers)?;
        if !kit::plain_version(&newest) {
            return Err(format!("couldn't tell the newest launcher from `{newest}`"));
        }
        match version_order(&newest).cmp(&version_order(this)) {
            std::cmp::Ordering::Equal => return Ok(format!("the launcher is {this}, the newest, already\n")),
            // A mirror can't roll the launcher back to an older one, with bugs since fixed, by
            // calling it the latest.
            std::cmp::Ordering::Less => {
                return Err(format!(
                    "the newest launcher the mirror names, {newest}, is older than this one, {this}"
                ));
            }
            std::cmp::Ordering::Greater => {}
        }
        let name = format!("v{newest}/fennec-launcher-v{newest}.manifest");
        let text = self.read_from(launchers, &name, MOST_BYTES)?;
        let signatures = String::from_utf8(self.read_from(launchers, &format!("{name}.sig"), MOST_BYTES)?)
            .map_err(|_| "the launcher's signatures aren't text".to_owned())?;
        let manifest = fennec_dist::verified_by(&text, &signatures, Kind::Launcher, &self.trusted)?;
        if manifest.version != newest {
            return Err(format!("the launcher release {newest} says it's {}", manifest.version));
        }
        let archive = manifest
            .archive(&self.target)
            .ok_or_else(|| format!("the launcher {newest} has no release for {}", self.target))?;
        let bytes = self.read_from(launchers, &format!("v{newest}/{}", archive.file), archive.size)?;
        archive.check(&bytes)?;
        let entries = unpacked(archive, &bytes, &programs.each_ref().map(String::as_str))?;
        let partial = bin.join(format!(".update.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&partial);
        let placed = unpack::write(&entries, &partial).and_then(|()| {
            programs
                .iter()
                .rev()
                .try_for_each(|program| put_in_place(&partial.join(program), &bin.join(program)))
        });
        let _ = std::fs::remove_dir_all(&partial);
        placed?;
        Ok(format!("the launcher: {this} → {newest}\n"))
    }

    /// Reads one of a release's files that its signed manifest says how big is, taking in no more:
    /// an archive.
    fn fetch_at_most(&self, version: &str, file: &str, most: u64) -> Result<Vec<u8>, String> {
        self.read_from(&self.dist, &format!("v{version}/{file}"), most)
    }

    /// Makes Fennec's folder and its toolchains' folder, if they aren't there, its user's alone, as
    /// the cache makes them: what's kept there is run. See `fennec_dist::private_folder`.
    fn private_toolchains(&self) -> Result<(), String> {
        fennec_dist::private_folder(&self.home)
            .and_then(|()| fennec_dist::private_folder(&self.toolchains()))
            .map_err(|refused| refused.0)
    }

    /// Reads one of a release's files.
    fn fetch(&self, version: &str, file: &str) -> Result<Vec<u8>, String> {
        self.read_from(&self.dist, &format!("v{version}/{file}"), MOST_BYTES)
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
        let bytes = self.fetch_at_most(version, &archive.file, archive.size)?;
        archive.check(&bytes)?;
        let entries = unpacked(archive, &bytes, &[&fennec_file()])?;
        let toolchains = self.toolchains();
        self.private_toolchains()?;
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

/// What a release's archive holds, checked already, which has to have each of `programs`, to run,
/// in its top folder.
fn unpacked(
    archive: &fennec_dist::Archive,
    bytes: &[u8],
    programs: &[&str],
) -> Result<Vec<unpack::Entry>, String> {
    let entries = if let Some(top) = archive.file.strip_suffix(".tar.gz") {
        unpack::tar_gz(bytes, top)?
    } else if let Some(top) = archive.file.strip_suffix(".zip") {
        unpack::zip(bytes, top)?
    } else {
        return Err(format!("{} isn't a .tar.gz or a .zip", archive.file));
    };
    for program in programs {
        let there = entries
            .iter()
            .any(|entry| entry.runnable && entry.bytes.is_some() && entry.path == Path::new(program));
        if !there {
            let shown = program.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(program);
            return Err(format!("{} has no `{shown}` to run", archive.file));
        }
    }
    Ok(entries)
}

/// Moves the program `new` to `at`, in place of the one there, in one step. Windows won't let a
/// program that's running be replaced, which the launcher is, but will let it be renamed: so there
/// the old one is moved aside first, to a name of its own, and taken away the next time, when it's
/// no longer running.
fn put_in_place(new: &Path, at: &Path) -> Result<(), String> {
    let failed = |err: std::io::Error| format!("couldn't put the new launcher in {}: {err}", at.display());
    if cfg!(windows) && at.exists() {
        let folder = at.parent().unwrap_or(Path::new("."));
        for old in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            if old.file_name().to_str().is_some_and(|name| name.starts_with(".replaced.")) {
                let _ = std::fs::remove_file(old.path());
            }
        }
        let name = at.file_name().and_then(|name| name.to_str()).unwrap_or("fennec");
        let aside = folder.join(format!(".replaced.{name}.{}", std::process::id()));
        std::fs::rename(at, &aside).map_err(failed)?;
        return std::fs::rename(new, at).map_err(|err| {
            // The old one goes back, so there's still a launcher to run.
            let _ = std::fs::rename(&aside, at);
            failed(err)
        });
    }
    std::fs::rename(new, at).map_err(failed)
}

/// The name of the `fennec` command on this platform.
fn fennec_file() -> String {
    format!("fennec{}", std::env::consts::EXE_SUFFIX)
}

/// Whether version `a` comes before `b`, both plain, as `0.4.2` before `0.4.10`.
fn older(a: &str, b: &str) -> bool {
    let parts =
        |version: &str| -> Vec<u64> { version.split('.').filter_map(|part| part.parse().ok()).collect() };
    parts(a) < parts(b)
}
