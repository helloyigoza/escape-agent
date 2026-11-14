//! Antigravity & Gemini CLI model name resolution, variant tier parsing, and thinking config.

use crate::accounts::ModelFamily;
use crate::constants::HeaderStyle;
use serde::{Deserialize, Serialize};

/// Resolved thinking configuration derived from model ID, variant, and request options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingMode {
    /// Thinking disabled.
    Disabled,
    /// Gemini 3 `thinkingLevel` mode (`minimal`, `low`, `medium`, `high`).
    GeminiLevel(String),
    /// Token-budget thinking mode used by Gemini 2.5 and Claude thinking models.
    Budget(u32),
}

/// Complete upstream target resolved from a catalog model ID and optional variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTargetModel {
    /// Original model ID passed in (e.g. `antigravity-gemini-3.1-pro`).
    pub raw_model: String,
    /// Upstream Cloud Code model name (e.g. `gemini-3.1-pro-high`, `claude-opus-4-6-thinking`).
    pub actual_model: String,
    /// Target model family.
    pub family: ModelFamily,
    /// Primary Cloud Code header/pool style (`antigravity` vs `gemini-cli`).
    pub header_style: HeaderStyle,
    /// Resolved thinking mode.
    pub thinking: ThinkingMode,
    /// True when the model comes from an explicit `antigravity-` catalog ID.
    pub is_antigravity_explicit: bool,
}

/// Resolve catalog model reference + optional variant into upstream Cloud Code parameters.
#[must_use]
pub fn resolve_antigravity_model(
    raw_model: &str,
    variant: Option<&str>,
    cli_first: bool,
) -> ResolvedTargetModel {
    let stripped_provider = raw_model
        .trim()
        .strip_prefix("google/")
        .unwrap_or(raw_model.trim());

    let (base_name, inline_variant) = match stripped_provider.split_once('#') {
        Some((m, v)) => (m.trim(), Some(v.trim())),
        None => (stripped_provider, None),
    };

    let effective_variant = variant
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| inline_variant.filter(|s| !s.is_empty()));

    let is_antigravity_explicit =
        base_name.starts_with("antigravity-") || base_name.contains("claude");
    let unprefixed = base_name.strip_prefix("antigravity-").unwrap_or(base_name);

    let lower = unprefixed.to_ascii_lowercase();
    let family = ModelFamily::from_model(&lower);

    let header_style = if family == ModelFamily::Claude {
        HeaderStyle::Antigravity
    } else if is_antigravity_explicit {
        if cli_first
            && !unprefixed.starts_with("gemini-3")
            && !unprefixed.starts_with("gemini-4")
            && lower.contains("2.5")
        {
            HeaderStyle::GeminiCli
        } else {
            HeaderStyle::Antigravity
        }
    } else {
        HeaderStyle::GeminiCli
    };

    if family == ModelFamily::Claude {
        return resolve_claude_model(
            base_name,
            unprefixed,
            effective_variant,
            is_antigravity_explicit,
        );
    }

    resolve_gemini_model(
        base_name,
        unprefixed,
        effective_variant,
        header_style,
        is_antigravity_explicit,
    )
}

