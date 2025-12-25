# minecraftpingws

```bash
cargo run
cargo build --release --target x86_64-unknown-linux-musl
```

Send `ping` (text) every 2 minutes to keep the connection alive, receive `pong` because connection closes after 3 minutes of inactivity. Subscriptions stop after 10 consecutive ping failures. When subscriber count is 0, ping task stops and cleans up. Only one ping task is created for each server. `PingRegistry` tracks all active ping tasks and their subscriber counts to share ping tasks across subscribers to the same server for efficiency. There's also an optional id parameter which echos back the id like "req-123", so we can correlate the response with the request for async messaging.

Ping request:

```json
{
  "action": "ping",
  "address": "mc.hypixel.net",
  "port": 25565,
  "bedrock": false,
  "id": "req-123"
}
```

Ping response:

```json
{
  "id": "req-123",
  "description": "                §aHypixel Network §c[1.8/1.21]\n       §2§lSB GREENHOUSE §8- §b§lBONUS COINS + XP",
  "favicon": "data:image/png;base64,iVBO...",
  "host": "172.65.197.160",
  "latency": 6,
  "players": {
    "max": 200000,
    "online": 43615,
    "sample": []
  },
  "port": 25565,
  "srvRecord": null,
  "version": {
    "name": "Requires MC 1.8 / 1.21",
    "protocol": 47
  }
}
```

Subscribe request:

```json
{
  "action": "subscribe",
  "address": "mc.hypixel.net",
  "port": 25565,
  "bedrock": false,
  "id": "sub-456"
}
```

Subscribe response (updates every 1 second):

```json
{
  "id": "sub-456",
  "server": "mc.hypixel.net:25565",
  "latency": 5,
  "players": {
    "online": 42770,
    "max": 200000
  }
}
```

Error subscribe response:

```json
{
  "id": "sub-456",
  "server": "mc.hypixel.net:25565",
  "latency": null,
  "players": null,
  "error": "Connection refused"
}
```

Unsubscribe request (not needed since pinging stops after no subscribers):

```json
{
  "action": "unsubscribe",
  "address": "mc.hypixel.net",
  "port": 25565,
  "bedrock": false,
  "id": "unsub-789"
}
```

Unsubscribe response:

```json
{
  "id": "unsub-789",
  "unsubscribed": "mc.hypixel.net:25565"
}
```
