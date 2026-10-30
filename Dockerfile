# Linux dev/CI environment: the same Nix flake as on macOS, realized into an
# image so CI and Linux developers get identical toolchains.
#
#   docker build -t wna-dev .
#   docker run --rm -v "$PWD":/src -w /src wna-dev just test-linux
FROM nixos/nix:2.28.3

RUN printf 'experimental-features = nix-command flakes\nfilter-syscalls = false\nsandbox = false\n' >> /etc/nix/nix.conf

WORKDIR /src
COPY flake.nix flake.lock rust-toolchain.toml ./
# Realize the dev shell (Rust, JDK, Kotlin, clang, wasm-tools, just); the
# profile is a GC root, so garbage collection keeps it.
RUN nix develop --profile /nix/var/nix/profiles/wna-dev -c true && nix-collect-garbage >/dev/null

# Keep the cargo registry and build outputs in volumes in CI if you can.
ENV CARGO_HOME=/cargo
ENTRYPOINT ["nix", "develop", "/src", "-c"]
CMD ["just", "test-linux"]
