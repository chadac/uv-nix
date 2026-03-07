{ pkgs, lib, config, inputs, ... }:

{
  # https://devenv.sh/packages/
  packages = [
    pkgs.cargo
    pkgs.patchelf
    pkgs.python3
    pkgs.python3Packages.pytest
    pkgs.python3Packages.docker
    pkgs.binutils  # provides readelf for test assertions
    pkgs.just
  ];

  # https://devenv.sh/languages/
  languages.rust.enable = true;

  enterShell = ''
    export PATH="$DEVENV_ROOT/uv/target/debug:$PATH"
  '';

  # https://devenv.sh/processes/
  processes.cargo-watch.exec = "cargo-watch";
}
