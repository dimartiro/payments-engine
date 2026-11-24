//! Validated monetary amounts carried by deposits and withdrawals.

use std::str::FromStr;

use rust_decimal::Decimal;
use thiserror::Error;

/// Largest amount accepted for a single transaction, in whole units.
const MAX_WHOLE_UNITS: u64 = 1_000_000_000_000_000;

/// An amount of money on a deposit or withdrawal.
///
/// Invariants, enforced by the only two constructors ([`FromStr`] and
/// [`TryFrom<Decimal>`]):
/// - strictly positive: a negative withdrawal would be a hidden deposit;
/// - at most [`Amount::SCALE`] decimal places: input is never rounded, because
///   rounding would create or destroy money;
/// - at most [`Amount::MAX`].
///
/// The cap keeps every balance exact. `Decimal` holds a 96-bit mantissa
/// (about 7.9e28), so with 4 decimal places it is exact below about 7.9e24;
/// past that, its addition silently rounds away decimal places instead of
/// failing. Even 2^32 deposits of `MAX` sum to about 4.3e24, below that limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(Decimal);

impl Amount {
    /// Number of decimal places the input format allows.
    pub const SCALE: u32 = 4;

    /// Largest accepted amount: 1e15.
    pub const MAX: Amount = Amount(Decimal::from_parts(
        // Low and middle 32-bit words of the 96-bit mantissa; 1e15 < 2^64.
        MAX_WHOLE_UNITS as u32,
        (MAX_WHOLE_UNITS >> 32) as u32,
        0,
        false,
        0,
    ));

    /// The amount as a plain decimal, for balance arithmetic.
    pub fn value(self) -> Decimal {
        self.0
    }
}

/// Why a piece of text or a decimal is not a valid [`Amount`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AmountError {
    /// Not plain decimal notation (digits with at most one `.`), or too many
    /// digits to represent at all.
    #[error("not a plain decimal number")]
    InvalidFormat,
    /// More than four decimal places after removing trailing zeros. (Text with
    /// more digits than a `Decimal` can hold at all is `InvalidFormat`.)
    #[error("more than {} decimal places", Amount::SCALE)]
    TooPrecise,
    /// Zero or negative.
    #[error("not greater than zero")]
    NotPositive,
    /// Greater than [`Amount::MAX`].
    #[error("greater than the maximum amount")]
    TooLarge,
}

impl FromStr for Amount {
    type Err = AmountError;

    /// Parses plain decimal notation such as `1`, `1.5`, `.5` or `0.0001`.
    ///
    /// Signs, exponents (`1e3`), digit separators (`1_000`) and surrounding
    /// whitespace are rejected; trimming is the caller's job. `rust_decimal`'s
    /// own parsers accept some of these, hence the shape check first.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !is_plain_decimal(s) {
            return Err(AmountError::InvalidFormat);
        }
        // `rust_decimal`'s parser recurses once per input byte and only bounds
        // the depth after a nonzero digit, so a long run of leading zeros
        // overflows the stack when built with `opt-level = 0`. Dropping them is
        // free and leaves at most ~57 bytes to parse, however this is compiled.
        let s = match s.trim_start_matches('0') {
            "" | "." => "0",
            rest => rest,
        };
        // `from_str_exact` errors instead of rounding when the text has more
        // digits than a `Decimal` can hold.
        let value = Decimal::from_str_exact(s).map_err(|_| AmountError::InvalidFormat)?;
        Self::try_from(value)
    }
}

impl TryFrom<Decimal> for Amount {
    type Error = AmountError;

    fn try_from(value: Decimal) -> Result<Self, Self::Error> {
        // Drop trailing zeros so `1.23450` counts as 4 decimal places.
        let value = value.normalize();
        if value.scale() > Self::SCALE {
            return Err(AmountError::TooPrecise);
        }
        if value <= Decimal::ZERO {
            return Err(AmountError::NotPositive);
        }
        if value > Self::MAX.0 {
            return Err(AmountError::TooLarge);
        }
        Ok(Self(value))
    }
}

