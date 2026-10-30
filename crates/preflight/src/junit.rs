use crate::model::{TestCase, TestCounts, TestFailure, TestOutput, TestReport};
use anyhow::{Context, Result, ensure};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

pub fn read(path: &Path) -> Result<TestReport> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("missing JUnit report {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "JUnit report is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4 * 1024 * 1024, "JUnit report exceeds 4 MiB");
    parse(std::str::from_utf8(&bytes).context("JUnit report is not UTF-8")?)
}
struct Suite {
    name: String,
    before: TestCounts,
    tests: Option<u64>,
    failed: Option<u64>,
    skipped: Option<u64>,
}
struct Case {
    output: TestOutput,
    file: Option<String>,
    line: Option<u64>,
    duration: Option<f64>,
    name: String,
    class_name: String,
    message: String,
    failed: bool,
    skipped: bool,
}
fn attr(event: &BytesStart<'_>, name: &str) -> Result<Option<String>> {
    for attribute in event.attributes() {
        let attribute = attribute?;
        if attribute.key.as_ref() == name {
            return Ok(Some(
                attribute
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}
fn count(event: &BytesStart<'_>, name: &str) -> Result<Option<u64>> {
    attr(event, name)?
        .map(|s| s.parse::<u64>().context("invalid JUnit count"))
        .transpose()
}
fn total(c: &TestCounts) -> u64 {
    c.passed + c.failed + c.skipped
}
pub fn parse(xml: &str) -> Result<TestReport> {
    ensure!(xml.len() <= 4 * 1024 * 1024, "JUnit report exceeds 4 MiB");
    let mut reader = Reader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut report = TestReport {
        cases_complete: true,
        ..TestReport::default()
    };
    let mut occurrences = std::collections::HashMap::<String, u64>::new();
    let mut suites = Vec::<Suite>::new();
    let mut case: Option<Case> = None;
    let mut depth = 0usize;
    let mut seen_root = false;
    let mut failure_depth = None;
    let mut output_depth: Option<(usize, bool)> = None;
    loop {
        match reader.read_event()? {
            Event::Start(event) => {
                let name = event.local_name();
                let name = name.as_ref();
                if depth == 0 {
                    ensure!(
                        !seen_root && matches!(name, "testsuite" | "testsuites"),
                        "invalid JUnit root"
                    );
                    seen_root = true;
                }
                depth += 1;
                ensure!(depth <= 64, "JUnit XML nesting exceeds 64");
                match name {
                    "testsuite" | "testsuites" => {
                        ensure!(case.is_none(), "nested suite inside testcase");
                        let failures = count(&event, "failures")?;
                        let errors = count(&event, "errors")?;
                        let failed = match (failures, errors) {
                            (None, None) => None,
                            (a, b) => Some(
                                a.unwrap_or(0)
                                    .checked_add(b.unwrap_or(0))
                                    .context("JUnit count overflow")?,
                            ),
                        };
                        suites.push(Suite {
                            name: attr(&event, "name")?
                                .unwrap_or_default()
                                .chars()
                                .take(256)
                                .collect(),
                            before: report.counts.clone(),
                            tests: count(&event, "tests")?,
                            failed,
                            skipped: count(&event, "skipped")?,
                        });
                    }
                    "testcase" => {
                        ensure!(
                            !suites.is_empty() && case.is_none(),
                            "invalid nested testcase"
                        );
                        case = Some(Case {
                            output: TestOutput::default(),
                            file: attr(&event, "file")?.filter(|v| v.len() <= 4096),
                            line: count(&event, "line")?.filter(|v| *v > 0 && *v <= 10_000_000),
                            duration: attr(&event, "time")?
                                .and_then(|s| s.parse::<f64>().ok())
                                .filter(|v| v.is_finite() && *v >= 0.0),
                            name: attr(&event, "name")?
                                .unwrap_or_else(|| "unnamed".into())
                                .chars()
                                .take(256)
                                .collect(),
                            class_name: attr(&event, "classname")?
                                .unwrap_or_default()
                                .chars()
                                .take(256)
                                .collect(),
                            message: String::new(),
                            failed: false,
                            skipped: false,
                        });
                    }
                    "system-out" | "system-err" if case.is_some() => {
                        output_depth = Some((depth, name == "system-err"));
                    }
                    "failure" | "error" => {
                        let case = case.as_mut().context("failure outside testcase")?;
                        case.failed = true;
                        if let Some(message) = attr(&event, "message")? {
                            case.message.extend(message.chars().take(2048));
                        }
                        failure_depth = Some(depth);
                    }
                    "skipped" => {
                        case.as_mut().context("skipped outside testcase")?.skipped = true;
                    }
                    _ => {}
                }
            }
            Event::End(event) => {
                ensure!(depth > 0, "unexpected XML end");
                if output_depth.is_some_and(|(level, _)| level == depth) {
                    output_depth = None;
                }
                if failure_depth == Some(depth) {
                    failure_depth = None;
                }
                match event.local_name().as_ref() {
                    "testcase" => {
                        let c = case.take().context("testcase end without start")?;
                        let suite = suites
                            .iter()
                            .map(|s| s.name.as_str())
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join("/");
                        let key = serde_json::to_string(&(&suite, &c.class_name, &c.name))?;
                        let occurrence = occurrences.entry(key.clone()).or_default();
                        let id = crate::hashing::hex(Sha256::digest(
                            format!("{key}:{occurrence}").as_bytes(),
                        ));
                        *occurrence += 1;
                        if report.cases.len() < 10000 {
                            report.cases.push(TestCase {
                                output: (!c.output.stdout.is_empty()
                                    || !c.output.stderr.is_empty())
                                .then_some(c.output),
                                id,
                                name: c.name.clone(),
                                class_name: c.class_name.clone(),
                                suite,
                                status: if c.failed {
                                    "failed"
                                } else if c.skipped {
                                    "skipped"
                                } else {
                                    "passed"
                                }
                                .into(),
                                message: c.message.clone(),
                                file: c.file,
                                line: c.line,
                                duration_seconds: c.duration,
                            });
                        } else {
                            report.cases_complete = false;
                        }

                        if c.failed {
                            report.counts.failed += 1;
                            if report.failures.len() < 20 {
                                report.failures.push(TestFailure {
                                    name: c.name,
                                    class_name: c.class_name,
                                    message: c.message,
                                })
                            } else {
                                report.truncated = true;
                            }
                        } else if c.skipped {
                            report.counts.skipped += 1;
                        } else {
                            report.counts.passed += 1;
                        }
                    }
                    "testsuite" | "testsuites" => {
                        let suite = suites.pop().context("suite end without start")?;
                        let actual = total(&report.counts) - total(&suite.before);
                        if actual == 0 && suite.tests.unwrap_or(0) > 0 {
                            let tests = suite.tests.unwrap();
                            let failed = suite
                                .failed
                                .context("summary-only JUnit requires failures/errors count")?;
                            let skipped = suite.skipped.unwrap_or(0);
                            ensure!(
                                tests <= 100_000 && failed <= tests && skipped <= tests - failed,
                                "inconsistent JUnit summary"
                            );
                            report.counts.failed += failed;
                            report.counts.skipped += skipped;
                            report.counts.passed += tests - failed - skipped;
                            report.truncated |= failed > 0;
                            report.cases_complete = false;
                        } else {
                            if let Some(tests) = suite.tests {
                                ensure!(
                                    tests == actual,
                                    "JUnit tests count does not match testcases"
                                );
                            }
                            if let Some(failed) = suite.failed {
                                ensure!(
                                    failed == report.counts.failed - suite.before.failed,
                                    "JUnit failure count does not match testcases"
                                );
                            }
                            if let Some(skipped) = suite.skipped {
                                ensure!(
                                    skipped == report.counts.skipped - suite.before.skipped,
                                    "JUnit skipped count does not match testcases"
                                );
                            }
                        }
                    }
                    _ => {}
                }
                depth -= 1;
                ensure!(
                    total(&report.counts) <= 100_000,
                    "JUnit exceeds 100000 cases"
                );
            }
            Event::Text(text) => {
                if let Some((_, stderr)) = output_depth
                    && let Some(case) = case.as_mut()
                {
                    capture_output(&mut case.output, stderr, &text.xml11_content());
                }
                if depth == 0 {
                    ensure!(
                        text.xml11_content().trim().is_empty(),
                        "text outside JUnit root"
                    );
                }
                if failure_depth.is_some()
                    && let Some(case) = case.as_mut()
                {
                    let text = text.xml11_content();
                    let remaining = 2048usize.saturating_sub(case.message.chars().count());
                    case.message.extend(text.chars().take(remaining));
                }
            }
            Event::CData(text) => {
                if let Some((_, stderr)) = output_depth
                    && let Some(case) = case.as_mut()
                {
                    capture_output(&mut case.output, stderr, &text);
                }
                if failure_depth.is_some()
                    && let Some(case) = case.as_mut()
                {
                    let text = text.as_ref();
                    let remaining = 2048usize.saturating_sub(case.message.chars().count());
                    case.message.extend(text.chars().take(remaining));
                }
            }
            Event::GeneralRef(reference) => {
                let encoded = format!("&{};", reference.as_ref());
                let decoded = quick_xml::escape::unescape(&encoded)?;
                if let Some((_, stderr)) = output_depth
                    && let Some(case) = case.as_mut()
                {
                    capture_output(&mut case.output, stderr, &decoded);
                }
                if failure_depth.is_some()
                    && let Some(case) = case.as_mut()
                {
                    let remaining = 2048usize.saturating_sub(case.message.chars().count());
                    case.message.extend(decoded.chars().take(remaining));
                }
            }
            Event::DocType(_) => anyhow::bail!("JUnit DTDs are not supported"),
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(
        seen_root && depth == 0 && case.is_none() && suites.is_empty(),
        "incomplete JUnit XML"
    );
    Ok(report)
}

fn capture_output(output: &mut TestOutput, stderr: bool, text: &str) {
    let target = if stderr {
        &mut output.stderr
    } else {
        &mut output.stdout
    };
    for ch in text.chars() {
        if target.len() + ch.len_utf8() > 65536 {
            output.truncated = true;
            break;
        }
        target.push(ch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_entities_and_cdata_preserve_test_evidence() {
        let report = parse(
            r#"<testsuite name="suite &amp; tools"><testcase name="unicode-é" file="tests/a&amp;b.rs"><failure message="bad &lt;value&gt;">detail &amp; &#xE9;<![CDATA[ <raw>]]></failure><system-out>out &lt;ok&gt;<![CDATA[ & raw]]></system-out><system-err>&#233;</system-err></testcase></testsuite>"#,
        )
        .unwrap();
        assert_eq!(report.counts.failed, 1);
        let case = &report.cases[0];
        assert_eq!(case.suite, "suite & tools");
        assert_eq!(case.name, "unicode-é");
        assert_eq!(case.file.as_deref(), Some("tests/a&b.rs"));
        assert_eq!(case.message, "bad <value>detail & é <raw>");
        let output = case.output.as_ref().unwrap();
        assert_eq!(output.stdout, "out <ok> & raw");
        assert_eq!(output.stderr, "é");
        assert!(
            parse(
                "<testsuite><testcase name='x'><failure>&unknown;</failure></testcase></testsuite>"
            )
            .is_err()
        );
    }
    #[test]
    fn nested_suites_count_once_and_errors_are_failures() {
        let report=parse(r#"<testsuites tests="4"><testsuite tests="4" failures="1" errors="1" skipped="1"><testcase name="ok"/><testcase name="bad"><failure message="x &amp; y">details</failure></testcase><testcase name="error"><error/></testcase><testcase name="skip"><skipped/></testcase></testsuite></testsuites>"#).unwrap();
        assert_eq!(
            (
                report.counts.passed,
                report.counts.failed,
                report.counts.skipped
            ),
            (1, 2, 1)
        );
        assert!(report.failures[0].message.contains("x & y"));
    }
    #[test]
    fn summary_only_is_supported_but_corrupt_reports_are_errors() {
        let r = parse(r#"<testsuite tests="4" failures="1" errors="0" skipped="1"/>"#).unwrap();
        assert_eq!(r.counts.passed, 2);
        for xml in [
            "",
            "<testsuite>",
            "<foo/>",
            "<testsuite/><testsuite/>",
            r#"<testsuite tests="2"><testcase name="one"/></testsuite>"#,
            r#"<testsuite tests="1" failures="2"/>"#,
            "<!DOCTYPE x><testsuite/>",
        ] {
            assert!(parse(xml).is_err(), "{xml}");
        }
    }
}
