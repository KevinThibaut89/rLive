//! Which chains are selected, and how that follows clicks, arrow keys and Auto Select. Pure
//! functions, so the rules can be tested without drawing anything.

use rlive_core::ChainId;

/// What a click on a chain does to the selection (manual p. 496): a plain click selects just that
/// chain, Cmd toggles it, Shift extends from the anchor. `order` is the list as shown.
pub fn click_selection(
    order: &[ChainId],
    current: &[ChainId],
    anchor: Option<ChainId>,
    clicked: ChainId,
    shift: bool,
    cmd: bool,
) -> Vec<ChainId> {
    let index = |id: ChainId| order.iter().position(|c| *c == id);
    if shift {
        if let (Some(a), Some(b)) = (anchor.and_then(index), index(clicked)) {
            let (from, to) = (a.min(b), a.max(b));
            return order[from..=to].to_vec();
        }
    }
    if cmd {
        let mut next = current.to_vec();
        match next.iter().position(|c| *c == clicked) {
            Some(i) if next.len() > 1 => {
                next.remove(i);
            }
            Some(_) => {}
            None => next.push(clicked),
        }
        return next;
    }
    vec![clicked]
}

/// The chain the arrow keys move to, `delta` rows from the last selected one (manual p. 496).
pub fn arrow_step(order: &[ChainId], current: &[ChainId], delta: isize) -> Option<ChainId> {
    let at = current
        .last()
        .and_then(|c| order.iter().position(|o| o == c));
    let next = match at {
        Some(i) => i
            .saturating_add_signed(delta)
            .min(order.len().saturating_sub(1)),
        None => 0,
    };
    order.get(next).copied()
}

/// Where chains dropped before the row `drop_row` land once they have left the list, which is what
/// `Session::move_chain` wants. `moved` are the chains being dragged.
pub fn insertion_index(order: &[ChainId], moved: &[ChainId], drop_row: usize) -> usize {
    order
        .iter()
        .take(drop_row)
        .filter(|c| !moved.contains(c))
        .count()
}

/// Auto Select's selection: the chains that are processing, or nothing to change while none is
/// (manual p. 496).
pub fn auto_select(current: &[ChainId], active: &[ChainId]) -> Option<Vec<ChainId>> {
    (!active.is_empty() && current != active).then(|| active.to_vec())
}

/// Whether a chain counts as processing: holding a note, just having been given one, or for an
/// Audio Effect Rack letting signal through.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChainActivity {
    pub last_pulse: u32,
    pub last_active: f64,
}

/// How long a short note keeps a chain selected, in seconds.
pub const ACTIVITY_HOLD: f64 = 0.15;

impl ChainActivity {
    pub fn update(&mut self, pulse: u32, held: bool, audio: bool, now: f64) -> bool {
        if pulse != self.last_pulse || held || audio {
            self.last_active = now;
        }
        self.last_pulse = pulse;
        now - self.last_active <= ACTIVITY_HOLD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u64) -> Vec<ChainId> {
        (0..n).map(ChainId).collect()
    }

    #[test]
    fn a_plain_click_selects_only_that_chain() {
        let o = ids(4);
        assert_eq!(
            click_selection(&o, &o, Some(o[0]), o[2], false, false),
            [o[2]]
        );
    }

    #[test]
    fn cmd_click_toggles_and_keeps_at_least_one() {
        let o = ids(4);
        let picked = click_selection(&o, &[o[0]], Some(o[0]), o[2], false, true);
        assert_eq!(picked, [o[0], o[2]]);
        let picked = click_selection(&o, &picked, Some(o[2]), o[2], false, true);
        assert_eq!(picked, [o[0]]);
        assert_eq!(
            click_selection(&o, &[o[0]], Some(o[0]), o[0], false, true),
            [o[0]]
        );
    }

    #[test]
    fn shift_click_extends_from_the_anchor_either_way() {
        let o = ids(5);
        assert_eq!(
            click_selection(&o, &[o[1]], Some(o[1]), o[3], true, false),
            [o[1], o[2], o[3]]
        );
        assert_eq!(
            click_selection(&o, &[o[3]], Some(o[3]), o[0], true, false),
            [o[0], o[1], o[2], o[3]]
        );
    }

    #[test]
    fn the_arrows_walk_the_list_and_stop_at_its_ends() {
        let o = ids(3);
        assert_eq!(arrow_step(&o, &[o[0]], 1), Some(o[1]));
        assert_eq!(arrow_step(&o, &[o[2]], 1), Some(o[2]));
        assert_eq!(arrow_step(&o, &[o[0]], -1), Some(o[0]));
        assert_eq!(arrow_step(&o, &[], 1), Some(o[0]));
        assert_eq!(arrow_step(&[], &[], 1), None);
    }

    #[test]
    fn a_drop_index_discounts_the_chains_leaving() {
        let o = ids(4);
        assert_eq!(insertion_index(&o, &[o[0]], 3), 2);
        assert_eq!(insertion_index(&o, &[o[3]], 1), 1);
        assert_eq!(insertion_index(&o, &[o[1], o[2]], 4), 2);
    }

    #[test]
    fn auto_select_follows_activity_and_leaves_things_alone_when_idle() {
        let o = ids(3);
        assert_eq!(auto_select(&[o[0]], &[o[2]]), Some(vec![o[2]]));
        assert_eq!(auto_select(&[o[2]], &[o[2]]), None);
        assert_eq!(auto_select(&[o[1]], &[]), None);
    }

    #[test]
    fn a_short_note_shows_for_a_moment_and_a_held_one_for_as_long_as_it_lasts() {
        let mut a = ChainActivity::default();
        assert!(a.update(1, false, false, 1.0), "the note arrived");
        assert!(a.update(1, false, false, 1.0 + ACTIVITY_HOLD / 2.0));
        assert!(!a.update(1, false, false, 1.0 + ACTIVITY_HOLD * 2.0));
        assert!(a.update(1, true, false, 2.0), "held");
        assert!(a.update(1, true, false, 3.0));
        assert!(a.update(1, false, true, 4.0), "audio passing");
    }
}
