# Overview
This nix configuration and flake creates a live CD for integration
testing the douglas system in a real linux environment.  It is set up
as a live cd, so on boot it is in a known good state.  It provides
* The Rust development chain
* git
* jq
* curl
* ssh with the provided keys (integrated at build time)
* Zeroconf to be a well-known host on douglas-dev.local

## Building
### Create keys
```bash
# Create keys for the host, these will be integrated in every build
mkdir -p ./ssh-host-keys
ssh-keygen -t rsa -b 4096 -f ./ssh-host-keys/ssh_host_rsa_key -N ""
ssh-keygen -t ed25519 -f ./ssh-host-keys/ssh_host_ed25519_key -N ""

# For SSH, also integrated in every build
mkdir -p ssh-keys
ssh-keygen -t ed25519 -f ./ssh-keys/douglas_id_ed25519 -C "douglas@livecd"
```

### Create a build environment
From the Linux environment of your choice, create a NixOS build
environment:

```bash
# Get the build tools
sudo apt update
sudo apt install -y \
  curl git xz-utils bzip2 gzip xorriso squashfs-tools \
  sudo bash build-essential pkg-config

# Create a build user
sudo useradd -m builder
echo "builder ALL=(ALL) NOPASSWD:ALL" | sudo tee -a /etc/sudoers
sudo su - builder
cd ~

# Install nix
sh <(curl -L https://nixos.org/nix/install) --daemon
. /etc/profile.d/nix.sh

# Verify the install
nix --version

# Enable some nice-to-haves
mkdir -p ~/.config/nix
echo "experimental-features = nix-command flakes" >> ~/.config/nix/nix.conf

# Run some quick updates
nix-channel --add https://github.com/NixOS/nixpkgs/archive/nixos-24.05.tar.gz nixos
nix-channel --update
```

Then build the files in this working directory with:

```bash
nix build .#live-iso
```

This creates a bootable live cd to use!
