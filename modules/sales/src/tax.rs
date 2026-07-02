//! The pure tax engine. Given a line's net amount, its quantity, and an ordered set of tax specs, it
//! returns the untaxed subtotal plus one result per tax (base + amount). It is the SINGLE place tax math
//! lives; the module's `apply_taxes` service resolves account.tax rows into specs, runs this, and
//! materializes the results as `sale.order.line.tax` rows + a blended back-compat `tax_rate`. No DB, no
//! I/O — unit-tested in isolation. (Relocated from kigumi-db: tax math is ERP, owned by the sales module.)
//!
//! Rounding discipline: every per-tax amount is rounded to the order currency's decimal places HERE, so
//! the line's stored computes (which sum the breakdown), the order totals, and the invoice GL all derive
//! from the SAME already-rounded numbers and cannot drift. For price-included taxes the net base is
//! rounded and the included tax is derived as gross − net so subtotal + tax == gross EXACTLY.

use rust_decimal::Decimal;

/// A tax resolved from `account.tax`, ready for the engine. `amount_type` is "percent", "fixed" (per
/// unit) or "division" (a price-included percentage). `price_include` is true for division or a tax
/// flagged included-in-price.
#[derive(Clone, Debug)]
pub struct TaxSpec {
    pub tax_id: i64,
    pub group_id: Option<i64>,
    pub amount_type: String,
    pub amount: Decimal,
    pub price_include: bool,
    pub include_base_amount: bool,
    pub sequence: i64,
}

/// One materialized tax line: which tax, its group, the base it applied to, and the resulting amount.
#[derive(Clone, Debug, PartialEq)]
pub struct TaxResult {
    pub tax_id: i64,
    pub group_id: Option<i64>,
    pub sequence: i64,
    pub base: Decimal,
    pub tax_amount: Decimal,
    pub is_price_include: bool,
}

fn is_included(s: &TaxSpec) -> bool {
    s.price_include || s.amount_type == "division"
}

