use url::Url;
use voxhold_api::{Client, LinkPreview};

pub(super) async fn load_preview(
    client: &Client,
    requested: &str,
) -> Result<LinkPreview, Option<u16>> {
    let preview = fetch_preview(client, requested).await?;
    let Some(community) = steam_community_url(requested) else {
        return Ok(preview);
    };
    if !preview.unavailable
        && ![
            "",
            "Site Error",
            "Steam Store",
            "Welcome to Steam",
            "Age Check",
            "store.steampowered.com",
        ]
        .iter()
        .any(|title| preview.title.trim().eq_ignore_ascii_case(title))
    {
        return Ok(preview);
    }

    // The Steam store can return an error or age gate with HTTP 200. The Steam
    // Community hub identifies the same app and is fetched by the instance,
    // just like any other preview. Its default artwork is the Steam logo.
    let title = fetch_preview(client, &community)
        .await
        .ok()
        .filter(|preview| !preview.unavailable)
        .and_then(|preview| {
            preview
                .title
                .strip_suffix(" :: Steam Community")
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(str::to_owned)
        });
    Ok(LinkPreview {
        url: requested.into(),
        site_name: "Steam".into(),
        unavailable: title.is_none(),
        title: title.unwrap_or_else(|| "Страница в Steam".into()),
        ..Default::default()
    })
}

async fn fetch_preview(client: &Client, requested: &str) -> Result<LinkPreview, Option<u16>> {
    let preview = client
        .link_preview(requested)
        .await
        .map_err(|error| error.status())?;
    let expected = Url::parse(requested).map_err(|_| None)?;
    let returned = Url::parse(&preview.url).map_err(|_| None)?;
    if expected != returned {
        return Err(None);
    }
    Ok(preview)
}

fn steam_community_url(href: &str) -> Option<String> {
    let url = Url::parse(href).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("store.steampowered.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let mut segments = url.path_segments()?;
    if segments.next()? != "app" {
        return None;
    }
    let id = segments.next()?;
    if !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let app = id.parse::<u32>().ok().filter(|id| *id > 0)?;
    Some(format!("https://steamcommunity.com/app/{app}"))
}

#[cfg(test)]
#[test]
fn a_steam_error_page_falls_back_to_the_apps_public_community_title() {
    let store = "https://store.steampowered.com/app/1091500/Cyberpunk_2077/";
    let community = "https://steamcommunity.com/app/1091500";
    let server = crate::test_support::TestServer::spawn(vec![
        crate::test_support::Reply::new(200, &serde_json::json!({
            "url": store, "title": "Site Error", "image_path": "ignored"
        }).to_string()),
        crate::test_support::Reply::new(200, &serde_json::json!({
            "url": community, "title": "Cyberpunk 2077 :: Steam Community", "image_path": "generic"
        }).to_string()),
    ]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let preview = runtime
        .block_on(load_preview(&Client::new(&server.base).unwrap(), store))
        .unwrap();
    assert_eq!(preview.url, store);
    assert_eq!(preview.title, "Cyberpunk 2077");
    assert!(preview.image_path.is_empty());
    assert_eq!(server.requests().len(), 2);
}
