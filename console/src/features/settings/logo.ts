/** The largest logo the gateway keeps inline: 64 KiB of image, as a `data:` URL. */
export const LOGO_MAX_BYTES = 64 * 1024;

const TYPES = ['image/png', 'image/jpeg', 'image/webp'];

export class LogoError extends Error {}

function readAsDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(typeof reader.result === 'string' ? reader.result : '');
    reader.onerror = () => reject(new LogoError("That file couldn't be read. Try another one."));
    reader.readAsDataURL(blob);
  });
}

/** The size in bytes of the image a base64 `data:` URL holds. */
export function dataUrlBytes(url: string): number {
  const comma = url.indexOf(',');
  if (comma < 0) return 0;
  const b64 = url.slice(comma + 1);
  const padding = b64.endsWith('==') ? 2 : b64.endsWith('=') ? 1 : 0;
  return Math.floor((b64.length * 3) / 4) - padding;
}

function loadImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new LogoError("That image couldn't be opened. Try a PNG, JPEG or WebP file."));
    img.src = url;
  });
}

/**
 * A logo file as the organization keeps it: a PNG, JPEG or WebP `data:` URL of at most 64 KiB. A
 * larger image is drawn smaller (at most 512 px on its long side, then smaller still) as WebP until
 * it fits.
 */
export async function logoDataUrl(file: File): Promise<string> {
  if (!TYPES.includes(file.type) && !file.type.startsWith('image/'))
    throw new LogoError('Choose a PNG, JPEG or WebP image.');
  const original = await readAsDataUrl(file);
  if (TYPES.includes(file.type) && file.size <= LOGO_MAX_BYTES) return original;
  const img = await loadImage(original);
  for (const side of [512, 384, 256, 192, 128, 96]) {
    const scale = Math.min(1, side / Math.max(img.naturalWidth, img.naturalHeight, 1));
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.round(img.naturalWidth * scale));
    canvas.height = Math.max(1, Math.round(img.naturalHeight * scale));
    const ctx = canvas.getContext('2d');
    if (!ctx) break;
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
    for (const quality of [0.9, 0.75, 0.6]) {
      const url = canvas.toDataURL('image/webp', quality);
      if (url.startsWith('data:image/webp') && dataUrlBytes(url) <= LOGO_MAX_BYTES) return url;
      const png = canvas.toDataURL('image/png');
      if (dataUrlBytes(png) <= LOGO_MAX_BYTES) return png;
    }
  }
  throw new LogoError('That image is too large even when shrunk. Try a smaller or simpler one.');
}
