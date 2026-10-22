//! Guide data and grid math: an EPG cache loaded in fixed time blocks, the visible
//! time window, and remote-style navigation across programs.

use std::collections::{HashMap, HashSet};

use tvh::EpgEvent;

/// Guide column width.
pub const SLOT: i64 = 30 * 60;
/// Visible time span (four columns).
pub const SPAN: i64 = 4 * SLOT;
/// Channel rows on screen.
pub const ROWS: usize = 7;
/// EPG is fetched for all channels in blocks of this size.
pub const BLOCK: i64 = 6 * 3600;
const DAY: i64 = 24 * 3600;

pub fn align(t: i64) -> i64 {
    t - t.rem_euclid(SLOT)
}

fn block_of(t: i64) -> i64 {
    t - t.rem_euclid(BLOCK)
}

#[derive(Debug, Default)]
pub struct EpgCache {
    /// Per channel, sorted by start, unique by event id.
    events: HashMap<String, Vec<EpgEvent>>,
    loaded: HashSet<i64>,
}

impl EpgCache {
    /// Block start times not yet loaded that overlap `[from, to)`.
    pub fn missing_blocks(&self, from: i64, to: i64) -> Vec<i64> {
        let mut b = block_of(from);
        let mut out = Vec::new();
        while b < to {
            if !self.loaded.contains(&b) {
                out.push(b);
            }
            b += BLOCK;
        }
        out
    }

    pub fn insert_block(&mut self, block: i64, events: Vec<EpgEvent>) {
        self.loaded.insert(block);
        let mut touched = HashSet::new();
        for e in events {
            touched.insert(e.channel_uuid.clone());
            let list = self.events.entry(e.channel_uuid.clone()).or_default();
            match list.iter_mut().find(|x| x.event_id == e.event_id) {
                Some(x) => *x = e,
                None => list.push(e),
            }
        }
        for ch in touched {
            if let Some(list) = self.events.get_mut(&ch) {
                list.sort_by_key(|e| e.start);
            }
        }
    }

    /// Drops events that ended before `t` to bound memory.
    pub fn prune(&mut self, t: i64) {
        for list in self.events.values_mut() {
            list.retain(|e| e.stop > t);
        }
        self.loaded.retain(|&b| b + BLOCK > t);
    }

    pub fn events(&self, channel: &str) -> &[EpgEvent] {
        self.events.get(channel).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn at(&self, channel: &str, t: i64) -> Option<&EpgEvent> {
        self.events(channel).iter().find(|e| e.is_airing(t))
    }

    pub fn now_next(&self, channel: &str, now: i64) -> (Option<&EpgEvent>, Option<&EpgEvent>) {
        let evs = self.events(channel);
        let cur = evs.iter().position(|e| e.is_airing(now));
        let next_from = cur.map(|i| evs[i].stop).unwrap_or(now);
        let next = evs.iter().find(|e| e.start >= next_from);
        (cur.map(|i| &evs[i]), next)
    }

    /// Updates recording markers in place after scheduling or cancelling.
    pub fn set_dvr(&mut self, event_id: u64, dvr_uuid: Option<String>) {
        if let Some(e) = self.events.values_mut().flatten().find(|e| e.event_id == event_id) {
            e.dvr_state = dvr_uuid.as_ref().map(|_| "scheduled".to_string());
            e.dvr_uuid = dvr_uuid;
        }
    }
}

/// A selectable piece of a guide row: a program, or filler for time without EPG data.
#[derive(Debug, Clone, Copy)]
pub struct Span<'a> {
    pub start: i64,
    pub stop: i64,
    pub event: Option<&'a EpgEvent>,
}

/// Covers `[from, to)` completely. Program spans keep their real (unclipped) bounds;
/// gaps are filled with slot-aligned filler.
pub fn spans(events: &[EpgEvent], from: i64, to: i64) -> Vec<Span<'_>> {
    let mut out = Vec::new();
    let mut t = from;
    let push_filler = |out: &mut Vec<Span<'_>>, t: &mut i64, until: i64| {
        while *t < until {
            let s = (align(*t) + SLOT).min(until);
            out.push(Span { start: *t, stop: s, event: None });
            *t = s;
        }
    };
    for e in events.iter().filter(|e| e.stop > from && e.start < to) {
        if e.stop <= t {
            continue; // overlapping bad data
        }
        push_filler(&mut out, &mut t, e.start.min(to));
        let start = if out.is_empty() { e.start } else { e.start.max(t) };
        out.push(Span { start, stop: e.stop, event: Some(e) });
        t = e.stop;
    }
    push_filler(&mut out, &mut t, to);
    out
}

