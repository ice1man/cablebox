//! The channel list as the viewer sees it: all channels, plus a filtered view
//! (favorites or a tag) that CH+/CH− and the guide follow.

use std::collections::HashSet;

use tvh::{Channel, ChannelNumber, ChannelTag};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Favorites,
    Tag(String),
}

impl Filter {
    pub fn parse(s: &str) -> Self {
        match s {
            "favorites" => Filter::Favorites,
            _ => match s.strip_prefix("tag:") {
                Some(t) => Filter::Tag(t.to_string()),
                None => Filter::All,
            },
        }
    }

    pub fn to_config(&self) -> String {
        match self {
            Filter::All => "all".into(),
            Filter::Favorites => "favorites".into(),
            Filter::Tag(t) => format!("tag:{t}"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Lineup {
    channels: Vec<Channel>,
    tags: Vec<ChannelTag>,
    favorites: HashSet<String>,
    filter: Filter,
    /// Indices into `channels` matching `filter`.
    visible: Vec<usize>,
}

impl Lineup {
    pub fn new(channels: Vec<Channel>, tags: Vec<ChannelTag>, favorites: impl IntoIterator<Item = String>) -> Self {
        let mut l = Self { channels, tags, favorites: favorites.into_iter().collect(), ..Default::default() };
        l.refilter();
        l
    }

    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn get(&self, uuid: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.uuid == uuid)
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// Applies a filter; falls back to All if it would leave nothing to watch.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.refilter();
        if self.visible.is_empty() && self.filter != Filter::All {
            self.filter = Filter::All;
            self.refilter();
        }
    }

    /// All → Favorites → each tag → All.
    pub fn cycle_filter(&mut self) {
        let mut order = vec![Filter::All, Filter::Favorites];
        order.extend(self.tags.iter().map(|t| Filter::Tag(t.uuid.clone())));
        let start = order.iter().position(|f| *f == self.filter).unwrap_or(0);
        for step in 1..=order.len() {
            let next = order[(start + step) % order.len()].clone();
            self.filter = next;
            self.refilter();
            if !self.visible.is_empty() {
                return;
            }
        }
        self.set_filter(Filter::All);
    }

    pub fn filter_label(&self) -> String {
        match &self.filter {
            Filter::All => "All Channels".into(),
            Filter::Favorites => "Favorites".into(),
            Filter::Tag(uuid) => self
                .tags
                .iter()
                .find(|t| &t.uuid == uuid)
                .map(|t| t.name.clone())
                .unwrap_or_else(|| "Tag".into()),
        }
    }

    pub fn visible(&self) -> impl ExactSizeIterator<Item = &Channel> + '_ {
        self.visible.iter().map(|&i| &self.channels[i])
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn visible_at(&self, pos: usize) -> Option<&Channel> {
        self.visible.get(pos).map(|&i| &self.channels[i])
    }

    /// Position of a channel in the filtered view.
    pub fn visible_position(&self, uuid: &str) -> Option<usize> {
        self.visible.iter().position(|&i| self.channels[i].uuid == uuid)
    }

    pub fn is_favorite(&self, uuid: &str) -> bool {
        self.favorites.contains(uuid)
    }

    /// Returns the new state.
    pub fn toggle_favorite(&mut self, uuid: &str) -> bool {
        let fav = if self.favorites.remove(uuid) {
            false
        } else {
            self.favorites.insert(uuid.to_string());
            true
        };
        if self.filter == Filter::Favorites {
            self.refilter();
        }
        fav
    }

    /// Favorites in lineup order (stable for the config file).
    pub fn favorites(&self) -> Vec<String> {
        self.channels.iter().filter(|c| self.favorites.contains(&c.uuid)).map(|c| c.uuid.clone()).collect()
    }

    /// Typed numbers search every channel, not just the filtered view — like a real box.
    /// A bare major (`5`) with no exact match picks its lowest subchannel (`5.1`).
    pub fn by_number(&self, n: ChannelNumber) -> Option<&Channel> {
        let exact = self.channels.iter().find(|c| c.number == n);
        if exact.is_some() || n.minor != 0 {
            return exact;
        }
        self.channels.iter().filter(|c| c.number.major == n.major).min_by_key(|c| c.number)
    }

    /// Next/previous channel in the filtered view, wrapping around. If `from` isn't in the
    /// view, picks the nearest by number in the requested direction.
    pub fn step(&self, from: Option<&str>, delta: i64) -> Option<&Channel> {
        let n = self.visible.len() as i64;
        if n == 0 {
            return None;
        }
        let pos = match from.and_then(|u| self.visible_position(u)) {
            Some(p) => (p as i64 + delta).rem_euclid(n),
            None => {
                let num = from.and_then(|u| self.get(u)).map(|c| c.number);
                let after = num.and_then(|num| self.visible().position(|c| c.number > num));
                match (after, delta > 0) {
                    (Some(p), true) => p as i64,
                    (Some(p), false) => (p as i64 - 1).rem_euclid(n),
                    (None, true) => 0,
                    (None, false) => n - 1,
                }
            }
        };
        self.visible_at(pos as usize)
    }

    /// Digit widths for channel-number entry: (major, minor).
    pub fn number_widths(&self) -> (usize, usize) {
        let digits = |n: u32| n.to_string().len();
        let major = self.channels.iter().map(|c| digits(c.number.major)).max().unwrap_or(1);
        let minor = self.channels.iter().filter(|c| c.number.minor > 0).map(|c| digits(c.number.minor)).max().unwrap_or(0);
        (major, minor)
    }

    fn refilter(&mut self) {
        self.visible = (0..self.channels.len())
            .filter(|&i| {
                let c = &self.channels[i];
                match &self.filter {
                    Filter::All => true,
                    Filter::Favorites => self.favorites.contains(&c.uuid),
                    Filter::Tag(t) => c.tags.iter().any(|ct| ct == t),
                }
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(uuid: &str, major: u32, minor: u32, tags: &[&str]) -> Channel {
        Channel {
            uuid: uuid.into(),
            enabled: true,
            name: uuid.to_uppercase(),
            number: ChannelNumber::new(major, minor),
            icon_public_url: None,
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn tag(uuid: &str, name: &str) -> ChannelTag {
        ChannelTag { uuid: uuid.into(), name: name.into(), enabled: true, internal: false, index: 0 }
    }

    fn lineup() -> Lineup {
        Lineup::new(
            vec![ch("a", 2, 0, &["news"]), ch("b", 5, 1, &[]), ch("c", 5, 2, &["news"]), ch("d", 120, 0, &[])],
            vec![tag("news", "News"), tag("empty", "Empty")],
            ["b".to_string()],
        )
    }

    #[test]
    fn step_wraps() {
        let l = lineup();
        assert_eq!(l.step(Some("a"), 1).unwrap().uuid, "b");
        assert_eq!(l.step(Some("d"), 1).unwrap().uuid, "a");
        assert_eq!(l.step(Some("a"), -1).unwrap().uuid, "d");
        assert_eq!(l.step(None, 1).unwrap().uuid, "a");
    }

    #[test]
    fn step_from_outside_filter_uses_number_order() {
        let mut l = lineup();
        l.set_filter(Filter::Tag("news".into()));
        // "b" (5.1) isn't tagged: up goes to 5.2, down goes to 2.
        assert_eq!(l.step(Some("b"), 1).unwrap().uuid, "c");
        assert_eq!(l.step(Some("b"), -1).unwrap().uuid, "a");
    }

    #[test]
    fn filters_and_cycle_skip_empty() {
        let mut l = lineup();
        l.cycle_filter();
        assert_eq!(l.filter(), &Filter::Favorites);
        assert_eq!(l.visible_len(), 1);
        l.cycle_filter();
        assert_eq!(l.filter_label(), "News");
        l.cycle_filter(); // "Empty" tag has no channels → skipped
        assert_eq!(l.filter(), &Filter::All);
    }

    #[test]
    fn empty_favorites_fall_back_to_all() {
        let mut l = lineup();
        l.toggle_favorite("b");
        l.set_filter(Filter::Favorites);
        assert_eq!(l.filter(), &Filter::All);
    }

    #[test]
    fn number_lookup_and_widths() {
        let l = lineup();
        assert_eq!(l.by_number(ChannelNumber::new(5, 2)).unwrap().uuid, "c");
        assert_eq!(l.by_number(ChannelNumber::new(5, 0)).unwrap().uuid, "b");
        assert!(l.by_number(ChannelNumber::new(5, 3)).is_none());
        assert!(l.by_number(ChannelNumber::new(7, 0)).is_none());
        assert_eq!(l.number_widths(), (3, 1));
    }

    #[test]
    fn filter_config_roundtrip() {
        for f in [Filter::All, Filter::Favorites, Filter::Tag("x".into())] {
            assert_eq!(Filter::parse(&f.to_config()), f);
        }
    }
}
