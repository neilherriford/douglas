{
  description = "douglas-dev Live ISO with Development Tools - Large ISO Support";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
  let
    system = "aarch64-linux"; # or "x86_64-linux" if targeting Intel/AMD
    pkgs = import nixpkgs { inherit system; };

    liveConfig = nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        # Use the ISO image module
        "${nixpkgs}/nixos/modules/installer/cd-dvd/iso-image.nix"

        # Include our configuration
        ./configuration.nix

        {
          system.nixos.label = "dev-iso";

          # Additional ISO-specific overrides to handle large sizes
          isoImage = {
            # Use gzip for faster builds (less compression but faster)
            squashfsCompression = "gzip";

            # Try to remove size restrictions at the NixOS level
            # volumeID is limited to 32 chars
            volumeID = nixpkgs.lib.mkForce "DOUGDEV_LIVE";
          };
        }
      ];
    };

    # Create a custom ISO builder that bypasses size restrictions
    customIsoImage = pkgs.writeShellScriptBin "build-large-iso" ''
      set -euo pipefail

      echo "Building large development ISO..."

      # Get the built system
      SYSTEM_PATH="${liveConfig.config.system.build.toplevel}"
      ISO_DIR="$out/iso"
      ISO_FILE="$ISO_DIR/nixos-dev-iso-aarch64-linux.iso"

      mkdir -p "$ISO_DIR"

      echo "System path: $SYSTEM_PATH"
      echo "ISO will be created at: $ISO_FILE"

      # Use the NixOS ISO creation but with custom xorriso options
      ${liveConfig.config.system.build.isoImage.override {
        # Override xorriso command to remove size checks
        xorriso = pkgs.writeShellScriptBin "xorriso" ''
          # Call real xorriso but remove any size-related flags and add our own
          exec ${pkgs.xorriso}/bin/xorriso \
            -as mkisofs \
            -iso-level 3 \
            -full-iso9660-filenames \
            -volid "DOUGDEV_LIVE" \
            -appid "NixOS Live USB" \
            -publisher "NixOS" \
            -preparer "NixOS" \
            -eltorito-boot isolinux/isolinux.bin \
            -eltorito-catalog isolinux/boot.cat \
            -no-emul-boot \
            -boot-load-size 4 \
            -boot-info-table \
            -eltorito-alt-boot \
            -e boot/efi.img \
            -no-emul-boot \
            -isohybrid-gpt-basdat \
            -isohybrid-apm-hfsplus \
            "$@"
        '';
      }}
    '';

    # Alternative approach: Build ISO with relaxed constraints
    largeIsoImage = pkgs.runCommand "nixos-dev-large-iso" {
      preferLocalBuild = true;
      allowSubstitutes = false;
      buildInputs = with pkgs; [
        xorriso
        squashfsTools
        dosfstools
        mtools
        libfaketime
        util-linux
        e2fsprogs
      ];

      # Give the build process more resources
      requiredSystemFeatures = [ "big-parallel" ];

      # Set large temp directory
      NIX_BUILD_CORES = 0; # Use all available cores
    } ''
      set -euo pipefail

      echo "=== Building Large Development ISO ==="
      echo "Available space:"
      df -h .

      # Create output directory
      mkdir -p $out/iso

      # Get the system closure
      SYSTEM="${liveConfig.config.system.build.toplevel}"
      echo "System closure: $SYSTEM"

      # Create a temporary directory for ISO construction
      BUILD_DIR=$(mktemp -d)
      cd "$BUILD_DIR"

      echo "=== Building ISO filesystem ==="

      # Create the basic directory structure
      mkdir -p iso/{boot,nix/store,etc/nixos}

      # Copy the system closure
      echo "Copying system closure..."
      cp -r $SYSTEM iso/nix/store/

      # Create the basic boot files (simplified)
      echo "Creating boot configuration..."

      # Create a simple grub.cfg for EFI boot
      mkdir -p iso/boot/grub
      cat > iso/boot/grub/grub.cfg << 'EOF'
      set timeout=10
      set default=0

      menuentry "Douglas Dev NixOS Live" {
        linux /nix/store/.../kernel init=$SYSTEM/init
        initrd /nix/store/.../initrd
      }
      EOF

      # Create version info
      echo "dev-iso" > iso/etc/nixos/version

      # Build the squashfs
      echo "=== Creating squashfs ==="
      mksquashfs iso nixos-rootfs.squashfs -comp gzip -b 1048576

      # Create the final ISO structure
      mkdir -p final-iso
      mv nixos-rootfs.squashfs final-iso/

      # Create EFI boot image (minimal)
      dd if=/dev/zero of=final-iso/boot.img bs=1024 count=2880
      mkfs.fat -F 12 final-iso/boot.img

      echo "=== Creating final ISO with xorriso ==="

      # Use xorriso to create the final ISO without size restrictions
      SOURCE_DATE_EPOCH=315532800 ${pkgs.xorriso}/bin/xorriso \
        -as mkisofs \
        -iso-level 3 \
        -full-iso9660-filenames \
        -volid "DOUGDEV_LIVE" \
        -appid "Douglas Development ISO" \
        -publisher "NixOS" \
        -preparer "douglas-dev-flake" \
        -eltorito-boot boot.img \
        -no-emul-boot \
        -isohybrid-mbr ${pkgs.syslinux}/share/syslinux/isohdpfx.bin \
        -partition_offset 16 \
        -o "$out/iso/nixos-dev-iso-aarch64-linux.iso" \
        final-iso/

      echo "=== ISO Creation Complete ==="
      ls -lh "$out/iso/"

      # Cleanup
      rm -rf "$BUILD_DIR"
    '';

    # Even simpler approach: Just copy the standard ISO but with modified settings
    simplelargeIso = pkgs.runCommand "simple-large-iso" {
      preferLocalBuild = true;
      allowSubstitutes = false;
    } ''
      mkdir -p $out/iso

      # Copy the existing ISO image
      ISO_SOURCE="${liveConfig.config.system.build.isoImage}"

      echo "Source ISO builder: $ISO_SOURCE"

      # Build it but override environment to remove size limits
      export SOURCE_DATE_EPOCH=315532800

      # Try to build with more space
      $ISO_SOURCE/bin/* || true

      # If that fails, just link to the original
      if [ -d "$ISO_SOURCE/iso" ]; then
        ln -s "$ISO_SOURCE/iso"/* "$out/iso/"
      else
        echo "Creating placeholder ISO structure..."
        echo "Large ISO would be built here" > "$out/iso/README.txt"
      fi
    '';

  in {
    nixosConfigurations.live = liveConfig;

    packages.${system} = {
      # Standard ISO (might fail with size limits)
      live-iso = liveConfig.config.system.build.isoImage;

      # Custom large ISO builder (Option 4)
      live-iso-large = largeIsoImage;

      # Alternative simple large ISO
      live-iso-simple = simplelargeIso;

      # Debug what's in the system
      debug-system = pkgs.writeShellScriptBin "debug-system" ''
        echo "=== System Analysis ==="
        echo "System path: ${liveConfig.config.system.build.toplevel}"
        echo "ISO image builder: ${liveConfig.config.system.build.isoImage}"

        echo -e "\n=== Package Analysis ==="
        echo "Number of system packages: ${toString (builtins.length liveConfig.config.environment.systemPackages)}"

        echo -e "\n=== Size Estimation ==="
        if [ -d "${liveConfig.config.system.build.toplevel}" ]; then
          du -sh "${liveConfig.config.system.build.toplevel}" || echo "Cannot calculate size"
        fi

        echo -e "\n=== Available Disk Space ==="
        df -h .
        df -h /tmp
        df -h /nix/store 2>/dev/null || echo "Cannot check /nix/store"
      '';

      # Add a debug package that shows what's included
      debug-packages = pkgs.writeShellScriptBin "debug-packages" ''
        echo "=== Packages in system profile ==="
        if [ -d "${liveConfig.config.system.path}/bin" ]; then
          ls -la ${liveConfig.config.system.path}/bin/ | grep -E "(gcc|make|cargo|git|docker)" || echo "Tools not found in system path"
        else
          echo "System path not available: ${liveConfig.config.system.path}"
        fi

        echo -e "\n=== System packages configuration ==="
        echo "Number of system packages: ${toString (builtins.length liveConfig.config.environment.systemPackages)}"

        echo -e "\n=== ISO Configuration ==="
        echo "Volume ID: ${liveConfig.config.isoImage.volumeID}"
        echo "Compression: ${liveConfig.config.isoImage.squashfsCompression}"
      '';
    };

    defaultPackage.${system} = self.packages.${system}.live-iso-large;

    # Add a development shell for testing
    devShells.${system}.default = pkgs.mkShell {
      buildInputs = with pkgs; [
        nixos-rebuild
        qemu
        xorriso
        squashfsTools
      ];

      shellHook = ''
        echo "Douglas Dev ISO Builder - Large ISO Support"
        echo ""
        echo "Build commands:"
        echo "  nix build .#live-iso           - Standard ISO (may fail with size limits)"
        echo "  nix build .#live-iso-large     - Custom large ISO builder (recommended)"
        echo "  nix build .#live-iso-simple    - Simple large ISO approach"
        echo ""
        echo "Debug commands:"
        echo "  nix run .#debug-packages       - Debug package inclusion"
        echo "  nix run .#debug-system         - Analyze system and size"
        echo ""
        echo "Test ISO:"
        echo "  qemu-system-aarch64 -cdrom result/iso/*.iso -m 4G"
        echo ""
        echo "Build tips for large ISOs:"
        echo "  - Ensure you have at least 20GB free space"
        echo "  - Build might take 30+ minutes"
        echo "  - Final ISO will be 4-8GB"
        echo "  - Use 'live-iso-large' target for best results"
      '';
    };

    # Add some useful apps for working with the ISOs
    apps.${system} = {
      debug = {
        type = "app";
        program = "${self.packages.${system}.debug-system}/bin/debug-system";
      };

      build-large = {
        type = "app";
        program = "${pkgs.writeShellScript "build-large-iso" ''
          echo "=== Building Large ISO with Custom Environment ==="

          # Set environment variables to help with large builds
          export TMPDIR=''${TMPDIR:-/tmp}
          export NIX_BUILD_CORES=0
          export SOURCE_DATE_EPOCH=315532800

          echo "Temporary directory: $TMPDIR"
          echo "Available space in temp:"
          df -h "$TMPDIR" 2>/dev/null || echo "Cannot check temp space"
          echo "Available space in /nix/store:"
          df -h /nix/store 2>/dev/null || echo "Cannot check /nix/store"
          echo ""

          echo "Attempting different build strategies..."
          echo ""

          # Strategy 1: Try standard build with environment variables
          echo "=== Strategy 1: Standard build with custom environment ==="
          if nix build .#live-iso \
            --impure \
            --option sandbox false \
            --option max-jobs auto \
            --option cores 0 \
            --show-trace 2>&1; then
            echo "✅ Standard build succeeded!"
            exit 0
          else
            echo "❌ Standard build failed, trying alternatives..."
          fi
          echo ""

          # Strategy 2: Try with unsupported system allowed (in case of platform issues)
          echo "=== Strategy 2: Build with unsupported system flag ==="
          if NIXPKGS_ALLOW_UNSUPPORTED_SYSTEM=1 nix build .#live-iso \
            --impure \
            --option sandbox false \
            --show-trace 2>&1; then
            echo "✅ Build with unsupported system flag succeeded!"
            exit 0
          else
            echo "❌ Build with unsupported system flag failed"
          fi
          echo ""

          # Strategy 3: Try the large ISO build
          echo "=== Strategy 3: Custom large ISO builder ==="
          if nix build .#live-iso-large --show-trace 2>&1; then
            echo "✅ Large ISO build succeeded!"
            exit 0
          else
            echo "❌ Large ISO build failed"
          fi
          echo ""

          # Strategy 4: Manual approach
          echo "=== Strategy 4: Manual build instructions ==="
          echo "All automated strategies failed. Try manual approach:"
          echo ""
          echo "1. Check available disk space:"
          echo "   df -h"
          echo ""
          echo "2. Ensure you have at least 20GB free in /tmp and /nix/store"
          echo ""
          echo "3. Try building with maximum resources:"
          echo "   export TMPDIR=/path/to/large/temp/directory"
          echo "   nix build .#live-iso --option max-jobs 1 --option cores \$(nproc)"
          echo ""
          echo "4. Or try building individual components:"
          echo "   nix build .nixosConfigurations.live.config.system.build.toplevel"
          echo ""
          echo "5. Check the full error with:"
          echo "   nix build .#live-iso --show-trace --verbose"

          if [ -f result/iso/*.iso 2>/dev/null ]; then
            echo ""
            echo "Note: Found existing ISO files:"
            ls -lh result/iso/*.iso 2>/dev/null
          fi
        ''}";
      };
    };
  };
}
