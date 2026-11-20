//! Pumpkin's clickable overview of the three GSit interaction preferences.
use super::{can_toggle, target_id};
use crate::{
    bindings::{
        CommandSender, ConsumedArgs, Context,
        pumpkin::plugin::{
            command::{Arg, ArgumentType, Command, CommandNode, CommandSenderType, StringType},
            player::Player,
            server::Server,
            uuid,
        },
    },
    message, preferences,
    storage::{FEATURES, Preference},
};

// The legacy command trees use handler IDs 8 through 63.
pub(super) const HANDLER: u32 = 64;

/// Registers a standalone command so that existing GSit arguments keep their Bukkit behavior.
pub(super) fn register(ctx: &Context) {
    let cmd = Command::new(&["gsitprefs".into()], "Show GSit interaction preferences");
    let target = CommandNode::argument("target", &ArgumentType::String(StringType::SingleWord));
    target.execute_with_handler_id(HANDLER);
    cmd.then(target);
    cmd.execute_with_handler_id(HANDLER);
    ctx.register_command(cmd, "PumpGsit:prefs");
}

/// Shows a player's own settings or a read-only console lookup of a known name or UUID.
/// Click actions go through the same permissions and command events as typed commands.
pub(super) async fn show(
    sender: &CommandSender,
    player: Option<&Player>,
    server: &Server,
    args: &ConsumedArgs,
) -> Result<(), &'static str> {
    let Arg::Simple(name) = args.get_value("target") else {
        return Err("target");
    };
    let id = match player {
        Some(player) if name.is_empty() => player.get_id(),
        Some(_) => return Err("prefs-self"),
        None if matches!(
            sender.get_command_sender_type(),
            CommandSenderType::Console | CommandSenderType::Rcon
        ) =>
        {
            if name.is_empty() {
                return Err("target");
            }
            target_id(&name, server).ok_or("target")?
        }
        None => return Err("command-sender-error"),
    };
    let online = player.is_some() || server.get_player_by_uuid(id).is_some();
    let values = preferences::snapshot(id, online).map_err(|error| {
        crate::warn(&error);
        "db"
    })?;

    let heading = message(player, "prefs-title");
    if player.is_none() {
        heading.add_text(&format!(" ({})", uuid::to_string(id)));
    }
    sender.send_message(heading);
    let may_run_commands =
        player.is_some() && sender.has_permission("PumpGsit:command".into()).await;
    for feature in FEATURES {
        let enabled = values[feature as usize];
        let (label, command) = match feature {
            Preference::Sit => ("prefs-sit", "/sit toggle"),
            Preference::PlayerSit => ("prefs-playersit", "/sit playertoggle"),
            Preference::Crawl => ("prefs-crawl", "/crawl toggle"),
        };
        let line = message(player, label);
        line.add_text("  ");
        line.add_child(message(
            player,
            if enabled { "prefs-on" } else { "prefs-off" },
        ));
        if may_run_commands && can_toggle(feature, sender).await {
            line.click_run_command(&format!("{command} {}", if enabled { "off" } else { "on" }));
            line.hover_show_text(message(player, "prefs-click"));
        }
        sender.send_message(line);
    }
    Ok(())
}
