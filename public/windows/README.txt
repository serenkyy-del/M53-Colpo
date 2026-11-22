Colposcopy standalone Windows x64

Extract the complete ZIP into a new directory. Verify the ZIP SHA-256 against
an independently received checksum from your administrator. Run Setup.ps1 in
Windows PowerShell from the extracted directory, then start Colposcopy using
the Start menu shortcut. You can also run Colposcopy.exe from the extracted ZIP.

Installation target: %LOCALAPPDATA%\Programs\Colposcopy.
Close the application before upgrading. Setup replaces program files only;
%LOCALAPPDATA%\Colposcopy contains profiles/local work and is preserved.
Do not delete that data directory to resolve synchronization problems.

Your administrator supplies your own HTTPS URL, workstation profile and login.
No credentials, external integration, updater or update signing key is included.
The manifest detects accidental changes; it is not a digital signature.

Printing is offline using the bundled pinned Chromium/PDF.js archive. Component
licenses are inside that archive; font and Rust notices are in licenses/.
For an MSVC build install the Microsoft Visual C++ 2015-2022 x64 Redistributable
from Microsoft, unless the administrator supplied an authorized app-local CRT.
The application's own code is licensed under AGPL-3.0-only. See
licenses/LICENSE. Obtain the matching Colposcopy-Source.zip from the same
release as the Windows ZIP. Third-party components keep their own licenses.

This package does not assert medical approval or Windows code signing.
