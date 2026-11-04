from edl import EditDecisionList, edited_to_original, original_to_edited


def test_delete_closes_gap_and_round_trips_kept_time() -> None:
    edl = EditDecisionList.full(20)
    edl.delete(5, 9)
    assert edl.kept == [(0, 5), (9, 20)]
    assert edl.edited_duration() == 16
    edited = original_to_edited(12, edl.kept)
    assert edited == 8
    assert edited_to_original(edited, edl.kept) == 12


def test_delete_clamps_and_merges_adjacent_cuts() -> None:
    edl = EditDecisionList.full(10)
    edl.delete(-2, 2)
    edl.delete(2, 4)
    assert edl.kept == [(4, 10)]


def test_zero_length_delete_does_not_change_edl() -> None:
    edl = EditDecisionList.full(10)
    edl.delete(5, 5)
    assert edl.kept == [(0, 10)]


def test_video_cut_rounds_outward_to_preserve_full_transcript_selection() -> None:
    edl = EditDecisionList.full(10)
    edl.delete(1.001, 2.001, fps=30)
    assert edl.kept == [(0, 1), (2 + 1 / 30, 10)]


def test_video_cut_keeps_exact_frame_boundaries() -> None:
    edl = EditDecisionList.full(10)
    edl.delete(1, 2, fps=30)
    assert edl.kept == [(0, 1), (2, 10)]
