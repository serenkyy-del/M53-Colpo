(async () => {
  const pdfjs = await import('data:text/javascript;base64,__PDF_MODULE__');
  pdfjs.GlobalWorkerOptions.workerSrc = 'data:text/javascript;base64,__PDF_WORKER__';
  const load = encoded => pdfjs.getDocument({
    data: Uint8Array.from(atob(encoded), c => c.charCodeAt(0)),
    isEvalSupported: false, useSystemFonts: false, disableAutoFetch: true,
  });
  const render = async (doc, number, scale) => {
    const page = await doc.getPage(number);
    const viewport = page.getViewport({ scale });
    if (viewport.width * viewport.height > 16 * 1024 * 1024) throw new Error('page size limit');
    const canvas = document.createElement('canvas');
    canvas.width = Math.ceil(viewport.width); canvas.height = Math.ceil(viewport.height);
    try {
      await page.render({ canvasContext: canvas.getContext('2d', { alpha: false }), viewport }).promise;
      return canvas.toDataURL('image/png').split(',')[1];
    } finally { canvas.width = 0; canvas.height = 0; page.cleanup(); }
  };
  const count = doc => { if (doc.numPages > 100) throw new Error('page limit'); return doc.numPages; };
  globalThis.m53RenderPdf = async encoded => {
    const task = load(encoded), doc = await task.promise;
    try {
      if (count(doc) > 20) throw new Error('M53_PREVIEW_LIMIT');
      const result = []; let total = 0, pixels = 0;
      for (let number = 1; number <= count(doc); number++) {
        const page = await doc.getPage(number);
        const scale = 1480 / page.getViewport({ scale: 1 }).width;
        const viewport = page.getViewport({ scale });
        pixels += Math.ceil(viewport.width) * Math.ceil(viewport.height);
        if (pixels > 64 * 1024 * 1024) throw new Error('M53_PREVIEW_LIMIT');
        const png = await render(doc, number, scale);
        total += png.length; if (total > 160 * 1024 * 1024) throw new Error('M53_PREVIEW_LIMIT');
        result.push(png);
      }
      return result;
    } finally { await task.destroy(); }
  };
  let printTask = null, printDoc = null;
  globalThis.m53ClosePrint = async () => {
    if (printTask) await printTask.destroy();
    printTask = null; printDoc = null;
  };
  globalThis.m53OpenPrint = async encoded => {
    await m53ClosePrint(); printTask = load(encoded); printDoc = await printTask.promise;
    return count(printDoc);
  };
  globalThis.m53PrintPage = async (number, dpi) => render(printDoc, number, dpi / 72);
  return true;
})()
