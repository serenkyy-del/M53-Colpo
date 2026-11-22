# Windows release preparation

Use a Windows x64 MSVC Rust toolchain and Windows PowerShell 5.1 or newer.
The script builds only the standalone editor binary, with no launcher/updater.
A prebuilt standalone binary can be supplied with `-AppExe` for independent
packaging. Verify its provenance and target yourself; renaming does not convert
an integrated build to the standalone edition.

The client embeds `assets/print-runtime/manifest.json`. Its archive hash must
match the packaged runtime exactly. For a clean build, Python 3.11+ can download
the pinned public Chromium/PDF.js sources, verify source and every selected file
SHA-256, and produce a deterministic ZIP_STORED archive:

```text
python public/windows/provision-runtime.py --cache /path/to/download-cache --output /path/to/new-runtime
python public/windows/verify.py /path/to/new-runtime/m53-print-runtime.zip /path/to/new-runtime/manifest.json
```

The source snapshot already contains the reviewed deterministic manifest.
Compare the emitted manifest with the checked-in one. Source pins and individual
file records must match. If a future runtime update changes the archive, copy the emitted manifest to `assets/print-runtime/manifest.json` BEFORE building
the client. This is an explicit change to the compiled trust anchor, so preserve
the reviewed manifest, resulting archive and source hashes together. The tool
never changes repository files itself and rejects existing output directories.
No Rust runtime checksum bypass is needed.

Collect all Rust dependency license notices for the exact isolated
`prototypes/gpui-editor/Cargo.lock` used for this build in a separate directory.
The packaging script requires that directory; it does not certify completeness.
Chromium and PDF.js license files remain in the verified print runtime archive;
the font license is copied separately. The application uses AGPL-3.0-only.
For distribution via downloads, provide the matching Colposcopy-Source.zip and
the rest of the Corresponding Source required by section 6 alongside the Windows
ZIP. Include the build and installation scripts and the source of covered
dependencies, preserving their license notices.

Run from the repository root:

```powershell
public/windows/package.ps1 -RuntimeArchive C:\build\runtime\m53-print-runtime.zip -RustLicensesDirectory C:\build\notices -OutputDirectory C:\build\release
```

For independent packaging add `-AppExe C:\build\Colposcopy.exe`. Optional
`-VCRuntimeDll` accepts an authorized app-local vcruntime140.dll; otherwise install
the official Visual C++ x64 redistributable separately. Review the executable's
imported DLLs for the actual build. Package creation does not sign the executable.

Output is `Colposcopy-Windows-x64.zip` plus `.sha256`. Package content includes
Colposcopy.exe, Setup.ps1, README, print archive, its manifest and license notices.
No profile, credentials, update keys or old updater are copied. Deliver the ZIP
hash using an independent channel. Extract into a new folder and run Setup.ps1;
the installer checks required files and hashes before copying and preserves the
separate application data directory during upgrades.

Linux verification: `python3 -m unittest discover -s public/windows -p 'test_*.py'`.
These tests cover deterministic runtime packing and modified/unexpected runtime
entry rejection. They do not prove Windows installation, shortcuts, execution or
printing. Run isolated Windows install/upgrade and first-PDF checks before release.
