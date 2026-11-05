# Home Manager module: the same `programs.zalo` options as ./module.nix,
# installing Zalo for one user instead of system-wide.
self:
{ config, lib, ... }:

let
  cfg = config.programs.zalo;
in
{
  imports = [ (import ./options.nix self) ];

  # Sound for calls (PipeWire or PulseAudio) is left to the system
  # configuration.
  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
  };
}
