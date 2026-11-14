import { beforeEach, describe, expect, mock, test } from "bun:test"

type QueryOptions = {
  enabled?: boolean
  refetchInterval?: number
  staleTime?: number
  retry?: boolean
  queryKey?: unknown[]
}

// `api.ts` imports the query layer and the server client at module scope, so the test replaces only
// those two edges; every formatter, the status vocabulary and the hook wiring stay real.
let lastOptions: QueryOptions | undefined
const invalidated: unknown[] = []

mock.module("@tanstack/solid-query", () => ({
  useQuery: (config: unknown) => {
    lastOptions = typeof config === "function" ? (config as () => QueryOptions)() : (config as QueryOptions)
    return { data: undefined, isPending: true, isError: false }
  },
  useQueryClient: () => ({
    invalidateQueries: async (input: unknown) => {
      invalidated.push(input)
    },
    setQueryData: () => undefined,
  }),
}))

mock.module("@/runtime/server/client", () => ({
  useServerSDK: () => ({ scope: "test", http: { url: "http://server.test" } }),
}))

const {
  QUOTA_STATUSES,
  QuotaRequestError,
  antigravityPoolRemaining,
  formatCompactNumber,
  formatCountdownDuration,
  formatCurrencyValue,
  formatQuotaWindowValue,
  quotasData,
  quotasUnavailable,
  statusLabelKey,
  useQuotasData,
} = await import("./api")

const { dict: english } = await import("@/runtime/i18n/en")
const { dict: turkish } = await import("@/runtime/i18n/tr")

beforeEach(() => {
  lastOptions = undefined
  invalidated.length = 0
})

describe("formatCurrencyValue", () => {
  test("formats two decimals for ordinary amounts", () => {
    expect(formatCurrencyValue(12.5, "USD", "en-US")).toBe("$12.50")
    expect(formatCurrencyValue(0, "USD", "en-US")).toBe("$0.00")
  })

  test("keeps four decimals for sub-cent amounts so a tiny spend is not shown as zero", () => {
    expect(formatCurrencyValue(0.0042, "USD", "en-US")).toBe("$0.0042")
    expect(formatCurrencyValue(-0.0042, "USD", "en-US")).toBe("-$0.0042")
  })

  test("accepts a lowercase ISO code and falls back to USD for an unknown one", () => {
    expect(formatCurrencyValue(3, "eur", "en-US")).toBe("€3.00")
    expect(formatCurrencyValue(3, "euros", "en-US")).toBe("$3.00")
    expect(formatCurrencyValue(3, "", "en-US")).toBe("$3.00")
  })
})

describe("formatCompactNumber", () => {
  test("leaves numbers below a thousand alone", () => {
    expect(formatCompactNumber(0, "en-US")).toBe("0")
    expect(formatCompactNumber(999, "en-US")).toBe("999")
    expect(formatCompactNumber(-42, "en-US")).toBe("-42")
  })

  test("uses one decimal for thousands and two for millions", () => {
    expect(formatCompactNumber(1_000, "en-US")).toBe("1.0K")
    expect(formatCompactNumber(1_540, "en-US")).toBe("1.5K")
    expect(formatCompactNumber(1_500_000, "en-US")).toBe("1.50M")
    expect(formatCompactNumber(-2_500, "en-US")).toBe("-2.5K")
  })
})

describe("formatCountdownDuration", () => {
  const now = 1_700_000_000_000

  test("returns nothing for a window that already reset or has no time", () => {
    expect(formatCountdownDuration(now, now)).toBeUndefined()
    expect(formatCountdownDuration(now - 1, now)).toBeUndefined()
    expect(formatCountdownDuration(Number.NaN, now)).toBeUndefined()
    expect(formatCountdownDuration(Number.POSITIVE_INFINITY, now)).toBeUndefined()
  })

  test("rounds up to whole minutes, then hours, then days", () => {
    expect(formatCountdownDuration(now + 30_000, now)).toBe("1m")
    expect(formatCountdownDuration(now + 59 * 60_000, now)).toBe("59m")
    expect(formatCountdownDuration(now + 60 * 60_000, now)).toBe("1h 0m")
    expect(formatCountdownDuration(now + (2 * 24 * 60 + 3 * 60) * 60_000, now)).toBe("2d 3h")
  })
})

