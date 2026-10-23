//! Read-only check of a real Tvheadend server with CableBox's client:
//!   TVH_URL=http://host:9981 TVH_USER=u TVH_PASS=p cargo run -p tvh --example check

use std::time::{SystemTime, UNIX_EPOCH};

use tvh::{Auth, Client};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("TVH_URL")?;
    let auth = match (std::env::var("TVH_USER"), std::env::var("TVH_PASS")) {
        (Ok(user), Ok(pass)) => Auth::Digest { user, pass },
        _ => Auth::None,
    };
    let c = Client::new(&url, auth)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

    let info = c.server_info().await?;
    println!("server: {} {} (api {})", info.name, info.sw_version, info.api_version);

    let channels = c.channels().await?;
    let tags = c.tags().await?;
    let with_minor = channels.iter().filter(|ch| ch.number.minor > 0).count();
    let numberless = channels.iter().filter(|ch| ch.number.major == 0).count();
    let icons = channels.iter().filter(|ch| ch.icon_public_url.is_some()).count();
    println!("channels: {} enabled ({with_minor} with minor numbers, {numberless} without a number, {icons} with icons)", channels.len());
    for ch in channels.iter().take(8) {
        println!("  {:>6}  {}  icon={}", ch.number.to_string(), ch.name, ch.icon_public_url.as_deref().unwrap_or("-"));
    }
    println!("tags: {}", tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "));

    let epg = c.epg(None, now, now + 6 * 3600).await?;
    let covered: std::collections::HashSet<_> = epg.iter().map(|e| e.channel_uuid.as_str()).collect();
    println!("epg next 6h: {} events across {} channels", epg.len(), covered.len());
    if let Some(e) = epg.iter().find(|e| e.is_airing(now)) {
        println!("  e.g. now on {}: {} ({}m){}", e.channel_uuid, e.title, (e.stop - e.start) / 60,
            e.episode_onscreen.as_deref().map(|s| format!(" {s}")).unwrap_or_default());
    }

    let (up, fin, auto) = (c.dvr_upcoming().await?, c.dvr_finished().await?, c.autorecs().await?);
    println!("dvr: {} upcoming, {} finished, {} series rules", up.len(), fin.len(), auto.len());
    if let Some(icon) = channels.iter().find_map(|ch| ch.icon_public_url.clone()) {
        let u = c.icon_url(&icon).unwrap();
        let same = c.is_same_origin(&u);
        match c.fetch(u.clone()).await {
            Ok(b) => println!("icon fetch ({}): {} bytes from {u}", if same { "server" } else { "external, no credentials" }, b.len()),
            Err(e) => println!("icon fetch failed: {e} ({u})"),
        }
    }
    Ok(())
}
