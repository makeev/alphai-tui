//! Open positions from a single statement date. Parse and merge the entire
//! file before taking the config lock.
mod csv;

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use csv::Record;

use crate::{config, portfolio::Position};

const SYMBOL: &[&str] = &["symbol", "ticker", "tickersymbol", "securitysymbol"];
const QUANTITY: &[&str] = &[
    "quantity",
    "qty",
    "qtyquantity",
    "shares",
    "units",
    "position",
];
const UNIT_COST: &[&str] = &[
    "averagecostbasis",
    "averagecost",
    "avgcost",
    "averageprice",
    "avgprice",
    "costprice",
    "costbasisprice",
    "pricepaid",
    "costpershare",
    "unitcost",
];
const TOTAL_COST: &[&str] = &[
    "costbasistotal",
    "costbasis",
    "totalcost",
    "costbasismoney",
    "bookvalue",
];

fn key(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

struct Columns {
    names: Vec<String>,
}

impl Columns {
    fn new(cells: &[String]) -> Self {
        Self {
            names: cells.iter().map(|s| key(s)).collect(),
        }
    }

    fn index(&self, aliases: &[&str]) -> Option<usize> {
        aliases
            .iter()
            .find_map(|alias| self.names.iter().position(|name| name == alias))
    }

    fn has_positions(&self) -> bool {
        self.index(SYMBOL).is_some() && self.index(QUANTITY).is_some()
    }

    fn field<'a>(&self, row: &'a Record, aliases: &[&str]) -> &'a str {
        self.index(aliases)
            .and_then(|i| row.cells.get(i))
            .map_or("", |s| s.trim())
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.has_positions(),
            "open positions need symbol and quantity columns"
        );
        ensure!(
            self.index(UNIT_COST).is_some() || self.index(TOTAL_COST).is_some(),
            "the file has a symbol and quantity but no average cost or cost basis column"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Activity,
    SectionedFlex,
    Flex,
    Generic,
}

impl Format {
    fn label(self) -> &'static str {
        match self {
            Self::Activity => "IBKR Activity Statement",
            Self::SectionedFlex | Self::Flex => "IBKR Flex Query",
            Self::Generic => "Positions CSV",
        }
    }
}

fn activity_row(row: &Record, section: &str, kind: &str) -> bool {
    row.cells
        .first()
        .is_some_and(|s| s.trim().eq_ignore_ascii_case(section))
        && row
            .cells
            .get(1)
            .is_some_and(|s| s.trim().eq_ignore_ascii_case(kind))
}

/// Section codes and line descriptors are optional in Flex CSV exports.
fn flex_descriptor(row: &Record) -> Option<(&str, &str)> {
    let code = row.cells.first()?.as_str();
    let kind = row.cells.get(1)?.as_str();
    (code.len() == 4
        && code.bytes().all(|b| b.is_ascii_uppercase())
        && matches!(kind, "BOS" | "EOS" | "HEADER" | "DATA"))
    .then_some((code, kind))
}

#[derive(Default)]
struct FlexSections {
    basis: Option<String>,
    reading_positions: bool,
    needs_header: bool,
}

impl FlexSections {
    fn position_row(&mut self, mut row: Record) -> Result<Option<Record>> {
        if let Some((code, kind)) = flex_descriptor(&row) {
            self.reading_positions = matches!(code, "POST" | "POSS") && kind != "EOS";
            if !self.reading_positions {
                self.needs_header = true;
                return Ok(None);
            }
            ensure!(
                self.basis.get_or_insert_with(|| code.to_owned()) == code,
                "export one position basis: trade date (POST) or settlement date (POSS)"
            );
            if kind == "BOS" {
                self.needs_header = true;
                return Ok(None);
            }
            row.cells.drain(..2);
        }
        if !self.reading_positions {
            return Ok(None);
        }
        if self.needs_header {
            ensure!(
                Columns::new(&row.cells).has_positions(),
                "include column headers for Flex positions at line {}",
                row.line
            );
            self.needs_header = false;
        }
        Ok(Some(row))
    }
}

