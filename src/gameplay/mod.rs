//! Player activities and their shared cleanup when a session or the plugin ends.
pub(crate) mod crawl;
pub(crate) mod events;
pub(crate) mod pose;
pub(crate) mod seat;

use crate::bindings::pumpkin::plugin::{player::Player, server::Server};

/// Clears owned effects without relocating a dead or teleporting player.
async fn cleanup(player: &Player, server: &Server) {
    seat::forget_return(player.as_entity().get_id());
    crawl::cleanup(player).await;
    seat::eject(player, server).await;
    seat::stop(player, server, false).await;
}

/// Clears all transient rides and poses on reload or unload.
pub(crate) async fn cleanup_all(server: &Server) {
    for p in server.get_all_players() {
        cleanup(&p, server).await
    }
}
