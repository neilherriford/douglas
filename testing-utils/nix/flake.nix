{
  description = "douglas-dev Live ISO with Development Tools";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
  let
    system = "aarch64-linux";

    liveSystem = nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        "${nixpkgs}/nixos/modules/installer/cd-dvd/iso-image.nix"
        ./configuration.nix
        {
          # ISO-specific settings
          isoImage.squashfsCompression = "gzip";
        }
      ];
    };

  in {
    # Package outputs
    packages.${system} = {
      default = liveSystem.config.system.build.isoImage;
      iso = liveSystem.config.system.build.isoImage;
    };

    # For reference
    nixosConfigurations.live = liveSystem;
  };
}
