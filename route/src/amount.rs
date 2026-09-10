//! Decimal-string <-> raw-unit conversion and address grammar (pure).
//!
//! Human amounts travel as decimal strings (`"25"`, `"0.5"`); raw amounts as
//! decimal integer strings in the asset's smallest unit. Arc USDC has two
//! views of one balance: 6 decimals over the ERC-20 interface and 18 as the
//! native gas asset; conversion is exactly x/÷ 1e12.

use alloy_primitives::{Address, U256};

/// Grammar for human amounts: `^[0-9]{1,12}(\.[0-9]{1,18})?$`, fractional
/// digits bounded by the asset's decimals, non-zero.
pub fn parse_decimal(input: &str, decimals: u32) -> Result<U256, String> {
    let (whole, frac) = match input.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (input, None),
    };
    if whole.is_empty() || whole.len() > 12 || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err("amount must match [0-9]{1,12}(.[0-9]{1,18})?".into());
    }
    if let Some(f) = frac
        && (f.is_empty() || f.len() > 18 || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("amount must match [0-9]{1,12}(.[0-9]{1,18})?".into());
    }
    let frac = frac.unwrap_or("");
    if frac.len() as u32 > decimals {
        return Err(format!(
            "amount has {} fractional digits but the asset has {decimals} decimals",
            frac.len()
        ));
    }
    let mut digits = String::with_capacity(whole.len() + decimals as usize);
    digits.push_str(whole);
    digits.push_str(frac);
    for _ in frac.len() as u32..decimals {
        digits.push('0');
    }
    let value = U256::from_str_radix(&digits, 10).map_err(|_| "amount overflows".to_string())?;
    if value == U256::ZERO {
        return Err("amount must be greater than zero".into());
    }
    Ok(value)
}

/// Render a raw amount as a human decimal string, trailing zeros trimmed.
pub fn format_units(raw: U256, decimals: u32) -> String {
    let digits = raw.to_string();
    let decimals = decimals as usize;
    if decimals == 0 {
        return digits;
    }
    let padded = if digits.len() <= decimals {
        format!("{}{digits}", "0".repeat(decimals - digits.len() + 1))
    } else {
        digits
    };
    let (whole, frac) = padded.split_at(padded.len() - decimals);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

pub fn pow10(decimals: u32) -> U256 {
    U256::from(10u64).pow(U256::from(decimals))
}

/// 6-decimal ERC-20 USDC raw -> 18-decimal native raw.
pub fn usdc6_to_native18(raw6: U256) -> U256 {
    raw6.saturating_mul(pow10(12))
}

/// 18-decimal native raw -> 6-decimal ERC-20 USDC raw (floor).
pub fn native18_to_usdc6(raw18: U256) -> U256 {
    raw18 / pow10(12)
}

/// Strict route-parameter grammar: lowercase `0x` + 40 hex digits.
pub fn is_lowercase_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parse a lowercase route-parameter address.
pub fn parse_route_address(value: &str) -> Result<Address, String> {
    if !is_lowercase_address(value) {
        return Err("address must be lowercase 0x-prefixed 20-byte hex".into());
    }
    value
        .parse::<Address>()
        .map_err(|_| "address is not valid hex".to_string())
}

/// Parse an address from a body or an upstream document (any case), or
/// `None` when it is not an address at all.
pub fn parse_any_address(value: &str) -> Option<Address> {
    let value = value.trim();
    if value.len() != 42
        || !value.starts_with("0x")
        || !value[2..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    value.parse::<Address>().ok()
}

/// Lowercase `0x…` rendering used everywhere in JSON output.
pub fn addr_hex(address: Address) -> String {
    format!("{address:?}")
}

pub fn is_bytes32_hex(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn u256_to_f64(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::INFINITY)
}

pub fn parse_u256_decimal(value: &str) -> Result<U256, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected a decimal integer string".into());
    }
    U256::from_str_radix(trimmed, 10).map_err(|_| "decimal integer overflows".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_amounts() {
        assert_eq!(parse_decimal("25", 6).unwrap(), U256::from(25_000_000u64));
        assert_eq!(parse_decimal("0.5", 6).unwrap(), U256::from(500_000u64));
        assert_eq!(
            parse_decimal("1234.5", 18).unwrap(),
            U256::from(1_234_500_000_000_000_000_000u128)
        );
        assert_eq!(parse_decimal("0.000001", 6).unwrap(), U256::from(1u64));
        assert_eq!(parse_decimal("000123", 0).unwrap(), U256::from(123u64));
    }

    #[test]
    fn rejects_bad_amounts() {
        for bad in [
            "",
            "0",
            "0.0",
            "-1",
            "1e3",
            ".5",
            "5.",
            "1,000",
            "abc",
            "0x10",
            " 5",
            "1234567890123",
            "1.1234567890123456789",
        ] {
            assert!(
                parse_decimal(bad, 18).is_err(),
                "{bad:?} should be rejected"
            );
        }
        // More fractional digits than the asset has.
        assert!(parse_decimal("0.0000001", 6).is_err());
        assert!(parse_decimal("0.000000", 6).is_err());
    }

    #[test]
    fn formats_units() {
        assert_eq!(format_units(U256::from(25_000_000u64), 6), "25");
        assert_eq!(format_units(U256::from(500_000u64), 6), "0.5");
        assert_eq!(format_units(U256::from(1u64), 6), "0.000001");
        assert_eq!(format_units(U256::ZERO, 18), "0");
        assert_eq!(
            format_units(U256::from(1_234_500_000_000_000_000_000u128), 18),
            "1234.5"
        );
        assert_eq!(format_units(U256::from(7u64), 0), "7");
    }

    #[test]
    fn scales_between_the_two_usdc_views() {
        let six = U256::from(25_000_000u64);
        let eighteen = usdc6_to_native18(six);
        assert_eq!(eighteen, U256::from(25_000_000_000_000_000_000u128));
        assert_eq!(native18_to_usdc6(eighteen), six);
        assert_eq!(
            native18_to_usdc6(eighteen + U256::from(999_999_999_999u64)),
            six
        );
    }

    #[test]
    fn address_grammar() {
        assert!(is_lowercase_address(
            "0x4753c45fb550fecaa143a47968659117e6ffc2ce"
        ));
        assert!(!is_lowercase_address(
            "0x4753C45fb550fecaa143a47968659117e6ffc2ce"
        ));
        assert!(!is_lowercase_address(
            "4753c45fb550fecaa143a47968659117e6ffc2ce"
        ));
        assert!(!is_lowercase_address(
            "0x4753c45fb550fecaa143a47968659117e6ffc2c"
        ));
        assert!(parse_route_address("0x4753C45fb550fecaa143a47968659117e6ffc2ce").is_err());
        assert!(parse_any_address("0x4753C45fb550fecaa143a47968659117e6ffc2ce").is_some());
        assert!(parse_any_address("0x4753c45fb550fecaa143a47968659117e6ffc2ce").is_some());
        assert!(parse_any_address("not-an-address").is_none());
        assert_eq!(
            addr_hex(parse_any_address("0x4753C45fb550fecaa143a47968659117e6ffc2ce").unwrap()),
            "0x4753c45fb550fecaa143a47968659117e6ffc2ce"
        );
        assert!(is_bytes32_hex(
            "0x5e61e0abb3fa7a794b2c7c233f290a832803d4030b1c113200fa37c9472d79f8"
        ));
        assert!(!is_bytes32_hex(
            "0x5e61e0abb3fa7a794b2c7c233f290a832803d4030b1c113200fa37c9472d79f"
        ));
    }
}
