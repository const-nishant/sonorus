//! Play queue: ordering and navigation logic. Pure state, no threads, no IO.

use std::collections::HashMap;

use crate::opener::TrackRef;

pub type EntryId = u64;

#[derive(Debug, Clone, PartialEq)]
pub struct QueueEntry {
    pub id: EntryId,
    pub track: TrackRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

#[derive(Debug, Clone)]
pub struct Queue {
    entries: Vec<QueueEntry>, // "original" order (what shuffle-off restores)
    order: Vec<EntryId>,      // play order; == entries' ids when shuffle is off
    pos: Option<usize>,       // index into `order` of the current entry
    shuffle: bool,
    repeat: RepeatMode,
    next_id: EntryId,
    rng: fastrand::Rng,
}

impl Queue {
    pub fn new(seed: u64) -> Self {
        Self {
            entries: Vec::new(),
            order: Vec::new(),
            pos: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            next_id: 1,
            rng: fastrand::Rng::with_seed(seed),
        }
    }

    /// Replace everything. Empty -> pos None, returns None. `start` clamped to len-1. Shuffle on ->
    /// order = [start entry] + the others shuffled, pos = 0. Shuffle off -> order = entries, pos = start.
    pub fn set_tracks(&mut self, tracks: Vec<TrackRef>, start: usize) -> Option<&QueueEntry> {
        self.entries.clear();
        self.order.clear();
        self.pos = None;
        if tracks.is_empty() {
            return None;
        }
        self.entries = self.make_entries(tracks);
        let ids: Vec<EntryId> = self.entries.iter().map(|e| e.id).collect();
        let start = start.min(ids.len().saturating_sub(1));
        let start_id = ids.get(start).copied();
        if self.shuffle {
            self.order = self.shuffled_after(start_id, &ids);
            self.pos = Some(0);
        } else {
            self.order = ids;
            self.pos = Some(start);
        }
        self.current()
    }

    pub fn current(&self) -> Option<&QueueEntry> {
        self.pos.and_then(|p| self.entry_at(p))
    }

    pub fn current_id(&self) -> Option<EntryId> {
        self.current().map(|e| e.id)
    }

    /// Index into play order (`order`) of the current entry.
    pub fn current_index(&self) -> Option<usize> {
        self.pos
    }

    /// All entries in play order.
    pub fn entries_in_order(&self) -> Vec<&QueueEntry> {
        let by_id = self.by_id();
        self.order
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect()
    }

    /// Entries after the current one in play order, no wrap. Empty when pos is None.
    pub fn upcoming(&self) -> Vec<&QueueEntry> {
        let Some(p) = self.pos else {
            return Vec::new();
        };
        let by_id = self.by_id();
        self.order
            .get(p.saturating_add(1)..)
            .unwrap_or_default()
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect()
    }

    pub fn get(&self, id: EntryId) -> Option<&QueueEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What plays when the current track finishes by itself. One -> current. Off -> order[pos+1] or None.
    /// All -> order[(pos+1) % len] (a single entry repeats itself). pos None -> None.
    pub fn peek_auto_next(&self) -> Option<&QueueEntry> {
        self.entry_at(self.auto_next_index()?)
    }

    /// Moves pos to `peek_auto_next`. When there is no next entry, pos is unchanged and None is returned.
    pub fn advance_auto(&mut self) -> Option<&QueueEntry> {
        let next = self.auto_next_index()?;
        self.pos = Some(next);
        self.current()
    }

    /// User "next": order[pos+1]; at the end, All -> wrap to order[0], Off/One -> None (pos unchanged).
    /// (Repeat One only affects auto-advance, never an explicit skip.)
    pub fn skip_next(&mut self) -> Option<&QueueEntry> {
        let next = self.skip_next_index()?;
        self.pos = Some(next);
        self.current()
    }

    /// User "previous": pos-1. At pos 0: All -> wrap to last; otherwise stay and return current.
    pub fn skip_previous(&mut self) -> Option<&QueueEntry> {
        let p = self.pos?;
        let target = match p.checked_sub(1) {
            Some(prev) => Some(prev),
            None if self.repeat == RepeatMode::All => self.order.len().checked_sub(1),
            None => None,
        };
        if let Some(t) = target {
            self.pos = Some(t);
        }
        self.current()
    }

