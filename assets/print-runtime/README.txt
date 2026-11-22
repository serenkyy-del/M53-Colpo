Pinned offline print runtime

HTML -> PDF: Google Chrome Headless Shell 156.0.8078.47 (Chrome for Testing win64).
PDF bytes -> PNG: Mozilla PDF.js 6.4.299. PDF.js renders the actual generated PDF;
preview is not a separate HTML screenshot/layout.

Official upstream references:
https://github.com/GoogleChromeLabs/chrome-for-testing
https://developer.chrome.com/docs/automation-and-testing/headless-chrome-shell
https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-printToPDF
https://mozilla.github.io/pdf.js/getting_started/
https://github.com/mozilla/pdf.js/releases/tag/v6.4.299

provision-print-runtime.ps1 downloads only pinned HTTPS archives and requires their
recorded sizes/SHA-256. The PDF.js archive SHA matches the release asset digest.
CfT does not publish a SHA-256 digest in its JSON API; its archive SHA was recorded
from the official Google HTTPS download and pinned locally (not an upstream signature).
manifest.json also pins every extracted file. The flat runtime ZIP is included in
the existing signed M53 update manifest and verified before lazy atomic extraction.
No development-machine path or installed Edge/Chrome is used in a distributed build.

Licenses and upstream acknowledgements are inside the signed runtime archive:
chrome/ABOUT, chrome/LICENSE.headless_shell (Chromium and third-party notices),
pdfjs/LICENSE (Apache-2.0). Do not strip these while redistributing the runtime.
The bundle is approximately 130.3 MB compressed and 299 MB extracted, once per
pinned version. It needs no separate browser install, WinRT PDF or WebView2.
Windows process/driver/printing facilities remain OS dependencies; this does not
claim that the Windows native GPUI/capture/installer stack is cross-platform.

Renderer isolation: background networking disabled, all http/https/ws/wss/file/ftp
page requests blocked by CDP, report CSP permits inline styles and data images/fonts
only. CDP binds an ephemeral port on 127.0.0.1; it is never exposed on LAN.
The two targets share one owned child process; job close terminates its descendants.
Only one request renders at a time. Last PDF and preview are content-keyed and bounded;
unchanged preview avoids Chrome startup and layout. Errors/timeouts discard the child.
The isolated profile is temporary and never contains HTML or PDF source files.

Upgrade procedure: intentionally change source pins, verify official provenance,
rebuild deterministic archive/manifest, run synthetic runtime check plus clinical
layout fixtures (synthetic only), review pagination, publish a newly signed client.
Do not silently download a moving "latest" version on a doctor's workstation.

Physical printing uses preview::print_pdf after explicit printer/range dialog OK,
then a single page at 300 DPI is passed to Windows GDI/spooler. Cancel creates no
job. The active action has no PDF reader association dependency. The OS printer
stack and the selected printer's driver are required. open_pdf remains only for
legacy explicitly requested file opening, not the active print workflow.
The synthetic runtime check verifies 300-DPI page/range/color rasterization with
a memory callback only; it never opens a print dialog or submits a printer job.
The in-memory preview is limited to 20 pages (about 250 MB of decoded A4 textures
at 1480 px width); larger PDFs show an explicit preview limit, never partial text.
Full PDF save and page-by-page printing retain the separate 100-page safety limit.
Background warm_up uses only a fixed synthetic sheet, no patient input/network/job.
