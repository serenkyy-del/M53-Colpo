#!/usr/bin/env python3
"""Package the built Linux desktop binary without bundling system libraries."""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
DEPENDENCIES = (
    "libc6 (>= 2.36), libgcc-s1, libfontconfig1, libfreetype6, libglib2.0-0, "
    "libx11-6, libx11-xcb1, libxcb1, libxkbcommon0, libxkbcommon-x11-0, "
    "libwayland-client0, libvulkan1, mesa-vulkan-drivers, libudev1, poppler-utils, "
    "xdg-desktop-portal"
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--notices', required=True, type=Path)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9][0-9.a-z+~\-]*', args.version):
        parser.error('Use a Debian-compatible version')
    if not (args.notices / 'DEPENDENCIES.tsv').is_file():
        parser.error('Provide notices and a dependency inventory for this build')
    binary = args.binary.resolve()
    with binary.open('rb') as f:
        elf_header = f.read(20)
    if not elf_header.startswith(b'\x7fELF\x02\x01') or elf_header[18:20] != b'\x3e\x00':
        parser.error('Expected a Linux x86-64 ELF binary')
    args.output.mkdir(parents=True, exist_ok=True)
    archive = args.output / 'Colposcopy-Linux-x64.deb'
    if archive.exists():
        parser.error('Choose an output directory without a package of this name')
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        metadata = root / 'DEBIAN'
        metadata.mkdir(mode=0o755)
        (metadata/'control').write_text(
            f"Package: colposcopy-desktop\nVersion: {args.version}\nArchitecture: amd64\n"
            "Section: science\nPriority: optional\n"
            "Maintainer: M53-Colpo contributors\n"
            f"Depends: {DEPENDENCIES}\nRecommends: cups-client, xdg-desktop-portal-gtk\n"
            "Homepage: https://github.com/serenkyy-del/M53-Colpo\n"
            "Description: Digital colposcopy desktop workstation\n"
            " Local login, examination records, image annotations and PDF reports.\n"
        )
        programs = root / 'usr/bin'
        programs.mkdir(parents=True)
        executable = programs / 'colposcopy'
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        launchers = root / 'usr/share/applications'
        launchers.mkdir(parents=True)
        (launchers/'colposcopy.desktop').write_text(
            "[Desktop Entry]\nType=Application\nName=Colposcopy\nName[ru]=Кольпоскопия\n"
            "Comment=Colposcopy workstation\nExec=colposcopy\n"
            "Icon=applications-science\nTerminal=false\nCategories=Science;MedicalSoftware;\n"
        )
        documentation = root / 'usr/share/doc/colposcopy-desktop'
        documentation.mkdir(parents=True)
        license_file = ROOT/'LICENSE'
        shutil.copyfile(license_file if license_file.is_file() else ROOT/'public/LICENSE', documentation/'copyright')
        shutil.copyfile(ROOT/'crates/client/assets/FONT-LICENSE.txt', documentation/'FONT-LICENSE.txt')
        shutil.copytree(args.notices, documentation/'rust')
        subprocess.run(['dpkg-deb','--root-owner-group','--build',str(root),str(archive)],check=True)


if __name__ == '__main__':
    main()