fn header_format(row: &Record) -> Option<Format> {
    if flex_descriptor(row).is_some() {
        return Some(Format::SectionedFlex);
    }
    // Detect statements before looking for ordinary tabular headers.
    // A Trades header can also contain Symbol and Quantity.
    if row
        .cells
        .get(1)
        .is_some_and(|s| s.eq_ignore_ascii_case("Header"))
    {
        return Some(Format::Activity);
    }
    let cols = Columns::new(&row.cells);
    cols.has_positions().then(|| {
        if cols
            .index(&[
                "levelofdetail",
                "currencyprimary",
                "conid",
                "fifopnlunrealized",
            ])
            .is_some()
            || (cols.index(&["assetclass"]).is_some()
                && cols.index(&["costbasisprice", "costbasismoney"]).is_some())
        {
            Format::Flex
        } else {
            Format::Generic
        }
    })
}

fn detect(text: &str) -> Result<(Format, char)> {
    for line in text.lines().take(30) {
        for sep in [',', ';', '\t'] {
            if line.contains(sep)
                && let Ok(rows) = csv::read(line, sep)
                && let Some(format) = rows.first().and_then(header_format)
            {
                return Ok((format, sep));
            }
        }
    }
    bail!("could not find an open positions header in the first 30 lines")
}

fn number(raw: &str, decimal_comma: bool) -> Option<f64> {
    let raw = raw.trim();
    let (raw, negative) = match raw.strip_prefix('(') {
        Some(rest) => (rest.strip_suffix(')')?, true),
        None => (raw, false),
    };
    let unsigned = raw.trim_matches(['$', '€', '£', ' ']);
    let (sign, digits) = if let Some(rest) = unsigned.strip_prefix('-') {
        if negative {
            return None;
        }
        (-1.0, rest)
    } else {
        (
            if negative { -1.0 } else { 1.0 },
            unsigned.trim_start_matches('+'),
        )
    };
    let digits = digits
        .trim_start_matches(['$', '€', '£'])
        .replace([' ', '\u{a0}', '\u{202f}'], "");
    let (decimal, thousands) = if decimal_comma {
        (',', '.')
    } else {
        ('.', ',')
    };
    let (whole, fraction) = digits.split_once(decimal).unwrap_or((&digits, ""));
    let mut groups = whole.split(thousands);
    let first = groups.next()?;
    if !first.bytes().all(|c| c.is_ascii_digit()) || !fraction.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if whole.contains(thousands)
        && (!(1..=3).contains(&first.len())
            || !groups.all(|s| s.len() == 3 && s.bytes().all(|c| c.is_ascii_digit())))
    {
        return None;
    }
    let value = digits
        .replace(thousands, "")
        .replace(decimal, ".")
        .parse::<f64>()
        .ok()?
        * sign;
    value.is_finite().then_some(value)
}

/// Semicolon exports may use either decimal convention. Grouped integers
/// alone (such as 1.234) cannot determine it; use all holding rows.
#[derive(Default)]
struct NumberFormat {
    decimal_comma: Option<bool>,
    ambiguous: bool,
}

impl NumberFormat {
    fn observe(&mut self, cols: &Columns, row: &Record) -> Result<()> {
        for aliases in [QUANTITY, UNIT_COST, TOTAL_COST] {
            let field = cols.field(row, aliases);
            if !field.contains([',', '.']) {
                continue;
            }
            let comma = match (number(field, false), number(field, true)) {
                (Some(a), Some(b)) => {
                    self.ambiguous |= a != b;
                    continue;
                }
                (None, None) => continue,
                (None, Some(_)) => true,
                (Some(_), None) => false,
            };
            ensure!(
                *self.decimal_comma.get_or_insert(comma) == comma,
                "mixed decimal conventions at line {}",
                row.line
            );
        }
        Ok(())
    }

    fn decimal_comma(&self) -> Result<bool> {
        ensure!(
            self.decimal_comma.is_some() || !self.ambiguous,
            "ambiguous decimal separator in semicolon CSV: export without thousands separators and use an explicit decimal part"
        );
        Ok(self.decimal_comma.unwrap_or(false))
    }
}

fn ticker(raw: &str, broker: bool) -> Option<String> {
    let mut s = raw.trim_end_matches('*').trim().to_ascii_uppercase();
    if broker
        && let Some((name, class)) = s.split_once(' ')
        && !name.is_empty()
        && class.len() == 1
        && class.as_bytes()[0].is_ascii_alphabetic()
    {
        s = format!("{name}-{class}");
    }
    let mut bytes = s.bytes();
    let first = bytes.next()?;
    (s.len() <= 20
        && (first.is_ascii_alphanumeric() || first == b'^')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b".-=^".contains(&b)))
    .then_some(s)
}

