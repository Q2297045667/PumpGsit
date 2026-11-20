//! Java protocol 777 acceptance for seats, restart positions and clickable chat components.

use crate::support::{Server, boot, data_dir};
use std::{
    fs,
    io::{Cursor, Read, Write},
    net::TcpStream,
    thread,
    time::{Duration, Instant},
};
use tempfile::tempdir;

/// Encodes a nonnegative Minecraft VarInt for a packet length or numeric field.
fn varint(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

/// Reads a fixed-size value from a complete packet or a connected socket.
fn take<const N: usize>(reader: &mut impl Read) -> [u8; N] {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes).unwrap();
    bytes
}

/// Reads an unsigned, at-most-five-byte VarInt.
fn integer(reader: &mut impl Read) -> u32 {
    let mut value = 0;
    for shift in (0..35).step_by(7) {
        let byte = take::<1>(reader)[0];
        value |= ((byte & 127) as u32) << shift;
        if byte < 128 {
            return value;
        }
    }
    panic!("Invalid protocol VarInt");
}

/// Encodes a length-prefixed Minecraft UTF-8 string.
fn string(value: &str) -> Vec<u8> {
    let mut bytes = varint(value.len() as u32);
    bytes.extend(value.as_bytes());
    bytes
}

#[derive(Clone, Copy)]
enum Phase {
    Login,
    Config,
    Play,
}

/// A synchronous client for the test server's explicit offline, uncompressed Java listener.
struct Player {
    stream: TcpStream,
    phase: Phase,
    locale: String,
    entity: Option<u32>,
    position: Option<[f64; 3]>,
    vehicle: Option<u32>,
    feedback: String,
    pong: u64,
}

impl Player {
    /// Logs in and waits for acknowledgement of the initial server position.
    fn connect(server: &mut Server, name: &str, locale: &str) -> Self {
        let port = server.port;
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.set_nodelay(true).unwrap();
        let mut player = Self {
            stream,
            phase: Phase::Login,
            locale: locale.into(),
            entity: None,
            position: None,
            vehicle: None,
            feedback: String::new(),
            pong: 0,
        };
        let mut handshake = varint(777);
        handshake.extend(string("127.0.0.1"));
        handshake.extend(port.to_be_bytes());
        handshake.push(2);
        player.send(0, &handshake);
        let mut login = string(name);
        // Use a stable, distinct login UUID for each offline client and its reconnect.
        let mut uuid = [0u8; 16];
        uuid[..name.len()].copy_from_slice(name.as_bytes());
        login.extend(uuid);
        player.send(0, &login);
        player.until(|p| p.position.is_some() && p.entity.is_some());
        player.sync();
        server.wait(&format!("{name} joined the game"));
        player
    }

    /// Sends one complete uncompressed Minecraft packet.
    fn send(&mut self, packet: u32, data: &[u8]) {
        let id = varint(packet);
        let mut frame = varint((id.len() + data.len()) as u32);
        frame.extend(id);
        frame.extend(data);
        self.stream.write_all(&frame).unwrap();
    }

    /// Handles setup, position, passengers, chat and liveness for a single server packet.
    fn receive(&mut self) {
        let length = integer(&mut self.stream) as usize;
        assert!(length <= 16 << 20, "Unexpected packet size {length}");
        let mut frame = vec![0; length];
        self.stream.read_exact(&mut frame).unwrap();
        let mut body = Cursor::new(&frame[..]);
        let id = integer(&mut body);
        let payload = &frame[body.position() as usize..];
        match (self.phase, id) {
            (Phase::Login, 2) => {
                self.send(3, &[]);
                self.phase = Phase::Config;
                let mut settings = string(&self.locale);
                settings.extend([3, 0, 1, 127, 1, 0, 1, 0]);
                self.send(0, &settings);
            }
            (Phase::Login, 3) => panic!("Expected test server to disable compression"),
            (Phase::Config, 15) => self.send(7, &[0]),
            (Phase::Config, 4 | 5) => self.send(id, payload),
            (Phase::Config, 3) => {
                self.send(3, &[]);
                self.phase = Phase::Play;
            }
            (Phase::Play, 50) => self.entity = Some(u32::from_be_bytes(take(&mut body))),
            (Phase::Play, 73) => {
                let mut ack = varint(integer(&mut body));
                let xyz: [u8; 24] = take(&mut body);
                self.position = Some(std::array::from_fn(|i| {
                    f64::from_be_bytes(xyz[i * 8..i * 8 + 8].try_into().unwrap())
                }));
                let _velocity: [u8; 24] = take(&mut body);
                ack.extend(xyz);
                ack.extend(take::<8>(&mut body));
                self.send(0, &ack);
                self.send(44, &[]);
            }
            (Phase::Play, 124) => self.feedback = String::from_utf8_lossy(payload).into_owned(),
            (Phase::Play, 109) => {
                let vehicle = integer(&mut body);
                let riders = (0..integer(&mut body))
                    .map(|_| integer(&mut body))
                    .collect::<Vec<_>>();
                if self.entity.is_some_and(|id| riders.contains(&id)) {
                    self.vehicle = Some(vehicle);
                } else if self.vehicle == Some(vehicle) {
                    self.vehicle = None;
                }
            }
            (Phase::Play, 63) => self.pong = u64::from_be_bytes(take(&mut body)),
            (Phase::Play, 45) => self.send(28, payload),
            (Phase::Play, 62) => self.send(45, payload),
            (Phase::Play, 11) => self.send(11, &5f32.to_be_bytes()),
            (Phase::Login, 0) | (Phase::Config, 2) | (Phase::Play, 32) => {
                panic!("Client disconnected: {}", String::from_utf8_lossy(payload))
            }
            _ => {}
        }
    }