    /// Unknown id -> None, pos unchanged.
    pub fn jump_to(&mut self, id: EntryId) -> Option<&QueueEntry> {
        let index = self.order.iter().position(|&x| x == id)?;
        self.pos = Some(index);
        self.current()
    }

    /// Add to the end of `order` and `entries`. Returns new ids. Does not change pos (None stays None).
    pub fn append(&mut self, tracks: Vec<TrackRef>) -> Vec<EntryId> {
        let new = self.make_entries(tracks);
        let ids: Vec<EntryId> = new.iter().map(|e| e.id).collect();
        self.order.extend(ids.iter().copied());
        self.entries.extend(new);
        ids
    }

    /// Insert right after pos in `order` (pos None -> same as append). Shuffle off -> also insert after
    /// the current entry in `entries`; shuffle on -> push to the end of `entries`. Returns new ids.
    pub fn play_next(&mut self, tracks: Vec<TrackRef>) -> Vec<EntryId> {
        let new = self.make_entries(tracks);
        let ids: Vec<EntryId> = new.iter().map(|e| e.id).collect();

        let order_at = match self.pos {
            Some(p) => p.saturating_add(1).min(self.order.len()),
            None => self.order.len(),
        };
        self.order.splice(order_at..order_at, ids.iter().copied());

        let len = self.entries.len();
        let entries_at = match (self.shuffle, self.current_id()) {
            (false, Some(cur)) => self
                .entries
                .iter()
                .position(|e| e.id == cur)
                .map_or(len, |i| i.saturating_add(1).min(len)),
            _ => len,
        };
        self.entries.splice(entries_at..entries_at, new);
        ids
    }

    /// Remove from both vecs. If it was current: the entry now at the same order index becomes current;
    /// if it was last, the new last; if empty, None. Entries before pos removed -> pos -= 1. false if unknown.
    pub fn remove(&mut self, id: EntryId) -> bool {
        let Some(entry_index) = self.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        let Some(order_index) = self.order.iter().position(|&x| x == id) else {
            return false;
        };
        self.entries.remove(entry_index);
        self.order.remove(order_index);

        let last = self.order.len().checked_sub(1);
        self.pos = match (self.pos, last) {
            (_, None) => None,
            (Some(p), Some(last)) if p == order_index => Some(order_index.min(last)),
            (Some(p), _) if order_index < p => Some(p.saturating_sub(1)),
            (other, _) => other,
        };
        true
    }

    /// Move within `order` to `to` (clamped to len-1); pos follows the current entry. Shuffle off -> apply the
    /// same move to `entries` so they stay identical. false if unknown id.
    pub fn move_entry(&mut self, id: EntryId, to: usize) -> bool {
        let Some(order_from) = self.order.iter().position(|&x| x == id) else {
            return false;
        };
        let Some(entries_from) = self.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        let cur = self.current_id();

        move_within(&mut self.order, order_from, to);
        if !self.shuffle {
            move_within(&mut self.entries, entries_from, to);
        }
        if let Some(cur) = cur {
            self.pos = self.order.iter().position(|&x| x == cur);
        }
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.pos = None;
    }

    /// On: order = [current] + others shuffled (pos = 0); with no current, shuffle all, pos None.
    /// Off: order = entries' ids, pos = index of current. No-op if unchanged.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        let cur = self.current_id();
        if on {
            let ids: Vec<EntryId> = self.entries.iter().map(|e| e.id).collect();
            self.order = self.shuffled_after(cur, &ids);
            self.pos = cur.map(|_| 0);
        } else {
            self.order = self.entries.iter().map(|e| e.id).collect();
            self.pos = cur.and_then(|c| self.entries.iter().position(|e| e.id == c));
        }
        self.shuffle = on;
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    fn entry_at(&self, index: usize) -> Option<&QueueEntry> {
        let id = self.order.get(index)?;
        self.get(*id)
    }

    fn by_id(&self) -> HashMap<EntryId, &QueueEntry> {
        self.entries.iter().map(|e| (e.id, e)).collect()
    }

    /// Allocates fresh ids (monotonic, never reused) for `tracks`.
    fn make_entries(&mut self, tracks: Vec<TrackRef>) -> Vec<QueueEntry> {
        tracks
            .into_iter()
            .map(|track| {
                let id = self.next_id;
                self.next_id = id.saturating_add(1);
                QueueEntry { id, track }
            })
            .collect()
    }