fn listing(symbol: &str, currency: &str, exchange: &str) -> Result<String> {
    let currency = currency.to_ascii_uppercase();
    let (suffix, local) = match exchange.to_ascii_uppercase().as_str() {
        "LSE" | "LSEETF" => (".L", "GBP"),
        "SBF" => (".PA", "EUR"),
        "AEB" => (".AS", "EUR"),
        "IBIS" | "IBIS2" | "XETRA" => (".DE", "EUR"),
        "EBS" | "SWX" => (".SW", "CHF"),
        "TSEJ" => (".T", "JPY"),
        "SEHK" => (".HK", "HKD"),
        "ASX" => (".AX", "AUD"),
        "TSE" | "TSX" => (".TO", "CAD"),
        _ if currency.is_empty() || currency == "USD" => return Ok(symbol.to_owned()),
        _ => bail!(
            "{currency} without a recognized listing exchange, add the quoted ticker manually"
        ),
    };
    ensure!(
        currency.is_empty() || currency == local,
        "{currency} on {exchange}, check ticker and quote currency manually"
    );
    let base = symbol.strip_suffix(suffix).unwrap_or(symbol);
    if suffix == ".HK" && base.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(format!("{base:0>4}{suffix}"));
    }
    Ok(format!("{base}{suffix}"))
}

struct Import {
    format: Format,
    positions: BTreeMap<String, Position>,
    skipped: BTreeMap<String, usize>,
    // Invalid costs or mixed sides must not replace an existing holding
    // with the valid subset of its rows from other accounts.
    rejected: BTreeMap<String, String>,
}

impl Import {
    fn skip(&mut self, reason: String) {
        *self.skipped.entry(reason).or_default() += 1;
    }

    fn row(&mut self, cols: &Columns, row: &Record, decimal_comma: bool) -> Result<()> {
        let get = |aliases: &[&str]| cols.field(row, aliases);
        let Some(symbol) = ticker(get(SYMBOL), self.format != Format::Generic) else {
            self.skip("rows without a ticker".into());
            return Ok(());
        };
        ensure!(
            row.cells.len() == cols.names.len(),
            "line {} has {} fields, expected {} for {symbol}",
            row.line,
            row.cells.len(),
            cols.names.len()
        );
        let level = get(&["datadiscriminator", "levelofdetail"]);
        if (self.format == Format::Activity || !level.is_empty())
            && !level.eq_ignore_ascii_case("summary")
        {
            self.skip("lot or non-summary rows".into());
            return Ok(());
        }
        let asset = get(&["assetcategory", "assetclass", "securitytype", "type"]);
        let supported = match self.format {
            Format::Activity => asset.eq_ignore_ascii_case("stocks"),
            Format::Flex | Format::SectionedFlex if !asset.is_empty() => {
                asset.eq_ignore_ascii_case("STK") || asset.eq_ignore_ascii_case("stocks")
            }
            _ => !matches!(
                key(asset).as_str(),
                "cashandmoneymarket"
                    | "moneymarket"
                    | "option"
                    | "options"
                    | "opt"
                    | "fop"
                    | "future"
                    | "futures"
                    | "fut"
                    | "bond"
                    | "bonds"
                    | "forex"
                    | "warrant"
                    | "warrants"
                    | "war"
            ),
        };
        if !supported {
            self.skip(format!("unsupported asset type {asset:?}"));
            return Ok(());
        }
        let currency = get(&["currencyprimary", "currency"]);
        let symbol = match listing(&symbol, currency, get(&["listingexchange", "exchange"])) {
            Ok(listed) => listed,
            Err(e) => {
                self.skip(format!("{symbol}: {e}"));
                return Ok(());
            }
        };
        let read = |aliases| number(get(aliases), decimal_comma);
        let Some(qty) = read(QUANTITY) else {
            self.rejected
                .insert(symbol, format!("invalid quantity at line {}", row.line));
            return Ok(());
        };
        if qty == 0.0 {
            self.skip("zero quantity".into());
            return Ok(());
        }
        let raw_price = get(UNIT_COST);
        let price = if matches!(raw_price.to_ascii_lowercase().as_str(), "" | "--" | "n/a") {
            read(TOTAL_COST).map(|cost| {
                if qty < 0.0 {
                    cost.abs() / qty.abs()
                } else {
                    cost / qty
                }
            })
        } else {
            number(raw_price, decimal_comma)
        };
        let Some(avg_price) = price.filter(|v| v.is_finite() && *v >= 0.0) else {
            self.rejected
                .insert(symbol, format!("invalid cost at line {}", row.line));
            return Ok(());
        };
        if let Some(existing) = self.positions.get_mut(&symbol) {
            if existing.qty.signum() != qty.signum() {
                self.rejected
                    .insert(symbol, "both long and short positions".into());
            } else {
                let combined = existing.qty + qty;
                existing.avg_price += (avg_price - existing.avg_price) * (qty / combined);
                existing.qty = combined;
                if !combined.is_finite() {
                    self.rejected.insert(symbol, "quantity out of range".into());
                }
            }
        } else {
            self.positions.insert(
                symbol.clone(),
                Position {
                    symbol,
                    qty,
                    avg_price,
                },
            );
        }
        Ok(())
    }

