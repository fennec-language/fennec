# Releases and who vouches for them

What the launcher installs is a release that the release keys have signed. CI builds a release, and a person signs it, offline, on a hardware key. The launcher builds in the public keys and trusts nothing else: not the release page, a mirror, a CDN, or TLS.

## What gets signed

A release is a **manifest**: its first line names what it is and its version, and then there's a line for each platform's archive, with the file's name, size and BLAKE3 hash.

```text
fennec toolchain 0.4.2
aarch64-apple-darwin fennec-v0.4.2-aarch64-apple-darwin.tar.gz 51234567 1f0c…
x86_64-unknown-linux-gnu fennec-v0.4.2-x86_64-unknown-linux-gnu.tar.gz 50999104 9ab4…
```

The release workflow writes it from the archives it built, as `fennec-v0.4.2.manifest` beside them, and publishes the release as a prerelease. It builds the launcher for each platform too, as `fennec-launcher-v0.4.2-<platform>.tar.gz` (`.zip` on Windows), whose one folder holds `fennec`, `fennec-setup` and `fennec-fetch`, and writes their manifest, `fennec-launcher-v0.4.2.manifest`, beside the toolchain's. Each night it installs both through the launcher's own code into an empty Fennec home, from a folder laid out as the bucket is and signed with keys made for the run, and uses them as a person would: `.github/scripts/release-dry-run.sh`. There's one way to write a manifest, so only that exact text reads as one. The launcher takes an archive only when its size and hash are the manifest's.

## Who signs

There are three release keys:

- two hardware keys (YubiKeys), whose Ed25519 key never leaves the device and signs only when it's touched;
- one key kept on paper, offline, for when a hardware key is lost.

| release | signatures it needs | namespace |
|---|---|---|
| a toolchain | 1 of the 3 keys | `fennec-toolchain` |
| the launcher | 2 of the 3, from different keys | `fennec-launcher` |

The launcher is what checks everything else, so one stolen key can't replace it. Each kind is signed in its own namespace, so a toolchain's signature never counts for a launcher. **The 2-of-3 rule for the launcher is locked:** Chris decided it on 2026-11-08, along with CI never signing. It's one number in `crates/dist/src/lib.rs`.

## Making the keys, once

