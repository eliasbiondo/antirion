import localFont from 'next/font/local';

/** Console fonts bundled into the build and served by the gateway. */
export const hankenGrotesk = localFont({
  src: [
    {
      path: '../../node_modules/@fontsource-variable/hanken-grotesk/files/hanken-grotesk-latin-wght-normal.woff2',
      weight: '100 900',
    },
    {
      path: '../../node_modules/@fontsource-variable/hanken-grotesk/files/hanken-grotesk-latin-ext-wght-normal.woff2',
      weight: '100 900',
    },
  ],
  variable: '--font-hanken',
  display: 'swap',
});

export const jetbrainsMono = localFont({
  src: [
    {
      path: '../../node_modules/@fontsource-variable/jetbrains-mono/files/jetbrains-mono-latin-wght-normal.woff2',
      weight: '100 800',
    },
    {
      path: '../../node_modules/@fontsource-variable/jetbrains-mono/files/jetbrains-mono-latin-ext-wght-normal.woff2',
      weight: '100 800',
    },
  ],
  variable: '--font-jetbrains',
  display: 'swap',
});

/** Vazir v30.1.0 from rastikerdar/vazirmatn, distributed under the accompanying SIL OFL. */
export const vazir = localFont({
  src: './fonts/Vazir-Variable.woff2',
  weight: '100 900',
  variable: '--font-vazir',
  display: 'swap',
  preload: false,
  adjustFontFallback: false,
});
