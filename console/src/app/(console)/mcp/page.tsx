import type { Metadata } from 'next';

import { McpScreen } from '@/features/mcp/components/mcp-screen';

export const metadata: Metadata = { title: 'MCP' };

export default function Page() {
  return <McpScreen />;
}
