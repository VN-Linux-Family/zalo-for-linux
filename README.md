# Zalo for Linux 🐧

**English** | [Tiếng Việt](./docs/user/README.vi.md)

[![Build Status](https://github.com/VN-Linux-Family/zalo-for-linux/actions/workflows/build.yml/badge.svg)](https://github.com/VN-Linux-Family/zalo-for-linux/actions/workflows/build.yml)

[![All Contributors](https://img.shields.io/github/all-contributors/VN-Linux-Family/zalo-for-linux?color=ee8449&style=flat-square)](#contributors)

An unofficial, community-driven port of the Zalo desktop application for **Linux only**, created by repackaging the official macOS client into a standard AppImage with integrated ZaDark.

Thanks **realdtn2** for the solution: [realdtn2/zalo-linux-2026](https://github.com/DMKha241/zalo-linux-2026) (original repo is deleted/hidden, we are using a backup repo from [DMKha241](https://github.com/DMKha241)).

## ⚡ Quick Start

### 1. AM

Install and update directly using [AM](https://github.com/ivan-hc/AM) (for a graphical interface, install [AM-GUI](https://github.com/Shikakiben/AM-GUI)):

```bash
# Install Zalo
am -i zalo

# Update Zalo
am -u
```

<p align="center">
  <img width="600" alt="installer" src="./sample/am-guide.png" />
</p>

---

### 2. Nix

The repository provides a Nix flake packaging the release AppImage with call dependencies:

**NixOS Configuration (`flake.nix` + `configuration.nix`):**

```nix
# flake.nix
{
  inputs.zalo-for-linux = {
    url = "github:VN-Linux-Family/zalo-for-linux";
    inputs.nixpkgs.follows = "nixpkgs";
  };
  outputs = { nixpkgs, zalo-for-linux, ... }: {
    nixosConfigurations.<hostname> = nixpkgs.lib.nixosSystem {
      modules = [ zalo-for-linux.nixosModules.default ./configuration.nix ];
    };
  };
}
```

```nix
# configuration.nix
programs.zalo = {
  enable = true;
  # variant = "original"; # omit for default ZaDark variant
};
```

**Home Manager (`home.nix`):**

```nix
programs.zalo.enable = true;
```

**Quick Try without installing:**

```bash
nix run github:VN-Linux-Family/zalo-for-linux
```

---

### 3. Other Installation Methods

<details>
<summary><b>⚙️ Gear Lever (Desktop Integration)</b></summary>

1. Download the latest `.AppImage` from [**Releases**](https://github.com/VN-Linux-Family/zalo-for-linux/releases).
2. Install **Gear Lever** from [Flathub](https://flathub.org/en/apps/it.mijorus.gearlever).
3. Open Gear Lever $\rightarrow$ click **Open** $\rightarrow$ select the downloaded `.AppImage`.
4. Click **Move to the app menu** to integrate Zalo into your desktop application launcher.
5. Updates are detected automatically inside Gear Lever via the **Update** button.

</details>

<details>
<summary><b>📦 Manual AppImage Run</b></summary>

Download the latest `.AppImage` from [**Releases**](https://github.com/VN-Linux-Family/zalo-for-linux/releases), make it executable, and run:

```bash
chmod +x Zalo-*.AppImage
./Zalo-*.AppImage
```

> 📞 **Voice & Video Calls Requirement**: Install audio utilities for native call support:
>
> - **Debian / Ubuntu**: `sudo apt install python3 libopus0 pulseaudio-utils`
> - **Arch Linux**: `sudo pacman -S python opus pulseaudio`
> - **Fedora / RHEL**: `sudo dnf install python3 opus pulseaudio-utils`
> - **openSUSE**: `sudo zypper install python3 libopus0 pulseaudio-utils`
> - **Gentoo**: `sudo emerge --ask dev-lang/python media-libs/opus media-sound/pulseaudio-daemon`

</details>

<details>
<summary><b>🛠️ Build from Source</b></summary>

Prerequisites: Node.js 22+, npm, `7z` (`p7zip-full`), C++ & Rust toolchains.

```bash
git clone https://github.com/VN-Linux-Family/zalo-for-linux.git
cd zalo-for-linux
git submodule update --init --recursive
npm ci --prefer-offline --no-audit
npm run main
```

The compiled AppImage will be saved in `dist/`. For build pipeline details and environment variables, see [development.md](./docs/dev/development.md).

</details>

---

## 📚 Documentation & Guides

| Guide                                                         | Purpose                                                                      |
| :------------------------------------------------------------ | :--------------------------------------------------------------------------- |
| [**Known Issues & Workarounds**](./docs/user/known-issues.md) | Symptom-to-fix lookup table, Ubuntu sandbox fix, and call troubleshooting.   |
| [**Feature Overview**](./docs/user/features.md)               | Offline mode, system tray autostart, custom userscripts, and ZaDark theme.   |
| [**Architecture Specs**](./docs/dev/architecture.md)          | Technical overview of Electron wrapping, patch pipeline, and native modules. |
| [**Development Guide**](./docs/dev/development.md)            | Setup instructions, debugging workflows, and local test commands.            |
| [**Patch Authoring Guide**](./docs/dev/patch-guide.md)        | How to write, register, and maintain resilient JavaScript patches.           |
| [**CI/CD & Release Workflow**](./docs/dev/ci-workflow.md)     | Upstream polling mechanism, release bot, Nix hash updates, and fork rules.   |
| [**Contributing Guidelines**](./CONTRIBUTING.md)              | Pull request standards, commit conventions, and testing requirements.        |

---

## ⚙️ How It Works

This project is not a from-scratch rewrite of Zalo. It works by:

1. Downloading the official macOS `.dmg` file.
2. Using `7z` to extract the `app.asar` archive, which contains the main application logic written in JavaScript.
3. Removing incompatible native macOS files.
4. Wrapping the extracted application in a minimal, Linux-compatible Electron shell.
5. Using `electron-builder`, then `quick-sharun` to package everything into a single, portable `AppImage` file.

For a deeper dive into the build pipeline and patching strategy, see [`ARCHITECTURE.md`](./docs/dev/architecture.md).

For native addons (db-cross-v4, etc.), see [`nativelibs/README.md`](./nativelibs/README.md).

For the native call engine, see [`zcall-native/README.md`](./zcall-native/README.md)

## 🐛 Troubleshooting & Debugging

If you encounter issues or want to inspect the app's behavior, you can easily open Chrome Developer Tools (DevTools) using the following methods:

- **Keyboard Shortcut**: Press `Ctrl` + `Shift` + `I` while the Zalo window is focused.
- **Tray Menu**: Right-click the Zalo tray icon and select **"Toggle DevTools"**.

## Contributors

Thanks goes to these wonderful people

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/doandat943"><img src="https://avatars.githubusercontent.com/u/113295259?v=4&size=100" width="100px" height="100px" alt="DANG DOAN DAT"/><br /><sub><b>DANG DOAN DAT</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=doandat943" title="Code">💻</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=doandat943" title="Documentation">📖</a> <a href="#infra-doandat943" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a> <a href="#projectManagement-doandat943" title="Project Management">📆</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/pulls?q=is%3Apr+reviewed-by%3Adoandat943" title="Reviewed Pull Requests">👀</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/nhktmdzhg"><img src="https://avatars.githubusercontent.com/u/57983253?v=4&size=100" width="100px" height="100px" alt="Nguyen Hoang Ky"/><br /><sub><b>Nguyen Hoang Ky</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=nhktmdzhg" title="Code">💻</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=nhktmdzhg" title="Documentation">📖</a> <a href="#infra-nhktmdzhg" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a> <a href="#maintenance-nhktmdzhg" title="Maintenance">🚧</a> <a href="#blog-nhktmdzhg" title="Blogposts">📝</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/pulls?q=is%3Apr+reviewed-by%3Anhktmdzhg" title="Reviewed Pull Requests">👀</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/DMKha241"><img src="https://avatars.githubusercontent.com/u/237471390?v=4&size=100" width="100px" height="100px" alt="DMKha"/><br /><sub><b>DMKha</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=DMKha241" title="Code">💻</a> <a href="#infra-DMKha241" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://truong.me/"><img src="https://avatars.githubusercontent.com/u/55949169?v=4&size=100" width="100px" height="100px" alt="truong.me"/><br /><sub><b>truong.me</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=nct88" title="Code">💻</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=nct88" title="Documentation">📖</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/imngkhang"><img src="https://avatars.githubusercontent.com/u/237411516?v=4&size=100" width="100px" height="100px" alt="imngkhang"/><br /><sub><b>imngkhang</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=imngkhang" title="Code">💻</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=imngkhang" title="Documentation">📖</a> <a href="#infra-imngkhang" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://ngxccc.github.io/"><img src="https://avatars.githubusercontent.com/u/163379246?v=4&size=100" width="100px" height="100px" alt="Ngoc Tran"/><br /><sub><b>Ngoc Tran</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=ngxccc" title="Code">💻</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=ngxccc" title="Tests">⚠️</a> <a href="#infra-ngxccc" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/akimiya7742"><img src="https://avatars.githubusercontent.com/u/105062613?v=4&size=100" width="100px" height="100px" alt="Akimiya"/><br /><sub><b>Akimiya</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=akimiya7742" title="Code">💻</a></td>
    </tr>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://orbisappsec.com/"><img src="https://avatars.githubusercontent.com/u/667857?v=4&size=100" width="100px" height="100px" alt="Anupam Mediratta"/><br /><sub><b>Anupam Mediratta</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=anupamme" title="Code">💻</a> <a href="#security-anupamme" title="Security">🛡️</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/justanoobcoder"><img src="https://avatars.githubusercontent.com/u/57614330?v=4&size=100" width="100px" height="100px" alt="Nguyễn Hồng Hiệp"/><br /><sub><b>Nguyễn Hồng Hiệp</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=justanoobcoder" title="Code">💻</a> <a href="#infra-justanoobcoder" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://retec.vn/"><img src="https://avatars.githubusercontent.com/u/48555338?v=4&size=100" width="100px" height="100px" alt="namtao"/><br /><sub><b>namtao</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=namtao" title="Code">💻</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=namtao" title="Documentation">📖</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/mncc8337"><img src="https://avatars.githubusercontent.com/u/134080447?v=4&size=100" width="100px" height="100px" alt="mncc"/><br /><sub><b>mncc</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=mncc8337" title="Code">💻</a> <a href="#infra-mncc8337" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/hainguyents13"><img src="https://avatars.githubusercontent.com/u/9494293?v=4&size=100" width="100px" height="100px" alt="Hải Nguyễn"/><br /><sub><b>Hải Nguyễn</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=hainguyents13" title="Documentation">📖</a> <a href="#translation-hainguyents13" title="Translation">🌍</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/dont-wait"><img src="https://avatars.githubusercontent.com/u/157901375?v=4&size=100" width="100px" height="100px" alt="sang-nt"/><br /><sub><b>sang-nt</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=dont-wait" title="Code">💻</a> <a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=dont-wait" title="Documentation">📖</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/NguyenDuc2309"><img src="https://avatars.githubusercontent.com/u/195840290?v=4&size=100" width="100px" height="100px" alt="Nguyễn Đức"/><br /><sub><b>Nguyễn Đức</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=NguyenDuc2309" title="Code">💻</a></td>
    </tr>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/collyn"><img src="https://avatars.githubusercontent.com/u/13034759?v=4&size=100" width="100px" height="100px" alt="Nguyen Tien Huy"/><br /><sub><b>Nguyen Tien Huy</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=collyn" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/hthienloc"><img src="https://avatars.githubusercontent.com/u/148019203?v=4&size=100" width="100px" height="100px" alt="Huỳnh Thiện Lộc"/><br /><sub><b>Huỳnh Thiện Lộc</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=hthienloc" title="Documentation">📖</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/brucenguyen1102"><img src="https://avatars.githubusercontent.com/u/49108434?v=4&size=100" width="100px" height="100px" alt="Bruce Nguyen"/><br /><sub><b>Bruce Nguyen</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=brucenguyen1102" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/lolmc0587"><img src="https://avatars.githubusercontent.com/u/59377673?v=4&size=100" width="100px" height="100px" alt="Tom"/><br /><sub><b>Tom</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=lolmc0587" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/mrd9999"><img src="https://avatars.githubusercontent.com/u/224975069?v=4&size=100" width="100px" height="100px" alt="MrD"/><br /><sub><b>MrD</b></sub></a><br /><a href="#promotion-mrd9999" title="Promotion">📣</a> <a href="#maintenance-mrd9999" title="Maintenance">🚧</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/nguyenphivn"><img src="https://avatars.githubusercontent.com/u/19565657?v=4&size=100" width="100px" height="100px" alt="nguyenphivn"/><br /><sub><b>nguyenphivn</b></sub></a><br /><a href="https://github.com/VN-Linux-Family/zalo-for-linux/commits?author=nguyenphivn" title="Code">💻</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

## 📄 License & Disclaimer

### License

This project is licensed under the BSD 3-Clause _“New” or “Revised”_ License.

### Disclaimer

This project is **an unofficial port and is not affiliated with, endorsed by, or sponsored by Zalo or VNG Corporation**.

Zalo is a trademark of VNG Corporation. This software **is provided for educational and personal use only**. **Users are responsible for complying with applicable terms of service and laws**.

This project **does not endorse, verify, or take responsibility** for any third-party forks OR forks requesting donations; any modified versions or forks containing malware operate completely independently and without our authorization, according to BSD 3-Clause license, Clause 3.
