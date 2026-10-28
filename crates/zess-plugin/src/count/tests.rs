use super::*;

#[test]
fn zero_does_not_start_a_count() {
    let mut count = CommandCount::default();
    count.push(0);
    assert_eq!(count.value(), None);
    assert_eq!(count.to_string(), "");

    count.push(1);
    count.push(0);
    assert_eq!(count.value(), Some(10));
    assert_eq!(count.to_string(), "10");
}

#[test]
fn digits_above_the_limit_are_ignored() {
    let mut count = CommandCount::default();
    for digit in [1, 0, 0, 0, 0, 0, 0, 0, 0] {
        count.push(digit);
    }
    assert_eq!(count.value(), Some(100_000_000));
    count.push(0);
    assert_eq!(count.value(), Some(100_000_000));

    let mut count = CommandCount::from(10_000_000);
    count.push(1);
    assert_eq!(count.value(), Some(10_000_000));
    count.push(0);
    assert_eq!(count.value(), Some(100_000_000));
}

#[test]
fn overflowing_digits_leave_the_count_unchanged() {
    let mut count = CommandCount::from(usize::MAX);
    count.push(9);
    assert_eq!(count.value(), Some(usize::MAX));
}
