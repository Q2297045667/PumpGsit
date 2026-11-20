//! Brigadier command trees, Bukkit permission mappings, and player preference writes.
use crate::{
    bindings::{
        CommandError, CommandSender, ConsumedArgs, Context,
        pumpkin::plugin::{
            command::{Arg, ArgumentType, Command, CommandNode, CommandSenderType, StringType},
            permission::{Permission, PermissionChild, PermissionDefault, PermissionLevel},
            player::Player,
            server::Server,
            uuid,
        },
    },
    config, crawl, data_dir, hint, message,
    pose::{self, Pose},
    seat::{self, Activity},
    storage::{self, Preference},
};

const SIT: u32 = 1;
const CRAWL: u32 = 6;
const RELOAD: u32 = 7;
// Each executable node carries the command and the literal words Brigadier consumes.
const PREFIXES: [(&str, &str); 8] = [
    ("", ""),
    ("toggle", ""),
    ("toggle", "on"),
    ("toggle", "off"),
    ("playertoggle", ""),
    ("playertoggle", "on"),
    ("playertoggle", "off"),
    ("help", ""),
];
const GROUPS: &[(&str, &[&str])] = &[
    ("sit.*", &["sit", "sitclick", "sittoggle"]),
    ("playersit.*", &["playersit", "playersittoggle"]),
    ("pose.*", &["lay", "legsup", "bellyflop", "spin"]),
    ("crawl.*", &["crawl", "crawlsneak", "crawltoggle"]),
    ("kick.*", &["kick.sit", "kick.pose"]),
    ("bypass.*", &["bypass.command", "bypass.world"]),
    (
        "*",
        &[
            "sit.*",
            "playersit.*",
            "pose.*",
            "crawl.*",
            "kick.*",
            "bypass.*",
            "reload",
        ],
    ),
];

/// Registers a permission, mapping Bukkit's GSit prefix into Pumpkin's required namespace.
fn permission(ctx: &Context, key: &str, operator: bool, children: &[&str]) -> Result<(), String> {
    let node = format!("PumpGsit:{key}");
    let registered = ctx.register_permission(&Permission {
        node: node.clone(),
        description: format!("PumpGsit {key}"),
        default: if operator {
            PermissionDefault::Op(PermissionLevel::Two)
        } else {
            PermissionDefault::Allow
        },
        children: children
            .iter()
            .map(|name| PermissionChild {
                node: format!("PumpGsit:{name}"),
                value: true,
            })
            .collect(),
    });
    // Pumpkin retains this registry across binary unload/load in the same server.
    match registered {
        Err(error) if error == format!("Permission {node} is already registered") => Ok(()),
        result => result,
    }
}

/// Keeps the remaining Bukkit words intact, including unrecognized or extra arguments.
fn tail(id: u32) -> CommandNode {
    let node = CommandNode::argument("args", &ArgumentType::String(StringType::Greedy));
    node.execute_with_handler_id(id);
    node
}

/// Builds a native Brigadier literal that accepts and forwards any remaining text.
fn literal(word: &str, id: u32) -> CommandNode {
    let node = CommandNode::literal(word);
    node.execute_with_handler_id(id);
    node.then(tail(id));
    node
}

/// Publishes native on/off completions in both the player and console argument positions.
fn toggle_branch(base: u32, slot: usize) -> CommandNode {
    let id = base + slot as u32;
    let node = CommandNode::literal(PREFIXES[slot].0);
    node.execute_with_handler_id(id);
    let target = CommandNode::argument("target", &ArgumentType::String(StringType::SingleWord));
    target.execute_with_handler_id(id);
    for (offset, mode) in [(1, "on"), (2, "off")] {
        node.then(literal(mode, id + offset));
        target.then(literal(mode, id + offset));
    }
    target.then(tail(id));
    node.then(target);
    node.then(tail(id));
    node
}

/// Registers seven command trees, including their original GSit aliases and permission groups.
pub fn register(ctx: &Context) -> Result<(), String> {
    for &(group, children) in GROUPS {
        if group != "*" {
            for leaf in children {
                permission(ctx, leaf, group == "kick.*" || group == "bypass.*", &[])?
            }
        }
        permission(ctx, group, true, children)?;
    }
    permission(ctx, "reload", true, &[])?;
    permission(ctx, "command", false, &[])?;
    let commands: &[(&[&str], &str, &str)] = &[
        (
            &["pumpgsit", "gsit", "sit"],
            "Sit down or set sitting preferences",
            "command",
        ),
        (&["glay", "lay", "glayback", "layback"], "Lie down", "lay"),
        (&["glegsup", "legsup"], "Rest with legs up", "legsup"),
        (&["gbellyflop", "bellyflop"], "Lie face down", "bellyflop"),
        (&["gspin", "spin"], "Spin in place", "spin"),
        (
            &["gcrawl", "crawl"],
            "Crawl or set crawling preferences",
            "command",
        ),
        (
            &["pumpgsitreload", "gsitreload", "gsitrl"],
            "Reload PumpGsit settings",
            "reload",
        ),
    ];
    for (i, (names, description, perm)) in commands.iter().enumerate() {
        let cmd = Command::new(
            &names.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            description,
        );
        let id = i as u32 + 1;
        let base = id * PREFIXES.len() as u32;
        cmd.execute_with_handler_id(base);
        if id == SIT {
            cmd.then(literal("help", base + 7));
            cmd.then(toggle_branch(base, 1));
            if config().player_sit.allow_sit {
                cmd.then(toggle_branch(base, 4));
            }
        } else if id == CRAWL && (config().crawl.corridor_crawl || config().crawl.double_sneak) {
            cmd.then(toggle_branch(base, 1));
        }
        cmd.then(tail(base));
        ctx.register_command(cmd, &format!("PumpGsit:{perm}"));
    }
    Ok(())
}

