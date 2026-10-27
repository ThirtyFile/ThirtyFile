use super::{parse::parse, *};

fn page(build: impl FnOnce(&mut Text)) -> String {
    let mut t = Text::new();
    build(&mut t);
    t.finish()
}

#[test]
fn a_counter_and_a_gauge_are_written_as_the_format_asks() {
    let out = page(|t| {
        t.counter("thirtyfile_things_total", "Things done").sample(&[("kind", "a")], 3.0).sample(&[("kind", "b")], 0.0);
        t.gauge("thirtyfile_level", "How high").sample(&[], 1.5);
    });
    assert_eq!(
        out,
        "# HELP thirtyfile_things_total Things done\n# TYPE thirtyfile_things_total counter\nthirtyfile_things_total{kind=\"a\"} 3\nthirtyfile_things_total{kind=\"b\"} 0\n\
         # HELP thirtyfile_level How high\n# TYPE thirtyfile_level gauge\nthirtyfile_level 1.5\n"
    );
    assert_eq!(parse(&out).unwrap().len(), 2);
}

#[test]
fn a_family_without_samples_is_not_written() {
    let out = page(|t| {
        t.gauge("thirtyfile_empty", "Nothing");
        t.gauge("thirtyfile_one", "One").sample(&[], 1.0);
        t.counter("thirtyfile_none_total", "None");
    });
    assert_eq!(out, "# HELP thirtyfile_one One\n# TYPE thirtyfile_one gauge\nthirtyfile_one 1\n");
}

#[test]
fn numbers_are_written_whole_or_as_short_as_they_can() {
    for (v, text) in [
        (0.0, "0"),
        (-0.0, "0"),
        (42.0, "42"),
        (1.5, "1.5"),
        (0.001, "0.001"),
        (1e15, "1e15"),
        (999999999999999.0, "999999999999999"),
        (1.5e300, "1.5e300"),
        (1e-7, "1e-7"),
        (0.000001, "0.000001"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "+Inf"),
        (f64::NEG_INFINITY, "-Inf"),
        (123456789012.0, "123456789012"),
    ] {
        assert_eq!(number(v), text);
    }
    for v in [0.1, 0.30000000000000004, 1.0 / 3.0, 123.456e-9] {
        assert_eq!(number(v).parse::<f64>().unwrap(), v, "{v} reads back as the same");
    }
}

#[test]
fn hostile_label_values_are_escaped_and_read_back_as_they_were() {
    let hostile = ["a\"b", "a\\b", "line\nbreak", "cr\rlf", "}{,=", "# HELP x y", "ünïcödé 檔案 🙂", "", "\\\"", "a b"];
    let out = page(|t| {
        let mut g = t.gauge("thirtyfile_hostile", "Help with a \\ and a\nline feed");
        for (i, v) in hostile.iter().enumerate() {
            g.sample(&[("value", v), ("n", &i.to_string())], i as f64);
        }
    });
    let families = parse(&out).unwrap_or_else(|e| panic!("{e}\n{out}"));
    let read: Vec<&str> = families[0].samples.iter().map(|s| s.labels["value"].as_str()).collect();
    assert_eq!(read, hostile);
    assert_eq!(families[0].help, "Help with a \\\\ and a\\nline feed", "the help is escaped too");
    assert_eq!(out.lines().count(), 2 + hostile.len(), "a value never makes another line");
}

#[test]
fn a_histogram_has_cumulative_buckets_a_sum_and_a_count() {
    let out = page(|t| {
        t.histogram("thirtyfile_wait_seconds", "Waits").histogram(&[("kind", "x")], &[0.1, 1.0, 10.0], &[1, 3, 3], 4.5, 4).histogram(&[], &[0.1], &[0], 0.0, 0);
    });
    assert!(out.contains("thirtyfile_wait_seconds_bucket{kind=\"x\",le=\"0.1\"} 1\n"));
    assert!(out.contains("thirtyfile_wait_seconds_bucket{kind=\"x\",le=\"+Inf\"} 4\n"));
    assert!(out.contains("thirtyfile_wait_seconds_sum{kind=\"x\"} 4.5\n"));
    assert!(out.contains("thirtyfile_wait_seconds_count{kind=\"x\"} 4\n"));
    assert!(out.contains("thirtyfile_wait_seconds_bucket{le=\"+Inf\"} 0\n"));
    assert!(parse(&out).is_ok());
}

// ───────────── The reader is strict ─────────────

#[test]
fn the_reader_refuses_what_the_format_does_not_allow() {
    let ok = "# HELP a_total x\n# TYPE a_total counter\na_total 1\n";
    assert!(parse(ok).is_ok());
    for (what, bad) in [
        ("no final line feed", "# HELP a x\n# TYPE a gauge\na 1".to_string()),
        ("an empty line", format!("{ok}\n")),
        ("a counter not ending in _total", "# HELP a x\n# TYPE a counter\na 1\n".into()),
        ("a sample before its type", "# HELP a x\na 1\n".into()),
        ("the same series twice", "# HELP a x\n# TYPE a gauge\na{k=\"v\"} 1\na{k=\"v\"} 2\n".into()),
        ("a family twice", "# HELP a x\n# TYPE a gauge\na 1\n# HELP a x\n# TYPE a gauge\na 2\n".into()),
        ("a sample of another family", "# HELP a x\n# TYPE a gauge\nb 1\n".into()),
        ("a family with no samples", "# HELP a x\n# TYPE a gauge\n".into()),
        ("a bad escape", "# HELP a x\n# TYPE a gauge\na{k=\"\\t\"} 1\n".into()),
        ("an unterminated value", "# HELP a x\n# TYPE a gauge\na{k=\"v} 1\n".into()),
        ("a duplicate label", "# HELP a x\n# TYPE a gauge\na{k=\"1\",k=\"2\"} 1\n".into()),
        ("a bad label name", "# HELP a x\n# TYPE a gauge\na{0k=\"1\"} 1\n".into()),
        ("a value that is not a number", "# HELP a x\n# TYPE a gauge\na one\n".into()),
        ("a timestamp", "# HELP a x\n# TYPE a gauge\na 1 1700000000\n".into()),
        ("a comment", format!("# just a note\n{ok}")),
        ("a type that is not known", "# HELP a x\n# TYPE a summary\na 1\n".into()),
    ] {
        assert!(parse(&bad).is_err(), "{what} should be refused:\n{bad}");
    }
}

#[test]
fn the_reader_refuses_a_histogram_that_does_not_add_up() {
    let good = |buckets: &str, count: &str| format!("# HELP h x\n# TYPE h histogram\n{buckets}h_sum 1\nh_count {count}\n");
    assert!(parse(&good("h_bucket{le=\"1\"} 1\nh_bucket{le=\"+Inf\"} 2\n", "2")).is_ok());
    for (what, bad) in [
        ("counts that fall", good("h_bucket{le=\"1\"} 3\nh_bucket{le=\"+Inf\"} 2\n", "2")),
        ("bounds that don't grow", good("h_bucket{le=\"2\"} 1\nh_bucket{le=\"1\"} 1\nh_bucket{le=\"+Inf\"} 2\n", "2")),
        ("no +Inf bucket", good("h_bucket{le=\"1\"} 1\n", "1")),
        ("a count that is not the +Inf bucket", good("h_bucket{le=\"1\"} 1\nh_bucket{le=\"+Inf\"} 2\n", "3")),
        ("no sum", "# HELP h x\n# TYPE h histogram\nh_bucket{le=\"+Inf\"} 1\nh_count 1\n".to_string()),
    ] {
        assert!(parse(&bad).is_err(), "{what} should be refused");
    }
}
