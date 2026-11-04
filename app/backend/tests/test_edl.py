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
    edl.delete(1.001, 2.001, timebase=30)
    assert edl.kept == [(0, 1), (2 + 1 / 30, 10)]


def test_video_cut_keeps_exact_frame_boundaries() -> None:
    edl = EditDecisionList.full(10)
    edl.delete(1, 2, timebase=30)
    assert edl.kept == [(0, 1), (2, 10)]


def test_fractional_frame_rate_rounds_video_cuts_outward() -> None:
    edl = EditDecisionList.full(10)
    fps = 30_000 / 1_001
    edl.delete(1, 2, timebase=fps)
    assert edl.kept[0] == (0, 29 / fps)
    assert edl.kept[1] == (60 / fps, 10)


def test_multiple_cuts_preserve_time_mapping_and_deleted_gaps() -> None:
    edl = EditDecisionList.full(12)
    edl.delete(2, 3)
    edl.delete(7, 9)

    assert edl.kept == [(0, 2), (3, 7), (9, 12)]
    assert edl.edited_duration() == 9
    assert original_to_edited(2.5, edl.kept) is None
    assert original_to_edited(8, edl.kept) is None

    for source_time in (1, 4, 6, 10, 11):
        edited_time = original_to_edited(source_time, edl.kept)
        assert edited_time is not None
        assert edited_to_original(edited_time, edl.kept) == source_time


def test_audio_cut_rounds_outward_to_sample_boundaries() -> None:
    edl = EditDecisionList.full(3)
    sample_rate = 48_000
    edl.delete(24_000.2 / sample_rate, 36_000.8 / sample_rate, timebase=sample_rate)

    assert edl.kept == [(0, 0.5), (36_001 / sample_rate, 3)]


def test_paste_aligns_target_and_source_intervals_to_timebase() -> None:
    edl = EditDecisionList.full(4)
    sample_rate = 48_000
    edl.insert(
        [(24_000.2 / sample_rate, 36_000.8 / sample_rate)],
        72_000.2 / sample_rate,
        timebase=sample_rate,
    )

    assert edl.kept == [
        (0, 1.5),
        (0.5, 36_001 / sample_rate),
        (1.5, 4),
    ]


def test_paste_target_stays_within_unaligned_media_duration() -> None:
    edl = EditDecisionList.full(10.02)
    edl.insert([(1, 2)], 10.02, timebase=30)

    assert edl.kept == [(0, 10.02), (1, 2)]
