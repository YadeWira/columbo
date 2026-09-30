// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Andrew C.E. Dent <https://github.com/ace-dent>

use super::*;

#[test]
fn bounded_finalization_belongs_to_one_graced_hard_deadline() {
    let graced = Deadline::with_grace(
        Instant::now(),
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    assert!(graced.hard_stop().permits_bounded_finalization());
    assert!(!graced.bounded_stop(true).permits_bounded_finalization());
    assert!(!RouteWindow::full(&graced)
        .stop()
        .permits_bounded_finalization());

    let ungraced = Deadline::with_grace(Instant::now(), Duration::from_secs(1), Duration::ZERO);
    assert!(!ungraced.hard_stop().permits_bounded_finalization());

    graced.cancel_routes();
    assert!(!graced.hard_stop().permits_bounded_finalization());
    assert!(SearchStop::always().permits_bounded_finalization());
}
