{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, flake-utils, nixpkgs }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
        {
          devShell = pkgs.mkShell {
            buildInputs = with pkgs; [ go rustc cargo gcc ];
          };
          packages.analyze = pkgs.buildGoModule {
            pname = "analyze";
            version = "0.2.0";
            src = ./analyze;
            vendorHash = "sha256-iuzm1kBhkObTDcUaayNkAqiVkzB/rlVdi6TKAhmpyTc=";
          };
        }
    );
}
