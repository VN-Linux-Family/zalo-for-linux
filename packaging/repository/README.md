# Signed distro repositories (optional upstream feature)

This adds package-manager installation and updates without a separate app or a
separate server. Package payloads and APT/Arch indexes are assets of the newest
normal GitHub Release. GitHub Pages hosts only the installer, public signing key
and RPM metadata.

The normal release continues to work when `REPO_ENABLED` is unset. This keeps a
pull request safe to merge before the upstream maintainer creates a signing key.

## What users run after upstream enables it

```bash
curl -fsSL https://vn-linux-family.github.io/zalo-for-linux/install.sh | bash -s -- --variant original
```

ZaDark instead:

```bash
curl -fsSL https://vn-linux-family.github.io/zalo-for-linux/install.sh | bash -s -- --variant zadark
```

Users who want to inspect the installer first can download it to a file and run
the same arguments with `bash install.sh`.

`--original` and `--zadark` are shortcuts. `--yes` accepts package-manager
prompts. `--print-plan` detects the distro and prints the plan without changing
the machine. Re-running the installer with the other variant switches packages
without deleting the user's Zalo data.

| System | Package installed | Future update command |
| --- | --- | --- |
| Debian, Ubuntu, Linux Mint, Pop!_OS | DEB | `sudo apt update && sudo apt upgrade` |
| Fedora | RPM | `sudo dnf upgrade` |
| openSUSE Leap | RPM | `sudo zypper update` |
| openSUSE Tumbleweed | RPM | `sudo zypper dup` |
| Arch, Manjaro, EndeavourOS | pacman package | `sudo pacman -Syu` |

The package names are stable: `zalo-for-linux` for Original and
`zalo-for-linux-zadark` for ZaDark. Each package conflicts with the other, so a
normal system update preserves the selected variant. AppImage, portable archive
and Nix users continue using their existing update methods.

Supported repository architectures are `x86_64` and `aarch64`. The application
requires glibc 2.35 or newer; Alpine/musl is not supported by this installer.

## One-time upstream setup

The maintainer only needs to run one command from a trusted checkout:

```bash
packaging/repository/setup-maintainer.sh VN-Linux-Family/zalo-for-linux
```

It creates a dedicated temporary signing key, uploads the private key as the
Actions secret `REPO_SIGNING_KEY`, sets `REPO_SIGNING_FINGERPRINT` and
`REPO_ENABLED=true`, enables the GitHub Actions Pages source through the GitHub
API, and queues one forced build so the repository becomes available immediately.
The private key is never written to the repository.

The setup script requires authenticated GitHub CLI (`gh auth status`) with
permission to manage Actions secrets, variables and Pages for the upstream
repository.
It defaults to the repository reported by `gh repo view`; pass `OWNER/REPO` as
its only argument to be explicit.

Manual setup is also possible. Store an ASCII-armored, unencrypted CI signing
private key as the Actions secret `REPO_SIGNING_KEY`; set repository variables
`REPO_SIGNING_FINGERPRINT` to its complete primary-key fingerprint and
`REPO_ENABLED=true`; enable GitHub Actions as the Pages source; then dispatch the
main workflow with `force_build=true`. Never commit the private key.

## Release and update flow

When a new Zalo version is detected, the existing build produces Original and
ZaDark packages for every supported format. The optional repository job then:

1. signs the RPM and Arch packages;
2. generates and signs APT, RPM and Arch repository metadata;
3. adds the signed packages and indexes to that version's normal GitHub Release;
4. deploys the installer, public key and RPM metadata to GitHub Pages.

The stable package URL is
`https://github.com/VN-Linux-Family/zalo-for-linux/releases/latest/download`.
Users therefore follow the newest non-prerelease Release automatically through
their distro's ordinary update command. No `packages-*` prereleases, external
web server, Nginx configuration or manual metadata upload are needed.

The installer verifies that the downloaded public key has the fingerprint
embedded at build time. APT uses a repository-scoped `Signed-By` keyring. RPM
checks both package and repository metadata signatures. Arch requires signed
packages and a signed database. Signing-key rotation still requires a deliberate
migration for already-installed clients.

## Checks

```bash
node --test scripts/build-linux-packages.test.js
python3 -m unittest discover -s packaging/repository -p 'test_*.py' -v
bash -n packaging/repository/install.sh.in
```

The repository test creates a disposable key and tiny DEBs, verifies the signed
index with an isolated APT state directory, and never installs on the host.
Publishing uses `apt-utils`, `createrepo-c`, `rpm`, `gnupg`,
`pacman-package-manager`, and `libarchive-tools` on the GitHub runner.
