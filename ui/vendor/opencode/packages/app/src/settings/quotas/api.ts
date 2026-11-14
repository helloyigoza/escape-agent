import { useQuery, useQueryClient } from "@tanstack/solid-query"
import { useServerSDK } from "@/runtime/server/client"
import { isConfigObject, serverMessage, useAgenticRequest } from "@/settings/agentic/config"

export type QuotaCapability = "live_api" | "rate_limit_headers_only"

/** Where the OpenRouter management key the server uses comes from. */
export type ManagementKeySource = "env" | "credential" | "config"

export interface UnifiedQuotaSummary {
  providerCount: number
  liveQuotaCount: number
  spend5hUsd: number
  spend24hUsd: number
  spend7dUsd: number
  spend30dUsd: number
}

export interface QuotaWindow {
  id: string
  label: string
  unit: string
  groupKey?: string
  limit?: number
  used?: number
  remaining?: number
  usedPercent?: number
  remainingPercent?: number
  limited?: boolean
  resetAtMs?: number
  resetLabel?: string
}

export interface ProviderBalanceSummary {
  currency: string
  total?: number
  remaining?: number
  used?: number
  toppedUp?: number
  granted?: number
  isAvailable?: boolean
  isFreeTier?: boolean
}

export interface ProviderApiSpend {
  currency: string
  total?: number
  daily?: number
  weekly?: number
  monthly?: number
}

export interface ProviderRateLimit {
  providerId: string
  modelId: string
  observedAtMs: number
  status: number
  requestsLimit?: number
  requestsRemaining?: number
  requestsResetAtMs?: number
  requestsResetRaw?: string
  tokensLimit?: number
  tokensRemaining?: number
  tokensResetAtMs?: number
  tokensResetRaw?: string
  retryAfterMs?: number
}

export interface AntigravityQuotaGroupSnapshot {
  remainingFraction?: number
  usedPercent?: number
  resetTimeMs?: number
}

export interface AntigravityCachedQuota {
  updatedAt: number
  groups: Record<string, AntigravityQuotaGroupSnapshot>
}

export interface AntigravityAccountStatus {
  index: number
  id: string
  email?: string
  label?: string
  activeClaude: boolean
  activeGemini: boolean
  enabled: boolean
  verificationRequired: boolean
  revoked: boolean
  projectId?: string
  tier?: string
  quota?: AntigravityCachedQuota
  addedAt: number
  lastUsed: number
}

export interface UsageWindowTotals {
  steps: number
  inputTokens: number
  outputTokens: number
  reasoningTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
  totalTokens: number
  costUsd: number
}

export interface ModelUsageRow {
  modelID: string
  steps: number
  totalTokens: number
  costUsd: number
}

export interface ProviderLocalUsage {
  fiveHours: UsageWindowTotals
  day: UsageWindowTotals
  week: UsageWindowTotals
  month: UsageWindowTotals
  allTime: UsageWindowTotals
  topModels?: ModelUsageRow[]
}

export interface ProviderQuotaEntry {
  providerID: string
  name: string
  connected: boolean
  connectionSource: string
  capability: QuotaCapability
  capabilityNote?: string
  status: string
  fetchedAtMs?: number
  cached?: boolean
  hasManagementKey?: boolean
  managementKeySource?: ManagementKeySource
  keyLabel?: string
  rateLimitRule?: string
  windows?: QuotaWindow[]
  balances?: ProviderBalanceSummary[]
  spendSummary?: ProviderApiSpend
  rateLimit?: ProviderRateLimit
  accounts?: AntigravityAccountStatus[]
  localUsage?: ProviderLocalUsage
  warnings?: string[]
  error?: string
}

export interface UnifiedQuotasResponse {
  fetchedAtMs: number
  summary: UnifiedQuotaSummary
  providers: ProviderQuotaEntry[]
}

/** The server answers `503` when it runs without the quota service. */
export interface QuotasUnavailable {
  unavailable: true
  message?: string
}

export type QuotasSnapshot = UnifiedQuotasResponse | QuotasUnavailable

