# Releases and who vouches for them

What the launcher installs is a release that the release keys have signed. CI builds a release, and a person signs it, offline, on a hardware key. The launcher builds in the public keys and trusts nothing else: not the release page, a mirror, a CDN, or TLS.

## What gets signed

A release is a **manifest**: its first line names what it is and its version, and then there's a line for each platform's archive, with the file's name, size and BLAKE3 hash.

```text
fennec toolchain 0.4.2
aarch64-apple-darwin fennec-v0.4.2-aarch64-apple-darwin.tar.gz 51234567 1f0c…
x86_64-unknown-linux-gnu fennec-v0.4.2-x86_64-unknown-linux-gnu.tar.gz 50999104 9ab4…
```

The release workflow writes it from the archives it built, as `fennec-v0.4.2.manifest` beside them, and publishes the release as a prerelease. There's one way to write a manifest, so only that exact text reads as one. The launcher takes an archive only when its size and hash are the manifest's.

## Who signs

There are three release keys:

- two hardware keys (YubiKeys), whose Ed25519 key never leaves the device and signs only when it's touched;
- one key kept on paper, offline, for when a hardware key is lost.

| release | signatures it needs | namespace |
|---|---|---|
| a toolchain | 1 of the 3 keys | `fennec-toolchain` |
| the launcher | 2 of the 3, from different keys | `fennec-launcher` |

The launcher is what checks everything else, so one stolen key can't replace it. Each kind is signed in its own namespace, so a toolchain's signature never counts for a launcher. **The 2-of-3 rule for the launcher is waiting on Chris's final OK.** It's one number in `crates/dist/src/lib.rs`.

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

1. Download `fennec-v0.4.2.manifest` from the prerelease.
2. Once release builds are reproducible, which comes next, build it again from the tag and check that its hashes are the manifest's.
3. Sign it, touching the key when it blinks:

   ```bash
   ssh-keygen -Y sign -f ~/.ssh/fennec_release_1 -n fennec-toolchain fennec-v0.4.2.manifest
   ```

4. Check it before uploading, with `fennec-dist`, which this repository builds:

   ```bash
   fennec-dist verify toolchain fennec-v0.4.2.manifest fennec-v0.4.2.manifest.sig
   ```

5. Upload `fennec-v0.4.2.manifest.sig` to the release, and mark it a release.

A launcher release is signed the same way in the `fennec-launcher` namespace, by two of the keys. The two signatures go one after the other in one `.sig` file.
