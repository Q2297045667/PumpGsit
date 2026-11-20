"""Check GSit dismount positions across a real Java 26.3 disconnect and Pumpkin restart."""

import argparse
import asyncio
from contextlib import asynccontextmanager
from io import BytesIO
from pathlib import Path
import struct
import tempfile
import tomllib
import uuid
import zlib

from server_smoke import boot


def varint(value):
    """Encode a nonnegative Minecraft VarInt."""
    data = bytearray()
    while value >= 128:
        data.append(128 | (value & 127))
        value >>= 7
    data.append(value)
    return bytes(data)


def integer(stream):
    """Read a VarInt from a packet body."""
    value = 0
    for shift in range(0, 35, 7):
        byte = stream.read(1)[0]
        value |= (byte & 127) << shift
        if byte < 128:
            return value
    raise ValueError("Invalid VarInt")


def string(value):
    """Encode a length-prefixed protocol string."""
    data = value.encode()
    return varint(len(data)) + data


class Player:
    """Protocol 777 client for observing authoritative positions and passenger lists."""

    def __init__(self, reader, writer):
        self.reader, self.writer = reader, writer
        self.threshold = self.entity = self.position = self.vehicle = None
        self.step = "login"
        self.pong = b""
        self.feedback = b""

    def send(self, packet, payload=b""):
        """Write one packet, honoring the server's negotiated compression threshold."""
        data = varint(packet) + payload
        if self.threshold is not None:
            data = varint(len(data)) + zlib.compress(data) if len(data) >= self.threshold else b"\0" + data
        self.writer.write(varint(len(data)) + data)

    async def receive(self):
        """Read a length-delimited packet, answering the setup, movement and liveness requests."""
        length = 0
        for shift in range(0, 35, 7):
            byte = (await self.reader.readexactly(1))[0]
            length |= (byte & 127) << shift
            if byte < 128:
                break
        else:
            raise ValueError("Invalid frame length")
        body = BytesIO(await self.reader.readexactly(length))
        if self.threshold is not None and integer(body):
            body = BytesIO(zlib.decompress(body.read()))
        packet = integer(body)
        if self.step == "login":
            if packet == 3:  # set_compression
                self.threshold = integer(body)
            elif packet == 2:  # login_finished
                self.send(3)
                self.step = "config"
                self.send(0, string("en_us") + bytes([3, 0, 1, 127, 1, 0, 1, 0]))
            elif packet == 0:
                raise RuntimeError(body.read())
        elif self.step == "config":
            if packet == 15:  # select_known_packs
                self.send(7, b"\0")
            elif packet in (4, 5):  # keep_alive, ping
                self.send(packet, body.read())
            elif packet == 3:  # finish_configuration
                self.send(3)
                self.step = "play"
            elif packet == 2:
                raise RuntimeError(body.read())
        elif packet == 50:  # login
            self.entity, = struct.unpack(">i", body.read(4))
        elif packet == 73:  # player_position
            teleport = integer(body)
            xyz = body.read(24)
            self.position = struct.unpack(">ddd", xyz)
            body.read(24)  # velocity
            self.send(0, varint(teleport) + xyz + body.read(8))  # acknowledge XYZ and rotation
            self.send(44)  # player_loaded
        elif packet == 124:  # system_chat (include the last message in failure diagnostics)
            self.feedback = body.read()
        elif packet == 109:  # set_passengers
            vehicle = integer(body)
            riders = [integer(body) for _ in range(integer(body))]
            if self.entity in riders:
                self.vehicle = vehicle
            elif self.vehicle == vehicle:
                self.vehicle = None
        elif packet == 63:  # response to our synchronization ping
            self.pong = body.read()
        elif packet in (45, 62):  # keep_alive, ping
            self.send(28 if packet == 45 else 45, body.read())
        elif packet == 11:  # chunk_batch_finished
            self.send(11, struct.pack(">f", 5))
        elif packet == 32:
            raise RuntimeError(body.read())

    async def until(self, predicate):
        """Process packets until the specified state is observed or the test times out."""
        try:
            async with asyncio.timeout(30):
                while not predicate():
                    await self.receive()
        except TimeoutError as error:
            raise AssertionError((self.position, self.vehicle, self.feedback)) from error

    async def sync(self):
        """Wait for a server reply to a packet sent after our last teleport acknowledgment."""
        nonce = uuid.uuid4().bytes[:8]
        self.send(38, nonce)
        await self.until(lambda: self.pong == nonce)


