/**
 * The console's English strings, the reference catalog: a translation is a file of the same shape
 * (`Messages`).
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
    hint: 'Choose the language of the console.',
  },
};

export type Messages = typeof en;
