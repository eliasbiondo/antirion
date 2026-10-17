/**
 * The console's English strings, the reference catalog: a translation is a file of the same shape
 * (`Messages`) listed in `LOCALES`. Only the account dialog reads its strings from here so far;
 * screens move over as they are translated.
 */
export const en = {
  account: {
    title: 'Account',
    sectionsLabel: 'Account sections',
    unsaved: 'Unsaved changes',
    sections: {
      profile: 'Profile',
      preferences: 'Preferences',
      security: 'Security',
      tokens: 'Personal tokens',
      notifications: 'Notifications',
    },
  },
  language: {
    label: 'Language',
    browser: 'Same as this browser',
    hint: 'The console is in English for now. Other languages appear here as they are translated.',
  },
};

export type Messages = typeof en;
