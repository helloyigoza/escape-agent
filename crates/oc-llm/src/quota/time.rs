/// Parse provider rate-limit reset strings: RFC3339 timestamps, epoch seconds/ms, or relative durations like `1m30s` / `250ms`.
#[must_use]
pub fn parse_reset_timestamp(raw: &str, now_ms: i64) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(ms) = parse_rfc3339_ms(trimmed) {
        return Some(ms);
    }
    if let Some(duration_ms) = parse_duration_ms(trimmed) {
        return Some(now_ms.saturating_add(duration_ms));
    }
    if let Ok(num) = trimmed.parse::<f64>()
        && num.is_finite()
        && num >= 0.0
    {
        return Some(normalize_epoch_value(num, now_ms));
    }
    None
}

/// Shared numeric-reset heuristic: milliseconds above 1e10, epoch seconds above
/// 1e9, otherwise relative seconds from `now_ms`.
#[allow(clippy::cast_possible_truncation)]
fn normalize_epoch_value(value: f64, now_ms: i64) -> i64 {
    if value > 10_000_000_000.0 {
        value as i64
    } else if value > 1_000_000_000.0 {
        (value * 1000.0) as i64
    } else {
        now_ms.saturating_add((value * 1000.0) as i64)
    }
}

/// Normalize a provider-supplied numeric reset value to Unix epoch milliseconds.
#[must_use]
pub(super) fn normalize_epoch_ms(value: i64) -> i64 {
    #[allow(clippy::cast_precision_loss)]
    let value = value as f64;
    normalize_epoch_value(value, super::now_epoch_ms())
}

fn parse_duration_ms(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut index = 0_usize;
    let mut total_ms = 0.0_f64;
    let mut parsed_any = false;
    while index < bytes.len() {
        let num_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if index == num_start || index >= bytes.len() {
            return None;
        }
        let value = raw[num_start..index].parse::<f64>().ok()?;
        let unit_start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let unit = &raw[unit_start..index];
        let multiplier = match unit {
            "ms" => 1.0,
            "s" => 1_000.0,
            "m" => 60_000.0,
            "h" => 3_600_000.0,
            "d" => 86_400_000.0,
            _ => return None,
        };
        total_ms += value * multiplier;
        parsed_any = true;
    }
    if !parsed_any || !total_ms.is_finite() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some(total_ms.round() as i64)
}

/// Parse an RFC 3339 timestamp string (`...Z` or `...±HH:MM`) into Unix epoch milliseconds.
#[must_use]
pub fn parse_rfc3339_ms(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    let (body, offset_ms) = if let Some(rest) = trimmed.strip_suffix('Z') {
        (rest, 0_i64)
    } else if let Some(plus) = trimmed.rfind('+')
        && plus > 10
    {
        let offset = parse_tz_offset_ms(&trimmed[plus + 1..], 1)?;
        (&trimmed[..plus], offset)
    } else if let Some(minus) = trimmed.rfind('-')
        && minus > 10
    {
        let offset = parse_tz_offset_ms(&trimmed[minus + 1..], -1)?;
        (&trimmed[..minus], offset)
    } else {
        return None;
    };

    let (date_part, time_part) = body.split_once('T').or_else(|| body.split_once(' '))?;
    let mut d_parts = date_part.split('-');
    let year: i64 = d_parts.next()?.parse().ok()?;
    let month: i64 = d_parts.next()?.parse().ok()?;
    let day: i64 = d_parts.next()?.parse().ok()?;
    if d_parts.next().is_some() {
        return None;
    }

    let (hms, frac_ms) = if let Some((left, frac)) = time_part.split_once('.') {
        let mut padded = String::with_capacity(3);
        for ch in frac.chars().take(3) {
            if ch.is_ascii_digit() {
                padded.push(ch);
            }
        }
        while padded.len() < 3 {
            padded.push('0');
        }
        (left, padded.parse::<i64>().unwrap_or(0))
    } else {
        (time_part, 0)
    };
    let mut t_parts = hms.split(':');
    let hour: i64 = t_parts.next()?.parse().ok()?;
    let minute: i64 = t_parts.next()?.parse().ok()?;
    let second: i64 = t_parts.next()?.parse().ok()?;
    if t_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day)?;
    let utc_ms = days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1_000 + frac_ms
        - offset_ms;
    Some(utc_ms)
}

fn parse_tz_offset_ms(raw: &str, sign: i64) -> Option<i64> {
    let (h, m) = raw.split_once(':')?;
    let hours: i64 = h.parse().ok()?;
    let mins: i64 = m.parse().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&mins) {
        return None;
    }
    Some(sign * (hours * 3_600_000 + mins * 60_000))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::{normalize_epoch_ms, parse_reset_timestamp};

    #[test]
    fn normalizes_epoch_millis_seconds_and_relative_values() {
        assert_eq!(normalize_epoch_ms(1_794_618_000_000), 1_794_618_000_000);
        assert_eq!(normalize_epoch_ms(1_794_618_000), 1_794_618_000_000);
        let now = 1_700_000_000_000;
        assert_eq!(
            parse_reset_timestamp("1794618000", now),
            Some(1_794_618_000_000)
        );
        assert_eq!(
            parse_reset_timestamp("1794618000000", now),
            Some(1_794_618_000_000)
        );
        assert_eq!(parse_reset_timestamp("90", now), Some(now + 90_000));
        let before = super::super::now_epoch_ms();
        let relative = normalize_epoch_ms(90);
        let after = super::super::now_epoch_ms();
        assert!(
            (before + 90_000..=after + 90_000).contains(&relative),
            "{relative} outside [{before}, {after}]"
        );
    }
}