/// `true` for ASCII digits with at most one `.` and at least one digit.
fn is_plain_decimal(s: &str) -> bool {
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    let all_digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    all_digits(whole) && all_digits(fraction) && !(whole.is_empty() && fraction.is_empty())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    #[test]
    fn max_is_one_quadrillion() {
        assert_eq!(Amount::MAX.value(), Decimal::from(MAX_WHOLE_UNITS));
    }

    #[test]
    fn accepts_plain_decimals_up_to_four_places() {
        let cases = [
            ("1", "1"),
            ("1.0", "1"),
            ("0.0001", "0.0001"),
            ("1.2345", "1.2345"),
            ("1.23450", "1.2345"),
            (".5", "0.5"),
            ("5.", "5"),
            ("007.5", "7.5"),
            ("1000000000000000", "1000000000000000"),
            ("999999999999999.9999", "999999999999999.9999"),
            // Kept exact: an f64 would lose the last digits of this one.
            ("123456789012345.6789", "123456789012345.6789"),
        ];
        for (input, expected) in cases {
            let amount: Amount = input.parse().unwrap_or_else(|e| panic!("{input:?}: {e}"));
            assert_eq!(amount.value(), dec(expected), "input {input:?}");
        }
    }

    #[test]
    fn rejects_text_that_is_not_plain_decimal() {
        let cases = [
            "", ".", "-1", "+1", "1e3", "1E3", "1_000", "1,5", "NaN", "inf", "1.2.3", " 1", "1 ",
            "0x1F", "١",
        ];
        for input in cases {
            assert_eq!(
                input.parse::<Amount>(),
                Err(AmountError::InvalidFormat),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn rejects_more_than_four_decimal_places_instead_of_rounding() {
        for input in ["1.23456", "0.00001", "1.00001"] {
            assert_eq!(
                input.parse::<Amount>(),
                Err(AmountError::TooPrecise),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn rejects_zero() {
        for input in ["0", "0.0", "0.0000", "000"] {
            assert_eq!(
                input.parse::<Amount>(),
                Err(AmountError::NotPositive),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn rejects_amounts_above_max() {
        for input in [
            "1000000000000000.0001",
            "1000000000000001",
            "99999999999999999999",
        ] {
            assert_eq!(
                input.parse::<Amount>(),
                Err(AmountError::TooLarge),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn long_runs_of_leading_zeros_do_not_overflow_the_stack() {
        // Without the guard this aborts the 2 MiB test thread, but only when
        // built with `opt-level = 0`: `CARGO_PROFILE_DEV_OPT_LEVEL=0 cargo test long_runs`.
        let zeros = "0".repeat(100_000);
        assert_eq!(
            format!("{zeros}1").parse::<Amount>().unwrap().value(),
            dec("1")
        );
        assert_eq!(
            format!("{zeros}.5").parse::<Amount>().unwrap().value(),
            dec("0.5")
        );
        assert_eq!(zeros.parse::<Amount>(), Err(AmountError::NotPositive));
    }

    #[test]
    fn zero_written_any_way_is_not_positive() {
        for input in ["0.", ".0", "00.", "000.000"] {
            assert_eq!(
                input.parse::<Amount>(),
                Err(AmountError::NotPositive),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn rejects_text_with_too_many_digits_to_represent() {
        let forty_digits = "1".repeat(40);
        assert_eq!(
            forty_digits.parse::<Amount>(),
            Err(AmountError::InvalidFormat)
        );
    }

    #[test]
    fn try_from_decimal_enforces_the_same_rules() {
        assert_eq!(Amount::try_from(dec("-1")), Err(AmountError::NotPositive));
        assert_eq!(
            Amount::try_from(dec("1.00005")),
            Err(AmountError::TooPrecise)
        );
        assert_eq!(Amount::try_from(dec("2.50")).unwrap().value(), dec("2.5"));
    }
}
