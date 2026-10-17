import type { Metadata } from 'next';

import { BudgetsScreen } from '@/features/budgets/components/budgets-screen';

export const metadata: Metadata = { title: 'Budgets' };

export default function Page() {
  return <BudgetsScreen />;
}
