{ config, pkgs, lib, ... }:

let
  # Folder containing your host identity keys (if you want static keys)
  hostKeysDir = ./ssh-host-keys;

  # Folder containing user's authorized public keys
  userKeysDir = ./ssh-keys;

  # Read all public keys for the user
  userPubKeys =
    builtins.filter (file: lib.hasSuffix ".pub" file)
      (builtins.attrNames (builtins.readDir userKeysDir));

  # Version information for your dev image
  devImageVersion = "0.0.2k";
  devImageDate = "2026-06-19";
  devImageName = "Douglas Development Environment";

  # Define our development packages explicitly
  devPackages = with pkgs; [
    # GNU build toolchain
    gcc
    gnumake
    binutils
    gdb
    pkg-config
    autoconf
    automake
    libtool
    cmake
    ninja

    # Rust toolchain - USE ONLY RUSTUP (not individual packages)
    rustup
    lldb

    # Development utilities
    git
    curl
    jq
    docker
    socat
    lsof

    # Network utilities
    bind          # Provides nslookup, dig, host, nslookup
    nettools      # Provides netstat, ifconfig, route
    iproute2      # Provides ip (modern replacement for ifconfig)

    # Additional useful development tools
    tree
    htop
    tmux
    wget
  ];
in
{
  ######################## DOCUMENTATION DISABLE ######################
  # Disable all documentation to avoid docbook issues
  documentation.enable = false;
  documentation.nixos.enable = false;
  documentation.man.enable = false;
  documentation.info.enable = false;
  documentation.doc.enable = false;

  ############################ BOOT SETTINGS ###########################
  boot.kernelModules = [ "virtiofs" ];
  boot.loader.grub.enable = false;
  boot.loader.timeout = lib.mkForce 0;
  boot.isContainer = false;

  ############################## UTM SHARE #############################
  systemd.services.mount-utm-share = {
    description = "Mount UTM Shared Folder";
    after = [ "local-fs.target" ];
    wants = [ "multi-user.target" ];
    preStart = ''
      mkdir -p /mnt/share
    '';
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${pkgs.util-linux}/bin/mount -t virtiofs share /mnt/share";
      RemainAfterExit = true;
    };
    wantedBy = [ "multi-user.target" ];
  };

  ############################## UTM CACHE #############################
  # Second virtiofs share for persistent build artifacts (~/.cargo, ~/.rustup).
  # In UTM: add a shared folder pointing at a stable directory on the host,
  # set its tag to "cache". If the share isn't configured the mount will fail
  # but that's non-fatal — users just get a normal (ephemeral) ~/.cargo.
  systemd.services.mount-utm-cache = {
    description = "Mount UTM Cache Folder";
    after = [ "local-fs.target" ];
    preStart = ''
      mkdir -p /mnt/cache
    '';
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${pkgs.util-linux}/bin/mount -t virtiofs cache /mnt/cache";
      RemainAfterExit = true;
    };
    wantedBy = [ "multi-user.target" ];
  };

  # Wire ~/.cargo and ~/.rustup for every user in /home to the persistent cache.
  # Uses `wants` (not `requires`) so a missing/unconfigured cache share is
  # non-fatal — the system boots fine and users get ephemeral cargo dirs instead.
  systemd.services.setup-cache-symlinks = {
    description = "Symlink ~/.cargo and ~/.rustup to UTM cache share";
    after = [ "mount-utm-cache.service" ];
    wants = [ "mount-utm-cache.service" ];
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = pkgs.writeShellScript "setup-cache-symlinks" ''
        # Bail out silently if the cache share isn't mounted.
        if ! mountpoint -q /mnt/cache; then
          echo "setup-cache-symlinks: /mnt/cache not mounted, skipping"
          exit 0
        fi

        mkdir -p /mnt/cache/cargo /mnt/cache/rustup

        for user_home in /home/*; do
          [ -d "$user_home" ] || continue
          user=$(basename "$user_home")

          # Only replace real directories; leave existing symlinks alone so
          # re-running this service is idempotent.
          if [ ! -L "$user_home/.cargo" ]; then
            rm -rf "$user_home/.cargo"
            ln -s /mnt/cache/cargo "$user_home/.cargo"
            chown -h "$user:users" "$user_home/.cargo"
          fi

          if [ ! -L "$user_home/.rustup" ]; then
            rm -rf "$user_home/.rustup"
            ln -s /mnt/cache/rustup "$user_home/.rustup"
            chown -h "$user:users" "$user_home/.rustup"
          fi
        done
      '';
    };
  };

  ############################## ZEROCONF ##############################
  services.avahi = {
    enable = true;
    nssmdns4 = true;  # integrates mDNS into /etc/nsswitch.conf
    openFirewall = true; # automatically open UDP 5353 for mDNS
    publish = {
      enable = true;
      workstation = true; # advertise hostname for SSH/SMB/etc.
      addresses = true;   # advertise the IP address
    };
  };
  networking.hostName = "douglas-dev";

  ########################## SSH CONFIGURATION #########################
  services.openssh = {
    enable = true;
    settings = {
      PermitRootLogin = "yes";
      PasswordAuthentication = false;
    };
    banner = ''

       w  e  l  c  o  m  e    t  o
   ⋀                     _
  ╱│╲      ╱            ╱╱
  ╱│╲   __╱ __ . . _,  ╱╱ __.  _
  ╱│╲  (_╱_(_)(_╱_(_)_<╱_(_╱│_╱_)_
   │   ⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼╱│⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼
       d e v      │╱       v${devImageVersion}

    '';
  };

  # Copy host SSH keys into /etc/ssh with enforced permissions
  environment.etc = {
    "ssh/ssh_host_rsa_key" = {
      source = "${hostKeysDir}/ssh_host_rsa_key";
      mode = "0600";
      user = "root";
      group = "root";
    };
    "ssh/ssh_host_rsa_key.pub" = {
      source = "${hostKeysDir}/ssh_host_rsa_key.pub";
      mode = "0644";
      user = "root";
      group = "root";
    };
    "ssh/ssh_host_ed25519_key" = {
      source = "${hostKeysDir}/ssh_host_ed25519_key";
      mode = "0600";
      user = "root";
      group = "root";
    };
    "ssh/ssh_host_ed25519_key.pub" = {
      source = "${hostKeysDir}/ssh_host_ed25519_key.pub";
      mode = "0644";
      user = "root";
      group = "root";
    };
  };

  ############################### DOCKER ###############################
  virtualisation.docker = {
    enable = true;
    enableOnBoot = true;
    storageDriver = "overlay2";
  };

  ######################### RUSTUP CONFIGURATION #######################
  # Fix: Use systemd user services to setup Rust properly for each user
  systemd.user.services.setup-rust = {
    description = "Setup Rust toolchain for user";
    wantedBy = [ "default.target" ];
    after = [ "graphical-session.target" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = pkgs.writeShellScript "setup-rust" ''
        # Setup rustup directories
        mkdir -p ~/.rustup ~/.cargo

        # Install and set default Rust toolchain (pinned to match rust-toolchain.toml)
        ${pkgs.rustup}/bin/rustup toolchain install 1.96.0 --component clippy rustfmt rust-src
        ${pkgs.rustup}/bin/rustup default 1.96.0

        # Verify installation
        ${pkgs.rustup}/bin/rustc --version
        ${pkgs.rustup}/bin/cargo --version
      '';
    };
  };

  # Also set up a more reliable system-wide approach using profile scripts
  environment.etc."profile.d/setup-rustup.sh" = {
    text = ''
      # Auto-setup rustup for any user that doesn't have it configured
      if command -v rustup >/dev/null 2>&1; then
        # Check if rustup is configured
        if ! rustup show >/dev/null 2>&1; then
          echo "🦀 Setting up Rust toolchain for $USER..."

          # Create directories
          mkdir -p "$HOME/.rustup" "$HOME/.cargo"

          # Install pinned toolchain quietly (matches rust-toolchain.toml)
          rustup toolchain install 1.96.0 --component clippy rustfmt rust-src 2>&1 | grep -v "info: " || true
          rustup default 1.96.0 2>&1 | grep -v "info: " || true

          echo "✅ Rust toolchain ready! Version: $(rustc --version 2>/dev/null || echo 'setup in progress')"
        fi
      fi
    '';
    mode = "0755";
  };

  # Alternative approach: Create a wrapper script that ensures rustup is configured
  environment.etc."rustup-wrapper.sh" = {
    text = ''
      #!/bin/bash
      # Wrapper script to ensure rustup is properly configured before use

      setup_rustup_if_needed() {
        if ! rustup show >/dev/null 2>&1; then
          echo "🦀 Initializing Rust toolchain..." >&2
          rustup default stable >/dev/null 2>&1
          rustup component add clippy rustfmt rust-src >/dev/null 2>&1
        fi
      }

      case "$(basename "$0")" in
        rustc|cargo|clippy|rustfmt)
          setup_rustup_if_needed
          exec ${pkgs.rustup}/bin/"$(basename "$0")" "$@"
          ;;
        *)
          echo "Unknown Rust tool: $(basename "$0")" >&2
          exit 1
          ;;
      esac
    '';
    mode = "0755";
  };


  ###################### PACKAGE CONFIGURATION ######################
  # Add development packages to the system, including auto-setup Rust tools
  environment.systemPackages = devPackages ++ [
    (pkgs.writeShellScriptBin "rustc-auto" ''
      if ! rustup show >/dev/null 2>&1; then
        echo "🦀 Setting up Rust toolchain..."
        rustup default stable
        rustup component add clippy rustfmt rust-src
      fi
      exec rustc "$@"
    '')
    (pkgs.writeShellScriptBin "cargo-auto" ''
      if ! rustup show >/dev/null 2>&1; then
        echo "🦀 Setting up Rust toolchain..."
        rustup default stable
        rustup component add clippy rustfmt rust-src
      fi
      exec cargo "$@"
    '')
    # Add a quick setup command for manual fixes
    (pkgs.writeShellScriptBin "setup-rust" ''
      echo "🦀 Setting up Rust toolchain for $USER..."
      mkdir -p "$HOME/.rustup" "$HOME/.cargo"

      if rustup show >/dev/null 2>&1; then
        echo "✅ Rust is already configured!"
        rustup show
      else
        echo "Installing Rust 1.96.0 toolchain..."
        rustup toolchain install 1.96.0 --component clippy rustfmt rust-src
        rustup default 1.96.0
        echo ""
        echo "✅ Rust setup complete!"
        rustc --version
        cargo --version
      fi

      echo ""
      echo "💡 Tip: Add ~/.cargo/bin to your PATH if needed:"
      echo "   export PATH=\"\$HOME/.cargo/bin:\$PATH\""
    '')
  ];

  ########################## TMPFS RAMDISK ##########################
  # Create a ramdisk for scratch work
  fileSystems."/tmp" = {
    device = "tmpfs";
    fsType = "tmpfs";
    options = [ "defaults" "size=2G" "mode=1777" ];
  };

  # Additional scratch space
  fileSystems."/scratch" = {
    device = "tmpfs";
    fsType = "tmpfs";
    options = [ "defaults" "size=2G" "mode=1777" ];
  };

  # Dedicated Docker storage
  fileSystems."/var/lib/docker" = {
    device = "tmpfs";
    fsType = "tmpfs";
    options = [ "defaults" "size=8G" "mode=0755" ];  # 8GB for Docker
  };

  # Ensure scratch directory is created
  systemd.tmpfiles.rules = [
    "d /scratch 1777 root root -"
  ];

  ######################### USER CONFIGURATION #########################
  users.users.dev = {
    isNormalUser = true;
    extraGroups = [ "wheel" "docker" ];
    initialPassword = "password";
    shell = pkgs.bash;

    # Bake in authorized public keys
    openssh.authorizedKeys.keys =
      map (file: builtins.readFile "${userKeysDir}/${file}") userPubKeys;
  };

  # Custom bashrc with Rust environment setup
  environment.etc."skel/.bashrc".text = ''
    # Default bashrc content
    [ -r /etc/bashrc ] && . /etc/bashrc

    # Custom development environment setup
    echo "🚀 ${devImageName} v${devImageVersion}"

    # Setup Rust if not already configured
    if command -v rustup >/dev/null 2>&1 && ! rustup show >/dev/null 2>&1; then
      echo "🦀 Configuring Rust toolchain..."
      rustup toolchain install 1.96.0 --component clippy rustfmt rust-src
      rustup default 1.96.0
      echo "✅ Rust ready: $(rustc --version)"
    fi

    # Useful aliases for development
    alias ll='ls -la'
    alias la='ls -A'
    alias l='ls -CF'
    alias ..='cd ..'
    alias ...='cd ../..'
    alias grep='grep --color=auto'
    alias rust-new='cargo new'
    alias rust-check='cargo check'
    alias rust-build='cargo build'
    alias rust-test='cargo test'
    alias verify-tools='/etc/verify-dev-tools.sh'

    # Add cargo to PATH if it exists
    if [ -d "$HOME/.cargo/bin" ]; then
      export PATH="$HOME/.cargo/bin:$PATH"
    fi

    # Show development environment info
    if [ -n "$SSH_CONNECTION" ]; then
      echo "📁 Available: /scratch (4GB tmpfs), /mnt/share (UTM share)"
      if mountpoint -q /mnt/cache 2>/dev/null; then
        echo "📦 Build cache: ~/.cargo and ~/.rustup → /mnt/cache (persistent)"
      else
        echo "📦 Build cache: ephemeral (add a 'cache' virtiofs share in UTM to persist)"
      fi
      echo "🔧 Run 'verify-tools' to check all development tools"
    fi
  '';

  # Ensure new users get the custom bashrc
  system.activationScripts.setupUserEnvironment = {
    text = ''
      # Copy custom bashrc to existing users
      for user_home in /home/*; do
        if [ -d "$user_home" ] && [ ! -f "$user_home/.bashrc" ]; then
          cp /etc/skel/.bashrc "$user_home/.bashrc"
          chown $(basename "$user_home"):users "$user_home/.bashrc"
        fi
      done
    '';
  };

  security.sudo.wheelNeedsPassword = false;
  # Allow trusted users to override Nix settings
  nix.settings = {
    trusted-users = [ "root" "dev" "@wheel" ];
    # Also enable flakes and nix-command for convenience
    experimental-features = [ "nix-command" "flakes" ];
  };

  # Enable networking for package downloads and development
  networking = {
    networkmanager.enable = true;
    wireless.enable = false; # Disable wpa_supplicant since we're using NetworkManager
  };

  ###################### ISO SPECIFIC CONFIGURATION ###################
  isoImage = {
    volumeID = "DOUGDEV_${lib.replaceStrings ["."] ["_"] devImageVersion}";
    makeEfiBootable = true;
    makeUsbBootable = true;

    # Include system build dependencies to ensure our packages are available
    includeSystemBuildDependencies = false;
  };

  # System configuration
  system.stateVersion = "25.05";

  ##################### DEBUGGING AND VERIFICATION ###################
  # Enhanced verification script with version info
  environment.etc."verify-dev-tools.sh" = {
    text = ''
      #!/bin/bash
      echo "=== ${devImageName} v${devImageVersion} (${devImageDate}) ==="
      echo ""

      tools=(
        "gcc:GNU Compiler Collection"
        "make:GNU Make"
        "ld:GNU Linker"
        "gdb:GNU Debugger"
        "pkg-config:Package Config"
        "autoconf:GNU Autoconf"
        "automake:GNU Automake"
        "libtool:GNU Libtool"
        "cmake:CMake"
        "ninja:Ninja Build"
        "rustup:Rustup (Rust installer)"
        "git:Git VCS"
        "vim:Vim Editor"
        "curl:cURL"
        "jq:JSON Processor"
        "docker:Docker"
      )

      rust_tools=(
        "rustc:Rust Compiler"
        "cargo:Cargo Package Manager"
        "clippy:Rust Clippy"
        "rustfmt:Rust Formatter"
      )

      missing=()
      for tool_desc in "''${tools[@]}"; do
        tool="''${tool_desc%:*}"
        desc="''${tool_desc#*:}"
        if command -v "$tool" >/dev/null 2>&1; then
          echo "✅ $desc ($tool)"
        else
          echo "❌ $desc ($tool) - MISSING"
          missing+=("$tool")
        fi
      done

      echo ""
      echo "=== Rust Tools (via rustup) ==="

      # Auto-setup rustup if needed
      if command -v rustup >/dev/null 2>&1 && ! rustup show >/dev/null 2>&1; then
        echo "🦀 Auto-configuring Rust toolchain..."
        rustup default stable >/dev/null 2>&1
        rustup component add clippy rustfmt rust-src >/dev/null 2>&1
      fi

      for tool_desc in "''${rust_tools[@]}"; do
        tool="''${tool_desc%:*}"
        desc="''${tool_desc#*:}"
        if command -v "$tool" >/dev/null 2>&1; then
          version_info=""
          case $tool in
            rustc) version_info=" - $(rustc --version 2>/dev/null | cut -d' ' -f2)" ;;
            cargo) version_info=" - $(cargo --version 2>/dev/null | cut -d' ' -f2)" ;;
          esac
          echo "✅ $desc ($tool)$version_info"
        else
          echo "❌ $desc ($tool) - MISSING"
          missing+=("$tool")
        fi
      done

      echo ""
      if [ ''${#missing[@]} -eq 0 ]; then
        echo "🎉 All development tools are available!"
        echo ""
        echo "🦀 Rust toolchain status:"
        rustup show 2>/dev/null | head -10
        echo ""
        echo "🔧 Quick test commands:"
        echo "  rustc --version"
        echo "  cargo --version"
        echo "  docker run hello-world"
        echo "  gcc --version"
      else
        echo "⚠️  Missing tools: ''${missing[*]}"
        echo ""
        echo "Rust toolchain status:"
        rustup show 2>/dev/null || echo "Rustup not configured - try logging out and back in"
      fi

      echo ""
      echo "📁 Storage information:"
      echo "  Scratch space: $(df -h /scratch 2>/dev/null | tail -1 | awk '{print $4}') available"
      echo "  UTM share: $(mountpoint -q /mnt/share && echo "mounted" || echo "not mounted")"
    '';
    mode = "0755";
  };

  # Enhanced post-boot commands with version info
  boot.postBootCommands = ''
    echo "=== ${devImageName} v${devImageVersion} Boot Complete ==="
    echo "Build date: ${devImageDate}"
    echo ""
    echo "Available development tools:"
    command -v gcc && echo "  ✅ GCC: $(gcc --version 2>/dev/null | head -1)"
    command -v make && echo "  ✅ Make: $(make --version 2>/dev/null | head -1)"
    command -v rustup && echo "  ✅ Rustup: $(rustup --version 2>/dev/null)"
    command -v git && echo "  ✅ Git: $(git --version 2>/dev/null)"
    command -v docker && echo "  ✅ Docker: $(docker --version 2>/dev/null)"
    command -v cmake && echo "  ✅ CMake: $(cmake --version 2>/dev/null | head -1)"
    echo ""
    echo "🔧 Run '/etc/verify-dev-tools.sh' to verify all tools"
    echo "📁 Scratch space available at /scratch (4GB tmpfs)"
    echo "🔗 UTM share will be mounted at /mnt/share"
    echo "🚀 SSH in to see full banner and auto-configured Rust environment"
  '';
}
