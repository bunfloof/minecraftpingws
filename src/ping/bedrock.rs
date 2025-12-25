use super::PingError;
use hickory_resolver::{Resolver, config::ResolverConfig, name_server::TokioConnectionProvider};
use serde_json::{Value, json};
use std::{net::IpAddr, time::Duration};
use tokio::{
    net::UdpSocket,
    time::{Instant, timeout},
};

pub struct BedrockPinger {
    address: String,
    port: u16,
    timeout: Duration,
}

impl BedrockPinger {
    pub fn new(address: &str, port: u16, timeout: Duration) -> Self {
        Self { address: address.to_string(), port, timeout }
    }

    pub async fn ping(&self) -> Result<Value, PingError> {
        let resolved_addr = self.resolve_address().await?;

        let socket =
            UdpSocket::bind("0.0.0.0:0").await.map_err(|e| PingError::IoError(e.to_string()))?;

        let target = format!("{}:{}", resolved_addr, self.port);
        socket.connect(&target).await.map_err(|e| PingError::IoError(e.to_string()))?;

        let packet = build_ping_packet();

        let start_time = Instant::now();

        socket.send(&packet).await.map_err(|e| PingError::IoError(e.to_string()))?;

        let mut buffer = vec![0u8; 4096];
        let recv_result = timeout(self.timeout, socket.recv(&mut buffer)).await;

        let n = match recv_result {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(PingError::IoError(e.to_string())),
            Err(_) => {
                return Err(PingError::Timeout(format!("{} seconds", self.timeout.as_secs_f32())));
            }
        };

        let latency = start_time.elapsed().as_millis() as u64;

        parse_response(&buffer[..n], latency, resolved_addr, self.port)
    }

    async fn resolve_address(&self) -> Result<IpAddr, PingError> {
        if let Ok(ip) = self.address.parse::<IpAddr>() {
            return Ok(ip);
        }

        let resolver = Resolver::builder_with_config(
            ResolverConfig::default(),
            TokioConnectionProvider::default(),
        )
        .build();

        match resolver.lookup_ip(&self.address).await {
            Ok(response) => {
                if let Some(ip) = response.iter().next() {
                    Ok(ip)
                } else {
                    Err(PingError::DnsError("No IP addresses found".to_string()))
                }
            }
            Err(e) => Err(PingError::DnsError(e.to_string())),
        }
    }
}

fn build_ping_packet() -> Vec<u8> {
    let mut packet = Vec::with_capacity(33);

    packet.push(0x01); // packet ID: 0x01 (Unconnected Ping)
    packet.extend_from_slice(&[0u8; 8]); // time since start (8 bytes)
    packet.extend_from_slice(&[
        0x00, 0xFF, 0xFF, 0x00, 0xFE, 0xFE, 0xFE, 0xFE, 0xFD, 0xFD, 0xFD, 0xFD, 0x12, 0x34, 0x56, 0x78,
    ]);
    packet.extend_from_slice(&[0u8; 8]); // client GUID (8 bytes)
    packet
}

fn parse_response(
    data: &[u8], latency: u64, resolved_addr: IpAddr, port: u16,
) -> Result<Value, PingError> {
    if data.is_empty() || data[0] != 0x1C {
        return Err(PingError::ParseError("Invalid packet received".to_string()));
    }

    if data.len() < 35 {
        return Err(PingError::ParseError("Packet too short".to_string()));
    }

    let server_guid = i64::from_be_bytes(data[1..9].try_into().unwrap());
    let response_string = String::from_utf8_lossy(&data[35..]).to_string();

    let parts: Vec<&str> = response_string.split(';').collect();

    if parts.len() < 6 {
        return Err(PingError::ParseError("Invalid response format".to_string()));
    }

    let edition = parts.first().unwrap_or(&"").to_string();
    let motd_line1 = parts.get(1).unwrap_or(&"").to_string();
    let protocol_version = parts.get(2).unwrap_or(&"0").parse::<i32>().unwrap_or(0);
    let version = parts.get(3).unwrap_or(&"").to_string();
    let online_players = parts.get(4).unwrap_or(&"0").parse::<i32>().unwrap_or(0);
    let max_players = parts.get(5).unwrap_or(&"0").parse::<i32>().unwrap_or(0);
    let server_id = parts.get(6).unwrap_or(&"").to_string();
    let motd_line2 = parts.get(7).unwrap_or(&"").to_string();
    let game_mode = parts.get(8).unwrap_or(&"").to_string();
    let game_mode_id = parts.get(9).unwrap_or(&"0").parse::<i32>().unwrap_or(0);
    let port_ipv4 = parts.get(10).and_then(|s| s.parse::<u16>().ok());
    let port_ipv6 = parts.get(11).and_then(|s| s.parse::<u16>().ok());

    let full_motd = if motd_line2.is_empty() {
        motd_line1.clone()
    } else {
        format!("{}\n{}", motd_line1, motd_line2)
    };

    Ok(json!({
        "edition": edition,
        "motd": {
            "raw": full_motd,
            "line1": motd_line1,
            "line2": motd_line2
        },
        "version": {
            "name": version,
            "protocol": protocol_version
        },
        "players": {
            "online": online_players,
            "max": max_players
        },
        "serverGUID": server_guid.to_string(),
        "serverID": server_id,
        "gameMode": game_mode,
        "gameModeID": game_mode_id,
        "portIPv4": port_ipv4,
        "portIPv6": port_ipv6,
        "latency": latency,
        "host": resolved_addr.to_string(),
        "port": port
    }))
}
