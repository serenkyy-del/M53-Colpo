FROM rust:1.100.0-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
    libfontconfig-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev libx11-xcb-dev \
    libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    libasound2-dev libudev-dev mesa-vulkan-drivers xvfb xauth dbus dbus-x11 \
    pkg-config python3 fonts-dejavu-core poppler-utils && rm -rf /var/lib/apt/lists/*
