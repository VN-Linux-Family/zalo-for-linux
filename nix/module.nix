self:
{ config, lib, ... }:

let
  cfg = config.programs.zalo;
in
{
  imports = [ (import ./options.nix self) ];

  # Sound for calls (PipeWire or PulseAudio) is left to the system
  # configuration: turning it on here can clash with it.
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
