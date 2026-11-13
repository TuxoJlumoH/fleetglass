# Packaging Fleetglass

This directory contains package manifests for community distributions and app stores.

## Formats

| Platform | Format | File |
|---|---|---|
| Linux | Flatpak | `flatpak/dev.fleetglass.Fleetglass.yml` |
| Arch Linux | AUR | `aur/PKGBUILD` (`fleetglass-bin`) |
| macOS | Homebrew Cask | `homebrew/fleetglass.rb` |
| Windows | WinGet | `winget/Fleetglass.*.yaml` |

## Validation

Run `scripts/packaging-validate.sh` to validate all manifests against available linters (`appstreamcli`, `desktop-file-validate`, `bash -n`, `ruby -c`, python YAML parser).

## Release Procedure

At release time:
1. Replace `@VERSION@` with the release version (e.g. `0.5.0`).
2. Replace `@SHA256@` placeholders with the SHA-256 sums from the release pipeline.
3. Replace `@REPO_URL@` with the GitHub repository URL (`https://github.com/TuxoJlumoH/fleetglass`).

### Auto-update (not implemented yet)
The in-app updater is not wired up yet, so release builds do not create updater artifacts
(`bundle.createUpdaterArtifacts: false` in `crates/app/tauri.conf.json`). Enabling it later means
adding `tauri-plugin-updater`, generating a signing key (`pnpm -C ui tauri signer generate`),
storing it as the `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository
secrets, and pointing the updater at
`https://github.com/TuxoJlumoH/fleetglass/releases/latest/download/latest.json`.
