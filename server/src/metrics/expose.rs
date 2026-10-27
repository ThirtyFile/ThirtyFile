//! The Prometheus text format (version 0.0.4): families written one after the other, each with its help and type lines
//! and then its samples. Names, label names and values are written as the format asks (values escaped), and a family
//! with no samples is not written at all.

use std::fmt::Write;

/// A page of metrics being written
#[derive(Default)]
pub struct Text {
    out: String,
}

/// What a family's samples mean
#[derive(Clone, Copy)]
enum Kind {
    Counter,
    Gauge,
    Histogram,
}

impl Text {
    pub fn new() -> Text {
        Text::default()
    }

    /// Starts a family of counters (a total that only grows; its name ends in `_total`)
    pub fn counter(&mut self, name: &str, help: &str) -> Family<'_> {
        self.family(name, help, Kind::Counter)
    }

    /// Starts a family of gauges (a value that goes up and down)
    pub fn gauge(&mut self, name: &str, help: &str) -> Family<'_> {
        self.family(name, help, Kind::Gauge)
    }

    /// Starts a family of histograms
    pub fn histogram(&mut self, name: &str, help: &str) -> Family<'_> {
        self.family(name, help, Kind::Histogram)
    }

    fn family(&mut self, name: &str, help: &str, kind: Kind) -> Family<'_> {
        debug_assert!(valid_name(name), "{name}");
        let start = self.out.len();
        let kind_word = match kind {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        };
        let _ = writeln!(self.out, "# HELP {name} {}", escape_help(help));
        let _ = writeln!(self.out, "# TYPE {name} {kind_word}");
        Family { text: self, name: name.to_string(), start, samples: 0 }
    }

    pub fn finish(self) -> String {
        self.out
    }
}

/// One family: its samples are added until it is dropped, and with none its lines are taken back
pub struct Family<'a> {
    text: &'a mut Text,
    name: String,
    start: usize,
    samples: usize,
}

impl Family<'_> {
    /// A sample of a counter or a gauge
    pub fn sample(&mut self, labels: &[(&str, &str)], value: f64) -> &mut Self {
        let _ = writeln!(self.text.out, "{}{} {}", self.name, label_text(labels, None), number(value));
        self.samples += 1;
        self
    }

    /// A histogram's series: `bounds` are the upper bounds (without +Inf), `cumulative` the number of observations at
    /// or below each of them, and `count` and `sum` all of them
    pub fn histogram(&mut self, labels: &[(&str, &str)], bounds: &[f64], cumulative: &[u64], sum: f64, count: u64) -> &mut Self {
        debug_assert_eq!(bounds.len(), cumulative.len());
        for (bound, n) in bounds.iter().zip(cumulative) {
            let _ = writeln!(self.text.out, "{}_bucket{} {}", self.name, label_text(labels, Some(&number(*bound))), n);
        }
        let _ = writeln!(self.text.out, "{}_bucket{} {}", self.name, label_text(labels, Some("+Inf")), count);
        let _ = writeln!(self.text.out, "{}_sum{} {}", self.name, label_text(labels, None), number(sum));
        let _ = writeln!(self.text.out, "{}_count{} {}", self.name, label_text(labels, None), count);
        self.samples += 1;
        self
    }
}

impl Drop for Family<'_> {
    fn drop(&mut self) {
        if self.samples == 0 {
            self.text.out.truncate(self.start);
        }
    }
}

/// `{a="1",b="2"}` (or nothing without labels), with `le` last when a bucket's bound is given
fn label_text(labels: &[(&str, &str)], le: Option<&str>) -> String {
    if labels.is_empty() && le.is_none() {
        return String::new();
    }
    let mut out = String::from("{");
    for (i, (name, value)) in labels.iter().enumerate() {
        debug_assert!(valid_name(name) && !name.contains(':'), "{name}");
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{name}=\"{}\"", escape_value(value));
    }
    if let Some(le) = le {
        if !labels.is_empty() {
            out.push(',');
        }
        let _ = write!(out, "le=\"{le}\"");
    }
    out.push('}');
    out
}

/// A label value: backslash, double quote and line feed escaped, which is all the format asks
fn escape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            // A carriage return would end the line for some readers
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// A help text: backslash and line feed escaped
fn escape_help(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\n', "\\n")
}

/// A sample's value: whole numbers without a fraction, the rest as the shortest text that reads back the same
fn number(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "+Inf" } else { "-Inf" }.into()
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else if v.abs() >= 1e15 || (v != 0.0 && v.abs() < 1e-6) {
        // Written out in full, the very large and the very small would be hundreds of digits
        format!("{v:e}")
    } else {
        format!("{v}")
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

#[cfg(test)]
pub(crate) mod parse;
#[cfg(test)]
mod tests;
