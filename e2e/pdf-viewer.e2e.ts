/**
 * FER-453: the sandboxed pdf.js viewer in real Chromium. A generated one-page PDF
 * is drawn to a canvas with real pixels; a broken file shows the fixed error; and
 * a frame with sandbox="allow-scripts" is proven opaque-origin, without Tauri
 * globals, without parent access and without network.
 */
import { expect, test, type Page } from '@playwright/test';
/** One page, 200x100 points, black text: a real PDF with a correct xref table. */
function smallPdfBase64(): string {
  const stream = 'BT /F1 40 Tf 10 40 Td (Hi) Tj ET';
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>',
    `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
  ];
  let out = '%PDF-1.4\n';
  const offsets: number[] = [];
  objects.forEach((body, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const off of offsets) out += `${String(off).padStart(10, '0')} 00000 n \n`;
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, 'latin1').toString('base64');
}

function brokenPdfBase64(): string {
  return Buffer.from('%PDF-1.4\nnot really a pdf \x00\x01 garbage', 'latin1').toString('base64');
}

/** Records every message the viewer page receives, so tests can wait for them. */
async function openViewer(page: Page): Promise<void> {
  await page.addInitScript(() => {
    (window as unknown as { __msgs: unknown[] }).__msgs = [];
    window.addEventListener('message', (ev) => {
      (window as unknown as { __msgs: unknown[] }).__msgs.push(ev.data);
    });
  });
  await page.goto('/pdf-viewer.html');
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __msgs: { type?: string }[] }).__msgs.some(
          (m) => m?.type === 'ferrum-pdf:ready',
        ),
      ),
    )
    .toBe(true);
}

test('a small generated PDF is drawn to a canvas with real pixels', async ({ page }) => {
  await openViewer(page);
  await page.evaluate(
    ({ b64 }) => {
      const bin = atob(b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      // Top-level page: window.parent === window, so this is the parent -> viewer message.
      window.postMessage({ type: 'ferrum-pdf:load', data: bytes.buffer }, '*', [bytes.buffer]);
    },
    { b64: smallPdfBase64() },
  );
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __msgs: { type?: string; pages?: number }[] }).__msgs.find(
          (m) => m?.type === 'ferrum-pdf:loaded',
        ),
      ),
    )
    .toMatchObject({ pages: 1 });

  // Pages draw lazily after 'loaded' is reported, so poll until the glyph pixels exist.
  const countDark = () =>
    page.evaluate(() => {
      const canvas = document.querySelector('#pages canvas') as HTMLCanvasElement | null;
      if (!canvas || canvas.width === 0) return 0;
      const data = canvas.getContext('2d')!.getImageData(0, 0, canvas.width, canvas.height).data;
      let dark = 0;
      for (let i = 0; i < data.length; i += 4) if (data[i] < 64 && data[i + 3] > 0) dark++;
      return dark;
    });
  await expect.poll(countDark, { timeout: 15000 }).toBeGreaterThan(20);
  expect(await page.locator('#pages canvas').count()).toBe(1);
  // Links, forms and scripting are off: no clickable anchors or form fields.
  expect(await page.locator('#pages a, #pages input, #pages form').count()).toBe(0);
});

test('a broken PDF shows the fixed error text and reports it, without a crash', async ({
  page,
}) => {
  await openViewer(page);
  await page.evaluate(
    ({ b64 }) => {
      const bin = atob(b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      window.postMessage({ type: 'ferrum-pdf:load', data: bytes.buffer }, '*', [bytes.buffer]);
    },
    { b64: brokenPdfBase64() },
  );
  await expect(page.locator('#status')).toHaveText('This PDF could not be opened.');
  const errMsg = await page.evaluate(() =>
    (window as unknown as { __msgs: { type?: string; message?: string }[] }).__msgs.find(
      (m) => m?.type === 'ferrum-pdf:error',
    ),
  );
  expect(errMsg).toEqual({ type: 'ferrum-pdf:error', message: 'This PDF could not be opened.' });
});

test('a sandbox="allow-scripts" frame is opaque: no Tauri globals, no parent access, no network', async ({
  page,
}) => {
  // Host = the app page: the viewer's own CSP (default-src 'none') would block framing it.
  await page.goto('/');
  const frameHandle = await page.evaluateHandle(() => {
    const f = document.createElement('iframe');
    f.setAttribute('sandbox', 'allow-scripts');
    f.src = '/pdf-viewer.html';
    document.body.append(f);
    return f;
  });
  const frame = await (await frameHandle.asElement()!.contentFrame())!;
  expect(frame.url()).toContain('/pdf-viewer.html');
  await expect.poll(() => frame.evaluate(() => document.readyState === 'complete')).toBe(true);

  const facts = await frame.evaluate(async () => {
    const tauri = window as unknown as Record<string, unknown>;
    let parentDoc = true;
    try {
      void window.parent.document.title;
    } catch {
      parentDoc = false;
    }
    let net = 'allowed';
    try {
      await fetch('/pdf-viewer.html');
    } catch {
      net = 'blocked';
    }
    return {
      origin: window.origin,
      internals: typeof tauri.__TAURI_INTERNALS__,
      global: typeof tauri.__TAURI__,
      parentDoc,
      net,
    };
  });
  expect(facts.origin).toBe('null');
  expect(facts.internals).toBe('undefined');
  expect(facts.global).toBe('undefined');
  expect(facts.parentDoc).toBe(false);
  expect(facts.net).toBe('blocked');
});

test('a parent sends the PDF to the sandboxed frame and the frame draws it', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => {
    const f = document.createElement('iframe');
    f.setAttribute('sandbox', 'allow-scripts');
    f.id = 'viewer';
    f.src = '/pdf-viewer.html';
    document.body.append(f);
    const w = window as unknown as { __frameMsgs: unknown[] };
    w.__frameMsgs = [];
    window.addEventListener('message', (ev) => {
      if (ev.source === f.contentWindow) w.__frameMsgs.push(ev.data);
    });
  });
  const framed = (type: string) =>
    page.evaluate(
      (t) =>
        (window as unknown as { __frameMsgs: { type?: string }[] }).__frameMsgs.some(
          (m) => m?.type === t,
        ),
      type,
    );
  await expect.poll(() => framed('ferrum-pdf:ready')).toBe(true);
  await page.evaluate(
    ({ b64 }) => {
      const f = document.getElementById('viewer') as HTMLIFrameElement;
      const bin = atob(b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      f.contentWindow!.postMessage({ type: 'ferrum-pdf:load', data: bytes.buffer }, '*', [
        bytes.buffer,
      ]);
    },
    { b64: smallPdfBase64() },
  );
  await expect.poll(() => framed('ferrum-pdf:loaded')).toBe(true);
  const frame = page
    .frames()
    .find((f) => f !== page.mainFrame() && f.url().endsWith('/pdf-viewer.html'));
  expect(frame).toBeTruthy();
  expect(await frame!.locator('#pages canvas').count()).toBe(1);
});
