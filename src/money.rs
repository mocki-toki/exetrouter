//! Human presentation only. Accounting and JSON retain exact numeric amounts.
pub fn usd(value: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    if value < 0. {
        return format!("-{}", usd(-value));
    }
    let abs = value.abs();
    for (scale, suffix) in [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "k")] {
        if abs >= scale - scale / 20_000. {
            let text = format!("{:.1}", value / scale);
            return format!(
                "${}{suffix}",
                text.trim_end_matches('0').trim_end_matches('.')
            );
        }
    }
    let places = if value.fract() == 0. {
        0
    } else if abs < 0.0001 {
        8
    } else if abs < 0.01 {
        5
    } else {
        2
    };
    if abs > 0. && abs < 0.00000001 {
        return "<$0.00000001".into();
    }
    let text = format!("{value:.places$}");
    format!(
        "${}",
        if places > 2 {
            text.trim_end_matches('0')
        } else {
            &text
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_dollars_preserve_unknown_zero_and_small_prices() {
        for (value, text) in [
            (0., "$0"),
            (-1., "-$1"),
            (-5000., "-$5k"),
            (100., "$100"),
            (12.5, "$12.50"),
            (5000., "$5k"),
            (12500., "$12.5k"),
            (1_200_000., "$1.2M"),
            (999_999., "$1M"),
            (1e9, "$1B"),
            (0.00000125, "$0.00000125"),
        ] {
            assert_eq!(usd(value), text);
        }
        assert_eq!(usd(f64::NAN), "—");
    }
}
