# Signing keys

`dev-signing.key` / `dev-signing.pub` are a **development** key pair, committed
so tests and local builds can sign feature packages. Anyone can sign with it:
a release build's `wna.compose.toml` must never list `dev-signing.pub` in
`trusted-keys`. Production keys live in your secret store; generate one with
`wna-pkg keygen -o <file>` and pin its printed public key.
