import { chromium, type FullConfig } from '@playwright/test';

import { signIn, STORAGE } from './fixtures';

/** Signs in once with the admin token; every test reuses the session cookie. */
export default async function globalSetup(config: FullConfig): Promise<void> {
  const baseURL = config.projects[0]?.use.baseURL;
  const browser = await chromium.launch();
  const page = await browser.newPage({ baseURL });
  await signIn(page);
  await page.context().storageState({ path: STORAGE });
  await browser.close();
}
