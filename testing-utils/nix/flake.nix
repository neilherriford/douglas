{
  description = "douglas-dev Live ISO with Development Tools, and the disposable douglas-ci image";

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

    # Disposable CI VM image: same ISO/live-boot mechanism as the dev
    # image (already proven to build here without needing the builder's
    # own KVM/nested-virtualization support, unlike a partitioned raw disk
    # image) — "discardable" just means killing the qemu process after
    # each run rather than rebooting it, not a different image format.
    ciSystem = nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        "${nixpkgs}/nixos/modules/installer/cd-dvd/iso-image.nix"
        ./ci-configuration.nix
        {
          isoImage.squashfsCompression = "gzip";
        }
      ];
    };

  in {
    # Package outputs
    packages.${system} = {
      default = liveSystem.config.system.build.isoImage;
      iso = liveSystem.config.system.build.isoImage;
      ci = ciSystem.config.system.build.isoImage;
    };

    # For reference
    nixosConfigurations.live = liveSystem;
  };
}