@asynccontextmanager
async def connect(port, name):
    """Log in offline and close the transport to initiate a real player disconnect."""
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    player = Player(reader, writer)
    try:
        player.send(0, varint(777) + string("127.0.0.1") + struct.pack(">HB", port, 2))
        player.send(0, string(name) + uuid.uuid5(uuid.NAMESPACE_DNS, name).bytes)
        await player.until(lambda: player.position is not None and player.entity is not None)
        await player.sync()
        yield player
    finally:
        writer.close()
        await writer.wait_closed()


async def logged(log, text):
    """Wait until Pumpkin has completed the named join or leave callback."""
    async with asyncio.timeout(20):
        while text not in log.read_text(errors="replace"):
            await asyncio.sleep(.05)


def endpoint(root):
    """Read the random loopback port selected by server_smoke.boot."""
    address = tomllib.loads((root / "pumpkin.toml").read_text())["networking"]["java"]["address"]
    return int(address.rsplit(":", 1)[1])


async def exercise(server, component, root):
    """Leave each distinct seat, restart once, and compare Java login coordinates to Paper 26.3."""
    # Each expected offset was observed independently on Paper with GSit 3.8.0.
    cases = [
        ("ClickDefault", "click", 0, (1.5, .525, .5)),
        ("LayDefault", "lay", 0, (.5, .025, .5)),
        ("ClickReturn", "click", 0, (.2, .525, .2)),
        ("LayReturn", "lay", 0, (.2, .025, .2)),
        ("HighReturn", "click", 1, (.2, 1, .2)),
    ]
    expected = {}
    y = 300
    with boot(server, component, root, "mount") as console:
        log = root / "smoke-mount.log"
        for index, (name, action, height, offset) in enumerate(cases):
            async with connect(endpoint(root), name) as player:
                await logged(log, f"{name} joined the game")
                if index == 0:
                    # Build inside the fresh world's already loaded spawn chunk.
                    x = int(player.position[0] // 16) * 16 + 7
                    base_z = int(player.position[2] // 16) * 16 + 3
                    console(f"fill {x-1} {y-1} {base_z-1} {x+2} {y-1} {base_z+9} stone", "filled")
                    for dz in (0, 4, 8):
                        console(f"setblock {x+1} {y} {base_z+dz} oak_slab[type=bottom]", "Changed the block")
                    console(f"setblock {x} {y} {base_z+8} stone", "Changed the block")
                if name == "ClickReturn":
                    config = root / "plugins/data/PumpGsit/config.toml"
                    initial = config.read_text()
                    assert "get_up_return = false" in initial
                    config.write_text(initial.replace("get_up_return = false", "get_up_return = true"))
                    console("gsitreload", "reloaded")

                z = base_z + 2 * index
                start = (x + .2, y + height, z + .2)
                console(f"tp {name} {start[0]} {start[1]} {start[2]} -77 {58 if height else 40}", "Teleported")
                await player.until(lambda: player.position == start)
                player.send(30, struct.pack(">dddB", *start, 1))  # grounded on the platform
                player.send(13)  # client_tick_end
                await player.sync()
                if action == "lay":
                    player.send(7, string("lay"))
                else:
                    block = (((x + 1) & 0x3ffffff) << 38) | ((z & 0x3ffffff) << 12) | y
                    player.send(66, b"\0" + struct.pack(">QBfffBB", block, 1, .5, .5, .5, 0, 0) + varint(1))
                await player.until(lambda: player.vehicle is not None)
                await player.sync()
                expected[name] = (x + offset[0], y + offset[1], z + offset[2])
            await logged(log, f"{name} left the game")

    with boot(server, component, root, "rejoin"):
        for name, target in expected.items():
            async with connect(endpoint(root), name) as player:
                await logged(root / "smoke-rejoin.log", f"{name} joined the game")
                await asyncio.sleep(.6)  # allow entity tracking to send passenger updates
                await player.sync()
                assert player.vehicle is None, (name, player.vehicle)
                assert all(abs(a - b) < .002 for a, b in zip(player.position, target)), (
                    name, player.position, target)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda value: Path(value).resolve())
    parser.add_argument("component", type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="PumpGsit-disconnect-") as directory:
        asyncio.run(exercise(args.server, args.component, Path(directory)))
    print("Passed slab and /lay disconnects, both get_up_return settings, high origin and restart.")
