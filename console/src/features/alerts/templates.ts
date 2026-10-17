import type { IconName } from '@/components/ui/icon';

import type { RuleSpec } from './api';

export interface RuleTemplate {
  id: string;
  title: string;
  /** One line on what it catches. */
  summary: string;
  icon: IconName;
  /** The rule name it suggests. */
  name: string;
  spec: RuleSpec;
  /** A scope dimension the person should fill ("provider" for a per-provider rule). */
  asks?: 'provider' | 'team' | 'model';
}

/**
 * The built-in rule templates. They live in the console: the gateway ships no template operation,
 * and none is on until it is saved as a rule.
 */
export const RULE_TEMPLATES: RuleTemplate[] = [
  {
    id: 'provider-errors',
    title: 'Provider error rate',
    summary:
      "Server errors from one provider above 5% for 5 minutes. The circuit breaker's failover shows up in the alert's activity.",
    icon: 'server',
    name: 'provider-errors',
    asks: 'provider',
    spec: {
      kind: 'metric',
      category: 'errors',
      severity: 'critical',
      metric: 'upstream_5xx_rate',
      operator: 'gt',
      threshold: 0.05,
      window: '5m',
      cooldown: '10m',
    },
  },
  {
    id: 'error-rate',
    title: 'Error rate',
    summary: 'More than 2% of requests fail over 5 minutes.',
    icon: 'alert',
    name: 'error-rate',
    spec: {
      kind: 'metric',
      category: 'errors',
      severity: 'critical',
      metric: 'error_rate',
      operator: 'gt',
      threshold: 0.02,
      window: '5m',
      cooldown: '10m',
    },
  },
  {
    id: 'latency-baseline',
    title: 'p95 latency against baseline',
    summary: 'p95 latency three times its usual level for this hour of the week.',
    icon: 'clock',
    name: 'latency-baseline',
    spec: {
      kind: 'metric',
      category: 'latency',
      severity: 'critical',
      metric: 'latency_baseline_ratio',
      operator: 'gt',
      threshold: 3,
      window: '5m',
      cooldown: '15m',
    },
  },
  {
    id: 'capacity',
    title: 'Capacity at 95%',
    summary: 'Requests per minute at 95% of the tightest limit that applies.',
    icon: 'gauge',
    name: 'capacity-95',
    spec: {
      kind: 'metric',
      category: 'capacity',
      severity: 'warning',
      metric: 'rpm_utilization',
      operator: 'gte',
      threshold: 0.95,
      window: '5m',
      cooldown: '10m',
    },
  },
  {
    id: 'budget-forecast',
    title: 'Projected budget overage',
    summary: 'A budget is on course to end its period over the amount.',
    icon: 'trendUp',
    name: 'budget-forecast',
    spec: {
      kind: 'metric',
      category: 'budget',
      severity: 'warning',
      metric: 'budget_projection_ratio',
      operator: 'gt',
      threshold: 1,
      window: '1h',
      cooldown: '6h',
    },
  },
  {
    id: 'budget-threshold',
    title: 'Budget threshold',
    summary: "Spend crosses one of a budget's alert percentages.",
    icon: 'wallet',
    name: 'budget-threshold',
    spec: {
      kind: 'event',
      category: 'budget',
      severity: 'warning',
      events: ['budget.threshold_crossed'],
      cooldown: '0s',
    },
  },
  {
    id: 'provider-down',
    title: 'Provider down',
    summary: 'A deployment goes down or its circuit breaker opens.',
    icon: 'plugOff',
    name: 'provider-down',
    spec: {
      kind: 'event',
      category: 'availability',
      severity: 'critical',
      events: ['deployment.state_changed'],
      cooldown: '0s',
    },
  },
  {
    id: 'pii-flags',
    title: 'PII flags',
    summary: 'More than 10 guardrail events in 15 minutes, filed as PII.',
    icon: 'eyeOff',
    name: 'pii-flags',
    spec: {
      kind: 'metric',
      category: 'pii',
      severity: 'warning',
      metric: 'guardrail_events',
      operator: 'gt',
      threshold: 10,
      window: '15m',
      cooldown: '15m',
    },
  },
  {
    id: 'safety-flags',
    title: 'Safety flags',
    summary: 'A spike of guardrail events: more than 5 in 5 minutes.',
    icon: 'shield',
    name: 'safety-flags',
    spec: {
      kind: 'metric',
      category: 'safety',
      severity: 'critical',
      metric: 'guardrail_events',
      operator: 'gt',
      threshold: 5,
      window: '5m',
      cooldown: '5m',
    },
  },
  {
    id: 'cache-drop',
    title: 'Cache hit-rate drop',
    summary: 'The cache answers fewer than 20% of requests over an hour.',
    icon: 'cache',
    name: 'cache-drop',
    spec: {
      kind: 'metric',
      category: 'cache',
      severity: 'info',
      metric: 'cache_hit_rate',
      operator: 'lt',
      threshold: 0.2,
      window: '1h',
      cooldown: '1h',
    },
  },
  {
    id: 'cost-baseline',
    title: 'Cost per request at 2×',
    summary: 'Cost per request twice its usual level for this hour of the week.',
    icon: 'coins',
    name: 'cost-baseline',
    spec: {
      kind: 'metric',
      category: 'cost',
      severity: 'warning',
      metric: 'cost_baseline_ratio',
      operator: 'gte',
      threshold: 2,
      window: '15m',
      cooldown: '1h',
    },
  },
];
