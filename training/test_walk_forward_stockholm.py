from datetime import date, timedelta

from walk_forward_stockholm import build_folds


def test_folds_are_expanding_forward_and_purge_one_label_horizon():
    sessions = [date(2020, 1, 1) + timedelta(days=index) for index in range(300)]
    folds = build_folds(
        sessions,
        start=sessions[100],
        end=sessions[299],
        count=5,
        horizon=20,
    )
    assert len(folds) == 5
    assert [fold.test_start for fold in folds] == [
        sessions[100],
        sessions[140],
        sessions[180],
        sessions[220],
        sessions[260],
    ]
    for fold in folds:
        start_index = sessions.index(fold.test_start)
        cutoff_index = sessions.index(fold.trained_through)
        assert cutoff_index == start_index - 21
        assert fold.purged_decision_sessions == 20


def test_fold_boundaries_remain_on_global_holding_grid():
    sessions = [date(2020, 1, 1) + timedelta(days=index) for index in range(333)]
    folds = build_folds(
        sessions,
        start=sessions[101],
        end=sessions[332],
        count=5,
        horizon=20,
    )
    first = sessions.index(folds[0].test_start)
    assert all((sessions.index(fold.test_start) - first) % 20 == 0 for fold in folds)
