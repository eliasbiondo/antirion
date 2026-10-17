import type { components } from './schema';

/** Short names for the contract's schemas the console uses. */
type S = components['schemas'];

export type Me = S['Me'];
export type LoginOptions = S['LoginOptions'];
export type RoleBinding = S['RoleBinding'];
export type Role = S['Role'];
export type ResourceMetadata = S['ResourceMetadata'];
export type AdminError = S['AdminError'];

export type ModelResource = S['ModelResource'];
export type ModelSpec = ModelResource['spec'];
export type DeploymentResource = S['DeploymentResource'];
export type DeploymentSpec = DeploymentResource['spec'];
export type ProviderResource = S['ProviderResource'];
export type ProviderType = S['ProviderType'];
export type RouteResource = S['RouteResource'];
export type Tier = S['Tier'];
export type EffectivePrice = S['EffectivePrice'];

export type KeyResource = S['KeyResource'];
export type KeySpec = KeyResource['spec'];
export type KeyMetadata = S['KeyMetadata'];
export type KeyCreated = S['KeyCreated'];
export type KeyRotated = S['KeyRotated'];

export type OrganizationResource = S['OrganizationResource'];
export type TeamResource = S['TeamResource'];
export type TeamSpec = TeamResource['spec'];
export type ProjectResource = S['ProjectResource'];
export type UserResource = S['UserResource'];
export type UserSpec = UserResource['spec'];
export type Invitation = S['Invitation'];
export type AdminToken = S['AdminToken'];
export type AdminTokenCreated = S['AdminTokenCreated'];

export type BudgetCounter = S['BudgetCounter'];
export type BudgetEvent = S['BudgetEvent'];
export type OverageRequest = S['OverageRequest'];
export type SpendSummaryRow = S['SpendSummaryRow'];

export type CacheSettings = S['CacheSettings'];
export type CacheStatus = S['CacheStatus'];
export type CacheEntry = S['CacheEntry'];
export type InvalidationRuleResource = S['InvalidationRuleResource'];

export type LogRecord = S['LogRecord'];
export type LogList = S['LogList'];
export type LogView = S['LogView'];

export type AuditEntry = S['AuditEntry'];
export type Health = S['Health'];
export type Status = S['Status'];
export type DeploymentHealth = S['DeploymentHealth'];
export type DiffResult = S['DiffResult'];
export type ApplyResult = S['ApplyResult'];
export type ValidateResult = S['ValidateResult'];
export type ConfigDocument = S['ConfigDocument'];

export type Alert = S['Alert'];
export type AlertRuleResource = S['AlertRuleResource'];
export type AlertChannelResource = S['AlertChannelResource'];
export type AlertTimelineEntry = S['AlertTimelineEntry'];

export type UsageAnalytics = S['UsageAnalytics'];
export type UsageBucket = S['UsageBucket'];
export type LatencyHistogram = S['LatencyHistogram'];

export type SafetyEvent = S['SafetyEvent'];
export type GuardrailResource = S['GuardrailResource'];
export type GuardrailSummary = S['GuardrailSummary'];

export type ChangeRequest = S['ChangeRequest'];
export type Session = S['Session'];
export type PersonalToken = S['PersonalToken'];
export type PersonalTokenCreated = S['PersonalTokenCreated'];
export type Preferences = S['Preferences'];
export type MfaFactor = S['MfaFactor'];
export type ScimStatus = S['ScimStatus'];
export type Anomaly = S['Anomaly'];
