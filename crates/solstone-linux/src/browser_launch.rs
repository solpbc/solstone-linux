// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Repair Konqueror's URL-less menu launcher without changing the owner's default.

use std::path::Path;

use gio::prelude::*;

fn needs_uri_argument(id: Option<&str>, command: Option<&Path>, supports_uris: bool) -> bool {
    id == Some("konqbrowser.desktop") && command == Some(Path::new("konqueror")) && !supports_uris
}

/// This runs only in the short-lived browser helper, outside the app service.
/// None leaves the existing desktop opener in charge; a failed repair is never
/// retried through the known URL-dropping launcher and reported as success.
pub(crate) fn open_known_konqueror(target: &str) -> Option<bool> {
    // Inspect only the scheme. The journal's one-time link stays opaque and is
    // passed unchanged, rather than reconstructed outside its authority seam.
    let (scheme, _) = target.split_once(':')?;
    let scheme = if scheme.eq_ignore_ascii_case("http") {
        "http"
    } else if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else {
        return None;
    };
    let selected = gio::AppInfo::default_for_uri_scheme(scheme)?;
    if !needs_uri_argument(
        selected.id().as_deref(),
        selected.commandline().as_deref(),
        selected.supports_uris(),
    ) {
        return None;
    }

    // Upstream konqbrowser.desktop is a menu entry: Exec=konqueror, without %u.
    // Give that same browser a URI-aware, in-memory launch description. The
    // known HTML type avoids a KIO MIME probe consuming a one-time journal link.
    // Keep the target out of the command template; GIO receives it as one URI.
    Some(
        gio::AppInfo::create_from_commandline(
            "konqueror --mimetype text/html",
            Some("Konqueror"),
            gio::AppInfoCreateFlags::SUPPORTS_URIS,
        )
        .and_then(|app| app.launch_uris(&[target], None::<&gio::AppLaunchContext>))
        .is_ok(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_packaged_url_less_entry_is_repaired() {
        assert!(needs_uri_argument(
            Some("konqbrowser.desktop"),
            Some(Path::new("konqueror")),
            false,
        ));
        for (id, command, uris) in [
            (Some("firefox.desktop"), Some("firefox"), false),
            (
                Some("kfmclient_html.desktop"),
                Some("kfmclient --mimetype text/html openURL %u"),
                true,
            ),
            (
                Some("konqbrowser.desktop"),
                Some("konqueror --profile custom"),
                false,
            ),
            (
                Some("konqbrowser.desktop"),
                Some("my-browser-wrapper"),
                false,
            ),
            (Some("konqbrowser.desktop"), Some("konqueror %U"), true),
            (Some("konqbrowser.desktop"), Some("konqueror"), true),
            (None, Some("konqueror"), false),
            (Some("konqbrowser.desktop"), None, false),
        ] {
            assert!(!needs_uri_argument(id, command.map(Path::new), uris));
        }
    }
}