    fn parse(text: &str) -> Result<Self> {
        let (format, sep) = detect(text)?;
        let mut import = Self {
            format,
            positions: BTreeMap::new(),
            skipped: BTreeMap::new(),
            rejected: BTreeMap::new(),
        };
        let mut headers = Vec::new();
        let mut pending = Vec::new();
        let mut current = None;
        let mut numbers = NumberFormat::default();
        let mut report_date = None;
        let mut flex = FlexSections {
            needs_header: true,
            ..Default::default()
        };
        for row in csv::read(text, sep)? {
            if row.cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            let row = if format == Format::SectionedFlex {
                let Some(row) = flex.position_row(row)? else {
                    continue;
                };
                row
            } else {
                row
            };
            // Flex queries can contain many sections and multiple daily snapshots.
            // Summing either would silently turn activity into current holdings.
            let section_marker = format != Format::Activity && row.cells.len() < 3;
            if section_marker && row.cells.first().is_some_and(|s| s == "BOS") {
                ensure!(
                    row.cells.get(1).is_some_and(|s| key(s) == "openpositions"),
                    "export just the Open Positions section of the Flex Query"
                );
                current = None;
                continue;
            }
            if section_marker
                && row
                    .cells
                    .first()
                    .is_some_and(|s| matches!(s.as_str(), "EOS" | "EOA" | "EOF"))
            {
                current = None;
                continue;
            }
            let is_header = if format == Format::Activity {
                activity_row(&row, "Open Positions", "Header")
            } else {
                Columns::new(&row.cells).has_positions()
            };
            if is_header {
                let cols = Columns::new(&row.cells);
                cols.validate()?;
                ensure!(
                    cols.index(&["tradeprice", "tradeid", "buysell"]).is_none(),
                    "export open positions, this header contains trade fields"
                );
                if format == Format::Activity {
                    ensure!(
                        cols.index(&["datadiscriminator"]).is_some()
                            && cols.index(&["assetcategory"]).is_some()
                            && cols.index(&["currency"]).is_some(),
                        "incomplete IBKR Open Positions header"
                    );
                }
                current = Some(headers.len());
                headers.push(cols);
            } else if let Some(index) = current
                && (format != Format::Activity || activity_row(&row, "Open Positions", "Data"))
            {
                let cols = &headers[index];
                let date = cols.field(&row, &["reportdate"]);
                if !date.is_empty() {
                    ensure!(
                        report_date.get_or_insert_with(|| date.to_owned()) == date,
                        "multiple report dates, export open positions for a single date"
                    );
                }
                if sep == ';' {
                    numbers.observe(cols, &row)?;
                }
                pending.push((index, row));
            }
        }
        let decimal_comma = numbers.decimal_comma()?;
        for (index, row) in pending {
            import.row(&headers[index], &row, decimal_comma)?;
        }
        for (symbol, reason) in std::mem::take(&mut import.rejected) {
            import.positions.remove(&symbol);
            import.skip(format!("{symbol}: {reason}"));
        }
        Ok(import)
    }

