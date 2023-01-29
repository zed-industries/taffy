//! Helper functions for intialising GridTrack's from styles
//! This mainly consists of evaluating GridAutoTracks
use super::types::{GridTrack, TrackCounts};
use crate::axis::AbsoluteAxis;
use crate::math::MaybeMath;
use crate::resolve::ResolveOrZero;
use crate::style::{GridTrackRepetition, LengthPercentage, NonRepeatedTrackSizingFunction, Style, TrackSizingFunction};
use crate::style_helpers::TaffyAuto;
use crate::sys::{GridTrackVec, Vec};
use core::cmp::{max, min};

/// Compute the number of rows and columns in the explicit grid
pub(crate) fn compute_explicit_grid_size_in_axis(style: &Style, axis: AbsoluteAxis) -> u16 {
    // Load the grid-template-rows or grid-template-columns definition (depending on the axis)
    let template = style.grid_template_tracks(axis);

    // If template contains no tracks, then there are trivially zero explcit tracks
    if template.is_empty() {
        return 0;
    }

    let auto_repetition_count = template.iter().filter(|track_def| track_def.is_auto_repetition()).count() as u16;
    let non_repeating_track_count = (template.len() as u16) - auto_repetition_count;
    let all_track_defs_have_fixed_component = template.iter().all(|track_def| match track_def {
        TrackSizingFunction::Single(sizing_function) => sizing_function.has_fixed_component(),
        TrackSizingFunction::AutoRepeat(_, tracks) => {
            tracks.iter().all(|sizing_function| sizing_function.has_fixed_component())
        }
    });

    let template_is_valid =
        auto_repetition_count == 0 || (auto_repetition_count == 1 && all_track_defs_have_fixed_component);

    // If the template is invalid because it contains multiple auto-repetition definitions or it combines an auto-repetition
    // definition with non-fixed-size track sizing functions, then disregard it entirely and default to zero explicit tracks
    if !template_is_valid {
        return 0;
    }

    // If there are no repetitions, then the number of explicit tracks is simply equal to the lengths of the track definition
    // vector (as each item in the Vec represents one track).
    if auto_repetition_count == 0 {
        return template.len() as u16;
    }

    let repetition_definition = template
        .iter()
        .find_map(|def| match def {
            TrackSizingFunction::Single(_) => None,
            TrackSizingFunction::AutoRepeat(_, tracks) => Some(tracks),
        })
        .unwrap();
    let repetition_track_count = repetition_definition.len() as u16;

    // If the repetition contains no tracks, then the whole definition should be considered invalid and we default to no explcit tracks
    if repetition_track_count == 0 {
        return 0u16;
    }

    // Otherwise, run logic to resolve the auto-repeated track count:
    //
    // If the grid container has a definite size or max size in the relevant axis:
    //   - then the number of repetitions is the largest possible positive integer that does not cause the grid to overflow the content
    //     box of its grid container.
    // Otherwise, if the grid container has a definite min size in the relevant axis:
    //   - then the number of repetitions is the smallest possible positive integer that fulfills that minimum requirement
    // Otherwise, the specified track list repeats only once.
    let style_size = style.size.get_abs(axis).into_option();
    let style_min_size = style.min_size.get_abs(axis).into_option();
    let style_max_size = style.max_size.get_abs(axis).into_option();

    let outer_container_size = style_size.maybe_min(style_max_size).or(style_max_size).or(style_min_size);
    let inner_container_size = outer_container_size.map(|size| {
        let padding_sum = style.padding.resolve_or_zero(outer_container_size).grid_axis_sum(axis);
        let border_sum = style.border.resolve_or_zero(outer_container_size).grid_axis_sum(axis);
        size - padding_sum - border_sum
    });
    let size_is_maximum = style_size.is_some() || style_max_size.is_some();

    // Determine the number of repetitions
    let num_repetitions: u16 = match inner_container_size {
        None => 1,
        Some(inner_container_size) => {
            let parent_size = Some(inner_container_size);

            /// ...treating each track as its max track sizing function if that is definite or as its minimum track sizing function
            /// otherwise, flooring the max track sizing function by the min track sizing function if both are definite
            fn track_definite_value(sizing_function: &NonRepeatedTrackSizingFunction, parent_size: Option<f32>) -> f32 {
                let max_size = sizing_function.max.definite_value(parent_size);
                let min_size = sizing_function.max.definite_value(parent_size);
                max_size.map(|max| max.maybe_min(min_size)).or(min_size).unwrap()
            }

            let non_repeating_track_used_space: f32 = template
                .iter()
                .map(|track_def| match track_def {
                    TrackSizingFunction::Single(sizing_function) => track_definite_value(sizing_function, parent_size),
                    TrackSizingFunction::AutoRepeat(_, _) => 0.0,
                })
                .sum();
            let gap_size = style.gap.get_abs(axis).resolve_or_zero(Some(inner_container_size));

            // Compute the amount of space that a single repetition of the repeated track list takes
            let per_repetition_track_used_space: f32 = repetition_definition
                .iter()
                .map(|sizing_function| track_definite_value(sizing_function, parent_size))
                .sum::<f32>();

            // We special case the first repetition here because the number of gaps in the first repetition
            // depends on the number of non-repeating tracks in the template
            let first_repetition_and_non_repeating_tracks_used_space = non_repeating_track_used_space
                + per_repetition_track_used_space
                + ((non_repeating_track_count + repetition_track_count).saturating_sub(1) as f32 * gap_size);

            // If a single repetition already overflows the container then we return 1 as the repetition count
            // (the number of repetitions is floored at 1)
            if first_repetition_and_non_repeating_tracks_used_space > inner_container_size {
                1u16
            } else {
                let per_repetition_gap_used_space = (repetition_definition.len() as f32) * gap_size;
                let per_repetition_used_space = per_repetition_track_used_space + per_repetition_gap_used_space;
                let num_repetition_that_fit = (inner_container_size
                    - first_repetition_and_non_repeating_tracks_used_space)
                    / per_repetition_used_space;

                // If the container size is a preferred or maximum size:
                //   Then we return the maximum number of repetitions that fit into the container without overflowing.
                // If the container size is a minimum size:
                //   - Then we return the minimum number of repititions required to overflow the size.
                //
                // In all cases we add the additional repetition that was already accounted for in the special-case computation above
                if size_is_maximum {
                    (num_repetition_that_fit.floor() as u16) + 1
                } else {
                    (num_repetition_that_fit.ceil() as u16) + 1
                }
            }
        }
    };

    non_repeating_track_count + (repetition_track_count * num_repetitions)
}

