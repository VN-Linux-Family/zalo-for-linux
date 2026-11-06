# Contributing to Zalo for Linux

**English** | [Tiếng Việt](./CONTRIBUTING.vi.md)

Thanks for taking the time to contribute. Zalo for Linux is a port that originates from the official macOS build and repackages it with patches for Linux.

## Before you start

Read the documents that explain how the project works:

- [docs/dev/architecture.md](./docs/dev/architecture.md) — the build pipeline and how the patches are applied
- [docs/dev/development.md](./docs/dev/development.md) — building from source, the scripts, environment variables, adding patches
- [docs/dev/patch-guide.md](./docs/dev/patch-guide.md) — how to write, test, and maintain JavaScript patches
- [docs/dev/ci-workflow.md](./docs/dev/ci-workflow.md) — GitHub Actions CI/CD, upstream polling, and release bot
- [docs/user/features.md](./docs/user/features.md) — what this port adds over the official client
- [docs/user/known-issues.md](./docs/user/known-issues.md) — known limitations and the history of resolved bugs

Then look through the [issues](https://github.com/VN-Linux-Family/zalo-for-linux/issues) and pull requests that already exist before starting: someone may be doing the same work.

Zalo itself is proprietary. Please never commit Zalo binaries, extracted `app.asar` contents, or prebuilt `.node` files — the native addons are always rebuilt from source and no proprietary code ships in this repository ([nativelibs/README.md](./nativelibs/README.md)).

## Ways to contribute

- **Bug reports** — see [Reporting bugs](#reporting-bugs).
- **Feature requests** — see [Feature requests](#feature-requests).
- **Code** — JavaScript patches in `scripts/patches/`, Rust and gyp addons in `nativelibs/` and `zocr/`, Node.js and Python in `zcall-native/`.
- **Documentation** — keep documentation accurate and clear (see [Documentation](#documentation)).
- **Testing** — especially on aarch64
- **Packaging** — the AppImage is the official artifact; the repository also has a Nix flake (see the NixOS section in [README.md](./README.md)).

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
npm ci --prefer-offline --no-audit
npm run main
```

Setup and packaging can also run separately:

```bash
npm run main:setup      # download + extract into app/ and temp/
npm run main:build      # package the AppImage from app/
npm run start           # run the extracted app in development mode
```

For the full list of scripts, environment variables (`ZALO_VERSION`, `ZADARK_VERSION`, `FORCE_DOWNLOAD`) and how to add a patch, see [DEVELOPMENT.md](./docs/dev/development.md).

## Code style

Patches live one per file in `scripts/patches/patch-<name>.js` and export `{ main }`. Always check that the expected pattern exists before replacing — a Zalo update changes the code, so the pattern is no longer found to patch — then register the patch in `scripts/prepare-app.js` (see "Adding a New Patch" in [DEVELOPMENT.md](./docs/dev/development.md)).

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

Documentation is organized centrally with full bilingual support (English & Vietnamese):

- **Developer & Technical Documentation** (`docs/dev/`): Architecture, development, patch guide, and CI workflow (`<doc>.md` and `<doc>.vi.md`).
- **User Documentation** (`docs/user/` and root `README`): Available in English and Vietnamese (`README.md` / `docs/user/README.vi.md`, `known-issues.md` / `known-issues.vi.md`, `features.md` / `features.vi.md`).
- **Component Documentation**: Native addons in `nativelibs/` contain their own dedicated README files.

## Commits and pull requests

Pull requests target **`main`**.

- Branch names: `feat/<slug>`, `fix/<slug>`, `docs/<slug>`, `chore/<slug>` — never open a PR from your fork's `main` branch into the original repository's `main`.
- One logical change per pull request; if you fix several, please split them — one feature per PR makes it easier for reviewers to read your code.
- Always rebase on `main` before opening the PR.
- The `app.setName` in `main.js` and the StartupWMClass entry in the `build` section of `package.json` **must be `Zalo`**.
- If you develop a new feature, please test it locally first, then capture a screenshot (if it contains your private infomation, please blur them, otherwise **we will delete your screenshot for your privacy**).
- If you add/update a new Native addon, please update [architecture.md](./docs/dev/architecture.md) and [nativelibs/README.md](./nativelibs/README.md), so that newcomers can understand what your addon will do.
- Commit subject: follow the convention — see [this link](https://www.conventionalcommits.org/en/v1.0.0/).
- Pull request description: what changed and why, `Closes #N` when it fixes an issue, the platform you tested (x86_64 or aarch64, X11 or Wayland), and screenshots for UI changes.
- No reverse-engineered codes or nativelibs may violate our guidelines; please refer to [the guidelines below](#re-and-rea-guidelines-for-nativelibs) for more information.

Checklist before opening:

- [ ] `npm test` passes, and the Rust tests above pass if you touched a crate.
- [ ] No generated files (`app/`, `temp/`, `dist/`, `node_modules/`).
- [ ] No prebuilt binaries or `.node` files.
- [ ] No RE codes or nativelibs may violate the guidelines (mandatory)
- [ ] Tested locally and captured the screenshot.
- [ ] Documentation updated (if your change affects user guides or dev specs).

## Reporting bugs

Include:

- the version and the variant (Original or ZaDark), and how you installed it (AppImage, AM, Gear Lever, NixOS, AUR);
- distribution and kernel version;
- desktop environment and display server (X11 or Wayland);
- steps to reproduce, what you expected and what happened;

## Feature requests

Check [features.md](./docs/user/features.md), [known-issues.md](./docs/user/known-issues.md) and the existing issues first. Describe the use case and why it belongs in this port rather than in Zalo itself.

## AI usage

We welcome the responsible use of AI tools (such as Codex, Claude Code, etc.) to assist with writing code patches, documentation, or debugging. However, please adhere to the following guidelines:

- **Review everything:** You are fully responsible for all code and content you submit. Ensure you thoroughly review, test, and understand any AI-generated output before opening a pull request.
- **Disclose significant use:** If a pull request or a major part of a patch/documentation is heavily generated or assisted by AI, please briefly mention it in the pull request description for transparency.
- **No unverified code:** Do not blindly copy-paste AI-generated code, especially for native addons (`nativelibs/`, `zocr/`) or security-sensitive components, as they may introduce memory safety issues or regressions.

## RE and REA guidelines for `nativelibs`

This project welcome reverse engineering (RE) and Reverse Engineer Anything (REA) efforts to help add back native addons or fix issues. However, to protect the project from proprietary contamination and legal risks, strict protocols must be followed:

- **Clean-room environment:** 
  - RE and REA activities must strictly follow clean room practices. If using the same AI model, you must start a brand new chat session and delete the old one (*using two different AI models is highly recommended to prevent cross-contamination*).
  - Inside your working directory, only the documentation files originating from the previous chat session are allowed.
  - Once the "dirty" stage (raw decompiled/unclean code or analysis) is finished, all associated binaries and code must be completely wiped out to prevent AI models from processing proprietary contents.
- **AI attribution for REA:** If any code or patch is produced via REA, you must explicitly state that it was generated by AI (refer to the [AI Usage](#ai-usage) section above).
- **Prohibitions:** 
  - **STRICTLY PROHIBITED:** REA performed within the same/shared chat session; utilizing proprietary or "All Rights Reserved" binaries/code for post-dirty code; or performing RE via direct copy-pasting. (If the original code is opened but restricted, please create a `.js` patch instead — see [this section](./docs/dev/development.md#adding-a-new-patch)).
- **Enforcement:** If these rules are not strictly met, **maintainers have the right to close your pull request immediately without prior notice.**

## Code from other repositories

If you incorporate code from another repository that does not use the BSD 3-Clause license, please include the original license information as follows:

- **For an entire directory:** Add a notice file (please name it `LICENSE-<directory>`) at the root of the taken directory:

  ```text
  This folder is licensed under the BSD 3-Clause LICENSE file at the project root.

  --------------------------------------------------------------------------------

  ORIGINAL CODE & PORTIONS NOTICE (<original-license>)

  The original code inside this directory was derived from:
  <repo> (you can add "now deleted/hidden" if the link to the repository is dead)

  Copyright (c) 2024-2026 <name> <email (if available)>

  <original-license-text>
  ```

- **For an individual file**: Add a comment header at the very top of the file:

  ```text
  Original copyright
  This file is derived from: <original-repo>
  Copyright (c) 2026 <name> <email>
  Licensed under the <original-license> License.
  ---------------------------------------------------------------------------
  Integration copyright
  Copyright (c) 2026 doandat943 and Việt Nam Linux Family
  Licensed under the BSD 3-Clause License.
  ```

- **Testing & Compatibility:** Just like code or patches assisted by AI, you are fully responsible for testing external code to ensure them works within our build pipeline without regressions or memory leaks.
- **Explicit attribution:** Clearly specify the exact source repository and origin in your pull request description and commit history.

## Security

Do not open a public issue for a vulnerability. Use the **Security** tab of the repository and choose "Report a vulnerability" (GitHub private advisory).

## License

By contributing you agree that your contributions are licensed under the same license as the project — see [LICENSE](./LICENSE). This repository has no CLA. Zalo is a trademark of VNG Corporation; this project is not affiliated with or endorsed by VNG Corporation.

## Code of conduct

This project follows the [Contributor Covenant, version 2.0](https://www.contributor-covenant.org/version/2/0/code_of_conduct/).

Thanks for contributing!