    fn reconcile(&self, cfg: &mut config::Config) -> (String, usize) {
        let mut warnings = Vec::new();
        let existing = config::resolve_positions(&cfg.positions, &mut warnings);
        let mut out = format!(
            "{}, {} position{}\n",
            self.format.label(),
            self.positions.len(),
            if self.positions.len() == 1 { "" } else { "s" }
        );
        for warning in warnings {
            writeln!(out, "  warning  {warning}").unwrap();
        }
        let mut changes = 0;
        for (symbol, p) in &self.positions {
            let old = existing.iter().find(|p| &p.symbol == symbol);
            let status = match old {
                None => "new",
                Some(prev) if prev == p => "same",
                Some(_) => "changed",
            };
            write!(
                out,
                "  {status:<8} {symbol:<10} {} @ {}",
                p.qty, p.avg_price
            )
            .unwrap();
            if let Some(prev) = old
                && prev != p
            {
                write!(out, "   was {} @ {}", prev.qty, prev.avg_price).unwrap();
            }
            if symbol.ends_with(".L") {
                out.push_str("   check units: London quotes may be in pence");
            }
            out.push('\n');
            if old != Some(p) {
                cfg.set_position(p);
                changes += 1;
            }
        }
        for p in existing
            .iter()
            .filter(|p| !self.positions.contains_key(&p.symbol))
        {
            writeln!(
                out,
                "  kept     {:<10} {} @ {}",
                p.symbol, p.qty, p.avg_price
            )
            .unwrap();
        }
        out.push_str(&self.skips());
        (out, changes)
    }

    fn skips(&self) -> String {
        let mut out = String::new();
        for (reason, n) in &self.skipped {
            writeln!(out, "  skipped  {reason} ({n})").unwrap();
        }
        out
    }
}

