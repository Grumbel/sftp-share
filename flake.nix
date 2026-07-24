{
  description = "sftp-share: a tiny standalone SFTP server for sharing arbitrary files and directories";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "sftp-share";
          version = "0.1.0";
          src = ./.;

          # Requires a Cargo.lock committed alongside this flake (run
          # `cargo generate-lockfile` once, or just `cargo build`).
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.pkg-config ];

          buildInputs = [ ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.darwin.apple_sdk.frameworks.Security
              pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
            ];

          doCheck = false;

          meta = with pkgs.lib; {
            description = "A tiny, standalone SFTP server for sharing arbitrary files and directories with virtual users";
            homepage = "https://github.com/example/sftp-share";
            license = licenses.mit;
            mainProgram = "sftp-share";
            platforms = platforms.unix ++ platforms.windows;
          };
        };

        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.rustfmt
            pkgs.clippy
            pkgs.pkg-config
          ];
        };
      });
}
