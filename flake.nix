{
  nixConfig.extra-substituters = [ "https://attic.kybe.xyz/main" ];
  nixConfig.extra-trusted-public-keys = [
    "main:cb7V485kGP0lG7LtQ/suOgKOgtVxNXrnD6i5yCtnaMQ="
  ];

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";

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
    {
      self,
      crane,
      nixpkgs,
      flake-utils,
      treefmt-nix,
      rust-overlay,
    }:
    let
      nixosModule = import ./nix/nixos-module.nix;
    in
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import rust-overlay)
            (final: prev: {
              webhook-router = (prev.callPackage ./nix/webhook-router.nix { inherit self crane; }).webhook-router;
            })
          ];
        };

        treefmt-eval = treefmt-nix.lib.evalModule pkgs ./nix/treefmt.nix;
        webhook-router = pkgs.callPackage ./nix/webhook-router.nix { inherit self crane; };

        nixosTest = pkgs.callPackage ./nix/nixos-test.nix { inherit nixosModule; };
      in
      {
        packages.default = webhook-router.webhook-router;

        checks = webhook-router.checks // {
          inherit nixosTest;
          formatting = treefmt-eval.config.build.check self;
        };

        formatter = treefmt-eval.config.build.wrapper;

        devShells.default = webhook-router.devShell;
      }
    )
    // {
      nixosModules = {
        default = nixosModule;
        webhook-router = nixosModule;
      };
    };
}
