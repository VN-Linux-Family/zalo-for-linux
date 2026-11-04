# Contributing to Zalo for Linux

**English** | [Tiếng Việt](./readme/CONTRIBUTING.vi.md)

Thanks for taking the time to contribute. Zalo for Linux is a port that originates from the official macOS build and repackages it with patches for Linux.

## Before you start

Read the documents that explain how the project works:

- [ARCHITECTURE.md](./ARCHITECTURE.md) — the build pipeline and how the patches are applied
- [DEVELOPMENT.md](./DEVELOPMENT.md) — building from source, the scripts, environment variables, adding patches
- [FEATURES.md](./FEATURES.md) — what this port adds over the official client
- [KNOWN_ISSUES.md](./KNOWN_ISSUES.md) — known limitations and the history of resolved bugs

Then look through the [issues](https://github.com/VN-Linux-Family/zalo-for-linux/issues) and pull requests that already exist before starting: someone may be doing the same work.

Zalo itself is proprietary. Never commit Zalo binaries, extracted `app.asar` contents, or prebuilt `.node` files — the native addons are always rebuilt from source and no proprietary code ships in this repository ([nativelibs/README.md](./nativelibs/README.md)).

## Ways to contribute

- **Bug reports** — see [Reporting bugs](#reporting-bugs).
- **Feature requests** — see [Feature requests](#feature-requests).
- **Code** — JavaScript patches in `scripts/patches/`, Rust addons in `nativelibs/` and `zocr/`, Node.js and Python in `zcall-native/`.
- **Documentation** — keep the English and Vietnamese versions in sync (see [Documentation](#documentation)).
- **Testing** — especially on aarch64
- **Packaging** — the AppImage is the official artefact; the repository also has a Nix flake (see the NixOS section in [README.md](./README.md)).

## Development setup

Requirements:

- Linux x86_64 or aarch64
- Node.js 22 and npm (CI uses `node-version: '22'`)
- `7z` for extracting the macOS app
- C++ build tools for the native addons
- Rust toolchain (stable)
- For calls: `python3`, libopus and the PulseAudio tools

```bash
git clone https://github.com/VN-Linux-Family/zalo-for-linux.git
cd zalo-for-linux
git submodule update --init --recursive
npm run main
```

Setup and packaging can also run separately:

```bash
npm run main:setup      # download + extract into app/ and temp/
npm run main:build      # package the AppImage from app/
npm run start           # run the extracted app in development mode
```

For the full list of scripts, environment variables (`ZALO_VERSION`, `ZADARK_VERSION`, `FORCE_DOWNLOAD`) and how to add a patch, see [DEVELOPMENT.md](./DEVELOPMENT.md).

## Code style

Patches live one per file in `scripts/patches/patch-<name>.js` and export `{ main }`. Always check that the expected pattern exists before replacing — a Zalo update changes the code, so the pattern is no longer found to patch — then register the patch in `scripts/prepare-app.js` (see "Adding a New Patch" in [DEVELOPMENT.md](./DEVELOPMENT.md)).

## Tests

```bash
npm ci --prefer-offline --no-audit   # run once
npm test                             # node --test 'tests/**/*.test.js'
```

Add tests under `tests/<area>-<topic>.test.js` — for example `scripts-patches.test.js`, `plugins-badge-icon.test.js`, `zcall-zrtp-packet.test.js`.

The Rust crates:

```bash
cargo test --manifest-path zocr/Cargo.toml --locked
for c in nativelibs/*/Cargo.toml; do cargo test --manifest-path "$c" --locked; done
```

Optional benchmarks: `npm run bench` and `npm run bench:patches`.

## Documentation

Every document exists twice: `<NAME>.md` at the repository root (English) and `readme/<NAME>.vi.md` (Vietnamese). Both carry a language switcher directly below the title:

```markdown
**English** | [Tiếng Việt](./readme/<NAME>.vi.md)
```

```markdown
[English](../<NAME>.md) | **Tiếng Việt**
```

Update both versions in the same pull request.

## Commits and pull requests

Pull requests target **`main`**.

- Branch names: `feat/<slug>`, `fix/<slug>`, `docs/<slug>`, `chore/<slug>` — never open a PR from your fork's `main` branch into the original repository's `main`.
- One logical change per pull request; if you fix several, please split them — one feature per PR makes it easier for reviewers to read your code.
- Always rebase on `main` before opening the PR.
- Commit subject: follow the convention — see [this link](https://www.conventionalcommits.org/en/v1.0.0/).
- Pull request description: what changed and why, `Closes #N` when it fixes an issue, the platform you tested (x86_64 or aarch64, X11 or Wayland), and screenshots for UI changes.

Checklist before opening:

- [ ] `npm test` passes, and the Rust tests above pass if you touched a crate.
- [ ] No generated files (`app/`, `temp/`, `dist/`, `node_modules/`).
- [ ] No prebuilt binaries or `.node` files.
- [ ] Documentation updated in both languages.

## Reporting bugs

Include:

- the version and the variant (Original or ZaDark), and how you installed it (AppImage, AM, Gear Lever, NixOS, AUR);
- distribution and kernel version;
- desktop environment and display server (X11 or Wayland);
- steps to reproduce, what you expected and what happened;

## Feature requests

Check [FEATURES.md](./FEATURES.md), [KNOWN_ISSUES.md](./KNOWN_ISSUES.md) and the existing issues first. Describe the use case and why it belongs in this port rather than in Zalo itself.

## Security

Do not open a public issue for a vulnerability. Use the **Security** tab of the repository and choose "Report a vulnerability" (GitHub private advisory).

## License

By contributing you agree that your contributions are licensed under the same license as the project — see [LICENSE](./LICENSE). This repository has no CLA. Zalo is a trademark of VNG Corporation; this project is not affiliated with or endorsed by VNG Corporation.

## Code of conduct

This project follows the [Contributor Covenant, version 2.0](https://www.contributor-covenant.org/version/2/0/code_of_conduct/).

Thanks for contributing!
