//! A strict reader of the text format, for the tests: it accepts what the format allows and nothing else (no duplicate
//! series, a family's lines together, a type before its samples, histograms whose buckets add up), so a page the
//! server writes that Prometheus would refuse is refused here too.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

#[derive(Debug, Clone)]
pub struct Family {
    pub name: String,
    pub kind: String,
    pub help: String,
    pub samples: Vec<Sample>,
}

pub fn parse(text: &str) -> Result<Vec<Family>, String> {
    if !text.ends_with('\n') {
        return Err("the page doesn't end with a line feed".into());
    }
    let mut families: Vec<Family> = Vec::new();
    let mut seen = BTreeSet::new();
    for (n, line) in text.lines().enumerate().map(|(i, l)| (i + 1, l)) {
        let at = |why: String| format!("line {n}: {why}: {line:?}");
        if line.is_empty() {
            return Err(at("an empty line".into()));
        }
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let (name, help) = rest.split_once(' ').ok_or_else(|| at("a help line without a text".into()))?;
            if !valid_name(name, true) {
                return Err(at("a bad family name".into()));
            }
            if families.iter().any(|f| f.name == name) {
                return Err(at("a family written twice".into()));
            }
            check_help(help).map_err(&at)?;
            families.push(Family { name: name.into(), kind: String::new(), help: help.into(), samples: vec![] });
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').ok_or_else(|| at("a type line without a type".into()))?;
            let family = families.last_mut().filter(|f| f.name == name && f.kind.is_empty()).ok_or_else(|| at("a type without its help line just before".into()))?;
            if !["counter", "gauge", "histogram"].contains(&kind) {
                return Err(at(format!("the type {kind}")));
            }
            if kind == "counter" && !name.ends_with("_total") {
                return Err(at("a counter whose name doesn't end in _total".into()));
            }
            family.kind = kind.into();
        } else if line.starts_with('#') {
            return Err(at("a comment".into()));
        } else {
            let sample = parse_sample(line).map_err(&at)?;
            let family = families.last_mut().ok_or_else(|| at("a sample before any family".into()))?;
            if family.kind.is_empty() {
                return Err(at("a sample before its type".into()));
            }
            let belongs = if family.kind == "histogram" {
                ["_bucket", "_sum", "_count"].iter().any(|s| sample.name == format!("{}{s}", family.name))
            } else {
                sample.name == family.name
            };
            if !belongs {
                return Err(at(format!("a sample of another family than {}", family.name)));
            }
            let key = format!("{}|{:?}", sample.name, sample.labels);
            if !seen.insert(key) {
                return Err(at("the same series twice".into()));
            }
            family.samples.push(sample);
        }
    }
    for f in &families {
        if f.kind.is_empty() {
            return Err(format!("{}: no type", f.name));
        }
        if f.samples.is_empty() {
            return Err(format!("{}: no samples", f.name));
        }
        if f.kind == "histogram" {
            check_histogram(f)?;
        }
    }
    Ok(families)
}

fn check_help(help: &str) -> Result<(), String> {
    let mut chars = help.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && !matches!(chars.next(), Some('\\' | 'n')) {
            return Err("a bad escape in a help text".into());
        }
    }
    Ok(())
}

fn valid_name(name: &str, colon: bool) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || (colon && c == ':'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || (colon && c == ':'))
}

fn parse_sample(line: &str) -> Result<Sample, String> {
    let name_end = line.find(['{', ' ']).ok_or("no value")?;
    let name = &line[..name_end];
    if !valid_name(name, true) {
        return Err("a bad metric name".into());
    }
    let mut rest = &line[name_end..];
    let mut labels = BTreeMap::new();
    if let Some(inner) = rest.strip_prefix('{') {
        let (parsed, after) = parse_labels(inner)?;
        labels = parsed;
        rest = after;
    }
    let value = rest.strip_prefix(' ').ok_or("no space before the value")?;
    if value.contains(' ') {
        return Err("a timestamp or more after the value".into());
    }
    let value = match value {
        "NaN" => f64::NAN,
        "+Inf" => f64::INFINITY,
        "-Inf" => f64::NEG_INFINITY,
        v => v.parse::<f64>().map_err(|_| "a value that is not a number")?,
    };
    Ok(Sample { name: name.into(), labels, value })
}

/// Reads `a="1",b="2"}` and what follows the closing brace
fn parse_labels(s: &str) -> Result<(BTreeMap<String, String>, &str), String> {
    let mut labels = BTreeMap::new();
    let mut rest = s;
    loop {
        if let Some(after) = rest.strip_prefix('}') {
            return Ok((labels, after));
        }
        if !labels.is_empty() {
            rest = rest.strip_prefix(',').ok_or("labels not separated by a comma")?;
        }
        let eq = rest.find('=').ok_or("a label without =")?;
        let name = &rest[..eq];
        if !valid_name(name, false) {
            return Err(format!("a bad label name {name:?}"));
        }
        rest = rest[eq + 1..].strip_prefix('"').ok_or("a label value without a quote")?;
        let mut value = String::new();
        let mut chars = rest.char_indices();
        let end = loop {
            match chars.next() {
                None => return Err("a label value that doesn't end".into()),
                Some((i, '"')) => break i,
                Some((_, '\\')) => match chars.next() {
                    Some((_, '\\')) => value.push('\\'),
                    Some((_, '"')) => value.push('"'),
                    Some((_, 'n')) => value.push('\n'),
                    Some((_, 'r')) => value.push('\r'),
                    _ => return Err("a bad escape in a label value".into()),
                },
                Some((_, '\n')) => return Err("a raw line feed in a label value".into()),
                Some((_, c)) => value.push(c),
            }
        };
        if labels.insert(name.to_string(), value).is_some() {
            return Err(format!("the label {name} twice"));
        }
        rest = &rest[end + 1..];
    }
}

/// Per series of a histogram: buckets with growing bounds, counts that don't fall, the last bound +Inf, whose count is
/// `_count`, and a `_sum`
fn check_histogram(f: &Family) -> Result<(), String> {
    let mut series: BTreeMap<String, Vec<&Sample>> = BTreeMap::new();
    for s in &f.samples {
        let mut labels = s.labels.clone();
        labels.remove("le");
        series.entry(format!("{labels:?}")).or_default().push(s);
    }
    for (key, samples) in series {
        let buckets: Vec<&&Sample> = samples.iter().filter(|s| s.name.ends_with("_bucket")).collect();
        let (mut last_bound, mut last_count) = (f64::NEG_INFINITY, 0.0);
        for b in &buckets {
            let le = b.labels.get("le").ok_or_else(|| format!("{}: a bucket without le", f.name))?;
            let bound = if le == "+Inf" { f64::INFINITY } else { le.parse::<f64>().map_err(|_| format!("{}: le {le}", f.name))? };
            if bound <= last_bound {
                return Err(format!("{} {key}: the bounds don't grow at {le}", f.name));
            }
            if b.value < last_count {
                return Err(format!("{} {key}: the counts fall at {le}", f.name));
            }
            (last_bound, last_count) = (bound, b.value);
        }
        if last_bound != f64::INFINITY {
            return Err(format!("{} {key}: no +Inf bucket", f.name));
        }
        let count = samples.iter().find(|s| s.name.ends_with("_count")).ok_or_else(|| format!("{} {key}: no _count", f.name))?;
        if samples.iter().all(|s| !s.name.ends_with("_sum")) {
            return Err(format!("{} {key}: no _sum", f.name));
        }
        if count.value != last_count {
            return Err(format!("{} {key}: _count is not the +Inf bucket", f.name));
        }
    }
    Ok(())
}
