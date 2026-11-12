{
  description = "wprs - xpra, but for wayland, and written in rust";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

      # Shared between the package and the devShell.
      nativeBuildInputs = pkgs: with pkgs; [
        pkg-config
      ];

      buildInputs = pkgs: with pkgs; [
        libxkbcommon
        wayland
      ];
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};

          wprs = pkgs.rustPlatform.buildRustPackage {
            pname = "wprs";
            version = "0.1.0";
            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = nativeBuildInputs pkgs;
            buildInputs = buildInputs pkgs;

            doCheck = false;
          };

          wprsLauncher = pkgs.writeShellScriptBin "wprs" ''
            exec ${pkgs.python3.withPackages (ps: [ ps.psutil ])}/bin/python3 ${./wprs} "$@"
          '';

          daemon = pkgs.runCommand "wprsd"
            {
              meta = {
                description = "wprsd Wayland remote display daemon";
                license = pkgs.lib.licenses.asl20;
                platforms = pkgs.lib.platforms.linux;
              };
            }
            ''
              mkdir -p $out/bin
              cp ${wprs}/bin/wprsd $out/bin/
              cp ${wprs}/bin/xwayland-xdg-shell $out/bin/

              mkdir -p $out/lib/systemd/user
              substitute ${./wprsd.service} $out/lib/systemd/user/wprsd.service \
                --replace-fail "/usr/bin/wprsd" "$out/bin/wprsd"

              mkdir -p $out/lib/systemd/user-preset
              cp ${./debian/50-wprsd.preset} $out/lib/systemd/user-preset/50-wprsd.preset
            '';

          client = pkgs.runCommand "wprsc"
            {
              meta = {
                description = "wprsc Wayland remote display client";
                license = pkgs.lib.licenses.asl20;
                platforms = pkgs.lib.platforms.linux;
              };
            }
            ''
              mkdir -p $out/bin
              cp ${wprs}/bin/wprsc $out/bin/
              cp ${wprsLauncher}/bin/wprs $out/bin/
            '';

          default = pkgs.symlinkJoin {
            name = "wprs";
            paths = [ daemon client ];
            meta = {
              description = "wprs - xpra, but for wayland, and written in rust";
              license = pkgs.lib.licenses.asl20;
              platforms = pkgs.lib.platforms.linux;
            };
          };
        in
        {
          inherit daemon client default;
        });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            # The toolchain comes from rustup (rust-toolchain.toml), so only
            # take the system dependencies from the package rather than using
            # inputsFrom, which would also pull in nixpkgs' cargo and rustc.
            nativeBuildInputs = nativeBuildInputs pkgs;
            buildInputs = buildInputs pkgs ++ (with pkgs; [
              rustup
              python3
              python3Packages.psutil
            ]);
            shellHook = ''
              export RUST_BACKTRACE=1
            '';
          };
        });

      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.wprsd;
        in
        {
          options.services.wprsd = {
            enable = lib.mkEnableOption "wprsd Wayland remote display daemon";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.daemon;
              defaultText = lib.literalExpression "wprs.packages.\${pkgs.system}.daemon";
              description = ''
                The wprsd package to use.
              '';
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.user.services.wprsd = {
              description = "wprsd Wayland remote display daemon";
              after = [ "network.target" ];
              wantedBy = [ "default.target" ];
              serviceConfig = {
                Type = "simple";
                ExecStart = "${cfg.package}/bin/wprsd";
                Environment = "RUST_BACKTRACE=1";
              };
            };
          };
        };
    };
}