pub fn run(
    file: &Path,
    path: &Path,
    dry_run: bool,
    source: Option<&str>,
    every: Option<u64>,
) -> Result<()> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("read positions CSV {}", file.display()))?;
    let import = Import::parse(&text)?;
    ensure!(
        !import.positions.is_empty(),
        "no positions to import\n{}",
        import.skips()
    );
    let (cfg, (report, changes)) = if dry_run {
        let mut cfg = config::load_from(path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "fix {} before importing: {}",
                    path.display(),
                    config::load_error(&e)
                )
            })?
            .unwrap_or_default();
        let result = import.reconcile(&mut cfg);
        (cfg, result)
    } else {
        config::update_positions(path, |cfg| import.reconcile(cfg))?
    };
    print!("{report}");
    if dry_run {
        println!("Dry run, nothing written.");
    } else if changes == 0 {
        println!("Already up to date.");
    } else {
        println!(
            "Wrote {changes} change{} to {}. Restart open panels to see them.",
            if changes == 1 { "" } else { "s" },
            path.display()
        );
    }
    let (settings, _) = config::resolve(&cfg, None);
    if let Ok(watchlist) = crate::startup_symbols(&[], &cfg.watchlist) {
        let count = crate::portfolio::polled_symbols(&watchlist, &settings.positions).len();
        crate::warn_rate(
            &cfg,
            source
                .or(cfg.source.as_deref())
                .unwrap_or(crate::source::registry::SOURCES[0].id),
            every.or(cfg.every).unwrap_or(15),
            count,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_uses_only_open_stock_summaries() {
        let import =
            Import::parse(include_str!("../tests/fixtures/import/ibkr_activity.csv")).unwrap();
        assert_eq!(import.positions.len(), 2);
        assert_eq!(import.positions["BRK-B"].qty, 2.0);
        assert_eq!(import.positions["XYZ"].qty, -5.0);
        assert_eq!(import.positions["XYZ"].avg_price, 31.0);
        assert!(import.skips().contains("ASML: EUR"));
        assert!(import.skips().contains("Futures"));
    }

    #[test]
    fn flex_resolves_listings_and_combines_accounts() {
        let mut cfg = config::Config::default();
        let import = Import::parse(include_str!("../tests/fixtures/import/ibkr_flex.csv")).unwrap();
        assert_eq!(import.positions["0700.HK"].avg_price, 151.0);
        assert_eq!(import.positions["7203.T"].avg_price, 201.0);
        assert_eq!(import.positions["XYZ.TO"].qty, 2.0);
        assert!(import.reconcile(&mut cfg).0.contains("pence"));

        let import = Import::parse(include_str!(
            "../tests/fixtures/import/ibkr_flex_multi_account.csv"
        ))
        .unwrap();
        assert_eq!(import.positions.len(), 1);
        assert_eq!(import.positions["AAPL"].qty, 5.0);
        assert_eq!(import.positions["AAPL"].avg_price, 115.0);
        assert!(import.skips().contains("XYZ: both long and short"));
    }

    #[test]
    fn broker_headers_preambles_delimiters_and_cost_columns() {
        for (csv, symbol, qty, price) in [
            (
                include_str!("../tests/fixtures/import/fidelity.csv"),
                "MSFT",
                2.0,
                20.0,
            ),
            (
                include_str!("../tests/fixtures/import/schwab.csv"),
                "ABC",
                10.0,
                30.0,
            ),
            (
                include_str!("../tests/fixtures/import/etrade.csv"),
                "FOO",
                3.0,
                17.0,
            ),
            (
                include_str!("../tests/fixtures/import/semicolon_decimal_comma.csv"),
                "ABC",
                2.5,
                1234.56,
            ),
            (
                include_str!("../tests/fixtures/import/bom_crlf.csv"),
                "AAA",
                -2.0,
                2345.10,
            ),
        ] {
            let import = Import::parse(csv).unwrap();
            assert_eq!(import.positions.len(), 1);
            assert_eq!(import.positions[symbol].qty, qty);
            assert_eq!(import.positions[symbol].avg_price, price);
        }
        assert!(
            Import::parse(include_str!(
                "../tests/fixtures/import/vanguard_no_cost.csv"
            ))
            .err()
            .unwrap()
            .to_string()
            .contains("no average cost")
        );
    }

    #[test]
    fn semicolon_numbers_use_one_format_for_the_file() {
        for (qty, price) in [("1.234", "2,50"), ("1,234", "2.50")] {
            let import = Import::parse(&format!("Symbol;Qty;Avg Cost\nXYZ;{qty};{price}")).unwrap();
            assert_eq!(import.positions["XYZ"].qty, 1234.0);
            assert_eq!(import.positions["XYZ"].avg_price, 2.5);
        }
        assert!(Import::parse("Symbol;Qty;Avg Cost\nXYZ;1.234;10").is_err());
        assert!(Import::parse("Symbol;Qty;Avg Cost\nXYZ;2,50;2.50").is_err());
    }

    #[test]
    fn flex_section_headers_separate_activity_and_cost_bases() {
        for (header, data) in [("", ""), ("POST,HEADER,", "POST,DATA,")] {
            let csv = format!(
                "TRNT,BOS,Transactions\nTRNT,HEADER,Symbol,Quantity,CostBasisPrice\nTRNT,DATA,WRONG,4,20\nTRNT,EOS,1,80\nPOST,BOS,Positions\n{header}Symbol,Quantity,CostBasisPrice,AssetClass\n{data}XYZ,2,30,STK\nPOST,EOS,1,60"
            );
            let import = Import::parse(&csv).unwrap();
            assert_eq!(import.positions.len(), 1);
            assert_eq!(import.positions["XYZ"].qty, 2.0);
            assert!(Import::parse(&format!("{csv}\nPOSS,BOS,Positions")).is_err());
        }
    }

    #[test]
    fn flex_section_codes_can_also_be_tickers() {
        let import = Import::parse("Symbol,Shares,Avg Price\nBOS,1,2\nEOS,3,4").unwrap();
        assert_eq!(import.positions["BOS"].qty, 1.0);
        assert_eq!(import.positions["EOS"].qty, 3.0);
    }

    #[test]
    fn malformed_or_partial_holdings_are_never_committed_as_a_subtotal() {
        for text in [
            "Symbol,Qty,Cost Basis\nAAA,2,\"123",
            "Symbol,Qty,Cost Basis\nAAA,1,2,3",
            "Symbol,Qty,Cost Basis,ReportDate\nAAA,1,20,20260102\nAAA,1,10,20260103",
        ] {
            assert!(Import::parse(text).is_err(), "{text}");
        }
        let partial = Import::parse(
            "Symbol,Qty,Avg Cost\nABC,2,30\nDEF,3,20\nABC,3,bad\nGHI,NaN,12\nXYZ,1,\"1,2\"",
        )
        .unwrap();
        assert_eq!(partial.positions.len(), 1);
        assert!(partial.positions.contains_key("DEF"));
        assert!(partial.skips().contains("ABC: invalid cost"));
    }
}
