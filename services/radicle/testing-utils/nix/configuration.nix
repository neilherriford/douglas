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
  devImageVersion = "0.0.2c";
  devImageDate = "2025-09-11";
  devImageName = "Douglas Development Environment";

  # Define our development packages explicitly - CHOOSE ONE RUST APPROACH
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

    # Development utilities
    git
    vim
    curl
    jq
    docker
    socat

    # Additional useful development tools
    tree
    htop
    tmux
    wget
  ];
in
{
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
  ╱│╲      /            //
  ╱│╲   __/ __ . . _,  // __.  _
  ╱│╲  (_/_(_)(_/_(_)_</_(_/|_/_)_
   │   ⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼/|⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼⎼
       d e v      |/        v${devImageVersion}

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

        # Install and set default Rust toolchain
        ${pkgs.rustup}/bin/rustup default stable

        # Add essential components
        ${pkgs.rustup}/bin/rustup component add clippy rustfmt rust-src

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

          # Install stable toolchain quietly
          rustup default stable >/dev/null 2>&1

          # Add essential components
          rustup component add clippy rustfmt rust-src >/dev/null 2>&1

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
    options = [ "defaults" "size=4G" "mode=1777" ];
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
      rustup default stable
      rustup component add clippy rustfmt rust-src
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
    includeSystemBuildDependencies = true;
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