fn resolve_claude_model(
    raw_model: &str,
    unprefixed: &str,
    variant: Option<&str>,
    is_antigravity_explicit: bool,
) -> ResolvedTargetModel {
    let lower = unprefixed.to_ascii_lowercase();

    // Strip any explicit variant suffix (`claude-opus-4-6-thinking-max` -> `claude-opus-4-6-thinking`).
    let (canonical, suffix_variant) = strip_known_suffix(
        &lower,
        &["xhigh", "extra", "minimal", "low", "medium", "high", "max"],
    );
    let chosen_variant = variant.or(suffix_variant);

    // Claude 5.x models (`claude-opus-5-5`, `claude-sonnet-5-5`) use tiered upstream model IDs
    // (`claude-opus-5-5-low`, `claude-opus-5-5-medium`, `claude-opus-5-5-high`) + thinking budget.
    if is_tiered_claude_model(&canonical) {
        let lower_variant = chosen_variant.map(str::to_ascii_lowercase);
        if matches!(lower_variant.as_deref(), Some("none" | "off" | "disabled")) {
            return ResolvedTargetModel {
                raw_model: raw_model.to_owned(),
                actual_model: format!("{canonical}-low"),
                family: ModelFamily::Claude,
                header_style: HeaderStyle::Antigravity,
                thinking: ThinkingMode::Disabled,
                is_antigravity_explicit,
            };
        }
        let (tier, budget) = match lower_variant.as_deref() {
            Some("minimal" | "low") => ("low", 8192),
            Some("high" | "max" | "xhigh" | "extra") => ("high", 32768),
            Some("medium") | None => ("medium", 16384),
            Some(other) => {
                if let Ok(b) = other.parse::<u32>() {
                    let t = if b <= 8192 {
                        "low"
                    } else if b <= 16384 {
                        "medium"
                    } else {
                        "high"
                    };
                    (t, b)
                } else {
                    ("medium", 16384)
                }
            }
        };
        return ResolvedTargetModel {
            raw_model: raw_model.to_owned(),
            actual_model: format!("{canonical}-{tier}"),
            family: ModelFamily::Claude,
            header_style: HeaderStyle::Antigravity,
            thinking: ThinkingMode::Budget(budget),
            is_antigravity_explicit,
        };
    }

    let is_thinking_model = canonical.contains("thinking");
    let thinking = if is_thinking_model {
        let budget = match chosen_variant.map(str::to_ascii_lowercase).as_deref() {
            Some("minimal" | "low") => 8192,
            Some("medium") => 16384,
            Some("high" | "max" | "xhigh" | "extra") | None => 32768,
            Some(other) => other.parse::<u32>().unwrap_or(32768),
        };
        ThinkingMode::Budget(budget)
    } else if let Some(v) = chosen_variant {
        let budget = match v.to_ascii_lowercase().as_str() {
            "minimal" | "low" => 4096,
            "medium" => 8192,
            "high" => 16384,
            "max" | "xhigh" | "extra" => 32768,
            _ => 0,
        };
        if budget > 0 {
            ThinkingMode::Budget(budget)
        } else {
            ThinkingMode::Disabled
        }
    } else {
        ThinkingMode::Disabled
    };

    ResolvedTargetModel {
        raw_model: raw_model.to_owned(),
        actual_model: canonical,
        family: ModelFamily::Claude,
        header_style: HeaderStyle::Antigravity,
        thinking,
        is_antigravity_explicit,
    }
}

fn is_tiered_claude_model(canonical: &str) -> bool {
    canonical == "claude-opus-5-5"
        || canonical == "claude-sonnet-5-5"
        || canonical.starts_with("claude-opus-5")
        || canonical.starts_with("claude-sonnet-5")
}

fn resolve_gemini_model(
    raw_model: &str,
    unprefixed: &str,
    variant: Option<&str>,
    header_style: HeaderStyle,
    is_antigravity_explicit: bool,
) -> ResolvedTargetModel {
    let lower = unprefixed.to_ascii_lowercase();

    // Open-weight OSS models hosted on Antigravity (e.g. `gpt-oss-120b-medium`) have tier baked into ID.
    if lower.starts_with("gpt-oss-") {
        return ResolvedTargetModel {
            raw_model: raw_model.to_owned(),
            actual_model: lower,
            family: ModelFamily::Gemini,
            header_style: HeaderStyle::Antigravity,
            thinking: ThinkingMode::Disabled,
            is_antigravity_explicit,
        };
    }

    let (canonical_base, suffix_variant) = strip_known_suffix(
        &lower,
        &["xhigh", "extra", "minimal", "low", "medium", "high", "max"],
    );
    let chosen_variant = variant
        .map(str::to_ascii_lowercase)
        .or_else(|| suffix_variant.map(str::to_owned));

    // Tiered Gemini 4 / 3.8 Antigravity Hub models (`<base>-{low,medium,high[,xhigh]}`).
    let tiered_spec = if canonical_base == "gemini-4-hydrogen" {
        Some(("gemini-4-hydrogen", true, "high"))
    } else if canonical_base == "gemini-4-argon" {
        Some(("gemini-4-argon", false, "medium"))
    } else if canonical_base == "gemini-3.8-flash" || canonical_base == "gemini-3.8-flash-tiered" {
        Some(("gemini-3.8-flash", false, "medium"))
    } else {
        None
    };
    if let Some((base_name, supports_xhigh, default_tier)) = tiered_spec {
        return resolve_tiered_hub_gemini(
            raw_model,
            base_name,
            chosen_variant.as_deref(),
            supports_xhigh,
            default_tier,
            header_style,
            is_antigravity_explicit,
        );
    }

    // Gemini 3 Pro / 3.1 Pro, Gemini 3 Flash, and newer discovered Gemini 3/4 models.
    if canonical_base.starts_with("gemini-3") || canonical_base.starts_with("gemini-4") {
        return resolve_gemini_3_or_newer(
            raw_model,
            unprefixed,
            canonical_base,
            chosen_variant,
            header_style,
            is_antigravity_explicit,
        );
    }

    // Gemini 2.5 Pro / Flash
    if canonical_base.contains("2.5") {
        return resolve_gemini_25_model(
            raw_model,
            canonical_base,
            chosen_variant.as_deref(),
            header_style,
            is_antigravity_explicit,
        );
    }

    ResolvedTargetModel {
        raw_model: raw_model.to_owned(),
        actual_model: unprefixed.to_owned(),
        family: ModelFamily::Gemini,
        header_style,
        thinking: ThinkingMode::Disabled,
        is_antigravity_explicit,
    }
}

