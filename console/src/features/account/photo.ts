/** The largest stored photo, as a `data:` URL: small enough to sit in the 16 KiB of preferences. */
export const MAX_PHOTO_CHARS = 8 * 1024;
/** The largest file the picker accepts before it is shrunk. */
export const MAX_PHOTO_FILE = 5 * 1024 * 1024;

export class PhotoError extends Error {}

function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === 'string') resolve(reader.result);
      else reject(new PhotoError("That file couldn't be read."));
    };
    reader.onerror = () => reject(new PhotoError("That file couldn't be read."));
    reader.readAsDataURL(file);
  });
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new PhotoError("That file isn't an image this browser can open."));
    img.src = src;
  });
}

/**
 * A square crop of the picture's centre, scaled down and compressed until it fits
 * `MAX_PHOTO_CHARS`. The file is read as a `data:` URL because the console's policy allows no
 * `blob:` images.
 */
export async function shrinkPhoto(file: File): Promise<string> {
  if (!file.type.startsWith('image/')) throw new PhotoError('Choose an image: PNG, JPEG, WebP or GIF.');
  if (file.size > MAX_PHOTO_FILE) throw new PhotoError('Choose an image of 5 MB or less.');
  const img = await loadImage(await readAsDataUrl(file));
  const side = Math.min(img.naturalWidth, img.naturalHeight);
  if (side < 1) throw new PhotoError("That file isn't an image this browser can open.");
  const sx = (img.naturalWidth - side) / 2;
  const sy = (img.naturalHeight - side) / 2;
  for (const size of [128, 96, 64]) {
    const canvas = document.createElement('canvas');
    canvas.width = size;
    canvas.height = size;
    const ctx = canvas.getContext('2d');
    if (!ctx) break;
    ctx.imageSmoothingQuality = 'high';
    ctx.drawImage(img, sx, sy, side, side, 0, 0, size, size);
    for (const [type, quality] of [
      ['image/webp', 0.82],
      ['image/jpeg', 0.8],
      ['image/jpeg', 0.6],
    ] as const) {
      const url = canvas.toDataURL(type, quality);
      if (url.startsWith(`data:${type}`) && url.length <= MAX_PHOTO_CHARS) return url;
    }
  }
  throw new PhotoError("That image couldn't be made small enough. Try a simpler picture.");
}
