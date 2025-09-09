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
in
{
  ############################ BOOT SETTINGS ###########################
  boot.kernelModules = [ "virtiofs" ];
  boot.loader.grub.enable = false;
  boot.loader.timeout = lib.mkForce 0;
  boot.isContainer = false;

  ########################### SYSTEM PACKAGES ##########################
  environment.systemPackages = with pkgs; [
    docker
    rustc
    cargo
    clippy
    rustfmt
    jq
    curl
    git
    vim
  ];

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
      ExecStart = "${pkgs.utillinux}/bin/mount -t virtiofs share /mnt/share";
      RemainAfterExit = true;
    };
    wantedBy = [ "multi-user.target" ];
  };

  ############################## ZEROCONF ##############################
  services.avahi = {
    enable = true;
    nssmdns = true;  # integrates mDNS into /etc/nsswitch.conf
    openFirewall = true; # automatically open UDP 5353 for mDNS
    publish = {
      enable = true;
      workstation = true; # advertise hostname for SSH/SMB/etc.
      addresses = true;   # advertise the IP address
    };
  };
  networking.hostName = "douglas-dev";

  ############################### DOCKER ###############################
  virtualisation.docker.enable = true;

  ########################## SSH CONFIGURATION #########################
  services.openssh = {
    enable = true;
    settings = {
      PermitRootLogin = "yes";
      PasswordAuthentication = false;
    };
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

  ######################### USER CONFIGURATION #########################
  users.users.douglas = {
    isNormalUser = true;
    extraGroups = [ "wheel" "docker" ];
    initialPassword = "password";

    # Bake in authorized public keys
    openssh.authorizedKeys.keys =
      map (file: builtins.readFile "${userKeysDir}/${file}") userPubKeys;
  };

  security.sudo.wheelNeedsPassword = false;

  ######################### RUSTUP AUTO-INSTALL ########################
  system.activationScripts.installRustupForDouglas = {
    text = ''
      # Install Rust stable globally
      ${pkgs.rustup}/bin/rustup install stable

      # Ensure douglas owns a rustup config dir
      mkdir -p /home/douglas/.rustup /home/douglas/.cargo
      chown -R douglas:users /home/douglas/.rustup /home/douglas/.cargo

      # Set stable as the default for douglas
      sudo -u douglas ${pkgs.rustup}/bin/rustup default stable
    '';
  };
}