    /// Processes packets until a server-authoritative observation or the test deadline.
    fn until(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !predicate(self) {
            assert!(
                Instant::now() < deadline,
                "position {:?}, vehicle {:?}, last message {:?}",
                self.position,
                self.vehicle,
                self.feedback
            );
            self.receive();
        }
    }

    /// Waits for a ping response after previously sent movement or acknowledgement packets.
    fn sync(&mut self) {
        let nonce = self.pong + 1;
        self.send(38, &nonce.to_be_bytes());
        self.until(|p| p.pong == nonce);
    }

    /// Teleports into the prepared test area, acknowledges the position and reports grounded feet.
    fn stand_at(&mut self, server: &mut Server, name: &str, pos: [f64; 3], yaw: i32, pitch: i32) {
        self.position = None;
        server.command(
            &format!("tp {name} {} {} {} {yaw} {pitch}", pos[0], pos[1], pos[2]),
            "Teleported",
        );
        self.until(|p| p.position == Some(pos));
        let mut ground: Vec<u8> = pos.into_iter().flat_map(f64::to_be_bytes).collect();
        ground.push(1);
        self.send(30, &ground);
        self.send(13, &[]);
        self.sync();
    }

    /// Sends the player's unsigned slash command through the normal Java play packet.
    fn command(&mut self, line: &str) {
        self.feedback.clear();
        self.send(7, &string(line));
    }

    /// Waits for a particular text component and returns its full wire payload for event checks.
    fn chat(&mut self, text: &str) -> String {
        self.until(|p| p.feedback.contains(text));
        std::mem::take(&mut self.feedback)
    }
}

/// Verifies labels, state, hover/click actions and self-only access in two client locales.
#[test]
#[ignore = "requires PUMPGSIT_SERVER and PUMPGSIT_COMPONENT"]
fn player_prefs() {
    let temp = tempdir().unwrap();
    let mut server = boot(temp.path(), "languages", true, false, None);
    for (name, locale, labels, enabled, disabled, confirmation, own, hover) in [
        (
            "PrefsEn",
            "en_us",
            [
                "Right-click blocks to sit",
                "Right-click players to stack",
                "Sneak gestures to crawl",
            ],
            "ON",
            "OFF",
            "have disabled",
            "without a target",
            "Click to switch",
        ),
        (
            "PrefsZh",
            "zh_cn",
            ["右键方块坐下", "右键玩家叠坐", "潜行手势爬行"],
            "开启",
            "关闭",
            "你不能通过右键坐下了",
            "直接输入 /gsitprefs",
            "点击切换",
        ),
    ] {
        let mut player = Player::connect(&mut server, name, locale);
        player.command("gsitprefs");
        for (label, command) in labels.into_iter().zip([
            "/sit toggle off",
            "/sit playertoggle off",
            "/crawl toggle off",
        ]) {
            let packet = player.chat(label);
            for expected in [enabled, command, "run_command", "show_text", hover] {
                assert!(
                    packet.contains(expected),
                    "Expected {expected:?}: {packet:?}"
                );
            }
        }
        // A click in vanilla sends the command from the component as a normal command packet.
        player.command("sit toggle off");
        player.chat(confirmation);
        player.command("gsitprefs");
        let packet = player.chat(labels[0]);
        assert!(
            packet.contains(disabled) && packet.contains("/sit toggle on"),
            "{packet:?}"
        );
        player.command("gsitprefs Other");
        player.chat(own);
        drop(player);
        server.wait(&format!("{name} left the game"));
    }
    let cfg = data_dir(temp.path()).join("config.toml");
    let settings = fs::read_to_string(&cfg).unwrap();
    fs::write(
        cfg,
        settings
            .replace("allow_sit = true", "allow_sit = false")
            .replace("corridor_crawl = true", "corridor_crawl = false"),
    )
    .unwrap();
    server.command("gsitreload", "successfully reloaded");
    let mut viewer = Player::connect(&mut server, "ReadOnlyViewer", "en_us");
    viewer.command("gsitprefs");
    assert!(
        viewer
            .chat("Right-click blocks to sit")
            .contains("/sit toggle off")
    );
    for label in ["Right-click players to stack", "Sneak gestures to crawl"] {
        let row = viewer.chat(label);
        assert!(
            row.contains("ON") && !row.contains("run_command") && !row.contains("show_text"),
            "{row:?}"
        );
    }
    drop(viewer);
    server.stop();
}

