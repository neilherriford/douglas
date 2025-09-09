{
  description = "douglas Live CD [NixOS(AVF/UTM) with Docker, Rust, VirtioFS/9p share]";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
  let
    system = "aarch64-linux";
    pkgs = import nixpkgs { inherit system; };
    liveConfig = nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        # Import the standard NixOS installer CD module — this exposes isoImage
        "${nixpkgs}/nixos/modules/installer/cd-dvd/installation-cd-minimal.nix"

        ./configuration.nix
      ];
    };
  in {
    nixosConfigurations.live = liveConfig;

    # Expose ISO builder at top-level
    packages.${system}.live-iso = liveConfig.config.system.build.isoImage;

    # Make it the default package for convenience
    defaultPackage.${system} = liveConfig.config.system.build.isoImage;
  };
}
