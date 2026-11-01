{
  description = "Zalo for Linux: the release AppImage packaged for Nix / NixOS";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              # Zalo itself is proprietary
              config.allowUnfreePredicate = pkg: nixpkgs.lib.getName pkg == "zalo";
            }
          )
        );
    in
    {
      packages = forAllSystems (pkgs: rec {
        zalo = pkgs.callPackage ./nix/package.nix { };
        zalo-original = pkgs.callPackage ./nix/package.nix { variant = "original"; };
        default = zalo;
      });

      overlays.default = final: prev: {
        zalo = final.callPackage ./nix/package.nix { };
      };

      nixosModules.default = ./nix/module.nix;
    };
}
