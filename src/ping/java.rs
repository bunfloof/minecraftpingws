use super::PingError;
use hickory_resolver::{Resolver, config::ResolverConfig, name_server::TokioConnectionProvider};
use serde_json::{Value, json};
use std::{net::IpAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Instant, timeout},
};

pub struct JavaPinger {
    address: String,
    port: u16,
    timeout: Duration,
    srv_prefix: String,
}

impl JavaPinger {
    pub fn new(address: &str, port: u16, timeout: Duration, srv_prefix: &str) -> Self {
        Self { address: address.to_string(), port, timeout, srv_prefix: srv_prefix.to_string() }
    }

    pub async fn ping(&self) -> Result<Value, PingError> {
        let (resolved_addr, resolved_port, srv_record) = self.resolve_address().await?;

        let start_time = Instant::now();

        let connect_result = timeout(
            self.timeout,
            TcpStream::connect(format!("{}:{}", resolved_addr, resolved_port)),
        )
        .await;

        let mut stream = match connect_result {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                return Err(PingError::Timeout(format!("{} seconds", self.timeout.as_secs_f32())));
            }
        };

        let latency = start_time.elapsed().as_millis() as u64;

        self.send_handshake(&mut stream).await?;
        self.send_status_request(&mut stream).await?;

        let response = timeout(self.timeout, self.read_response(&mut stream)).await;

        let mut json_response = match response {
            Ok(Ok(json)) => json,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(PingError::Timeout(format!("{} seconds", self.timeout.as_secs_f32())));
            }
        };

        json_response["srvRecord"] = if srv_record.is_some() {
            json!(format!("{}.{}", self.srv_prefix, self.address))
        } else {
            Value::Null
        };
        json_response["host"] = json!(resolved_addr.to_string());
        json_response["port"] = json!(resolved_port);
        json_response["latency"] = json!(latency);

        Ok(json_response)
    }

    async fn resolve_address(&self) -> Result<(IpAddr, u16, Option<String>), PingError> {
        if let Ok(ip) = self.address.parse::<IpAddr>() {
            return Ok((ip, self.port, None));
        }

        let resolver = Resolver::builder_with_config(
            ResolverConfig::default(),
            TokioConnectionProvider::default(),
        )
        .build();

        let srv_name = format!("{}.{}.", self.srv_prefix, self.address);
        if let Ok(srv_response) = resolver.srv_lookup(&srv_name).await {
            if let Some(record) = srv_response.iter().next() {
                let target = record.target().to_string();
                let port = record.port();

                if let Ok(ip_response) = resolver.lookup_ip(target.trim_end_matches('.')).await {
                    if let Some(ip) = ip_response.iter().next() {
                        return Ok((ip, port, Some(srv_name)));
                    }
                }
            }
        }

        match resolver.lookup_ip(&self.address).await {
            Ok(response) => {
                if let Some(ip) = response.iter().next() {
                    Ok((ip, self.port, None))
                } else {
                    Err(PingError::DnsError("No IP addresses found".to_string()))
                }
            }
            Err(e) => Err(PingError::DnsError(e.to_string())),
        }
    }

    async fn send_handshake(&self, stream: &mut TcpStream) -> Result<(), PingError> {
        let mut packet = Vec::new();

        packet.push(0x00);
        write_varint(&mut packet, -1i32 as u32);
        write_string(&mut packet, &self.address);
        packet.push((self.port >> 8) as u8);
        packet.push((self.port & 0xFF) as u8);
        write_varint(&mut packet, 1);

        let mut final_packet = Vec::new();
        write_varint(&mut final_packet, packet.len() as u32);
        final_packet.extend(packet);

        stream.write_all(&final_packet).await.map_err(|e| PingError::IoError(e.to_string()))?;

        Ok(())
    }

    async fn send_status_request(&self, stream: &mut TcpStream) -> Result<(), PingError> {
        stream.write_all(&[0x01, 0x00]).await.map_err(|e| PingError::IoError(e.to_string()))?;

        Ok(())
    }

    async fn read_response(&self, stream: &mut TcpStream) -> Result<Value, PingError> {
        let mut buffer = vec![0u8; 65535];
        let mut data = Vec::new();

        loop {
            let n =
                stream.read(&mut buffer).await.map_err(|e| PingError::IoError(e.to_string()))?;

            if n == 0 {
                break;
            }

            data.extend_from_slice(&buffer[..n]);

            if let Some(json) = extract_json(&data) {
                return Ok(json);
            }
        }

        Err(PingError::ParseError("Could not parse server response".to_string()))
    }
}

fn extract_json(data: &[u8]) -> Option<Value> {
    let raw = String::from_utf8_lossy(data);
    let mut depth = 0i32;
    let mut json_start = None;
    let mut json_end = None;

    for (i, c) in raw.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    json_start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    json_end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    if let (Some(start), Some(end)) = (json_start, json_end) {
        serde_json::from_str(&raw[start..end]).ok()
    } else {
        None
    }
}

fn write_varint(buf: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    write_varint(buf, bytes.len() as u32);
    buf.extend_from_slice(bytes);
}
