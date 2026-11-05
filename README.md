# Zalo for Linux 🐧

**English** | [Tiếng Việt](./README.vi.md)

[![Build Status](https://github.com/VN-Linux-Family/zalo-for-linux/actions/workflows/build.yml/badge.svg)](https://github.com/VN-Linux-Family/zalo-for-linux/actions/workflows/build.yml)

[![All Contributors](https://img.shields.io/github/all-contributors/VN-Linux-Family/zalo-for-linux?color=ee8449&style=flat-square)](#contributors)

An unofficial, community-driven port of the Zalo desktop application for **Linux only**, created by repackaging the official macOS client into a standard AppImage with integrated ZaDark.

Thanks **realdtn2** for the solution: [realdtn2/zalo-linux-2026](https://github.com/DMKha241/zalo-linux-2026) (original repo is deleted/hidden, we are using a backup repo from [DMKha241](https://github.com/DMKha241)).

## ⚠️ Important: Known Issues

> 💡 **For the full list of resolved issues, workarounds, and community credits, see [KNOWN_ISSUES.md](./docs/user/known-issues.md).**

This project is best suited for users who need a native-feeling Zalo client on Linux and are comfortable with the technical workarounds required for full functionality.

## ✨ Features in this project

This project added some features in the app, that the official version doesn't have equivalents.

✨ **For the full list of the features that included in this project, see [FEATURES.md](./docs/user/features.md).**

## 🚀 Quick Start

### Usage

We strongly recommend using **Gear Lever** or **AM** to integrate the AppImage perfectly into your system menu.

**Note:** Zalo for Linux comes with embed update info. You don't need to setup any update infomations for updating the apps.

#### For AM users:

*Note: Replace `am` with `appman` in all lines containing this string if you chose to install `appman`.*

1. Install **AM** from [AM's guide](https://github.com/ivan-hc/AM#using-the-am-installer-script-to-choose-between-local-and-system-wide-installation)
2. Run this command to install Zalo:
   ```bash
   am -i zalo
   ```
3. A prompt like this will appear:
   <img width="747" height="600" alt="installer" src="./sample/am-guide.png" />

   *You can choose what variants you like from this, AM will download the correct version for you.*
4. The app is **now installed**. You can now go to your system's application launcher to launch Zalo.

   Or run from the command line:
   ```bash
   zalo
   ```
5. How to update:
   You just need to run this command, it will **automatically update all your AM packages (including Zalo)** for you:
   ```bash
   am -u
   ```
   *Tips: It is recommended to install `appimageupdatetool` from AM by running the command `am -i appimageupdatetool` to update apps faster. The first version (which has number 1) is more recommended than the second one (which has number 2).*

#### For Gear Lever users:

1. Download the latest `.AppImage` file from the [**Releases**](https://github.com/VN-Linux-Family/zalo-for-linux/releases) page.
2. Install **Gear Lever** from [Flathub](https://flathub.org/en/apps/it.mijorus.gearlever).
3. Open **Gear Lever**.
4. Click the **"Open"** button in the top-left corner and select the `.AppImage` file you downloaded.
5. The app will now appear in Gear Lever. Click the **"Unlock"** button, then choose **"Move to the app menu"** to integrate it into your system's application launcher.
6. How to update:
   - Open **Gear Lever**.
   - If any updates is available, the button **No updates available** will turn to **Update**, just simply click to the button and Zalo is updated.

#### For NixOS users:

The repository is a flake that installs the release AppImage, with what calls need (`python3`, libopus, PulseAudio tools) packaged inside.

1. Add the flake to your system's `flake.nix` and import its module:
   ```nix
   {
     inputs.zalo-for-linux = {
       url = "github:VN-Linux-Family/zalo-for-linux";
       inputs.nixpkgs.follows = "nixpkgs";
     };

     outputs = { nixpkgs, zalo-for-linux, ... }: {
       nixosConfigurations.<hostname> = nixpkgs.lib.nixosSystem {
         modules = [
           zalo-for-linux.nixosModules.default
           ./configuration.nix
         ];
       };
     };
   }
   ```
2. Enable it in `configuration.nix`:
   ```nix
   programs.zalo = {
     enable = true;
     # variant = "original";  # the AppImage without ZaDark
   };
   ```
   Calls need sound through PipeWire (`services.pipewire.pulse.enable = true;`) or PulseAudio.
3. Apply it with `sudo nixos-rebuild switch`.
4. How to update:
   ```bash
   nix flake update zalo-for-linux
   sudo nixos-rebuild switch
   ```
   *Note: each rebuild deletes the old AppImages from the Releases page. If `nixos-rebuild` fails to download the file (404), run `nix flake update zalo-for-linux` to point the flake to the latest build, then rebuild.*

#### For Home Manager users:

The flake also has a Home Manager module with the same `programs.zalo` options, which installs Zalo for your user only (on NixOS or any other distro).

1. Add the flake to your `flake.nix` and import its module:
   ```nix
   {
     inputs.zalo-for-linux = {
       url = "github:VN-Linux-Family/zalo-for-linux";
       inputs.nixpkgs.follows = "nixpkgs";
     };

     outputs = { nixpkgs, home-manager, zalo-for-linux, ... }: {
       homeConfigurations.<username> = home-manager.lib.homeManagerConfiguration {
         pkgs = nixpkgs.legacyPackages.x86_64-linux;
         modules = [
           zalo-for-linux.homeModules.default
           ./home.nix
         ];
       };
     };
   }
   ```
   If you use Home Manager as a NixOS module, add `zalo-for-linux.homeModules.default` to `home-manager.sharedModules` (or to the `imports` of your user's config) instead.
2. Enable it in `home.nix`:
   ```nix
   programs.zalo = {
     enable = true;
     # variant = "original";  # the AppImage without ZaDark
   };
   ```
3. Apply it with `home-manager switch` (or `sudo nixos-rebuild switch` if Home Manager is a NixOS module).
4. How to update:
   ```bash
   nix flake update zalo-for-linux
   home-manager switch
   ```

To try it without installing: `nix run github:VN-Linux-Family/zalo-for-linux`.

#### Run it manually (this is not recommended):

1. Download the latest `.AppImage` file from the [**Releases**](https://github.com/VN-Linux-Family/zalo-for-linux/releases) page.
2. Make it executable:
   ```bash
   chmod +x <downloaded-file> # Get the filename with ls, or you can use your file manager's GUI
   ```
3. Run it manually by this command:
   ```bash
   ./<downloaded-file>
4. For updates, just **simply redownload the latest `.AppImage` file** from the **Releases** page, then **repeat step 2 and 3**.

*NOTE: Remember to quit the app and relaunch again after updates, for new changes can be applied in your side.* 
   

### Build from Source

Prerequisites:

- Linux x86_64 or aarch64
- Node.js and npm
- 7z (p7zip-full) for extracting the macOS app during setup
- C++ build tools (for native addons): `build-essential`, `libssl-dev`, `liblzma-dev`
- Rust toolchain, `clang`, `libclang-dev`, `llvm` (for the Rust native addons)
- To make calls with the built app: `python3`, `libopus0`, `pulseaudio-utils` (nothing to compile, no Wine)

On Debian/Ubuntu:

```bash
sudo apt update && sudo apt install -y build-essential liblzma-dev p7zip-full gcc g++ clang libclang-dev llvm libx11-dev libxcb1-dev libxext-dev zsync
```

Steps:

```bash
# Clone the repository
git clone https://github.com/VN-Linux-Family/zalo-for-linux.git
cd zalo-for-linux
# Then initialize or update submodules
git submodule update --init --recursive

# Run setup + build (downloads DMG, extracts, patches, packages)
npm run main
```

The final AppImage will be in the `dist/` directory.

> For a detailed walkthrough of the build pipeline, scripts, environment variables, and how to add new patches, see [`DEVELOPMENT.md`](./docs/dev/development.md).


## ⚙️ How It Works

This project is not a from-scratch rewrite of Zalo. It works by:

1.  Downloading the official macOS `.dmg` file.
2.  Using `7z` to extract the `app.asar` archive, which contains the main application logic written in JavaScript.
3.  Removing incompatible native macOS files.
4.  Wrapping the extracted application in a minimal, Linux-compatible Electron shell.
5.  Using `electron-builder`, then `quick-sharun` to package everything into a single, portable `AppImage` file.

For a deeper dive into the build pipeline and patching strategy, see [`ARCHITECTURE.md`](./docs/dev/architecture.md).

For native addons (db-cross-v4, etc.), see [`nativelibs/README.md`](./nativelibs/README.md).

For the native call engine, see [`zcall-native/README.md`](./zcall-native/README.md)

## 🐛 Troubleshooting & Debugging

If you encounter issues or want to inspect the app's behavior, you can easily open Chrome Developer Tools (DevTools) using the following methods:
- **Keyboard Shortcut**: Press `Ctrl` + `Shift` + `I` while the Zalo window is focused.
- **Tray Menu**: Right-click the Zalo tray icon and select **"Toggle DevTools"**.

## 📚 More Documentation

- [KNOWN_ISSUES.md](./docs/user/known-issues.md) — Known issues, workarounds, and resolved bug history
- [ARCHITECTURE.md](./docs/dev/architecture.md) — How the build pipeline and patches work
- [DEVELOPMENT.md](./docs/dev/development.md) — Building from source, scripts, adding patches
- [nativelibs/README.md](./nativelibs/README.md) — Native addons (db-cross-v4, etc.)
- [zcall-native/README.md](./zcall-native/README.md) — native call engine

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
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

## 📄 License & Disclaimer

### License

This project is licensed under the BSD 3-Clause *“New” or “Revised”* License. 

### Disclaimer

This project is **an unofficial port and is not affiliated with, endorsed by, or sponsored by Zalo or VNG Corporation**. 

Zalo is a trademark of VNG Corporation. This software **is provided for educational and personal use only**. **Users are responsible for complying with applicable terms of service and laws**. 

This project **does not endorse, verify, or take responsibility** for any third-party forks OR forks requesting donations; any modified versions or forks containing malware operate completely independently and without our authorization, according to BSD 3-Clause license, Clause 3.
