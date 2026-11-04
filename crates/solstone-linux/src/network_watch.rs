// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Follows NetworkManager's network state. When the network comes back, a fresh status
//! check shows a returning connection as soon as the journal answers. When it goes, the
//! status can say offline at once, once the journal also stops answering.

use futures_util::StreamExt;

use crate::sync::NewEpisode;

const NETWORK_MANAGER: &str = "org.freedesktop.NetworkManager";
const NETWORK_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
/// NetworkManager's lowest connected state. A journal on the same network can answer
/// before there is a route to the internet.
const CONNECTED_LOCAL: u32 = 50;
const NM_STATE_UNKNOWN: u32 = 0;

/// NetworkManager says there is no network: asleep, disconnected, disconnecting, or still
/// connecting. Unknown says nothing.
pub(crate) fn no_network(state: u32) -> bool {
    state != NM_STATE_UNKNOWN && state < CONNECTED_LOCAL
}

/// The network has just come back: a connected state right after one that was not.
pub(crate) fn network_came_back(previous: Option<u32>, state: u32) -> bool {
    state >= CONNECTED_LOCAL && previous.is_some_and(|previous| previous < CONNECTED_LOCAL)
}

/// Runs for the life of the app. Without NetworkManager there is nothing to watch, and the
/// regular status checks still find a returning connection.
pub(crate) async fn watch(episode: NewEpisode) {
    if let Err(error) = watch_changes(&episode).await {
        tracing::debug!(%error, "Network changes are not watched");
    }
}

async fn watch_changes(episode: &NewEpisode) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        NETWORK_MANAGER,
        NETWORK_MANAGER_PATH,
        NETWORK_MANAGER,
    )
    .await?;
    let mut changes = proxy.receive_signal("StateChanged").await?;
    let mut previous = proxy.get_property::<u32>("State").await.ok();
    if previous.is_some_and(no_network) {
        episode.network_lost();
    }
    while let Some(message) = changes.next().await {
        let Ok(state) = message.body().deserialize::<u32>() else {
            continue;
        };
        if network_came_back(previous, state) {
            episode.begin();
        } else if no_network(state) && !previous.is_some_and(no_network) {
            episode.network_lost();
        }
        previous = Some(state);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_return_to_a_connected_state_counts() {
        assert!(network_came_back(Some(20), 70));
        assert!(network_came_back(Some(40), 50));
        assert!(!network_came_back(Some(70), 70));
        assert!(!network_came_back(Some(60), 70));
        assert!(!network_came_back(Some(70), 20));
        assert!(!network_came_back(None, 70));
    }

    #[test]
    fn asleep_disconnected_and_connecting_are_no_network() {
        for state in [10, 20, 30, 40] {
            assert!(no_network(state), "{state}");
        }
        for state in [0, 50, 60, 70] {
            assert!(!no_network(state), "{state}");
        }
    }
}