describe("formatQuotaWindowValue", () => {
  test("prints a percent window without a unit suffix", () => {
    expect(formatQuotaWindowValue({ id: "w", label: "Context", unit: "percent" }, 42.4, "en-US")).toBe("42%")
  })

  test("treats a currency unit as money and anything else as a compact count", () => {
    expect(formatQuotaWindowValue({ id: "w", label: "Spend", unit: "usd" }, 12.5, "en-US")).toBe("$12.50")
    expect(formatQuotaWindowValue({ id: "w", label: "Tokens", unit: "tokens" }, 1_540, "en-US")).toBe("1.5K tokens")
  })
})

describe("antigravityPoolRemaining", () => {
  test("reads a fraction as a percentage and a whole number as one", () => {
    expect(antigravityPoolRemaining({ remainingFraction: 0.42 })).toBeCloseTo(42)
    expect(antigravityPoolRemaining({ remainingFraction: 42 })).toBe(42)
  })

  test("derives the remainder from usedPercent and clamps to 0..100", () => {
    expect(antigravityPoolRemaining({ usedPercent: 30 })).toBe(70)
    // A value above 1 is already a percentage, not a fraction: 1.4 stays 1.4%.
    expect(antigravityPoolRemaining({ remainingFraction: 1.4 })).toBe(1.4)
    expect(antigravityPoolRemaining({ remainingFraction: -0.5 })).toBe(0)
    expect(antigravityPoolRemaining({ usedPercent: 130 })).toBe(0)
    expect(antigravityPoolRemaining({ usedPercent: -20 })).toBe(100)
  })

  test("reports nothing when the snapshot carries no number", () => {
    expect(antigravityPoolRemaining({})).toBeUndefined()
  })
})

describe("statusLabelKey", () => {
  test("every server status maps to a localized, non-empty label in both shipped locales", () => {
    for (const status of QUOTA_STATUSES) {
      const key = statusLabelKey(status)
      expect(key.startsWith("settings.quotas.status.")).toBe(true)
      expect((english[key] ?? "").trim().length).toBeGreaterThan(0)
      expect((turkish[key] ?? "").trim().length).toBeGreaterThan(0)
    }
  })

  test("an unknown token never reaches the UI as raw server text", () => {
    expect(statusLabelKey("vendor_specific_thing" as never)).toBe("settings.quotas.status.unavailable")
    expect(english[statusLabelKey("something_new")]).toBe("Unavailable")
  })
})

describe("quotasData / quotasUnavailable", () => {
  test("splits the detached snapshot from a real payload", () => {
    const payload = {
      fetchedAtMs: 1,
      summary: {
        providerCount: 1,
        liveQuotaCount: 1,
        spend5hUsd: 0,
        spend24hUsd: 0,
        spend7dUsd: 0,
        spend30dUsd: 0,
      },
      providers: [],
    }

    expect(quotasData(payload)).toBe(payload)
    expect(quotasUnavailable(payload)).toBeUndefined()
    expect(quotasData({ unavailable: true, message: "detached" })).toBeUndefined()
    expect(quotasUnavailable({ unavailable: true, message: "detached" })).toEqual({
      unavailable: true,
      message: "detached",
    })
    expect(quotasData(undefined)).toBeUndefined()
  })
})

describe("QuotaRequestError", () => {
  test("keeps the status and the server's kind for in-place error states", () => {
    const error = new QuotaRequestError(409, "env owns the key", "env_managed")

    expect(error).toBeInstanceOf(Error)
    expect(error.name).toBe("QuotaRequestError")
    expect(error.status).toBe(409)
    expect(error.kind).toBe("env_managed")
    expect(new QuotaRequestError(500, "boom").kind).toBeUndefined()
  })
})

describe("useQuotasData", () => {
  test("polls only when asked: a disabled reader passes enabled=false and no interval", () => {
    useQuotasData({ enabled: () => false })

    expect(lastOptions?.enabled).toBe(false)
    expect(lastOptions?.refetchInterval).toBeUndefined()
    expect(lastOptions?.staleTime).toBe(30_000)
    // A detached 503 is a state, not a failure: retrying would only repeat it.
    expect(lastOptions?.retry).toBe(false)
  })

  test("defaults to an enabled, non-polling query", () => {
    useQuotasData()

    expect(lastOptions?.enabled).toBe(true)
    expect(lastOptions?.refetchInterval).toBeUndefined()
  })

  test("the settings tab is the only caller that sets an interval", () => {
    useQuotasData({ refetchInterval: 60_000 })

    expect(lastOptions?.enabled).toBe(true)
    expect(lastOptions?.refetchInterval).toBe(60_000)
  })
})
