# Zalo for Linux from the release AppImage, for Nix / NixOS.
# nix/sources.json pins the release assets; the release job of
# .github/workflows/build.yml rewrites it after each build.
{
  lib,
  stdenv,
  appimageTools,
  fetchurl,
  runCommand,
  dwarfs,
  libopus,
  libpulseaudio,
  runtimeShell,
  # "zadark" (with ZaDark) or "original"
  variant ? "zadark",
  # extra flags for the app, e.g. [ "--hidden" ] to start in the tray
  commandLineArgs ? [ ],
}:

let
  sources = lib.importJSON ./sources.json;
  system = stdenv.hostPlatform.system;
  asset =
    sources.assets.${variant}.${system}
      or (throw "zalo: no ${variant} AppImage for ${system} in nix/sources.json");

  pname = "zalo";
  inherit (sources) version;

  src = fetchurl {
    name = "zalo-${version}-${variant}-${system}.AppImage";
    url = "https://github.com/VN-Linux-Family/zalo-for-linux/releases/download/${version}/${
      lib.replaceStrings [ "+" ] [ "%2B" ] asset.name
    }";
    inherit (asset) hash;
  };

  # The payload is DwarFS (uruntime), which appimageTools.extractType2 can't
  # read, and the runtime can't run in the build sandbox.
  contents = runCommand "${pname}-${version}-extracted" { nativeBuildInputs = [ dwarfs ]; } ''
    mkdir $out
    dwarfsextract --input ${src} --image-offset auto --output $out
  '';
in
appimageTools.wrapAppImage {
  inherit pname version;
  src = contents;

  # Calls (zcall-native) run python3, which loads libopus and libpulse-simple
  # through ctypes, and the PulseAudio tools (pactl, pacat, parec).
  extraPkgs =
    pkgs: with pkgs; [
      python3
      libopus
      libpulseaudio
      pulseaudio
      alsa-utils
    ];

  profile = ''
    export LD_LIBRARY_PATH=${
      lib.makeLibraryPath [
        libopus
        libpulseaudio
      ]
    }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
  '';

  extraInstallCommands = ''
    ${lib.optionalString (commandLineArgs != [ ]) ''
      mv $out/bin/zalo $out/bin/.zalo-wrapped
      cat > $out/bin/zalo <<EOF
      #!${runtimeShell}
      exec $out/bin/.zalo-wrapped ${lib.escapeShellArgs commandLineArgs} "\$@"
      EOF
      chmod +x $out/bin/zalo
    ''}

    install -Dm644 ${contents}/zalo.desktop $out/share/applications/zalo.desktop
    sed -i 's|^Exec=.*|Exec=zalo %U|' $out/share/applications/zalo.desktop
    cp -r ${contents}/usr/share/icons $out/share/
  '';

  meta = {
    description = "Unofficial Zalo desktop client for Linux, repackaged from the macOS client";
    homepage = "https://github.com/VN-Linux-Family/zalo-for-linux";
    license = lib.licenses.unfree;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    mainProgram = "zalo";
  };
}