fn resolve_gemini_3_or_newer(
    raw_model: &str,
    unprefixed: &str,
    canonical_base: String,
    chosen_variant: Option<String>,
    header_style: HeaderStyle,
    is_antigravity_explicit: bool,
) -> ResolvedTargetModel {
    if (canonical_base == "gemini-3.1-pro" || canonical_base == "gemini-3-pro")
        && header_style == HeaderStyle::Antigravity
    {
        let level = match chosen_variant.as_deref() {
            Some("minimal" | "low") => "low",
            _ => "high",
        };
        return ResolvedTargetModel {
            raw_model: raw_model.to_owned(),
            actual_model: format!("{canonical_base}-{level}"),
            family: ModelFamily::Gemini,
            header_style,
            thinking: ThinkingMode::GeminiLevel(level.to_owned()),
            is_antigravity_explicit,
        };
    }

    if canonical_base == "gemini-3-flash" || canonical_base == "gemini-3-flash-preview" {
        let level = match chosen_variant.as_deref() {
            Some("minimal") => "minimal",
            Some("low") => "low",
            Some("high" | "max") => "high",
            _ => "medium",
        };
        return ResolvedTargetModel {
            raw_model: raw_model.to_owned(),
            actual_model: if header_style == HeaderStyle::Antigravity {
                "gemini-3-flash".to_owned()
            } else {
                canonical_base
            },
            family: ModelFamily::Gemini,
            header_style,
            thinking: ThinkingMode::GeminiLevel(level.to_owned()),
            is_antigravity_explicit,
        };
    }

    let thinking = if let Some(level) = chosen_variant {
        ThinkingMode::GeminiLevel(level)
    } else if canonical_base.contains("lite") || canonical_base.contains("image") {
        ThinkingMode::Disabled
    } else {
        ThinkingMode::GeminiLevel("high".to_owned())
    };
    ResolvedTargetModel {
        raw_model: raw_model.to_owned(),
        actual_model: unprefixed.to_owned(),
        family: ModelFamily::Gemini,
        header_style,
        thinking,
        is_antigravity_explicit,
    }
}

fn resolve_gemini_25_model(
    raw_model: &str,
    canonical_base: String,
    chosen_variant: Option<&str>,
    header_style: HeaderStyle,
    is_antigravity_explicit: bool,
) -> ResolvedTargetModel {
    let thinking = match chosen_variant {
        Some("none" | "off" | "disabled") => ThinkingMode::Disabled,
        Some("minimal" | "low") | None => ThinkingMode::Budget(if canonical_base.contains("pro") {
            8192
        } else {
            4096
        }),
        Some("medium") => ThinkingMode::Budget(12288),
        Some("high" | "max") => ThinkingMode::Budget(24576),
        Some(other) => other
            .parse::<u32>()
            .map_or(ThinkingMode::Disabled, ThinkingMode::Budget),
    };
    ResolvedTargetModel {
        raw_model: raw_model.to_owned(),
        actual_model: canonical_base,
        family: ModelFamily::Gemini,
        header_style,
        thinking,
        is_antigravity_explicit,
    }
}

fn resolve_tiered_hub_gemini(
    raw_model: &str,
    base_name: &str,
    chosen_variant: Option<&str>,
    supports_xhigh: bool,
    default_tier: &str,
    header_style: HeaderStyle,
    is_antigravity_explicit: bool,
) -> ResolvedTargetModel {
    let tier = match chosen_variant {
        Some("minimal" | "low") => "low",
        Some("medium") => "medium",
        Some("xhigh" | "extra" | "max") if supports_xhigh => "xhigh",
        Some("high" | "max" | "xhigh" | "extra") => "high",
        _ => default_tier,
    };
    ResolvedTargetModel {
        raw_model: raw_model.to_owned(),
        actual_model: if header_style == HeaderStyle::Antigravity {
            format!("{base_name}-{tier}")
        } else {
            base_name.to_owned()
        },
        family: ModelFamily::Gemini,
        header_style,
        thinking: ThinkingMode::GeminiLevel(tier.to_owned()),
        is_antigravity_explicit,
    }
}