pub fn span_at<'a>(spans: &[Span<'a>], t: i64) -> Option<Span<'a>> {
    spans.iter().find(|s| s.start <= t && t < s.stop).copied()
}

/// Horizontal placement of a span within the window as fractions (x, w).
pub fn place(span: &Span<'_>, from: i64) -> (f32, f32) {
    let to = from + SPAN;
    let a = span.start.max(from);
    let b = span.stop.min(to);
    ((a - from) as f32 / SPAN as f32, (b - a).max(0) as f32 / SPAN as f32)
}

/// Cursor state for the grid. Rows are positions in the lineup's filtered view.
#[derive(Debug, Clone, Default)]
pub struct GuideNav {
    /// Window start (slot-aligned).
    pub start: i64,
    /// First visible row.
    pub top: usize,
    /// Selected row.
    pub row: usize,
    /// Time cursor: the selected span is the one containing it.
    pub anchor: i64,
}

impl GuideNav {
    pub fn open(row: usize, rows: usize, now: i64) -> Self {
        let mut nav = Self { start: align(now), top: 0, row, anchor: now };
        nav.clamp_rows(rows);
        nav
    }

    pub fn end(&self) -> i64 {
        self.start + SPAN
    }

    fn clamp_rows(&mut self, rows: usize) {
        if rows == 0 {
            self.row = 0;
            self.top = 0;
            return;
        }
        self.row = self.row.min(rows - 1);
        if self.row < self.top {
            self.top = self.row;
        } else if self.row >= self.top + ROWS {
            self.top = self.row + 1 - ROWS;
        }
        self.top = self.top.min(rows.saturating_sub(ROWS));
    }

    /// Moves `delta` rows, wrapping around the lineup.
    pub fn move_rows(&mut self, delta: i64, rows: usize) {
        if rows == 0 {
            return;
        }
        self.row = (self.row as i64 + delta).rem_euclid(rows as i64) as usize;
        self.clamp_rows(rows);
    }

    /// Jumps a page; stops at the ends instead of wrapping.
    pub fn page(&mut self, delta: i64, rows: usize) {
        if rows == 0 {
            return;
        }
        let row = (self.row as i64 + delta * ROWS as i64).clamp(0, rows as i64 - 1) as usize;
        self.top = (self.top as i64 + delta * ROWS as i64).clamp(0, rows.saturating_sub(ROWS) as i64) as usize;
        self.row = row;
        self.clamp_rows(rows);
    }

    /// Selects the next program on the current row, scrolling the window as needed.
    pub fn right(&mut self, events: &[EpgEvent]) {
        let s = spans(events, self.start, self.end() + SPAN);
        let next = span_at(&s, self.anchor).map(|sp| sp.stop).unwrap_or(align(self.anchor) + SLOT);
        self.anchor = next;
        while self.anchor >= self.end() {
            self.start += SLOT;
        }
        self.snap(events);
    }

    /// Selects the previous program; never scrolls before `now`'s slot.
    pub fn left(&mut self, events: &[EpgEvent], now: i64) {
        let min_start = align(now);
        let s = spans(events, self.start - SPAN, self.end());
        let edge = span_at(&s, self.anchor).map(|sp| sp.start).unwrap_or(align(self.anchor));
        let target = edge - 1;
        if target < min_start {
            // Already at the earliest selectable program; just make sure it's in view.
            self.start = self.start.min(align(self.anchor)).max(min_start);
            return;
        }
        self.anchor = target;
        // Scroll back far enough to show where the newly selected program begins.
        let begins = span_at(&s, target).map(|sp| sp.start).unwrap_or(target);
        self.start = self.start.min(align(begins).max(min_start));
        self.snap(events);
    }

    /// Shifts by whole days (Red/Green keys).
    pub fn days(&mut self, delta: i64, events_now: i64) {
        let min_start = align(events_now);
        self.start = (self.start + delta * DAY).max(min_start);
        self.anchor = (self.anchor + delta * DAY).max(self.start).max(events_now);
    }

    /// Re-anchors to the start of the selected span (clamped to the window) so vertical
    /// moves land on the program under the selection's left edge.
    pub fn snap(&mut self, events: &[EpgEvent]) {
        let s = spans(events, self.start, self.end());
        if let Some(sp) = span_at(&s, self.anchor) {
            self.anchor = sp.start.max(self.start);
        }
    }