With OpenSSH 8.2 or later, built with FIDO support (Homebrew's `openssh` is; the copy macOS ships may not be):

```bash
ssh-keygen -t ed25519-sk -O verify-required -O application=ssh:fennec-release -C "fennec release, YubiKey 1" -f ~/.ssh/fennec_release_1
```

That's one for each YubiKey. The file it writes is only a handle: the key itself stays on the device. The paper key, on a machine that's never online:

```bash
ssh-keygen -t ed25519 -C "fennec release, paper" -f fennec_release_paper
```

Print the private key, then destroy the file. Each `.pub` file's line goes in `RELEASE_KEYS` in `crates/dist/src/lib.rs`. Until that's done, nothing verifies, so nothing can be installed. Nobody but the person signing ever sees a private key: not CI, not this repository, and not an agent.

## Signing a release

1. Download `fennec-v0.4.2.manifest` and `fennec-launcher-v0.4.2.manifest` from the prerelease.
2. Check that the release's `Built twice, the same` job passed. CI builds each archive twice, from checkouts in two places on two machines, and that job holds that both builds made the same bytes, on every platform. One script builds them, `.github/scripts/release-build.sh`, with the environment in `release-env.sh`. On Linux they're built in containers pinned by digest, with their compilers pinned by version in `release-container.sh`, so the same commit builds the same bytes off GitHub too: no path of the machine's, and no time but the commit's, goes in what's built.
3. Build it again yourself, off GitHub, and sign only if it says every archive is the same:

   ```bash
   .github/scripts/rebuild-release.sh v0.4.2 fennec-v0.4.2.manifest fennec-launcher-v0.4.2.manifest
   ```

   It clones the tag afresh and builds this machine's Linux archives, musl runtime and all, in the containers and with the compilers the tag pins. Each archive's size and BLAKE3 hash has to be the manifest's line for it. It exits 0 only when every archive is; otherwise it names each one that isn't, says not to sign, and keeps its build to look at. It needs git and Docker: on a Mac with Colima, run it with `REBUILD_DOCKER="docker --context <yours>"`. See [what the rebuild checks](#what-the-rebuild-checks).
4. Sign it, touching the key when it blinks:

   ```bash
   ssh-keygen -Y sign -f ~/.ssh/fennec_release_1 -n fennec-toolchain fennec-v0.4.2.manifest
   ```

5. Check it before uploading, with `fennec-dist`, which this repository builds:

   ```bash
   fennec-dist verify toolchain fennec-v0.4.2.manifest fennec-v0.4.2.manifest.sig
   ```

6. Upload `fennec-v0.4.2.manifest.sig` to the release, and mark it a release.

A launcher release is signed the same way in the `fennec-launcher` namespace, by two of the keys. The two signatures go one after the other in one `.sig` file.

### Putting the launcher up

Toolchains are fetched from the GitHub release. The launcher is fetched from the bucket `fennec-dl`, served at `https://dl.fennec-lang.dev`. Once `fennec-launcher-v0.4.2.manifest.sig` is on the release, start the release workflow by hand on the tag:

```bash
gh workflow run release.yml --ref v0.4.2
```

It builds the tag again, and its `upload` job waits for Chris to approve the `release` environment, which holds the bucket's token. Before anything else, it lists the bucket, writing nothing, so a wrong token fails there. Then it uploads only when all of these hold, and otherwise fails saying which one didn't:
- the release has the `.sig`;
- the release keys built into `fennec-dist` signed the manifest as the launcher needs;
- the manifest is the one this run built, byte for byte;
- the run's own `Built twice, the same` check and dry run passed.

The archives it uploads are the ones that run built, kept 14 days for this, so approve it within 14 days. After that, the upload fails for want of them, uploading nothing, and a new run on the tag builds them again.

It puts the archives, the manifest and its `.sig` under `launcher/v0.4.2/`, never replacing a file already there with other bytes. It moves `latest` last, and never to an older launcher. Then it reads both back from `https://dl.fennec-lang.dev`. Until there are release keys, nothing can verify, so it uploads nothing. The script is `.github/scripts/release-upload.sh`.

### What the rebuild checks

The rebuild is what shows a release is its tag's source and nothing else, since it doesn't run on GitHub's machines. It builds what the signing machine runs without emulation:

| archive | checked by |
|---|---|
| Arm Linux (`aarch64-unknown-linux-gnu`) | the signer's rebuild, from a Mac or an Arm Linux machine |
| x86-64 Linux (`x86_64-unknown-linux-gnu`) | the signer's rebuild from an x86-64 Linux machine; from a Mac, CI's `Built twice, the same` check only |
| Arm Mac (`aarch64-apple-darwin`) | CI's `Built twice, the same` check only, until the release's Xcode is pinned and the signer has that Xcode |
| Intel Mac (`x86_64-apple-darwin`) | CI's `Built twice, the same` check only |
| Windows (`x86_64-pc-windows-gnullvm`) | CI's `Built twice, the same` check only |

CI's check builds each archive twice on two of GitHub's machines, so it shows a build doesn't depend on where it ran, but not that GitHub built from the tag.

`fennec self update` installs a launcher release. It reads `latest`, the newest launcher's version, then `v<version>/fennec-launcher-v<version>.manifest`, its `.sig` and the archive for its platform, from `https://dl.fennec-lang.dev/launcher` or `FENNEC_LAUNCHER_DIST`. The archive's one top folder, `fennec-launcher-v<version>-<platform>`, holds the launcher's three programs: `fennec`, `fennec-setup` and `fennec-fetch`. It replaces only the launcher installed as `~/.fennec/bin/fennec`, only with a newer version, and only one two of the keys signed. Until there are keys, it refuses before fetching anything.