    /// `first` (if any) followed by the ids in `ids` other than `first`, shuffled.
    fn shuffled_after(&mut self, first: Option<EntryId>, ids: &[EntryId]) -> Vec<EntryId> {
        let mut rest: Vec<EntryId> = ids
            .iter()
            .copied()
            .filter(|id| Some(*id) != first)
            .collect();
        self.rng.shuffle(&mut rest);
        first.into_iter().chain(rest).collect()
    }

    fn auto_next_index(&self) -> Option<usize> {
        let p = self.pos?;
        let n = self.order.len();
        let next = match self.repeat {
            RepeatMode::One => p,
            RepeatMode::Off => p.saturating_add(1),
            RepeatMode::All => {
                let candidate = p.saturating_add(1);
                if candidate < n { candidate } else { 0 }
            }
        };
        (next < n).then_some(next)
    }

    fn skip_next_index(&self) -> Option<usize> {
        let p = self.pos?;
        let n = self.order.len();
        let next = p.saturating_add(1);
        if next < n {
            Some(next)
        } else if self.repeat == RepeatMode::All && n > 0 {
            Some(0)
        } else {
            None
        }
    }
}

/// Moves the element at `from` to index `to` (clamped to the last index). No-op if `from` is out of range.
fn move_within<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from >= v.len() {
        return;
    }
    let item = v.remove(from);
    v.insert(to.min(v.len()), item);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn t(n: u32) -> TrackRef {
        TrackRef {
            id: format!("id{n}"),
            key: format!("music/{n}.flac"),
            size: u64::from(n),
            gain: None,
        }
    }

    fn tracks(n: u32) -> Vec<TrackRef> {
        (1..=n).map(t).collect()
    }

    fn ids(q: &Queue) -> Vec<EntryId> {
        q.entries_in_order().iter().map(|e| e.id).collect()
    }

    fn upcoming_ids(q: &Queue) -> Vec<EntryId> {
        q.upcoming().iter().map(|e| e.id).collect()
    }

    /// Asserts every structural invariant of the queue.
    fn check(q: &Queue) {
        let entry_ids: Vec<EntryId> = q.entries.iter().map(|e| e.id).collect();
        let entry_set: HashSet<EntryId> = entry_ids.iter().copied().collect();
        assert_eq!(entry_set.len(), entry_ids.len(), "entry ids must be unique");

        // order is a permutation of the entries' ids
        assert_eq!(
            q.order.len(),
            entry_ids.len(),
            "order length must match entries"
        );
        let order_set: HashSet<EntryId> = q.order.iter().copied().collect();
        assert_eq!(
            order_set.len(),
            q.order.len(),
            "order must not contain duplicates"
        );
        assert_eq!(
            order_set, entry_set,
            "order must be a permutation of entries"
        );

        // shuffle off => order == entries' ids
        if !q.shuffle {
            assert_eq!(q.order, entry_ids, "shuffle off: order must equal entries");
        }

        // pos is None when empty, otherwise < len
        if q.entries.is_empty() {
            assert!(q.pos.is_none(), "empty queue must have pos None");
        }
        if let Some(p) = q.pos {
            assert!(p < q.order.len(), "pos must be inside order");
        }

        // ids are below the next id to hand out, and next_id never drops below 1
        assert!(q.next_id >= 1);
        assert!(
            entry_ids.iter().all(|&id| id < q.next_id),
            "ids must be < next_id"
        );

        // derived views agree with the raw state
        assert_eq!(q.entries_in_order().len(), q.len());
        assert_eq!(
            q.current().map(|e| e.id),
            q.pos.and_then(|p| q.order.get(p).copied())
        );
    }

    #[test]
    fn queue_new_is_empty() {
        let q = Queue::new(1);
        check(&q);
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert!(q.current().is_none());
        assert_eq!(q.current_id(), None);
        assert_eq!(q.current_index(), None);
        assert!(q.upcoming().is_empty());
        assert!(q.entries_in_order().is_empty());
        assert!(q.get(1).is_none());
        assert_eq!(q.repeat(), RepeatMode::Off);
        assert!(!q.shuffle());
    }

    #[test]
    fn queue_set_tracks_respects_start_and_clamps() {
        let mut q = Queue::new(1);
        let c = q.set_tracks(tracks(3), 1).map(|e| e.id);
        check(&q);
        assert_eq!(c, Some(2));
        assert_eq!(q.current_index(), Some(1));
        assert_eq!(ids(&q), vec![1, 2, 3]);

        let c = q.set_tracks(tracks(3), 99).map(|e| e.id);
        check(&q);
        assert_eq!(c, Some(6));
        assert_eq!(q.current_index(), Some(2));
        assert_eq!(q.current().map(|e| e.track.id.as_str()), Some("id3"));
    }