/** The unified payload, or `undefined` while pending or when the service is detached. */
export function quotasData(value: QuotasSnapshot | undefined): UnifiedQuotasResponse | undefined {
  return value && !("unavailable" in value) ? value : undefined
}

export function quotasUnavailable(value: QuotasSnapshot | undefined): QuotasUnavailable | undefined {
  return value && "unavailable" in value ? value : undefined
}

/** A failed quota request, with the server's error kind when it names one. */
export class QuotaRequestError extends Error {
  readonly status: number
  readonly kind: string | undefined

  constructor(status: number, message: string, kind?: string) {
    super(message)
    this.name = "QuotaRequestError"
    this.status = status
    this.kind = kind
  }
}

/** The server's status vocabulary for a provider entry. */
export const QUOTA_STATUSES = [
  "ok",
  "partial",
  "error",
  "unsupported",
  "headers_only",
  "local_only",
  "no_api_key",
  "not_connected",
  "disconnected",
  "unavailable",
] as const

export type QuotaStatusLabelKey =
  | "settings.quotas.status.ok"
  | "settings.quotas.status.partial"
  | "settings.quotas.status.error"
  | "settings.quotas.status.unsupported"
  | "settings.quotas.status.headersOnly"
  | "settings.quotas.status.localOnly"
  | "settings.quotas.status.noApiKey"
  | "settings.quotas.status.notConnected"
  | "settings.quotas.status.disconnected"
  | "settings.quotas.status.unavailable"

/** Every server status maps to a localized label; unknown tokens never reach the UI. */
export function statusLabelKey(status: string): QuotaStatusLabelKey {
  switch (status) {
    case "ok":
      return "settings.quotas.status.ok"
    case "partial":
      return "settings.quotas.status.partial"
    case "error":
      return "settings.quotas.status.error"
    case "unsupported":
      return "settings.quotas.status.unsupported"
    case "headers_only":
      return "settings.quotas.status.headersOnly"
    case "local_only":
      return "settings.quotas.status.localOnly"
    case "no_api_key":
      return "settings.quotas.status.noApiKey"
    case "not_connected":
      return "settings.quotas.status.notConnected"
    case "disconnected":
      return "settings.quotas.status.disconnected"
    default:
      return "settings.quotas.status.unavailable"
  }
}

const QUOTAS_QUERY_KEY = ["quotas", "unified"] as const

export interface UseQuotasDataOptions {
  /** Reads reactively; a detached dialog passes `() => false` until it needs the data. */
  enabled?: () => boolean
  /** Poll interval in ms. Only the settings panel polls; the dialog reads the cached snapshot. */
  refetchInterval?: number
}

/** Reads the error kind out of a failed response without consuming the body twice. */
async function requestError(response: Response): Promise<QuotaRequestError> {
  const copy = response.clone()
  const message = await serverMessage(response)
  let kind: string | undefined

  try {
    const body: unknown = await copy.json()
    if (isConfigObject(body)) {
      const data = body.data
      if (isConfigObject(data) && typeof data.kind === "string") kind = data.kind
      else if (typeof body.kind === "string") kind = body.kind
    }
  } catch {
    // Not JSON.
  }

  return new QuotaRequestError(response.status, message, kind)
}

