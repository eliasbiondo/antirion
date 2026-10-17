import localFont from 'next/font/local';

/** Hanken Grotesk and JetBrains Mono, bundled into the build (no font is fetched at run time). */
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
