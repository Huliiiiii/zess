use hegel::{TestCase, generators as gs};

use super::*;

#[hegel::test(derandomize = true)]
fn margins_choose_the_nearest_start_that_keeps_the_cursor_visible(tc: TestCase) {
    let requested = tc.draw(gs::integers::<usize>().max_value(48));
    let row = tc.draw(gs::integers::<usize>().max_value(32));
    let height = tc.draw(gs::integers::<usize>().max_value(16));
    let scrolloff = tc.draw(gs::integers::<usize>().max_value(24));
    let visible_height = height.max(1);
    let margin = scrolloff.min((visible_height - 1) / 2);

    // Enumerate visible cursor positions instead of duplicating the clamp formula.
    // At the beginning of the document, an unavailable top margin is waived.
    let legal = (0..=row)
        .filter(|start| {
            let offset = row - start;
            offset < visible_height
                && (*start == 0 || offset >= margin)
                && offset + margin < visible_height
        })
        .collect::<Vec<_>>();
    let nearest = legal
        .into_iter()
        .min_by_key(|start| start.abs_diff(requested))
        .unwrap();
    let actual = scroll_with_margin(requested, row, height, scrolloff);

    assert_eq!(actual, nearest);
    assert!(actual <= row && row - actual < visible_height);
    assert_eq!(scroll_with_margin(actual, row, height, scrolloff), actual);
}

#[test]
fn half_page_counts_saturate_without_wrapping_the_requested_scroll() {
    for (height, count, distance) in [
        (0, 0, 0),
        (0, 1, 1),
        (1, usize::MAX, usize::MAX),
        (4, usize::MAX, usize::MAX),
        (usize::MAX, 3, usize::MAX),
    ] {
        let down = HalfPageScroll::new(height, count, ScrollDirection::Down);
        let up = HalfPageScroll::new(height, count, ScrollDirection::Up);
        assert_eq!(down.distance(), distance);
        assert_eq!(up.distance(), distance);
    }

    let down = HalfPageScroll::new(8, usize::MAX, ScrollDirection::Down);
    assert_eq!(down.viewport_start(usize::MAX - 1, 20, 1), 19);
    let up = HalfPageScroll::new(8, usize::MAX, ScrollDirection::Up);
    assert_eq!(up.viewport_start(usize::MAX - 1, 20, 1), 14);
}

#[test]
fn the_scroll_limit_keeps_the_last_row_at_the_top_margin() {
    for (rows, height, scrolloff, expected) in [
        (0, 20, 5, 0),
        (1, 20, 5, 0),
        (4, 20, 5, 0),
        (40, 20, 5, 34),
        (40, 4, 5, 38),
        (40, 1, 5, 39),
        (40, 0, 5, 39),
        (40, 20, 0, 39),
    ] {
        assert_eq!(scroll_limit_with_margin(rows, height, scrolloff), expected);
    }
}
