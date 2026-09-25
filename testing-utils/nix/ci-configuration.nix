{ config, pkgs, lib, ... }:

let
  hostKeysDir = ./ssh-host-keys;
  userKeysDir = ./ssh-keys;

  userPubKeys =
    builtins.filter (file: lib.hasSuffix ".pub" file)
      (builtins.attrNames (builtins.readDir userKeysDir));

  # Runtime utilities the smoke suite's own ssh_out invocations rely on
  # directly (curl for HTTP checks, jq for parsing douglas's own JSON
  # output, docker for the build/push steps, etc.) — deliberately not the
  # dev image's C/Rust toolchain (gcc, cmake, rustup, ...), since this
  # image never builds anything: douglas is cross-compiled on the host
  # and rsync'd in as a static binary.
  ciPackages = with pkgs; [
    git
    curl
    jq
    docker
    socat
    lsof
    bind
    nettools
    iproute2
    tree
    htop
    tmux
    wget
  ];
in
{
  documentation.enable = false;
  documentation.nixos.enable = false;
  documentation.man.enable = false;
  documentation.info.enable = false;
  documentation.doc.enable = false;

  boot.loader.grub.enable = false;
  boot.loader.timeout = lib.mkForce 0;
  boot.isContainer = false;
  boot.kernelParams = [ "console=hvc0,115200n8" "console=tty0" "panic=0" "loglevel=7" ];
  boot.consoleLogLevel = 7;
  systemd.services."serial-getty@hvc0".enable = true;

  networking.hostName = "douglas-ci";
  networking.networkmanager.enable = true;
  networking.wireless.enable = false;

  services.openssh = {
    enable = true;
    settings = {
      PermitRootLogin = "yes";
      PasswordAuthentication = false;
    };
  };

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

  virtualisation.docker = {
    enable = true;
    enableOnBoot = true;
    storageDriver = "overlay2";
  };

  fileSystems."/tmp" = {
    device = "tmpfs";
    fsType = "tmpfs";
    options = [ "defaults" "size=2G" "mode=1777" ];
  };

  # Nothing that grows lives in RAM. The live-ISO root is a tmpfs capped at
  # half of RAM (971MB on the 2GB guest), and everything written to it counts
  # against memory. Two throwaway virtio disks, attached per run by
  # ci-fanout.sh (sparse files, deleted when the scenario passes), hold the
  # paths that grow; autoFormat mkfs's each one on first mount since they
  # always start blank.
  #   /dev/vdb -> /var/lib/douglas: retained binaries, resin's cache, and
  #               docker's data (data-root below, so one disk covers both)
  #   /dev/vdc -> /home: ~/douglas and the upgrade candidates
  fileSystems."/var/lib/douglas" = {
    device = "/dev/vdb";
    fsType = "ext4";
    autoFormat = true;
  };

  fileSystems."/home" = {
    device = "/dev/vdc";
    fsType = "ext4";
    autoFormat = true;
  };

  # /home was created on the root tmpfs during activation and is hidden by
  # the mount, so recreate the dev user's home on the fresh disk.
  systemd.tmpfiles.rules = [ "d /home/dev 0700 dev users -" ];

  virtualisation.docker.daemon.settings.data-root = "/var/lib/douglas/docker";
  systemd.services.docker.unitConfig.RequiresMountsFor = [ "/var/lib/douglas" ];

  users.users.dev = {
    isNormalUser = true;
    extraGroups = [ "wheel" "docker" ];
    initialPassword = "password";
    shell = pkgs.bash;
    openssh.authorizedKeys.keys =
      map (file: builtins.readFile "${userKeysDir}/${file}") userPubKeys;
  };

  environment.systemPackages = ciPackages;

  security.sudo.wheelNeedsPassword = false;
  security.sudo.extraConfig = ''
    #includedir /etc/sudoers.d
  '';

  nix.settings = {
    trusted-users = [ "root" "dev" "@wheel" ];
    experimental-features = [ "nix-command" "flakes" ];
  };

  isoImage = {
    makeEfiBootable = true;
    makeUsbBootable = true;
    includeSystemBuildDependencies = false;
  };

  system.stateVersion = "25.05";
}
