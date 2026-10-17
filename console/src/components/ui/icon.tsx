import {
  IconActivity,
  IconActivityHeartbeat,
  IconAdjustmentsHorizontal,
  IconAlertCircle,
  IconAlertTriangle,
  IconArrowDown,
  IconArrowRight,
  IconArrowUp,
  IconArrowUpRight,
  IconArrowsSort,
  IconBell,
  IconBook,
  IconChartBar,
  IconCheck,
  IconChevronDown,
  IconChevronLeft,
  IconChevronRight,
  IconChevronUp,
  IconCircleCheck,
  IconCircleCheckFilled,
  IconCircleDashed,
  IconClock,
  IconCode,
  IconCopy,
  IconCube,
  IconCurrencyDollar,
  IconDatabase,
  IconDots,
  IconDownload,
  IconExternalLink,
  IconEye,
  IconEyeOff,
  IconFileText,
  IconFilter,
  IconGripVertical,
  IconHelp,
  IconInfoCircle,
  IconKey,
  IconLink,
  IconList,
  IconLock,
  IconLogout,
  IconMail,
  IconMessages,
  IconMenu2,
  IconMoon,
  IconPencil,
  IconPlayerPause,
  IconPlayerPlay,
  IconPlug,
  IconPlus,
  IconRefresh,
  IconRotateClockwise2,
  IconRoute,
  IconSearch,
  IconSend,
  IconServer,
  IconSettings,
  IconShield,
  IconStar,
  IconStarFilled,
  IconSun,
  IconTerminal2,
  IconTrash,
  IconUsers,
  IconWebhook,
  IconWorld,
  IconX,
  IconBuilding,
  IconPrinter,
  IconDeviceMobile,
  IconMinus,
  IconCalendar,
  IconGitPullRequest,
  IconGitCompare,
  IconHistory,
  IconKeyboard,
  IconLayoutColumns,
  IconUser,
  IconGauge,
  IconTrendingUp,
  IconWallet,
  IconPlugConnectedX,
  IconCoins,
  IconTemplate,
  IconTools,
  IconRobot,
  IconPuzzle,
  IconReceipt,
  IconEraser,
  IconFlask,
  IconArrowsShuffle,
  type Icon as TablerIcon,
} from '@tabler/icons-react';
import type { SVGProps } from 'react';

/**
 * The console's icons, by name. Every glyph comes from Tabler (outline, bundled and tree-shaken);
 * callers use these names, never Tabler's, so the pack can change in one place.
 */
const ICONS = {
  activity: IconActivity,
  list: IconList,
  chart: IconChartBar,
  dollar: IconCurrencyDollar,
  shield: IconShield,
  cube: IconCube,
  users: IconUsers,
  settings: IconSettings,
  alert: IconAlertTriangle,
  alertCircle: IconAlertCircle,
  bell: IconBell,
  key: IconKey,
  cache: IconDatabase,
  route: IconRoute,
  search: IconSearch,
  download: IconDownload,
  play: IconPlayerPlay,
  pause: IconPlayerPause,
  x: IconX,
  menu: IconMenu2,
  sun: IconSun,
  moon: IconMoon,
  filter: IconFilter,
  refresh: IconRefresh,
  copy: IconCopy,
  chevronRight: IconChevronRight,
  chevronDown: IconChevronDown,
  chevronLeft: IconChevronLeft,
  chevronUp: IconChevronUp,
  clock: IconClock,
  edit: IconPencil,
  plus: IconPlus,
  check: IconCheck,
  checkCircle: IconCircleCheck,
  checkCircleFilled: IconCircleCheckFilled,
  circleDashed: IconCircleDashed,
  lock: IconLock,
  globe: IconWorld,
  link: IconLink,
  external: IconExternalLink,
  webhook: IconWebhook,
  eye: IconEye,
  eyeOff: IconEyeOff,
  mail: IconMail,
  logout: IconLogout,
  trash: IconTrash,
  rotate: IconRotateClockwise2,
  info: IconInfoCircle,
  file: IconFileText,
  heart: IconActivityHeartbeat,
  server: IconServer,
  grip: IconGripVertical,
  star: IconStar,
  starFilled: IconStarFilled,
  columns: IconLayoutColumns,
  messages: IconMessages,
  book: IconBook,
  help: IconHelp,
  arrowRight: IconArrowRight,
  arrowUpRight: IconArrowUpRight,
  arrowUp: IconArrowUp,
  arrowDown: IconArrowDown,
  sort: IconArrowsSort,
  plug: IconPlug,
  terminal: IconTerminal2,
  code: IconCode,
  send: IconSend,
  more: IconDots,
  sliders: IconAdjustmentsHorizontal,
  building: IconBuilding,
  printer: IconPrinter,
  phone: IconDeviceMobile,
  minus: IconMinus,
  calendar: IconCalendar,
  pullRequest: IconGitPullRequest,
  compare: IconGitCompare,
  history: IconHistory,
  keyboard: IconKeyboard,
  user: IconUser,
  gauge: IconGauge,
  trendUp: IconTrendingUp,
  wallet: IconWallet,
  plugOff: IconPlugConnectedX,
  coins: IconCoins,
  template: IconTemplate,
  tools: IconTools,
  robot: IconRobot,
  puzzle: IconPuzzle,
  receipt: IconReceipt,
  eraser: IconEraser,
  flask: IconFlask,
  shuffle: IconArrowsShuffle,
} satisfies Record<string, TablerIcon>;

export type IconName = keyof typeof ICONS;

/**
 * The size scale: 12 for dense inline marks (chips, sort arrows), 14 inline with text, 16 in nav
 * and buttons, 20 for empty-state spots. One stroke width everywhere.
 */
export const ICON_SIZES = { xs: 12, sm: 14, md: 16, lg: 20 } as const;
export const ICON_STROKE = 1.75;

export interface IconProps extends Omit<SVGProps<SVGSVGElement>, 'name' | 'ref' | 'stroke'> {
  name: IconName;
  size?: number;
  /** A label makes the icon meaningful to assistive technology; without one it is decorative. */
  label?: string;
}

export function Icon({ name, size = ICON_SIZES.sm, label, ...rest }: IconProps) {
  const Glyph = ICONS[name];
  return (
    <Glyph
      size={size}
      stroke={ICON_STROKE}
      aria-hidden={label ? undefined : true}
      role={label ? 'img' : undefined}
      aria-label={label}
      focusable="false"
      {...rest}
    />
  );
}

/** The brand mark from the mockups: a solid triangle (drawn here, not from the icon pack). */
export function BrandMark({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={(size * 20) / 23} viewBox="0 0 23 20" aria-hidden="true" focusable="false">
      <path d="M11.5 0L23 20H0L11.5 0Z" fill="currentColor" />
    </svg>
  );
}
