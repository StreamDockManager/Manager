# Build-only SDK image. Never starts a compositor or loads the resulting module
FROM --platform=linux/amd64 archlinux:base-devel@sha256:fa2d4ce3310ddd90a4208e5168be8ad367da5129af1245f38fd2d803bcbe3239 AS builder
ARG ARCH_SNAPSHOT=2026/10/03
COPY download-sdk.sh /download-sdk.sh
RUN bash /download-sdk.sh "$ARCH_SNAPSHOT"
RUN test "$(pacman -Q hyprland)" = 'hyprland 0.56.2-4' \
 && grep -F '"efb50993780079460b0cbed1363e2166a2de1d9f"' /usr/include/hyprland/src/version.h
WORKDIR /src
COPY CMakeLists.txt main.cpp Dockerfile download-sdk.sh inspect-elf.py ./
ARG SDM_MANAGER_EXECUTABLE
RUN cmake -S . -B /build -DCMAKE_BUILD_TYPE=Release \
      -DSDM_CROSS_BUILD_TARGET_PATH=ON -DSDM_MANAGER_EXECUTABLE="$SDM_MANAGER_EXECUTABLE" \
 && cmake --build /build --parallel 2 \
 && mkdir /output \
 && cp /build/sdm-keyboard-guard.so /output/ \
 && cp /usr/include/hyprland/src/version.h /output/hyprland-sdk-version.h \
 && pacman -Q > /output/sdk-packages.txt \
 && cp /sdk-cache/package-urls.txt /sdk-cache/package-sha256sums.txt /output/ \
 && { uname -m; c++ --version; cmake --version; for package in hyprland wayland-server aquamarine hyprutils hyprlang hyprcursor hyprgraphics; do printf "%s: " "$package"; pkg-config --modversion "$package"; done; printf "Arch snapshot: %s\nSDK package: hyprland 0.56.2-4\nBuilder image: archlinux:base-devel@sha256:fa2d4ce3310ddd90a4208e5168be8ad367da5129af1245f38fd2d803bcbe3239\n" "$ARCH_SNAPSHOT"; printf 'Manager allowlist: %s\n' "$SDM_MANAGER_EXECUTABLE"; } > /output/build-manifest.txt \
 && file /output/sdm-keyboard-guard.so > /output/elf-file.txt \
 && readelf -h -d -V /output/sdm-keyboard-guard.so > /output/elf-details.txt \
 && readelf --dyn-syms --wide /output/sdm-keyboard-guard.so > /output/elf-symbols.txt \
 && grep -E ' (pluginInit|pluginExit|pluginAPIVersion)$' /output/elf-symbols.txt > /output/plugin-exports.txt \
 && tar -xOf /sdk-cache/hyprland-0.56.2-4-x86_64.pkg.tar.zst .BUILDINFO > /output/hyprland-package-buildinfo.txt \
 && printf '163bc740241880192d90027fad39ddb809b1705b04800a1f0185a58f3ce0884a  /sdk-cache/hyprland-0.56.2-4-x86_64.pkg.tar.zst\n' | sha256sum --check \
 && python /src/inspect-elf.py /output/sdm-keyboard-guard.so /output \
 && (cd /src; sha256sum main.cpp CMakeLists.txt Dockerfile download-sdk.sh inspect-elf.py) > /output/source-sha256sums.txt \
 && (cd /output; sha256sum sdm-keyboard-guard.so) > /output/SHA256SUMS
FROM scratch AS artifact
COPY --from=builder /output/ /
