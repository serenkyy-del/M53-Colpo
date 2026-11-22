Colposcopy standalone Windows x64

Extract the complete ZIP into a new directory. Verify the ZIP SHA-256 against
an independently received checksum from your administrator. Run Setup.ps1 in
Windows PowerShell from the extracted directory, then start Colposcopy using
the Start menu shortcut. Initial setup downloads the verified print components.
To use the application directly from the extracted ZIP, run
Install-PrintRuntime.ps1 once for printing, then open Colposcopy.exe.

Installation target: %LOCALAPPDATA%\Programs\Colposcopy.
Close the application before upgrading. Setup replaces program files only;
%LOCALAPPDATA%\Colposcopy contains profiles/local work and is preserved.
Do not delete that data directory to resolve synchronization problems.

Your administrator supplies your own HTTPS URL, workstation profile and login.
No credentials, external integration, updater or update signing key is included.
The manifest detects accidental changes; it is not a digital signature.

Setup obtains Chromium Headless and PDF.js from their official download
locations and verifies the archive and individual file SHA-256 hashes.
Once installed, printing is available offline. Component licenses are in the
installed print runtime. Font and Rust notices are in licenses/.
If Windows reports missing Visual C++ libraries, install the Microsoft x64
Redistributable from https://aka.ms/vc14/vc_redist.x64.exe .
The application's own code is licensed under AGPL-3.0-only. See
licenses/LICENSE. Obtain the matching Colposcopy-Source.zip from the same
release as the Windows ZIP. Third-party components keep their own licenses.

This package does not assert medical approval or Windows code signing.
