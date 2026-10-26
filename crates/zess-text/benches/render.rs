use std::{hint::black_box, time::Instant};

use zess_text::{Cursor, TextBuffer};

fn main() {
    let text = format!("{}\n", "abcdefghij".repeat(8)).repeat(1_000);
    for (name, count, visible) in [
        ("single", 1, false),
        ("100 spread", 100, false),
        ("1000 spread", 1_000, false),
        ("100 visible", 100, true),
        ("1000 visible", 1_000, true),
    ] {
        let mut buffer = TextBuffer::new(&text);
        let cursors = (0..count)
            .map(|index| {
                let (row, column) = if visible {
                    (index / 40, index % 40 * 2)
                } else {
                    (index * 1_000 / count, 2)
                };
                Cursor {
                    row,
                    column,
                    anchor_row: row,
                    anchor_column: column,
                }
            })
            .collect::<Vec<_>>();
        buffer.set_selections(1, &cursors, 0);
        for _ in 0..100 {
            black_box(buffer.render(1, 80, 30, true));
        }
        for sample in 1..=5 {
            let iterations = 2_000;
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(buffer.render(1, 80, 30, true));
            }
            println!(
                "{name}, sample {sample}: {:.2} us/render",
                start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64
            );
        }
    }
}