/// Resolve the track sizing functions of explicit tracks, automatically created tracks, and gutters
/// given a set of track counts and all of the relevant styles
pub(super) fn initialize_grid_tracks(
    tracks: &mut Vec<GridTrack>,
    counts: TrackCounts,
    track_template: &GridTrackVec<TrackSizingFunction>,
    auto_tracks: &Vec<NonRepeatedTrackSizingFunction>,
    gap: LengthPercentage,
    track_has_items: impl Fn(usize) -> bool,
) {
    // Clear vector (in case this is a re-layout), reserve space for all tracks ahead of time to reduce allocations,
    // and push the initial gutter
    tracks.clear();
    tracks.reserve((counts.len() * 2) + 1);
    tracks.push(GridTrack::gutter(gap));

    // Create negative implicit tracks
    if auto_tracks.is_empty() {
        let iter = core::iter::repeat(NonRepeatedTrackSizingFunction::AUTO);
        create_implicit_tracks(tracks, counts.negative_implicit, iter, gap)
    } else {
        let max_count = max(auto_tracks.len(), counts.negative_implicit as usize);
        let min_count = min(auto_tracks.len(), counts.negative_implicit as usize);
        let offset = max_count % min_count;
        let iter = auto_tracks.iter().copied().cycle().skip(offset);
        create_implicit_tracks(tracks, counts.negative_implicit, iter, gap)
    }

    let mut current_track_index = (counts.negative_implicit) as usize;

    // Create explicit tracks
    // An explicit check against the count (rather than just relying on track_template being empty) is required here
    // because a count of zero can result from the track_template being invalid, in which case it should be ignored.
    if counts.explicit > 0 {
        track_template.iter().for_each(|track_sizing_function| match track_sizing_function {
            TrackSizingFunction::Single(sizing_function) => {
                tracks
                    .push(GridTrack::new(sizing_function.min_sizing_function(), sizing_function.max_sizing_function()));
                tracks.push(GridTrack::gutter(gap));
                current_track_index += 1;
            }
            TrackSizingFunction::AutoRepeat(repetition_kind, repeated_tracks) => {
                let auto_repeated_track_count = (counts.explicit - (track_template.len() as u16 - 1)) as usize;
                let iter = repeated_tracks.iter().copied().cycle();
                for track_def in iter.take(auto_repeated_track_count) {
                    let mut track = GridTrack::new(track_def.min_sizing_function(), track_def.max_sizing_function());
                    let mut gutter = GridTrack::gutter(gap);

                    // Auto-fit tracks that don't contain should be collapsed.
                    if *repetition_kind == GridTrackRepetition::AutoFit && !track_has_items(current_track_index) {
                        track.collapse();
                        gutter.collapse();
                    }

                    tracks.push(track);
                    tracks.push(gutter);

                    current_track_index += 1;
                }
            }
        });
    }

    // Create positive implicit tracks
    if auto_tracks.is_empty() {
        let iter = core::iter::repeat(NonRepeatedTrackSizingFunction::AUTO);
        create_implicit_tracks(tracks, counts.positive_implicit, iter, gap)
    } else {
        let iter = auto_tracks.iter().copied().cycle();
        create_implicit_tracks(tracks, counts.positive_implicit, iter, gap)
    }

    // Mark first and last grid lines as collapsed
    tracks.first_mut().unwrap().collapse();
    tracks.last_mut().unwrap().collapse();
}