/// Disconnects from block seats and poses, then compares saved positions with the Paper baseline.
#[test]
#[ignore = "requires PUMPGSIT_SERVER and PUMPGSIT_COMPONENT"]
fn disconnect_smoke() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    // Offsets observed on Paper 26.3 using the original GSit 3.8.0.
    let cases = [
        ("ClickDefault", true, 0, [1.5, 0.525, 0.5]),
        ("LayDefault", false, 0, [0.5, 0.025, 0.5]),
        ("ClickReturn", true, 0, [0.2, 0.525, 0.2]),
        ("LayReturn", false, 0, [0.2, 0.025, 0.2]),
        ("HighReturn", true, 1, [0.2, 1.0, 0.2]),
    ];
    let y: i32 = 300;
    let mut targets = Vec::new();
    let mut x = 0;
    let mut base_z = 0;
    let mut server = boot(root, "mount", true, false, None);
    for (index, (name, click, height, offset)) in cases.into_iter().enumerate() {
        let mut player = Player::connect(&mut server, name, "en_us");
        if index == 0 {
            let pos = player.position.unwrap();
            x = (pos[0].floor() as i32).div_euclid(16) * 16 + 7;
            base_z = (pos[2].floor() as i32).div_euclid(16) * 16 + 3;
            server.command(
                &format!(
                    "fill {} {} {} {} {} {} stone",
                    x - 1,
                    y - 1,
                    base_z - 1,
                    x + 2,
                    y - 1,
                    base_z + 9
                ),
                "filled",
            );
            for dz in [0, 4, 8] {
                server.command(
                    &format!(
                        "setblock {} {y} {} oak_slab[type=bottom]",
                        x + 1,
                        base_z + dz
                    ),
                    "Changed the block",
                );
            }
            server.command(
                &format!("setblock {x} {y} {} stone", base_z + 8),
                "Changed the block",
            );
        }
        if name == "ClickReturn" {
            let path = data_dir(root).join("config.toml");
            let settings = fs::read_to_string(&path).unwrap();
            assert!(settings.contains("get_up_return = false"));
            fs::write(
                path,
                settings.replace("get_up_return = false", "get_up_return = true"),
            )
            .unwrap();
            server.command("gsitreload", "reloaded");
        }
        let z = base_z + 2 * index as i32;
        let start = [
            f64::from(x) + 0.2,
            f64::from(y + height),
            f64::from(z) + 0.2,
        ];
        player.stand_at(
            &mut server,
            name,
            start,
            -77,
            if height == 0 { 40 } else { 58 },
        );
        if click {
            let block =
                (((x + 1) as u64 & 0x3ffffff) << 38) | ((z as u64 & 0x3ffffff) << 12) | y as u64;
            let mut hit = vec![0];
            hit.extend(block.to_be_bytes());
            hit.push(1);
            for _ in 0..3 {
                hit.extend(0.5f32.to_be_bytes());
            }
            hit.extend([0, 0, 1]);
            player.send(66, &hit);
        } else {
            player.command("lay");
        }
        player.until(|p| p.vehicle.is_some());
        player.sync();
        targets.push((
            name,
            [
                f64::from(x) + offset[0],
                f64::from(y) + offset[1],
                f64::from(z) + offset[2],
            ],
        ));
        drop(player);
        server.wait(&format!("{name} left the game"));
    }
    server.stop();
    let mut server = boot(root, "rejoin", true, false, None);
    for (name, target) in targets {
        let mut player = Player::connect(&mut server, name, "en_us");
        thread::sleep(Duration::from_millis(600));
        player.sync();
        assert!(player.vehicle.is_none(), "{name} remained mounted");
        for (actual, expected) in player.position.unwrap().into_iter().zip(target) {
            assert!(
                (actual - expected).abs() < 0.002,
                "{name}: {:?} != {target:?}",
                player.position
            );
        }
    }
    server.stop();
}