/// Runs the engine. `line_net` = qty × unit price × (1 − discount%) (the gross, price-included taxes are
/// extracted FROM it); `qty` drives fixed-per-unit taxes; `dp` = the order currency's decimal places.
/// Returns (subtotal, results) where subtotal = line_net − Σ(price-included tax) and Σ results == the
/// total tax. Specs are applied in (sequence, tax_id) order.
pub fn compute_tax_lines(line_net: Decimal, qty: Decimal, specs: &[TaxSpec], dp: u32) -> (Decimal, Vec<TaxResult>) {
    let hundred = Decimal::from(100);
    let mut ordered: Vec<&TaxSpec> = specs.iter().collect();
    ordered.sort_by(|a, b| a.sequence.cmp(&b.sequence).then(a.tax_id.cmp(&b.tax_id)));

    // --- Inclusive pass: extract price-included taxes from the gross line_net. ---
    let included: Vec<&TaxSpec> = ordered.iter().copied().filter(|s| is_included(s)).collect();
    let mut percent_rate = Decimal::ZERO; // Σ included percentage rates
    let mut fixed_incl = Decimal::ZERO; // Σ included fixed per-unit amounts
    for s in &included {
        if s.amount_type == "fixed" {
            fixed_incl += s.amount;
        } else {
            percent_rate += s.amount; // percent or division
        }
    }
    let net_base = if included.is_empty() {
        line_net
    } else {
        ((line_net - fixed_incl * qty) / (Decimal::ONE + percent_rate / hundred)).round_dp(dp)
    };
    let total_included = line_net - net_base; // exact: subtotal + included tax == gross

    let mut results: Vec<TaxResult> = Vec::new();
    // Apportion the extracted total across the included taxes; the LAST absorbs the residual so the sum
    // is exact (no cent drift). Each records `net_base` as its base.
    let mut allocated = Decimal::ZERO;
    for (idx, s) in included.iter().enumerate() {
        let last = idx == included.len() - 1;
        let amount = if last {
            total_included - allocated
        } else {
            let nominal = if s.amount_type == "fixed" { s.amount * qty } else { net_base * s.amount / hundred };
            let r = nominal.round_dp(dp);
            allocated += r;
            r
        };
        results.push(TaxResult {
            tax_id: s.tax_id,
            group_id: s.group_id,
            sequence: s.sequence,
            base: net_base,
            tax_amount: amount,
            is_price_include: true,
        });
    }

    // --- Exclusive pass: taxes added on top of the net base, in order, compounding when flagged. ---
    let mut running_base = net_base;
    for s in ordered.iter().copied().filter(|s| !is_included(s)) {
        let tax_amount = match s.amount_type.as_str() {
            "fixed" => (s.amount * qty).round_dp(dp),
            _ => (running_base * s.amount / hundred).round_dp(dp), // percent (division can't be exclusive)
        };
        results.push(TaxResult {
            tax_id: s.tax_id,
            group_id: s.group_id,
            sequence: s.sequence,
            base: running_base,
            tax_amount,
            is_price_include: false,
        });
        if s.include_base_amount {
            running_base += tax_amount;
        }
    }

    (net_base, results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn spec(tax_id: i64, group: i64, kind: &str, amount: &str, incl: bool, compound: bool, seq: i64) -> TaxSpec {
        TaxSpec {
            tax_id,
            group_id: Some(group),
            amount_type: kind.to_string(),
            amount: d(amount),
            price_include: incl,
            include_base_amount: compound,
            sequence: seq,
        }
    }
    fn total(rs: &[TaxResult]) -> Decimal {
        rs.iter().map(|r| r.tax_amount).sum()
    }

    #[test]
    fn percent_single() {
        let (sub, rs) = compute_tax_lines(d("100"), d("1"), &[spec(1, 1, "percent", "22", false, false, 10)], 2);
        assert_eq!(sub, d("100"));
        assert_eq!(total(&rs), d("22"));
        assert_eq!(rs[0].base, d("100"));
    }

    #[test]
    fn multi_independent() {
        // 22% on 100 (group VAT) + fixed 5/unit (group Eco), qty 1 -> 27 total, two distinct rows.
        let specs = [spec(1, 1, "percent", "22", false, false, 20), spec(2, 2, "fixed", "5", false, false, 10)];
        let (sub, rs) = compute_tax_lines(d("100"), d("1"), &specs, 2);
        assert_eq!(sub, d("100"));
        assert_eq!(total(&rs), d("27"));
        assert_eq!(rs.len(), 2);
    }

    #[test]
    fn fixed_per_unit_scales_with_qty() {
        let (_, rs) = compute_tax_lines(d("150"), d("3"), &[spec(1, 1, "fixed", "5", false, false, 10)], 2);
        assert_eq!(total(&rs), d("15"));
    }

    #[test]
    fn compound() {
        // A 10% (compound) seq10 then B 5% seq20: A base 100 -> 10; B base 110 -> 5.50; total 15.50.
        let specs = [spec(1, 1, "percent", "10", false, true, 10), spec(2, 2, "percent", "5", false, false, 20)];
        let (_, rs) = compute_tax_lines(d("100"), d("1"), &specs, 2);
        assert_eq!(total(&rs), d("15.50"));
        let b = rs.iter().find(|r| r.tax_id == 2).unwrap();
        assert_eq!(b.base, d("110"));
        assert_eq!(b.tax_amount, d("5.50"));
    }

    #[test]
    fn price_included_exact() {
        // gross 122 includes 22% -> net 100.00, tax 22.00, sum preserves the gross.
        let (sub, rs) = compute_tax_lines(d("122"), d("1"), &[spec(1, 1, "percent", "22", true, false, 10)], 2);
        assert_eq!(sub, d("100.00"));
        assert_eq!(total(&rs), d("22.00"));
        assert_eq!(sub + total(&rs), d("122"));
    }

    #[test]
    fn price_included_awkward_rounding() {
        // gross 100.00 includes 7.5% -> net 93.02, tax 6.98, no residual cent.
        let (sub, rs) = compute_tax_lines(d("100.00"), d("1"), &[spec(1, 1, "percent", "7.5", true, false, 10)], 2);
        assert_eq!(sub, d("93.02"));
        assert_eq!(total(&rs), d("6.98"));
        assert_eq!(sub + total(&rs), d("100.00"));
    }

    #[test]
    fn division_is_price_included() {
        let (sub, rs) = compute_tax_lines(d("122"), d("1"), &[spec(1, 1, "division", "22", false, false, 10)], 2);
        assert_eq!(sub, d("100.00"));
        assert_eq!(total(&rs), d("22.00"));
        assert!(rs[0].is_price_include);
    }

    #[test]
    fn negative_tax() {
        let (_, rs) = compute_tax_lines(d("100"), d("1"), &[spec(1, 1, "percent", "-10", false, false, 10)], 2);
        assert_eq!(total(&rs), d("-10"));
    }

    #[test]
    fn empty_set() {
        let (sub, rs) = compute_tax_lines(d("100"), d("1"), &[], 2);
        assert_eq!(sub, d("100"));
        assert!(rs.is_empty());
    }

    #[test]
    fn mixed_inclusive_and_exclusive() {
        // gross 122 includes 22% (-> net 100, incl tax 22); plus an exclusive 5% on the net 100 (-> 5).
        let specs = [spec(1, 1, "percent", "22", true, false, 10), spec(2, 2, "percent", "5", false, false, 20)];
        let (sub, rs) = compute_tax_lines(d("122"), d("1"), &specs, 2);
        assert_eq!(sub, d("100.00"));
        assert_eq!(total(&rs), d("27.00")); // 22 included + 5 exclusive
        // subtotal + tax = 100 + 27 = 127 (the customer pays gross 122 + 5 added tax).
    }
}
