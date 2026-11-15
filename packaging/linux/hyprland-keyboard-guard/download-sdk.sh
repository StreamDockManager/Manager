#!/usr/bin/env bash
# Download signed packages explicitly, then use pacman's offline verifier/installer
# No pacman download-sandbox or Docker security setting is disabled
set -euo pipefail
snapshot=${1:?snapshot required}
base="https://archive.archlinux.org/repos/$snapshot"
mkdir -p /var/lib/pacman/sync /sdk-cache
printf 'Server = %s/$repo/os/$arch\n' "$base" > /etc/pacman.d/mirrorlist
for repo in core extra; do
  curl --fail --location --retry 2 --connect-timeout 15 --max-time 180 \
    "$base/$repo/os/x86_64/$repo.db" -o "/var/lib/pacman/sync/$repo.db"
done
pacman -Suu --print --print-format '%l' hyprland cmake nlohmann-json > /sdk-cache/package-urls.txt
while IFS= read -r url; do
  case "$url" in https://archive.archlinux.org/*.pkg.tar.zst) ;; *) echo "Unexpected package URL: $url" >&2; exit 1;; esac
done < /sdk-cache/package-urls.txt
cat /sdk-cache/package-urls.txt | xargs -P 6 -n 1 bash -c '
  set -e
  url=$1
  file=/sdk-cache/${url##*/}
  curl -fsSL --retry 2 --connect-timeout 15 --max-time 240 "$url" -o "$file"
  curl -fsSL --retry 2 --connect-timeout 15 --max-time 90 "$url.sig" -o "$file.sig"
' _
# Require signatures for these local package files too (the stock local-file
# policy is optional). Keep all trust roots and package signature checks intact
sed 's/^LocalFileSigLevel.*/LocalFileSigLevel = Required/' /etc/pacman.conf > /sdk-cache/pacman-offline.conf
pacman --config /sdk-cache/pacman-offline.conf -U --noconfirm /sdk-cache/*.pkg.tar.zst
(cd /sdk-cache; sha256sum *.pkg.tar.zst > package-sha256sums.txt)
