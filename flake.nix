{
  nixConfig = {
    extra-substituters = [ "https://attic.kybe.xyz/main" ];
    extra-trusted-public-keys = [
      "main:cb7V485kGP0lG7LtQ/suOgKOgtVxNXrnD6i5yCtnaMQ="
    ];
  };

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";

    crane.url = "github:ipetkov/crane";

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      {
        self,
        ...
      }:
      {
        flake = {
          nixosModules =
            let
              nixosModule = { pkgs, ... }@args: import ./nix/nixos-module.nix (args // { inherit self pkgs; });
            in
            {
              default = nixosModule;
              webhook-router = nixosModule;
            };
        };

        systems = [
          "x86_64-linux"
          "aarch64-linux"
        ];

        perSystem =
          {
            pkgs,
            config,
            ...
          }:
          let
            pkgs' = pkgs.extend inputs.rust-overlay.overlays.default;
            treefmt-eval = inputs.treefmt-nix.lib.evalModule pkgs' ./nix/treefmt.nix;
            webhook-router = pkgs'.callPackage ./nix/webhook-router.nix {
              inherit self;
              inherit (inputs) crane;
            };
          in
          {
            packages.default = webhook-router.webhook-router;
            devShells.default = webhook-router.devShell;

            checks = webhook-router.checks // {
              formatting = treefmt-eval.config.build.check self;
            };
            formatter = treefmt-eval.config.build.wrapper;
          };
      }
    );
}
