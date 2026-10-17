import type { Metadata } from 'next';

import { TeamsUsersScreen } from '@/features/teams-users/components/teams-users-screen';

export const metadata: Metadata = { title: 'Teams & users' };

export default function Page() {
  return <TeamsUsersScreen />;
}