/// Returns the configured default used when the UUID is absent from the legacy table.
pub fn default_for(feature: Preference) -> bool {
    let cfg = config();
    match feature {
        Preference::Sit => cfg.sit.default_enabled,
        Preference::PlayerSit => cfg.player_sit.default_enabled,
        Preference::Crawl => cfg.crawl.default_enabled,
    }
}

/// Matches a preference subcommand with its Bukkit command's case rules.
fn preference(id: u32, word: &str) -> Option<Preference> {
    match id {
        SIT if word == "toggle" => Some(Preference::Sit),
        SIT if word == "playertoggle" => Some(Preference::PlayerSit),
        CRAWL if word.eq_ignore_ascii_case("toggle") => Some(Preference::Crawl),
        _ => None,
    }
}

/// Checks the current gameplay switches and sender permissions before changing a preference.
async fn can_toggle(pref: Preference, sender: &CommandSender) -> bool {
    let cfg = config();
    let node = match pref {
        Preference::Sit => "PumpGsit:sittoggle",
        Preference::PlayerSit if cfg.player_sit.allow_sit => "PumpGsit:playersittoggle",
        Preference::Crawl if cfg.crawl.corridor_crawl || cfg.crawl.double_sneak => {
            "PumpGsit:crawltoggle"
        }
        _ => return false,
    };
    sender.has_permission(node.into()).await
}

/// Reads stored server identities available through the Wasm API, including offline entries.
fn known_profiles(server: &Server) -> impl Iterator<Item = (String, uuid::Uuid)> {
    let ops = server
        .get_op_manager()
        .list_ops()
        .into_iter()
        .map(|p| (p.name, p.uuid));
    let whitelist = server
        .get_whitelist_manager()
        .list_entries()
        .into_iter()
        .map(|p| (p.name, p.uuid));
    let bans = server
        .get_ban_manager()
        .list_player_bans()
        .into_iter()
        .map(|p| (p.name, p.uuid));
    ops.chain(whitelist).chain(bans)
}

/// Resolves a console target by UUID, connected player or an unambiguous stored profile.
fn target_id(name: &str, server: &Server) -> Option<uuid::Uuid> {
    if let Some(id) = uuid::parse(name) {
        return Some(id);
    }
    if let Some(player) = server.get_player_by_name(name) {
        return Some(player.get_id());
    }
    let mut ids = known_profiles(server)
        .filter(|(profile, _)| profile.eq_ignore_ascii_case(name))
        .map(|(_, id)| id);
    let id = ids.next()?;
    ids.all(|other| (other.high, other.low) == (id.high, id.low))
        .then_some(id)
}

/// Commits the preference before acknowledging it and invalidating cached gameplay state.
fn preferences(
    pref: Preference,
    target: uuid::Uuid,
    mode: Option<&str>,
) -> Result<&'static str, &'static str> {
    let dir = data_dir();
    let key = uuid::to_string(target);
    let default = default_for(pref);
    let change = || -> Result<bool, String> {
        let value = match mode {
            Some(mode) if mode.eq_ignore_ascii_case("on") => true,
            Some(mode) if mode.eq_ignore_ascii_case("off") => false,
            _ => !storage::enabled(&dir, pref, &key, default)?,
        };
        storage::set_enabled(&dir, pref, &key, default, value)?;
        crate::forget_preferences(target);
        Ok(value)
    };
    match change() {
        Ok(value) => Ok(match (pref, value) {
            (Preference::Sit, true) => "command-gsit-toggle-on",
            (Preference::Sit, false) => "command-gsit-toggle-off",
            (Preference::PlayerSit, true) => "command-gsit-playertoggle-on",
            (Preference::PlayerSit, false) => "command-gsit-playertoggle-off",
            (Preference::Crawl, true) => "command-gcrawl-toggle-on",
            (Preference::Crawl, false) => "command-gcrawl-toggle-off",
        }),
        Err(error) => {
            crate::warn(&error);
            Err("db")
        }
    }
}

