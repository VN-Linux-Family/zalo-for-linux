{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.zalo;
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
      default = pkgs.callPackage ./package.nix { inherit (cfg) variant commandLineArgs; };
      defaultText = lib.literalMD "Zalo from this flake, built with `variant` and `commandLineArgs`";
      description = "The Zalo package to install.";
    };
  };

  # Sound for calls (PipeWire or PulseAudio) is left to the system
  # configuration: turning it on here can clash with it.
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
