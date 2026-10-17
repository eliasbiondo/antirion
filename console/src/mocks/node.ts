import { setupServer } from 'msw/node';

import { contractHandlers, identityHandlers } from './handlers';

/** The test server; tests add handlers for the live operations they exercise. */
export const server = setupServer(...contractHandlers, ...identityHandlers);
