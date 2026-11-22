//! Synthetic regression cases for bounded procedure-test validation.
use m53_domain::{
    protocol::{Assessment, Field, Origin, ProcedureTest, Protocol, Region, TestApplication},
    validation::{self, Dictionary, Term, TermKind, ValidationError, ValidationMode},
};

fn fixture() -> (Protocol, Dictionary) {
    let wire: serde_json::Value =
        serde_json::from_str(include_str!("../../../examples/protocol/draft.json")).unwrap();
    let mut protocol: Protocol = serde_json::from_value(wire["protocol"].clone()).unwrap();
    protocol.tests.clear();
    let dictionary = Dictionary {
        version_id: protocol.terminology_version_id,
        clinically_approved: false,
        terms: vec![
            term("probe-a", Region::Cervix, TermKind::Test),
            term("probe-a", Region::Vagina, TermKind::Test),
            term("Проба-é", Region::Cervix, TermKind::Test),
        ],
    };
    (protocol, dictionary)
}
fn term(code: &str, region: Region, kind: TermKind) -> Term {
    Term {
        label: String::new(),
        code: code.into(),
        region,
        kind,
    }
}
fn test(code: &str, region: Region) -> ProcedureTest {
    ProcedureTest {
        code: code.into(),
        region,
        application: Field {
            value: TestApplication::NotAssessed,
            origin: Origin::Physician,
        },
        result: Field {
            value: Assessment::NotAssessed,
            origin: Origin::Physician,
        },
    }
}
fn errors(protocol: &Protocol, dictionary: &Dictionary) -> Vec<ValidationError> {
    validation::validate(protocol, dictionary, ValidationMode::Draft)
}

#[test]
fn procedure_test_identity_is_exact_region_and_utf8_code() {
    let (mut p, d) = fixture();
    p.tests = vec![
        test("probe-a", Region::Cervix),
        test("probe-a", Region::Vagina),
        test("Проба-é", Region::Cervix),
    ];
    assert!(errors(&p, &d).is_empty());
    p.tests[1] = test("probe-a", Region::Cervix);
    let found = errors(&p, &d);
    assert_eq!(
        found.iter().filter(|e| e.code == "duplicate_test").count(),
        1
    );
    assert!(
        found
            .iter()
            .any(|e| e.path == "tests[1]" && e.code == "duplicate_test")
    );
    p.tests = vec![test("Проба-e\u{301}", Region::Cervix)];
    assert!(
        errors(&p, &d)
            .iter()
            .any(|e| e.code == "unknown_or_inapplicable_term")
    );
}

#[test]
fn procedure_test_cap_counts_unique_test_pairs_only() {
    let (mut p, mut d) = fixture();
    d.terms
        .push(term("probe-a", Region::Cervix, TermKind::Test));
    d.terms
        .push(term("not-a-test", Region::Cervix, TermKind::Finding));
    p.tests = (0..4)
        .map(|i| test(&format!("unknown-{i}"), Region::Cervix))
        .collect();
    let found = errors(&p, &d);
    assert!(
        found
            .iter()
            .any(|e| e.path == "tests" && e.code == "tests_too_many")
    );
    assert!(
        !found.iter().any(|e| e.path.starts_with("tests[")),
        "oversized tests must not be traversed"
    );
}

#[test]
fn excessive_tests_do_not_skip_validation_of_other_subtrees() {
    let (mut p, d) = fixture();
    p.tests = (0..1000)
        .map(|i| test(&format!("unknown-{i}"), Region::Cervix))
        .collect();
    p.history = Field {
        value: Assessment::Assessed("x".repeat(16_385)),
        origin: Origin::Physician,
    };
    let found = errors(&p, &d);
    assert!(
        found
            .iter()
            .any(|e| e.path == "tests" && e.code == "tests_too_many")
    );
    assert!(
        found
            .iter()
            .any(|e| e.path == "history" && e.code == "text_too_long")
    );
    assert!(!found.iter().any(|e| e.path.starts_with("tests[")));
}

#[test]
fn empty_dictionary_allows_empty_tests_and_bounds_nonempty_tests() {
    let (mut p, mut d) = fixture();
    d.terms.clear();
    assert!(errors(&p, &d).is_empty());
    p.tests.push(test("unknown", Region::Cervix));
    assert!(
        errors(&p, &d)
            .iter()
            .any(|e| e.path == "tests" && e.code == "tests_too_many")
    );
}
