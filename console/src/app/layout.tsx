import type { Metadata, Viewport } from 'next';
import type { ReactNode } from 'react';

import { DevMocks } from '@/components/layout/dev-mocks';
import { hankenGrotesk, jetbrainsMono } from '@/styles/fonts';

import '@/styles/globals.css';

const UI_PATH = process.env.NEXT_PUBLIC_UI_PATH ?? '';

export const metadata: Metadata = {
  title: { default: 'Antirion', template: '%s · Antirion' },
  description: 'Antirion gateway console',
  robots: { index: false, follow: false },
  icons: { icon: `${UI_PATH}/icon.svg` },
};

export const viewport: Viewport = {
  colorScheme: 'light dark',
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html
      lang="en"
      className={`${hankenGrotesk.variable} ${jetbrainsMono.variable}`}
      suppressHydrationWarning
    >
      <head>
        {/* Applies the stored theme before first paint; a file, since the CSP forbids inline scripts. */}
        <script src={`${UI_PATH}/theme-init.js`} />
      </head>
      <body>
        <DevMocks>{children}</DevMocks>
      </body>
    </html>
  );
}