    /// The selected span on the given row's events.
    pub fn selected<'a>(&self, events: &'a [EpgEvent]) -> Option<Span<'a>> {
        span_at(&spans(events, self.start, self.end()), self.anchor.max(self.start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_761_080_400; // a :00 slot boundary

    fn ev(id: u64, ch: &str, start: i64, mins: i64) -> EpgEvent {
        EpgEvent {
            event_id: id,
            channel_uuid: ch.into(),
            start,
            stop: start + mins * 60,
            title: format!("e{id}"),
            subtitle: None,
            summary: None,
            description: None,
            episode_onscreen: None,
            serieslink_uri: None,
            dvr_uuid: None,
            dvr_state: None,
        }
    }

    #[test]
    fn align_to_slots() {
        assert_eq!(align(T0 + 1799), T0);
        assert_eq!(align(T0 + 1800), T0 + 1800);
    }

    #[test]
    fn spans_fill_gaps_and_keep_real_bounds() {
        // Event from 15 min before the window for 45 min, gap, then one at +60.
        let evs = vec![ev(1, "a", T0 - 900, 45), ev(2, "a", T0 + 3600, 60)];
        let s = spans(&evs, T0, T0 + SPAN);
        let shape: Vec<_> = s.iter().map(|s| (s.start - T0, s.stop - T0, s.event.map(|e| e.event_id))).collect();
        assert_eq!(
            shape,
            [(-900, 1800, Some(1)), (1800, 3600, None), (3600, 7200, Some(2))]
        );
        let (x, w) = place(&s[0], T0);
        assert_eq!((x, w), (0.0, 0.25));
    }

    #[test]
    fn empty_row_is_all_filler() {
        let s = spans(&[], T0 + 600, T0 + SPAN);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].start, T0 + 600);
        assert_eq!(s[0].stop, T0 + 1800);
    }

    #[test]
    fn right_and_left_walk_programs_and_scroll() {
        let evs = vec![ev(1, "a", T0, 60), ev(2, "a", T0 + 3600, 90), ev(3, "a", T0 + 9000, 30)];
        let now = T0 + 300;
        let mut nav = GuideNav::open(0, 1, now);
        assert_eq!(nav.selected(&evs).unwrap().event.unwrap().event_id, 1);
        nav.right(&evs);
        assert_eq!(nav.selected(&evs).unwrap().event.unwrap().event_id, 2);
        assert_eq!(nav.start, T0);
        nav.right(&evs); // event 3 at +150min is past the 2h window → scroll
        assert_eq!(nav.selected(&evs).unwrap().event.unwrap().event_id, 3);
        assert_eq!(nav.start, T0 + 2 * SLOT);
        nav.left(&evs, now);
        assert_eq!(nav.selected(&evs).unwrap().event.unwrap().event_id, 2);
        nav.left(&evs, now);
        assert_eq!(nav.selected(&evs).unwrap().event.unwrap().event_id, 1);
        assert_eq!(nav.start, T0);
        nav.left(&evs, now); // can't go into the past
        assert_eq!(nav.selected(&evs).unwrap().event.unwrap().event_id, 1);
        assert_eq!(nav.start, T0);
    }

    #[test]
    fn rows_wrap_and_scroll() {
        let mut nav = GuideNav::open(0, 20, T0);
        nav.move_rows(-1, 20);
        assert_eq!((nav.row, nav.top), (19, 13));
        nav.move_rows(1, 20);
        assert_eq!((nav.row, nav.top), (0, 0));
        nav.page(1, 20);
        assert_eq!((nav.row, nav.top), (7, 7));
        nav.page(1, 20);
        assert_eq!((nav.row, nav.top), (14, 13));
    }

    #[test]
    fn cache_blocks_and_now_next() {
        let mut c = EpgCache::default();
        let b = T0 - T0.rem_euclid(BLOCK);
        assert_eq!(c.missing_blocks(T0, T0 + SPAN).first(), Some(&b));
        c.insert_block(b, vec![ev(2, "a", T0 + 1800, 30), ev(1, "a", T0, 30)]);
        c.insert_block(b + BLOCK, vec![ev(2, "a", T0 + 1800, 30)]); // duplicate across blocks
        assert_eq!(c.events("a").len(), 2);
        let (now, next) = c.now_next("a", T0 + 60);
        assert_eq!((now.unwrap().event_id, next.unwrap().event_id), (1, 2));
        c.set_dvr(2, Some("d".into()));
        assert_eq!(c.events("a")[1].dvr_uuid.as_deref(), Some("d"));
        c.prune(T0 + 1800);
        assert_eq!(c.events("a").len(), 1);
    }
}