    #[test]
    fn queue_set_tracks_empty_returns_none() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), 0);
        check(&q);
        assert!(q.set_tracks(Vec::new(), 0).is_none());
        check(&q);
        assert!(q.is_empty());
        assert_eq!(q.current_index(), None);
    }

    #[test]
    fn queue_set_tracks_huge_start_clamps() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), usize::MAX);
        check(&q);
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_ids_start_at_one_and_are_never_reused() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), 0);
        check(&q);
        assert_eq!(ids(&q), vec![1, 2]);

        q.clear();
        check(&q);
        let added = q.append(tracks(1));
        check(&q);
        assert_eq!(added, vec![3]);

        assert!(q.remove(3));
        check(&q);
        let added = q.append(tracks(2));
        check(&q);
        assert_eq!(added, vec![4, 5]);
    }

    #[test]
    fn queue_shuffle_same_seed_is_deterministic() {
        let run = |seed: u64| {
            let mut q = Queue::new(seed);
            q.set_tracks(tracks(20), 3);
            check(&q);
            q.set_shuffle(true);
            check(&q);
            ids(&q)
        };
        assert_eq!(run(42), run(42));
        assert_ne!(run(42), run(43));
    }

    #[test]
    fn queue_set_tracks_with_shuffle_puts_start_first() {
        let mut q = Queue::new(7);
        q.set_shuffle(true);
        check(&q);
        let c = q.set_tracks(tracks(10), 4).map(|e| e.id);
        check(&q);
        assert_eq!(c, Some(5));
        assert_eq!(q.current_index(), Some(0));
        let mut sorted = ids(&q);
        sorted.sort_unstable();
        assert_eq!(sorted, (1..=10).collect::<Vec<EntryId>>());
    }

    #[test]
    fn queue_set_shuffle_on_keeps_current_at_front() {
        let mut q = Queue::new(9);
        q.set_tracks(tracks(8), 5);
        check(&q);
        q.set_shuffle(true);
        check(&q);
        assert!(q.shuffle());
        assert_eq!(q.current_id(), Some(6));
        assert_eq!(q.current_index(), Some(0));
        assert_eq!(ids(&q)[0], 6);
    }

    #[test]
    fn queue_set_shuffle_off_restores_original_order_and_current() {
        let mut q = Queue::new(3);
        q.set_tracks(tracks(6), 2);
        check(&q);
        q.set_shuffle(true);
        check(&q);
        q.skip_next();
        check(&q);
        q.skip_next();
        check(&q);
        let cur_before = q.current_id();

        q.set_shuffle(false);
        check(&q);
        assert_eq!(ids(&q), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(q.current_id(), cur_before);
        assert_eq!(q.current_index(), cur_before.map(|c| (c - 1) as usize));
    }

    #[test]
    fn queue_set_shuffle_noop_when_unchanged() {
        let mut q = Queue::new(3);
        q.set_tracks(tracks(5), 0);
        check(&q);
        let before = ids(&q);
        q.set_shuffle(false);
        check(&q);
        assert_eq!(ids(&q), before);
        assert_eq!(q.current_index(), Some(0));

        q.set_shuffle(true);
        check(&q);
        let shuffled = ids(&q);
        q.set_shuffle(true);
        check(&q);
        assert_eq!(ids(&q), shuffled);
    }

    #[test]
    fn queue_set_shuffle_without_current_leaves_pos_none() {
        let mut q = Queue::new(5);
        q.append(tracks(4));
        check(&q);
        assert!(q.current().is_none());

        q.set_shuffle(true);
        check(&q);
        assert_eq!(q.current_index(), None);
        assert!(q.current().is_none());
        let mut sorted = ids(&q);
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3, 4]);

        q.set_shuffle(false);
        check(&q);
        assert_eq!(q.current_index(), None);
        assert_eq!(ids(&q), vec![1, 2, 3, 4]);
    }

    #[test]
    fn queue_current_index_and_upcoming_follow_play_order() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(4), 1);
        check(&q);
        assert_eq!(q.current_index(), Some(1));
        assert_eq!(upcoming_ids(&q), vec![3, 4]);

        q.skip_next();
        check(&q);
        q.skip_next();
        check(&q);
        assert_eq!(q.current_index(), Some(3));
        assert!(upcoming_ids(&q).is_empty());
    }

    #[test]
    fn queue_upcoming_empty_when_pos_none() {
        let mut q = Queue::new(1);
        q.append(tracks(2));
        check(&q);
        assert!(q.upcoming().is_empty());
    }

    #[test]
    fn queue_get_by_id() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 0);
        check(&q);
        assert_eq!(q.get(2).map(|e| e.id), Some(2));
        assert!(q.get(99).is_none());
        assert!(q.get(0).is_none());
    }

    #[test]
    fn queue_repeat_off_auto_advance_stops_at_end() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), 0);
        check(&q);
        assert_eq!(q.peek_auto_next().map(|e| e.id), Some(2));
        assert_eq!(q.current_index(), Some(0));

        assert_eq!(q.advance_auto().map(|e| e.id), Some(2));
        check(&q);
        assert_eq!(q.current_index(), Some(1));

        assert!(q.peek_auto_next().is_none());
        assert!(q.advance_auto().is_none());
        check(&q);
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_repeat_all_wraps_both_directions() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 2);
        q.set_repeat(RepeatMode::All);
        check(&q);
        assert_eq!(q.peek_auto_next().map(|e| e.id), Some(1));

        assert_eq!(q.skip_next().map(|e| e.id), Some(1));
        check(&q);
        assert_eq!(q.skip_previous().map(|e| e.id), Some(3));
        check(&q);
        assert_eq!(q.advance_auto().map(|e| e.id), Some(1));
        check(&q);
        assert_eq!(q.current_index(), Some(0));

        assert_eq!(q.skip_previous().map(|e| e.id), Some(3));
        check(&q);
        assert_eq!(q.current_index(), Some(2));

        assert_eq!(q.skip_next().map(|e| e.id), Some(1));
        check(&q);
    }

    #[test]
    fn queue_repeat_all_single_entry_repeats_itself() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(1), 0);
        q.set_repeat(RepeatMode::All);
        check(&q);
        assert_eq!(q.peek_auto_next().map(|e| e.id), Some(1));
        assert_eq!(q.advance_auto().map(|e| e.id), Some(1));
        check(&q);
        assert_eq!(q.skip_next().map(|e| e.id), Some(1));
        check(&q);
        assert_eq!(q.skip_previous().map(|e| e.id), Some(1));
        check(&q);
    }

    #[test]
    fn queue_repeat_one_repeats_on_auto_not_on_skip() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 0);
        q.set_repeat(RepeatMode::One);
        check(&q);
        assert_eq!(q.peek_auto_next().map(|e| e.id), Some(1));
        assert_eq!(q.advance_auto().map(|e| e.id), Some(1));
        check(&q);
        assert_eq!(q.current_index(), Some(0));

        assert_eq!(q.skip_next().map(|e| e.id), Some(2));
        check(&q);
        assert_eq!(q.advance_auto().map(|e| e.id), Some(2));
        check(&q);
        assert_eq!(q.current_index(), Some(1));

        q.jump_to(3);
        check(&q);
        assert_eq!(q.skip_next().map(|e| e.id), None);
        check(&q);
        assert_eq!(q.current_index(), Some(2));
        assert_eq!(q.advance_auto().map(|e| e.id), Some(3));
        check(&q);
    }

    #[test]
    fn queue_skip_previous_at_start_stays_when_repeat_off() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), 0);
        check(&q);
        assert_eq!(q.skip_previous().map(|e| e.id), Some(1));
        check(&q);
        assert_eq!(q.current_index(), Some(0));

        assert_eq!(q.skip_next().map(|e| e.id), Some(2));
        check(&q);
        assert_eq!(q.skip_next().map(|e| e.id), None);
        check(&q);
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_jump_to_known_and_unknown() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 0);
        check(&q);
        assert_eq!(q.jump_to(3).map(|e| e.id), Some(3));
        check(&q);
        assert_eq!(q.current_index(), Some(2));

        assert!(q.jump_to(99).is_none());
        check(&q);
        assert_eq!(q.current_index(), Some(2));
        assert!(q.jump_to(0).is_none());
        check(&q);
        assert_eq!(q.current_index(), Some(2));
    }

    #[test]
    fn queue_append_keeps_pos_and_adds_to_end() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), 1);
        check(&q);
        let added = q.append(tracks(2));
        check(&q);
        assert_eq!(added, vec![3, 4]);
        assert_eq!(ids(&q), vec![1, 2, 3, 4]);
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_append_to_empty_keeps_pos_none() {
        let mut q = Queue::new(1);
        let added = q.append(tracks(2));
        check(&q);
        assert_eq!(added, vec![1, 2]);
        assert_eq!(q.len(), 2);
        assert!(q.current().is_none());
    }

    #[test]
    fn queue_append_with_shuffle_pushes_to_end() {
        let mut q = Queue::new(2);
        q.set_tracks(tracks(3), 0);
        q.set_shuffle(true);
        check(&q);
        let cur = q.current_id();
        let added = q.append(tracks(2));
        check(&q);
        assert_eq!(q.current_id(), cur);
        assert_eq!(&ids(&q)[3..], added.as_slice());
    }

    #[test]
    fn queue_play_next_shuffle_off_inserts_after_current_in_both() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 0);
        check(&q);
        let new = q.play_next(vec![t(10), t(11)]);
        check(&q);
        assert_eq!(new, vec![4, 5]);
        assert_eq!(ids(&q), vec![1, 4, 5, 2, 3]);
        assert_eq!(q.current_id(), Some(1));

        q.skip_next();
        check(&q);
        q.skip_next();
        check(&q);
        assert_eq!(q.current_id(), Some(5));

        let new = q.play_next(vec![t(12)]);
        check(&q);
        assert_eq!(new, vec![6]);
        assert_eq!(ids(&q), vec![1, 4, 5, 6, 2, 3]);
        assert_eq!(q.current_id(), Some(5));
        assert_eq!(q.current_index(), Some(2));
    }

    #[test]
    fn queue_play_next_at_end_appends() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), 1);
        check(&q);
        let new = q.play_next(vec![t(9)]);
        check(&q);
        assert_eq!(new, vec![3]);
        assert_eq!(ids(&q), vec![1, 2, 3]);
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_play_next_without_current_appends() {
        let mut q = Queue::new(1);
        q.append(tracks(2));
        check(&q);
        let new = q.play_next(tracks(1));
        check(&q);
        assert_eq!(new, vec![3]);
        assert_eq!(ids(&q), vec![1, 2, 3]);
        assert_eq!(q.current_index(), None);
    }

    #[test]
    fn queue_play_next_shuffle_on_pushes_entries_to_end() {
        let mut q = Queue::new(4);
        q.set_tracks(tracks(4), 0);
        q.set_shuffle(true);
        check(&q);
        let cur = q.current_id();
        let new = q.play_next(vec![t(9)]);
        check(&q);
        assert_eq!(q.current_id(), cur);
        assert_eq!(ids(&q)[1], new[0]);
        assert_eq!(q.entries.last().map(|e| e.id), Some(new[0]));

        q.set_shuffle(false);
        check(&q);
        assert_eq!(ids(&q), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn queue_remove_before_current_decrements_pos() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(4), 2);
        check(&q);
        assert!(q.remove(1));
        check(&q);
        assert_eq!(q.current_id(), Some(3));
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_remove_after_current_keeps_pos() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(4), 1);
        check(&q);
        assert!(q.remove(4));
        check(&q);
        assert_eq!(q.current_id(), Some(2));
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_remove_unknown_returns_false() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(2), 0);
        check(&q);
        assert!(!q.remove(99));
        check(&q);
        assert_eq!(ids(&q), vec![1, 2]);

        let mut empty = Queue::new(1);
        assert!(!empty.remove(1));
        check(&empty);
    }

    #[test]
    fn queue_remove_current_in_middle_selects_next_at_same_index() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(4), 1);
        check(&q);
        assert!(q.remove(2));
        check(&q);
        assert_eq!(q.current_id(), Some(3));
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_remove_current_at_end_selects_new_last() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 2);
        check(&q);
        assert!(q.remove(3));
        check(&q);
        assert_eq!(q.current_id(), Some(2));
        assert_eq!(q.current_index(), Some(1));
    }

    #[test]
    fn queue_remove_only_entry_clears_pos() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(1), 0);
        check(&q);
        assert!(q.remove(1));
        check(&q);
        assert!(q.is_empty());
        assert!(q.current().is_none());
        assert_eq!(q.current_index(), None);
    }

    #[test]
    fn queue_remove_current_with_shuffle_selects_entry_at_same_index() {
        let mut q = Queue::new(6);
        q.set_tracks(tracks(4), 0);
        q.set_shuffle(true);
        check(&q);
        let cur = q.current_id().unwrap_or_default();
        let next = ids(&q)[1];
        assert!(q.remove(cur));
        check(&q);
        assert_eq!(q.current_index(), Some(0));
        assert_eq!(q.current_id(), Some(next));
    }

    #[test]
    fn queue_move_current_keeps_it_current() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(4), 0);
        check(&q);
        assert!(q.move_entry(1, 2));
        check(&q);
        assert_eq!(ids(&q), vec![2, 3, 1, 4]);
        assert_eq!(q.current_id(), Some(1));
        assert_eq!(q.current_index(), Some(2));
    }

    #[test]
    fn queue_move_other_across_current_adjusts_pos() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(4), 2);
        check(&q);
        assert!(q.move_entry(4, 0));
        check(&q);
        assert_eq!(ids(&q), vec![4, 1, 2, 3]);
        assert_eq!(q.current_index(), Some(3));

        assert!(q.move_entry(1, 3));
        check(&q);
        assert_eq!(ids(&q), vec![4, 2, 3, 1]);
        assert_eq!(q.current_id(), Some(3));
        assert_eq!(q.current_index(), Some(2));
    }

    #[test]
    fn queue_move_clamps_and_rejects_unknown() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 0);
        check(&q);
        assert!(q.move_entry(1, usize::MAX));
        check(&q);
        assert_eq!(ids(&q), vec![2, 3, 1]);
        assert_eq!(q.current_index(), Some(2));

        assert!(!q.move_entry(99, 0));
        check(&q);
        assert_eq!(ids(&q), vec![2, 3, 1]);

        let mut empty = Queue::new(1);
        assert!(!empty.move_entry(1, 0));
        check(&empty);
    }

    #[test]
    fn queue_move_with_shuffle_on_leaves_entries_untouched() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 0);
        q.set_shuffle(true);
        check(&q);
        let cur = q.current_id();
        assert!(q.move_entry(cur.unwrap_or_default(), 2));
        check(&q);
        assert_eq!(q.current_index(), Some(2));
        q.set_shuffle(false);
        check(&q);
        assert_eq!(ids(&q), vec![1, 2, 3]);
    }

    #[test]
    fn queue_move_with_shuffle_off_applies_to_entries() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 0);
        check(&q);
        assert!(q.move_entry(1, 2));
        check(&q);
        q.set_shuffle(true);
        check(&q);
        q.set_shuffle(false);
        check(&q);
        assert_eq!(ids(&q), vec![2, 3, 1]);
    }

    #[test]
    fn queue_clear_resets_queue_but_keeps_modes() {
        let mut q = Queue::new(1);
        q.set_tracks(tracks(3), 1);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        check(&q);
        q.clear();
        check(&q);
        assert!(q.is_empty());
        assert_eq!(q.current_index(), None);
        assert!(q.shuffle());
        assert_eq!(q.repeat(), RepeatMode::All);
    }

    #[test]
    fn queue_repeat_mode_accessors() {
        let mut q = Queue::new(1);
        assert_eq!(q.repeat(), RepeatMode::Off);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.repeat(), RepeatMode::One);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.repeat(), RepeatMode::All);
    }

    #[test]
    fn queue_empty_operations_do_not_panic() {
        let mut q = Queue::new(1);
        for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            q.set_repeat(mode);
            check(&q);
            assert!(q.peek_auto_next().is_none());
            assert!(q.advance_auto().is_none());
            check(&q);
            assert!(q.skip_next().is_none());
            check(&q);
            assert!(q.skip_previous().is_none());
            check(&q);
            assert!(q.jump_to(1).is_none());
            check(&q);
        }
        assert!(!q.remove(1));
        check(&q);
        assert!(!q.move_entry(1, usize::MAX));
        check(&q);
        assert!(q.upcoming().is_empty());
        assert!(q.entries_in_order().is_empty());
        assert!(q.get(1).is_none());

        q.set_shuffle(true);
        check(&q);
        q.set_shuffle(false);
        check(&q);
        assert!(q.set_tracks(Vec::new(), usize::MAX).is_none());
        check(&q);
        assert!(q.play_next(Vec::new()).is_empty());
        check(&q);
        assert!(q.append(Vec::new()).is_empty());
        check(&q);
        q.clear();
        check(&q);
    }
}