fn strip_known_suffix<'a>(model: &str, suffixes: &[&'a str]) -> (String, Option<&'a str>) {
    for &suf in suffixes {
        if let Some(prefix) = model.strip_suffix(&format!("-{suf}")) {
            return (prefix.to_owned(), Some(suf));
        }
    }
    (model.to_owned(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_gemini_and_claude_variants_and_pools() {
        let r1 = resolve_antigravity_model("antigravity-gemini-3.1-pro", Some("low"), false);
        assert_eq!(r1.actual_model, "gemini-3.1-pro-low");
        assert_eq!(r1.header_style, HeaderStyle::Antigravity);
        assert_eq!(r1.thinking, ThinkingMode::GeminiLevel("low".to_owned()));

        let r2 = resolve_antigravity_model("antigravity-gemini-3-flash#high", None, false);
        assert_eq!(r2.actual_model, "gemini-3-flash");
        assert_eq!(r2.thinking, ThinkingMode::GeminiLevel("high".to_owned()));

        let r3 =
            resolve_antigravity_model("antigravity-claude-opus-4-6-thinking", Some("max"), false);
        assert_eq!(r3.actual_model, "claude-opus-4-6-thinking");
        assert_eq!(r3.family, ModelFamily::Claude);
        assert_eq!(r3.thinking, ThinkingMode::Budget(32768));

        let r4 = resolve_antigravity_model("gemini-2.5-flash", None, false);
        assert_eq!(r4.actual_model, "gemini-2.5-flash");
        assert_eq!(r4.header_style, HeaderStyle::GeminiCli);

        let h2_xhigh =
            resolve_antigravity_model("antigravity-gemini-4-hydrogen", Some("xhigh"), false);
        assert_eq!(h2_xhigh.actual_model, "gemini-4-hydrogen-xhigh");
        assert_eq!(h2_xhigh.family, ModelFamily::Gemini);
        assert_eq!(h2_xhigh.header_style, HeaderStyle::Antigravity);
        assert_eq!(
            h2_xhigh.thinking,
            ThinkingMode::GeminiLevel("xhigh".to_owned())
        );

        let h2_extra =
            resolve_antigravity_model("antigravity-gemini-4-hydrogen#extra", None, false);
        assert_eq!(h2_extra.actual_model, "gemini-4-hydrogen-xhigh");
        assert_eq!(
            h2_extra.thinking,
            ThinkingMode::GeminiLevel("xhigh".to_owned())
        );

        let argon_med =
            resolve_antigravity_model("antigravity-gemini-4-argon", Some("medium"), false);
        assert_eq!(argon_med.actual_model, "gemini-4-argon-medium");
        assert_eq!(
            argon_med.thinking,
            ThinkingMode::GeminiLevel("medium".to_owned())
        );

        let flash38 =
            resolve_antigravity_model("antigravity-gemini-3.8-flash", Some("high"), false);
        assert_eq!(flash38.actual_model, "gemini-3.8-flash-high");
        assert_eq!(
            flash38.thinking,
            ThinkingMode::GeminiLevel("high".to_owned())
        );

        let opus55 = resolve_antigravity_model("antigravity-claude-opus-5-5", Some("high"), false);
        assert_eq!(opus55.actual_model, "claude-opus-5-5-high");
        assert_eq!(opus55.family, ModelFamily::Claude);
        assert_eq!(opus55.thinking, ThinkingMode::Budget(32768));

        let sonnet55 = resolve_antigravity_model("antigravity-claude-sonnet-5-5", None, false);
        assert_eq!(sonnet55.actual_model, "claude-sonnet-5-5-medium");
        assert_eq!(sonnet55.family, ModelFamily::Claude);
        assert_eq!(sonnet55.thinking, ThinkingMode::Budget(16384));

        let oss = resolve_antigravity_model("antigravity-gpt-oss-120b-medium", None, false);
        assert_eq!(oss.actual_model, "gpt-oss-120b-medium");
        assert_eq!(oss.header_style, HeaderStyle::Antigravity);
        assert_eq!(oss.thinking, ThinkingMode::Disabled);
    }
}