/// Toggles a player posture after checking permission, current activity, world and server acceptance.
async fn posture(id: u32, player: &Player, server: &Server) -> Result<(), &'static str> {
    let (activity, perm, pose) = match id {
        SIT => (Activity::Sit, "PumpGsit:sit", None),
        2 => (Activity::Lay, "PumpGsit:lay", Some(Pose::Lay)),
        3 => (Activity::LegsUp, "PumpGsit:legsup", Some(Pose::LegsUp)),
        4 => (
            Activity::BellyFlop,
            "PumpGsit:bellyflop",
            Some(Pose::BellyFlop),
        ),
        5 => (Activity::Spin, "PumpGsit:spin", Some(Pose::Spin)),
        CRAWL => {
            if !player.has_permission("PumpGsit:crawl".into()).await {
                return Err("command-permission-error");
            }
            if crawl::active(player) {
                crawl::stop(player).await;
                return Ok(());
            }
            if !seat::allowed(player).await {
                return Err("action-crawl-world-error");
            }
            crawl::start(player)
                .await
                .map_err(|_| "action-crawl-now-error")?;
            hint(player, "action-crawl-info");
            return Ok(());
        }
        _ => return Err("state"),
    };
    if !player.has_permission(perm.into()).await {
        return Err("command-permission-error");
    }
    if seat::activity(player) == Some(activity) {
        seat::stop(player, server, true).await;
        return Ok(());
    }
    if pose.is_some() && player.as_java().is_none() {
        return Err("edition");
    }
    let surface = seat::start(player, server, activity, None)
        .await
        .map_err(|reason| match (pose.is_some(), reason) {
            (false, "state") => "action-sit-now-error",
            (false, "world") => "action-sit-world-error",
            (false, "location") => "action-sit-location-error",
            (false, "occupied") => "action-sit-kick-error",
            (false, _) => "action-sit-error",
            (true, "state") => "action-pose-now-error",
            (true, "world") => "action-pose-world-error",
            (true, "location") => "action-pose-location-error",
            (true, "occupied") => "action-pose-kick-error",
            (true, _) => "action-pose-error",
        })?;
    if let Some(kind) = pose
        && let Err(error) = pose::start_at(player, kind, server, surface).await
    {
        crate::warn(&error);
        seat::stop(player, server, true).await;
        return Err("action-pose-error");
    }
    hint(
        player,
        if pose.is_some() {
            "action-pose-info"
        } else {
            "action-sit-info"
        },
    );
    Ok(())
}

/// Handles preference arguments before falling back to the original command's posture checks.
async fn execute<'a>(
    id: u32,
    sender: &CommandSender,
    player: Option<&Player>,
    server: &Server,
    mut words: impl Iterator<Item = &'a str>,
) -> Result<Option<&'static str>, &'static str> {
    let first = words.next();
    if id == SIT && first == Some("help") {
        return Ok(Some("help"));
    }
    if id == RELOAD {
        if !matches!(
            sender.get_command_sender_type(),
            CommandSenderType::Rcon | CommandSenderType::Console | CommandSenderType::Player(_)
        ) {
            return Err("command-sender-error");
        }
        if !sender.has_permission("PumpGsit:reload".into()).await {
            return Err("command-permission-error");
        }
        return crate::reload(server)
            .await
            .map(|()| Some("plugin-reload"))
            .map_err(|err| {
                crate::warn(&err);
                "bad_config"
            });
    }
    if let Some(pref) = first.and_then(|word| preference(id, word))
        && can_toggle(pref, sender).await
    {
        if let Some(player) = player {
            return preferences(pref, player.get_id(), words.next()).map(Some);
        }
        if let Some(name) = words.next() {
            let target = target_id(name, server).ok_or("target")?;
            return preferences(pref, target, words.next()).map(Some);
        }
    }
    posture(id, player.ok_or("command-sender-error")?, server).await?;
    Ok(None)
}

/// Executes one registered handler and renders all responses in the sender's language.
pub async fn run(
    id: u32,
    sender: CommandSender,
    server: Server,
    args: ConsumedArgs,
) -> Result<i32, CommandError> {
    let player = sender.as_player();
    let (Arg::Simple(target), Arg::Simple(args)) =
        (args.get_value("target"), args.get_value("args"))
    else {
        return Ok(0);
    };
    let (command, mode) = PREFIXES[id as usize % PREFIXES.len()];
    let words = [command, target.as_str(), mode]
        .into_iter()
        .filter(|word| !word.is_empty())
        .chain(args.split_ascii_whitespace());
    let reply = execute(
        id / PREFIXES.len() as u32,
        &sender,
        player.as_ref(),
        &server,
        words,
    )
    .await;
    match reply {
        Ok(Some(key)) => {
            sender.send_message(message(player.as_ref(), key));
            Ok(1)
        }
        Ok(None) => Ok(1),
        Err(key) => {
            sender.send_error(message(player.as_ref(), key));
            Ok(0)
        }
    }
}
