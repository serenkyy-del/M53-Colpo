use m53_domain::colpo::{Answer, Colpo};

fn context(exam: &str, lmp: &str) -> Colpo {
    let mut c = Colpo::default();
    c.fields
        .insert("context.exam_date".into(), Answer::Text(exam.into()));
    c.fields
        .insert("context.lmp".into(), Answer::Text(lmp.into()));
    c
}

#[test]
fn cycle_uses_exam_date_inclusively_without_monthly_wrap() {
    assert!(context("2026-10-15", "2026-10-13").errors().is_empty());
    assert_eq!(context("2026-10-15", "2026-10-13").cycle_day(), Some(3));
    assert_eq!(context("2026-10-13", "2026-10-13").cycle_day(), Some(1));
    assert_eq!(context("2026-02-01", "2025-12-31").cycle_day(), Some(33));
    assert_eq!(context("2024-03-01", "2024-02-28").cycle_day(), Some(3));
    assert_eq!(context("2100-03-01", "2100-02-28").cycle_day(), Some(2));
}

#[test]
fn missing_invalid_future_and_non_applicable_dates_do_not_produce_a_day() {
    for (exam, lmp) in [
        ("", "2026-10-13"),
        ("2026-10-13", "2026-10-14"),
        ("2026-02-29", "2026-02-01"),
        ("2026-10-15", "2026-13-01"),
        ("2026-10-15", "2026-10-00"),
        ("2026-10-15", "13.10.2026"),
        ("2026-10-15", "абвг-де-жз"),
    ] {
        assert_eq!(context(exam, lmp).cycle_day(), None);
    }
    for flag in ["context.pregnancy", "context.menopause"] {
        let mut c = context("2026-10-15", "2026-10-13");
        c.fields.insert(flag.into(), Answer::Checked(true));
        assert_eq!(c.cycle_day(), None);
        c.fields.insert(flag.into(), Answer::Checked(false));
        assert_eq!(c.cycle_day(), Some(3));
    }
}
