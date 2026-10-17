import type { IconName } from '@/components/ui/icon';
import type { Favourite, FavouriteKind } from '@/stores/preferences';

import { activeNav, navItem } from './nav';

const KIND: Record<
  Exclude<FavouriteKind, 'page'>,
  { label: string; icon: IconName; path: string; param: string }
> = {
  model: { label: 'Model', icon: 'cube', path: '/models/', param: 'model' },
  key: { label: 'API key', icon: 'key', path: '/keys/', param: 'key' },
  route: { label: 'Route', icon: 'route', path: '/routing/', param: 'route' },
  team: { label: 'Team', icon: 'users', path: '/teams/', param: 'team' },
  user: { label: 'Person', icon: 'user', path: '/teams/', param: 'user' },
  mcp_server: { label: 'MCP server', icon: 'tools', path: '/mcp/', param: 'server' },
  agent: { label: 'Agent', icon: 'robot', path: '/agents/', param: 'agent' },
  plugin: { label: 'Plugin', icon: 'puzzle', path: '/plugins/', param: 'plugin' },
};

export interface FavouriteLink {
  label: string;
  /** "API key", "Page". */
  kind: string;
  icon: IconName;
  path: string;
  query: Record<string, string>;
}

/** Where a favourite leads, how it is named and drawn. */
export function favouriteLink(f: Favourite): FavouriteLink {
  if (f.kind === 'page') {
    const item = navItem(activeNav(f.name));
    return {
      label: item?.label ?? f.name,
      kind: 'Page',
      icon: item?.icon ?? 'file',
      path: f.name,
      query: item?.query ?? {},
    };
  }
  const k = KIND[f.kind];
  return {
    label: f.name,
    kind: k.label,
    icon: k.icon,
    path: k.path,
    query: f.kind === 'team' ? { tab: 'teams', team: f.name } : { [k.param]: f.name },
  };
}