export function useQuotasData(options: UseQuotasDataOptions = {}) {
  const client = useQueryClient()
  const sdk = useServerSDK()
  const request = useAgenticRequest()

  const query = useQuery<QuotasSnapshot>(() => ({
    queryKey: [sdk.scope, ...QUOTAS_QUERY_KEY],
    queryFn: async (): Promise<QuotasSnapshot> => {
      const response = await request("/api/quotas")
      // A server without the quota service is a soft state, not an error.
      if (response.status === 503) {
        return { unavailable: true, message: await serverMessage(response) }
      }
      if (!response.ok) throw await requestError(response)
      const payload = (await response.json()) as { data?: UnifiedQuotasResponse }
      if (!payload.data) throw new Error("Missing quotas response data")
      return payload.data
    },
    staleTime: 30_000,
    enabled: options.enabled?.() ?? true,
    retry: false,
    refetchInterval: options.refetchInterval,
  }))

  const refreshLive = async () => {
    const response = await request("/api/quotas/refresh", { method: "POST" })
    if (!response.ok) throw await requestError(response)
    const payload = (await response.json()) as { data?: UnifiedQuotasResponse }
    if (payload.data) {
      client.setQueryData([sdk.scope, ...QUOTAS_QUERY_KEY], payload.data)
    } else {
      await client.invalidateQueries({ queryKey: [sdk.scope, ...QUOTAS_QUERY_KEY] })
    }
    return payload.data
  }

  const invalidate = () => client.invalidateQueries({ queryKey: [sdk.scope, ...QUOTAS_QUERY_KEY] })

  const saveOpenRouterManagementKey = async (key: string) => {
    const response = await request("/api/quotas/openrouter/management-key", {
      method: "PUT",
      body: JSON.stringify({ key }),
    })
    if (!response.ok) throw await requestError(response)
    // One invalidation re-reads the cache; a live probe of every provider is reserved for Refresh.
    await invalidate()
  }

  const clearOpenRouterManagementKey = async () => {
    const response = await request("/api/quotas/openrouter/management-key", {
      method: "DELETE",
    })
    if (!response.ok) throw await requestError(response)
    await invalidate()
  }

  const mutateAntigravityAccount = async (id: string, action: "select" | "enable" | "disable") => {
    const response = await request(`/api/antigravity/accounts/${encodeURIComponent(id)}/${action}`, {
      method: "POST",
    })
    if (!response.ok) throw await requestError(response)
    await invalidate()
  }

  return {
    query,
    refreshLive,
    invalidate,
    saveOpenRouterManagementKey,
    clearOpenRouterManagementKey,
    mutateAntigravityAccount,
  }
}

export function formatCurrencyValue(value: number, currency = "USD", locale?: string): string {
  const code = /^[A-Z]{3}$/.test(currency.toUpperCase()) ? currency.toUpperCase() : "USD"
  return new Intl.NumberFormat(locale, {
    style: "currency",
    currency: code,
    minimumFractionDigits: Math.abs(value) > 0 && Math.abs(value) < 0.01 ? 4 : 2,
    maximumFractionDigits: Math.abs(value) > 0 && Math.abs(value) < 0.01 ? 4 : 2,
  }).format(value)
}

export function formatCompactNumber(value: number, locale?: string): string {
  if (Math.abs(value) >= 1_000_000) {
    return `${(value / 1_000_000).toFixed(2)}M`
  }
  if (Math.abs(value) >= 1_000) {
    return `${(value / 1_000).toFixed(1)}K`
  }
  return new Intl.NumberFormat(locale).format(value)
}

export function formatCountdownDuration(targetMs: number, nowMs = Date.now()): string | undefined {
  const diff = targetMs - nowMs
  if (!Number.isFinite(diff) || diff <= 0) return undefined
  const totalMinutes = Math.ceil(diff / 60_000)
  const days = Math.floor(totalMinutes / (60 * 24))
  const hours = Math.floor((totalMinutes % (60 * 24)) / 60)
  const mins = totalMinutes % 60
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${mins}m`
  return `${mins}m`
}

export function formatQuotaWindowValue(window: QuotaWindow, value: number, locale?: string): string {
  if (window.unit === "percent") return `${Math.round(value)}%`
  if (/^[A-Z]{3}$/.test(window.unit.toUpperCase())) {
    return formatCurrencyValue(value, window.unit, locale)
  }
  return `${formatCompactNumber(value, locale)} ${window.unit}`
}

export function antigravityPoolRemaining(group: AntigravityQuotaGroupSnapshot): number | undefined {
  if (typeof group.remainingFraction === "number") {
    const pct = group.remainingFraction <= 1 ? group.remainingFraction * 100 : group.remainingFraction
    return Math.max(0, Math.min(100, pct))
  }
  if (typeof group.usedPercent === "number") {
    return Math.max(0, Math.min(100, 100 - group.usedPercent))
  }
  return undefined
}
