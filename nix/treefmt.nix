{
  projectRootFile = "flake.nix";
  programs = {
    taplo.enable = true;
    typos.enable = true;
    nixfmt.enable = true;
    rustfmt.enable = true;
  };
  settings = {
    excludes = [
      "target/*"
      "result/*"
      ".git/*"
    ];
  };
}