/// A flying stack base keeps its passenger when descending, then ejects them on a grounded sneak.
#[test]
#[ignore = "requires PUMPGSIT_SERVER and PUMPGSIT_COMPONENT"]
fn flying_stack() {
    let temp = tempdir().unwrap();
    let mut server = boot(temp.path(), "flying-stack", true, false, None);
    let mut base = Player::connect(&mut server, "SeatBottom", "en_us");
    let mut rider = Player::connect(&mut server, "SeatRider", "en_us");
    let spawn = base.position.unwrap();
    let x = (spawn[0].floor() as i32).div_euclid(16) * 16 + 7;
    let z = (spawn[2].floor() as i32).div_euclid(16) * 16 + 3;
    server.command(
        &format!("fill {} 299 {} {} 299 {} stone", x - 1, z - 1, x + 2, z + 2),
        "filled",
    );
    for (name, player, dx) in [("SeatBottom", &mut base, 0), ("SeatRider", &mut rider, 1)] {
        let location = [f64::from(x + dx) + 0.5, 300., f64::from(z) + 0.5];
        player.stand_at(&mut server, name, location, 0, 0);
    }

    server.command("gamemode creative SeatBottom", "Creative");
    base.send(40, &[2]); // player_abilities: flying
    base.sync();
    let mut click = varint(base.entity.unwrap());
    click.extend([0, 0, 0]); // main hand, zero relative hit vector, not sneaking
    rider.send(26, &click);
    rider.until(|p| p.vehicle == base.entity);
    base.send(43, &[32]); // player_input: sneak
    base.sync();
    // Check the server's current passenger relation even if its next entity packet is delayed.
    server.command(
        "execute as SeatRider on vehicle run tag @s add still_riding",
        "still_riding",
    );
    rider.sync();
    assert_eq!(rider.vehicle, base.entity);

    base.send(43, &[0]);
    base.send(40, &[0]); // stop flying
    base.sync();
    base.send(43, &[32]);
    base.sync();
    rider.until(|p| p.vehicle.is_none());
    drop(rider);
    drop(base);
    server.wait("SeatRider left the game");
    server.wait("SeatBottom left the game");
    server.stop();
}

/// A replacement solid block still supports the seat. Air follows the configured get-up rule.
#[test]
#[ignore = "requires PUMPGSIT_SERVER and PUMPGSIT_COMPONENT"]
fn support_replacement() {
    let temp = tempdir().unwrap();
    let mut server = boot(temp.path(), "support", true, false, None);
    for (get_up_break, name) in [(true, "SupportBreak"), (false, "SupportKeep")] {
        if !get_up_break {
            let path = data_dir(temp.path()).join("config.toml");
            let config = fs::read_to_string(&path).unwrap();
            assert!(config.contains("get_up_break = true"));
            fs::write(
                &path,
                config.replace("get_up_break = true", "get_up_break = false"),
            )
            .unwrap();
            server.command("gsitreload", "reloaded");
        }
        let mut player = Player::connect(&mut server, name, "en_us");
        let spawn = player.position.unwrap();
        let x = spawn[0].floor() as i32;
        let z = spawn[2].floor() as i32;
        let block = format!("setblock {x} 299 {z}");
        server.command(&format!("{block} stone"), "Changed the block");
        player.stand_at(
            &mut server,
            name,
            [f64::from(x) + 0.5, 300., f64::from(z) + 0.5],
            0,
            0,
        );
        player.command("sit");
        player.until(|p| p.vehicle.is_some());
        let vehicle = player.vehicle;
        server.command(&format!("{block} andesite"), "Changed the block");
        thread::sleep(Duration::from_millis(250));
        player.sync();
        assert_eq!(
            player.vehicle, vehicle,
            "The solid replacement ejected the rider"
        );
        server.command(&format!("{block} air"), "Changed the block");
        if get_up_break {
            player.until(|p| p.vehicle.is_none());
        } else {
            thread::sleep(Duration::from_millis(250));
            player.sync();
            assert_eq!(
                player.vehicle, vehicle,
                "Disabling get_up_break must retain the seat"
            );
        }
        drop(player);
        server.wait(&format!("{name} left the game"));
    }
    server.stop();
}
