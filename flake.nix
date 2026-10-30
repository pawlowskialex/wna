{
  description = "WNA: WIT-native platform for a cross-platform monorepo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      }));
    in {
      devShells = forAll (pkgs:
        let
          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          jdk = pkgs.jdk21_headless;
        in {
          # mkShellNoCC: on macOS the C/Swift toolchain comes from Xcode, so the
          # shell must not inject a Nix SDK/compiler that would shadow it.
          default = pkgs.mkShellNoCC {
            packages = [
              rust
              jdk
              pkgs.kotlin
              pkgs.just
              pkgs.wasm-tools
              pkgs.wasmtime
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.clang ];
            JAVA_HOME = jdk.home;
            # On macOS, C/C++/Swift and Rust linking use Xcode's toolchain and
            # SDKs (needed for iOS anyway); drop the Nix cc-wrapper that the
            # Rust toolchain propagates.
            shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              unset SDKROOT DEVELOPER_DIR NIX_CC NIX_CC_FOR_BUILD CC CXX LD AR NM RANLIB STRIP
              for v in $(env | grep -oE '^NIX_(CFLAGS|LDFLAGS|CC_WRAPPER|BINTOOLS)[A-Za-z0-9_]*'); do unset "$v"; done
              export PATH="$(echo "$PATH" | tr ':' '\n' | grep -vE 'clang-wrapper|-clang-[0-9]|cctools|binutils-wrapper' | paste -sd: -)"
            '';
          };
        });
    };
}
