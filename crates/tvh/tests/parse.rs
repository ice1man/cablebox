use tvh::*;

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Grid<T> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn channels() {
    let g: Grid<Channel> = load("channels.json");
    assert_eq!(g.entries.len(), 5);
    let nums: Vec<_> = g.entries.iter().map(|c| c.number.to_string()).collect();
    assert_eq!(nums, ["5.1", "4", "99", "5.2", "0"]);
    assert!(!g.entries[2].enabled);
    assert_eq!(g.entries[0].tags, ["t1", "t2"]);
    assert_eq!(g.entries[0].icon_public_url.as_deref(), Some("imagecache/7"));
}

#[test]
fn epg() {
    let g: Grid<EpgEvent> = load("epg.json");
    let [news, show] = &g.entries[..] else { panic!() };
    assert_eq!(news.event_id, 1001);
    assert_eq!(news.dvr_uuid.as_deref(), Some("d1"));
    assert_eq!(news.synopsis(), "The full evening newscast.");
    assert!(news.is_airing(1761080400));
    assert!(!news.is_airing(1761082200));
    assert_eq!(show.episode_onscreen.as_deref(), Some("S03E12"));
    assert_eq!(show.synopsis(), "");
}

#[test]
fn dvr() {
    let g: Grid<DvrEntry> = load("dvr_upcoming.json");
    let e = &g.entries[0];
    assert_eq!(e.disp_title, "Evening News");
    assert_eq!(e.channel.as_deref(), Some("c2"));
    assert_eq!(e.broadcast, Some(1001));
    assert!(!e.is_recording());
}
