//! Behavioral checks for geometry, counts, and document-point preservation.

use super::geometry::{
    center_cursor_axis, fit, move_cursor_axis, parse_page_command, reveal_axis, take_count,
};
use crate::resolution::CellPixels;
#[test]
fn centering_preserves_document_point_including_edge_margins() {
    for (cursor, offset, viewport) in [(3, 0, 61), (59, 0, 61), (2, 50, 41), (39, -20, 41)] {
        let (center, next_offset) = center_cursor_axis(cursor, offset, viewport);
        assert_eq!(center, viewport / 2);
        assert_eq!(cursor as i64 + offset, center as i64 + next_offset);
        assert_eq!(
            center_cursor_axis(center, next_offset, viewport),
            (center, next_offset)
        );
    }
}

#[test]
fn numeric_prefix_is_consumed_once() {
    let mut input = "10".to_string();
    assert_eq!(take_count(&mut input), 10);
    assert!(input.is_empty());
    assert_eq!(take_count(&mut input), 1);
}

#[test]
fn counted_cursor_movement_matches_repeated_steps() {
    for delta in [-100_i32, -10, 10, 100] {
        let mut repeated = (5, 10);
        for _ in 0..delta.abs() {
            repeated = move_cursor_axis(repeated.0, repeated.1, 20, (0, 30), delta.signum());
        }
        assert_eq!(move_cursor_axis(5, 10, 20, (0, 30), delta), repeated);
    }
}

#[test]
fn cursor_moves_before_panning_and_stops_at_document_edges() {
    assert_eq!(move_cursor_axis(5, 0, 10, (0, 20), 1), (6, 0));
    assert_eq!(move_cursor_axis(9, 0, 10, (0, 20), 1), (9, 1));
    assert_eq!(move_cursor_axis(0, 5, 10, (0, 20), -1), (0, 4));
    assert_eq!(move_cursor_axis(0, 0, 10, (0, 20), -1), (0, 0));
    assert_eq!(move_cursor_axis(9, 20, 10, (0, 20), 1), (9, 20));
    assert_eq!(move_cursor_axis(0, 0, 1, (0, 0), -1), (0, 0));
}
#[test]
fn fit_centers_portrait_page() {
    let (x, y, w, h) = fit((612.0, 792.0), (100, 40), CellPixels::default());
    assert!(x > 0.0 && (y - 1.0).abs() < 0.01);
    assert!((h - 38.0).abs() < 0.01);
    assert!(w < 100.0);
}
#[test]
fn colon_command_accepts_only_existing_pages() {
    assert_eq!(parse_page_command("8", 8), Ok(8));
    assert!(parse_page_command("0", 8).is_err());
    assert!(parse_page_command("9", 8).is_err());
    assert!(parse_page_command("abc", 8).is_err());
}
#[test]
fn reveal_axis_keeps_visible_target_and_centers_hidden_target() {
    assert_eq!(reveal_axis(30, 35.0, 100, 20), 30);
    assert_eq!(reveal_axis(30, 75.0, 100, 20), 65);
    assert_eq!(reveal_axis(0, 95.0, 100, 20), 80);
    assert_eq!(reveal_axis(10, 5.0, 12, 20), 0);
}