/// Utility function for repeating logic of creating implicit tracks
fn create_implicit_tracks(
    tracks: &mut Vec<GridTrack>,
    count: u16,
    mut auto_tracks_iter: impl Iterator<Item = NonRepeatedTrackSizingFunction>,
    gap: LengthPercentage,
) {
    for _ in 0..count {
        let track_def = auto_tracks_iter.next().unwrap();
        tracks.push(GridTrack::new(track_def.min_sizing_function(), track_def.max_sizing_function()));
        tracks.push(GridTrack::gutter(gap));
    }
}

#[cfg(test)]
mod test {
    use super::compute_explicit_grid_size_in_axis;
    use super::initialize_grid_tracks;
    use crate::axis::AbsoluteAxis;
    use crate::compute::grid::types::GridTrackKind;
    use crate::compute::grid::types::TrackCounts;
    use crate::compute::grid::util::*;
    use crate::prelude::*;

    #[test]
    fn explicit_grid_sizing_no_repeats() {
        let grid_style = (600.0, 600.0, 2, 4).into_grid();
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 2);
        assert_eq!(height, 4);
    }

    #[test]
    fn explicit_grid_sizing_auto_fill_exact_fit() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            size: Size { width: points(120.0), height: points(80.0) },
            grid_template_columns: vec![repeat(AutoFill, vec![points(40.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0)])],
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 3);
        assert_eq!(height, 4);
    }

    #[test]
    fn explicit_grid_sizing_auto_fill_non_exact_fit() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            size: Size { width: points(140.0), height: points(90.0) },
            grid_template_columns: vec![repeat(AutoFill, vec![points(40.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0)])],
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 3);
        assert_eq!(height, 4);
    }

    #[test]
    fn explicit_grid_sizing_auto_fill_min_size_exact_fit() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            min_size: Size { width: points(120.0), height: points(80.0) },
            grid_template_columns: vec![repeat(AutoFill, vec![points(40.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0)])],
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 3);
        assert_eq!(height, 4);
    }

    #[test]
    fn explicit_grid_sizing_auto_fill_min_size_non_exact_fit() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            min_size: Size { width: points(140.0), height: points(90.0) },
            grid_template_columns: vec![repeat(AutoFill, vec![points(40.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0)])],
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 4);
        assert_eq!(height, 5);
    }

    #[test]
    fn explicit_grid_sizing_auto_fill_multiple_repeated_tracks() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            size: Size { width: points(140.0), height: points(100.0) },
            grid_template_columns: vec![repeat(AutoFill, vec![points(40.0), points(20.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0), points(10.0)])],
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 4); // 2 repetitions * 2 repeated tracks = 4 tracks in total
        assert_eq!(height, 6); // 3 repetitions * 2 repeated tracks = 4 tracks in total
    }

    #[test]
    fn explicit_grid_sizing_auto_fill_gap() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            size: Size { width: points(140.0), height: points(100.0) },
            grid_template_columns: vec![repeat(AutoFill, vec![points(40.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0)])],
            gap: points(20.0),
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 2); // 2 tracks + 1 gap
        assert_eq!(height, 3); // 3 tracks + 2 gaps
    }

    #[test]
    fn explicit_grid_sizing_no_defined_size() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            grid_template_columns: vec![repeat(AutoFill, vec![points(40.0), percent(0.5), points(20.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0)])],
            gap: points(20.0),
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 3);
        assert_eq!(height, 1);
    }

    #[test]
    fn explicit_grid_sizing_mix_repeated_and_non_repeated() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            size: Size { width: points(140.0), height: points(100.0) },
            grid_template_columns: vec![points(20.0), repeat(AutoFill, vec![points(40.0)])],
            grid_template_rows: vec![points(40.0), repeat(AutoFill, vec![points(20.0)])],
            gap: points(20.0),
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 3); // 3 tracks + 2 gaps
        assert_eq!(height, 2); // 2 tracks + 1 gap
    }

    #[test]
    fn explicit_grid_sizing_mix_with_padding() {
        use GridTrackRepetition::AutoFill;
        let grid_style = Style {
            display: Display::Grid,
            size: Size { width: points(120.0), height: points(120.0) },
            padding: Rect { left: points(10.0), right: points(10.0), top: points(20.0), bottom: points(20.0) },
            grid_template_columns: vec![repeat(AutoFill, vec![points(20.0)])],
            grid_template_rows: vec![repeat(AutoFill, vec![points(20.0)])],
            ..Default::default()
        };
        let width = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Horizontal);
        let height = compute_explicit_grid_size_in_axis(&grid_style, AbsoluteAxis::Vertical);
        assert_eq!(width, 5); // 40px horizontal padding
        assert_eq!(height, 4); // 20px vertical padding
    }

    #[test]
    fn test_initialize_grid_tracks() {
        let px0 = LengthPercentage::Points(0.0);
        let px20 = LengthPercentage::Points(20.0);
        let px100 = LengthPercentage::Points(100.0);

        // Setup test
        let track_template = vec![points(100.0), minmax(points(100.0), flex(2.0)), flex(1.0)];
        let track_counts =
            TrackCounts { negative_implicit: 3, explicit: track_template.len() as u16, positive_implicit: 3 };
        let auto_tracks = vec![auto(), points(100.0)];
        let gap = px20;

        // Call function
        let mut tracks = Vec::new();
        initialize_grid_tracks(&mut tracks, track_counts, &track_template, &auto_tracks, gap, |_| false);

        // Assertions
        let expected = vec![
            // Gutter
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px0), MaxTrackSizingFunction::Fixed(px0)),
            // Negative implict tracks
            (GridTrackKind::Track, MinTrackSizingFunction::Fixed(px100), MaxTrackSizingFunction::Fixed(px100)),
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            (GridTrackKind::Track, MinTrackSizingFunction::Auto, MaxTrackSizingFunction::Auto),
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            (GridTrackKind::Track, MinTrackSizingFunction::Fixed(px100), MaxTrackSizingFunction::Fixed(px100)),
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            // Explicit tracks
            (GridTrackKind::Track, MinTrackSizingFunction::Fixed(px100), MaxTrackSizingFunction::Fixed(px100)),
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            (GridTrackKind::Track, MinTrackSizingFunction::Fixed(px100), MaxTrackSizingFunction::Flex(2.0)), // Note: separate min-max functions
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            (GridTrackKind::Track, MinTrackSizingFunction::Auto, MaxTrackSizingFunction::Flex(1.0)), // Note: min sizing function of flex sizing functions is auto
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            // Positive implict tracks
            (GridTrackKind::Track, MinTrackSizingFunction::Auto, MaxTrackSizingFunction::Auto),
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            (GridTrackKind::Track, MinTrackSizingFunction::Fixed(px100), MaxTrackSizingFunction::Fixed(px100)),
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px20), MaxTrackSizingFunction::Fixed(px20)),
            (GridTrackKind::Track, MinTrackSizingFunction::Auto, MaxTrackSizingFunction::Auto),
            (GridTrackKind::Gutter, MinTrackSizingFunction::Fixed(px0), MaxTrackSizingFunction::Fixed(px0)),
        ];

        assert_eq!(tracks.len(), expected.len(), "Number of tracks doesn't match");

        for (idx, (actual, (kind, min, max))) in tracks.into_iter().zip(expected).enumerate() {
            assert_eq!(actual.kind, kind, "Track {idx} (0-based index)");
            assert_eq!(actual.min_track_sizing_function, min, "Track {idx} (0-based index)");
            assert_eq!(actual.max_track_sizing_function, max, "Track {idx} (0-based index)");
        }
    }
}
