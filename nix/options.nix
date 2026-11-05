# `programs.zalo` options shared by the NixOS module (./module.nix) and the
# Home Manager module (./hm-module.nix). Each of them only decides where the
# package is installed.
#
# `self` is this flake. The default package comes from `self.packages`, which
# is built with a nixpkgs that already allows Zalo as unfree, so
# `enable = true` works without the user allowing it in their own config.
self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.zalo;
  system = pkgs.stdenv.hostPlatform.system;
in
{
  options.programs.zalo = {
    enable = lib.mkEnableOption "Zalo for Linux";

    variant = lib.mkOption {
      type = lib.types.enum [
        "zadark"
        "original"
      ];
      default = "zadark";
      description = "Which AppImage to install: with ZaDark, or the original one.";
    };

    commandLineArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "--hidden" ];
      description = "Extra command line flags for Zalo.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default =
        (self.packages.${system}.zalo
          or (throw "programs.zalo: no Zalo package for ${system}, set programs.zalo.package explicitly")
        ).override
          { inherit (cfg) variant commandLineArgs; };
      defaultText = lib.literalMD "Zalo from this flake, built with `variant` and `commandLineArgs`";
      description = "The Zalo package to install.";
    };
  };
}
