#!/usr/bin/env bash
# packaging-validate.sh - Validates packaging manifests
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

echo "==> Validating packaging manifests..."

# 1. Flatpak manifest YAML
if python3 -c "import yaml" >/dev/null 2>&1; then
    python3 -c "import yaml; yaml.safe_load(open('packaging/flatpak/dev.fleetglass.Fleetglass.yml'))"
    echo "OK: Flatpak manifest parses as YAML"
else
    echo "SKIP: python3 yaml (PyYAML) module not installed"
fi

# 2. Desktop file
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate packaging/flatpak/dev.fleetglass.Fleetglass.desktop
    echo "OK: desktop file validates"
else
    echo "SKIP: desktop-file-validate not installed"
fi

# 3. Metainfo file
if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net packaging/flatpak/dev.fleetglass.Fleetglass.metainfo.xml
    echo "OK: metainfo validates"
else
    echo "SKIP: appstreamcli not installed"
fi

# 4. AUR PKGBUILD syntax
bash -n packaging/aur/PKGBUILD
echo "OK: PKGBUILD bash syntax valid"

# 5. Homebrew Cask ruby syntax
if command -v ruby >/dev/null 2>&1; then
    ruby -c packaging/homebrew/fleetglass.rb >/dev/null
    echo "OK: Homebrew cask Ruby syntax valid"
else
    echo "SKIP: ruby not installed"
fi

# 6. Winget YAML manifests
if python3 -c "import yaml" >/dev/null 2>&1; then
    python3 -c "import yaml; [yaml.safe_load(open(f)) for f in ['packaging/winget/Fleetglass.version.yaml', 'packaging/winget/Fleetglass.installer.yaml', 'packaging/winget/Fleetglass.locale.en-US.yaml']]"
    echo "OK: WinGet manifests parse as YAML"
else
    echo "SKIP: python3 yaml (PyYAML) module not installed"
fi

echo "==> All packaging validation checks passed."
