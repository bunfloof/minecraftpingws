mod ping;

use futures_util::{SinkExt, StreamExt};
use ping::{BedrockPinger, JavaPinger};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{RwLock, broadcast},
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const IDLE_TIMEOUT: Duration = Duration::from_secs(180);
const PING_TIMEOUT: Duration = Duration::from_secs(5);
const SUBSCRIBE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_SUBSCRIBE_ERRORS: u32 = 10;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum Request {
    Ping {
        address: String,
        port: Option<u16>,
        #[serde(default)]
        bedrock: bool,
        #[serde(rename = "srvPrefix")]
        srv_prefix: Option<String>,
        #[serde(default)]
        id: Option<String>,
    },
    Subscribe {
        address: String,
        port: Option<u16>,
        #[serde(default)]
        bedrock: bool,
        #[serde(rename = "srvPrefix")]
        srv_prefix: Option<String>,
        #[serde(default)]
        id: Option<String>,
    },
    Unsubscribe {
        address: String,
        port: Option<u16>,
        #[serde(default)]
        bedrock: bool,
        #[serde(default)]
        id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
struct LatencyUpdate {
    server: String,
    latency: Option<u64>,
    players: Option<Players>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(flatten)]
    data: ResponseData,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ResponseData {
    Ping {
        #[serde(flatten)]
        result: serde_json::Value,
    },
    Latency {
        server: String,
        latency: Option<u64>,
        players: Option<Players>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Subscribed {
        subscribed: String,
    },
    Unsubscribed {
        unsubscribed: String,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize)]
struct Players {
    online: i32,
    max: i32,
}

struct SharedPingTask {
    // shared state for a single server's ping task
    sender: broadcast::Sender<LatencyUpdate>,
    subscriber_count: usize,
}

struct PingRegistry {
    // global registry of active ping tasks
    tasks: HashMap<String, SharedPingTask>,
}

impl PingRegistry {
    fn new() -> Self {
        Self { tasks: HashMap::new() }
    }
}

type SharedRegistry = Arc<RwLock<PingRegistry>>;

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("0.0.0.0:45296").await.expect("Failed to bind");
    println!("WebSocket server running on ws://0.0.0.0:45296");

    let registry: SharedRegistry = Arc::new(RwLock::new(PingRegistry::new()));

    loop {
        if let Ok((stream, addr)) = listener.accept().await {
            let registry = registry.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, registry).await {
                    eprintln!("Connection error from {}: {}", addr, e);
                }
            });
        }
    }
}

struct ClientSubscription {
    // per-client subscription info
    id: Option<String>,
    receiver: broadcast::Receiver<LatencyUpdate>,
}

async fn handle_connection(
    stream: TcpStream, registry: SharedRegistry,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_stream = accept_async(stream).await?;
    let (mut write, mut read) = ws_stream.split();

    let mut subscriptions: HashMap<String, ClientSubscription> = HashMap::new(); // track this client's subscriptions: server_key -> subscription info

    loop {
        let subscription_update = async {
            // create a future that receives from any active subscription
            if subscriptions.is_empty() {
                std::future::pending::<Option<(String, LatencyUpdate)>>().await // no subscriptions, wait forever (will be cancelled by other branches)
            } else {
                let mut result = None; // poll all subscriptions
                for (key, sub) in subscriptions.iter_mut() {
                    match sub.receiver.try_recv() {
                        Ok(update) => {
                            result = Some((key.clone(), update));
                            break;
                        }
                        Err(broadcast::error::TryRecvError::Empty) => continue,
                        Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                        Err(broadcast::error::TryRecvError::Closed) => {
                            result = None;
                            break;
                        }
                    }
                }
                if result.is_some() {
                    result
                } else {
                    tokio::time::sleep(Duration::from_millis(50)).await; // nothing ready, yield and try again
                    None
                }
            }
        };

        tokio::select! {
            biased;

            update = subscription_update => { // handle subscription updates first (higher priority)
                if let Some((key, update)) = update {
                    let id = subscriptions.get(&key).and_then(|s| s.id.clone());
                    let response = Response {
                        id,
                        data: ResponseData::Latency {
                            server: update.server,
                            latency: update.latency,
                            players: update.players,
                            error: update.error,
                        },
                    };
                    write.send(Message::Text(serde_json::to_string(&response)?.into())).await?;
                }
            }

            result = timeout(IDLE_TIMEOUT, read.next()) => { // handle incoming websocket messages
                match result {
                    Ok(Some(Ok(msg))) => {
                        match msg {
                            Message::Text(text) => {
                                if text.trim().eq_ignore_ascii_case("ping") {
                                    write.send(Message::Text("pong".into())).await?;
                                } else {
                                    let response = process_request(
                                        &text,
                                        &mut subscriptions,
                                        &registry,
                                    ).await;
                                    write.send(Message::Text(response.into())).await?;
                                }
                            }
                            Message::Ping(data) => {
                                write.send(Message::Pong(data)).await?;
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                    Ok(Some(Err(e))) => return Err(e.into()),
                    Ok(None) => break,
                    Err(_) => {
                        let _ = write.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        }
    }

    for (key, _) in subscriptions.drain() {
        // clean up: unsubscribe from all servers
        decrement_subscriber(&registry, &key).await;
    }

    Ok(())
}

async fn process_request(
    text: &str, subscriptions: &mut HashMap<String, ClientSubscription>, registry: &SharedRegistry,
) -> String {
    let req: Request = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            return serde_json::to_string(&Response {
                id: None,
                data: ResponseData::Error {
                    error: format!(
                        "Invalid JSON: {}. Expected: {{\"action\":\"ping|subscribe|unsubscribe\",\"address\":\"...\",\"port\":25565,\"bedrock\":false,\"id\":\"...(optional)\"}}",
                        e
                    ),
                },
            })
            .unwrap();
        }
    };

    match req {
        Request::Ping { address, port, bedrock, srv_prefix, id } => {
            let port = port.unwrap_or(if bedrock { 19132 } else { 25565 });
            let srv_prefix = srv_prefix.unwrap_or_else(|| "_minecraft._tcp".to_string());

            let result = do_ping(&address, port, bedrock, &srv_prefix).await;

            serde_json::to_string(&Response { id, data: ResponseData::Ping { result } }).unwrap()
        }

        Request::Subscribe { address, port, bedrock, srv_prefix, id } => {
            let port = port.unwrap_or(if bedrock { 19132 } else { 25565 });
            let srv_prefix = srv_prefix.unwrap_or_else(|| "_minecraft._tcp".to_string());
            let key = format!("{}:{}:{}", address, port, bedrock);

            if subscriptions.contains_key(&key) {
                // already subscribed?
                return serde_json::to_string(&Response {
                    id,
                    data: ResponseData::Error { error: format!("Already subscribed to {}", key) },
                })
                .unwrap();
            }

            let receiver = // get or create shared ping task
                get_or_create_ping_task(registry, &key, address.clone(), port, bedrock, srv_prefix)
                    .await;

            subscriptions.insert(key, ClientSubscription { id: id.clone(), receiver });

            serde_json::to_string(&Response {
                id,
                data: ResponseData::Subscribed { subscribed: format!("{}:{}", address, port) },
            })
            .unwrap()
        }

        Request::Unsubscribe { address, port, bedrock, id } => {
            let port = port.unwrap_or(if bedrock { 19132 } else { 25565 });
            let key = format!("{}:{}:{}", address, port, bedrock);

            if subscriptions.remove(&key).is_some() {
                decrement_subscriber(registry, &key).await;

                serde_json::to_string(&Response {
                    id,
                    data: ResponseData::Unsubscribed {
                        unsubscribed: format!("{}:{}", address, port),
                    },
                })
                .unwrap()
            } else {
                serde_json::to_string(&Response {
                    id,
                    data: ResponseData::Error { error: format!("Not subscribed to {}", key) },
                })
                .unwrap()
            }
        }
    }
}

async fn get_or_create_ping_task(
    registry: &SharedRegistry, key: &str, address: String, port: u16, bedrock: bool,
    srv_prefix: String,
) -> broadcast::Receiver<LatencyUpdate> {
    let mut reg = registry.write().await;

    if let Some(task) = reg.tasks.get_mut(key) {
        task.subscriber_count += 1; // task already exists, increment subscriber count and return receiver
        println!("[{}:{}] New subscriber joined (total: {})", address, port, task.subscriber_count);
        return task.sender.subscribe();
    }

    let (sender, receiver) = broadcast::channel::<LatencyUpdate>(16); // create new task

    reg.tasks
        .insert(key.to_string(), SharedPingTask { sender: sender.clone(), subscriber_count: 1 });

    println!("[{}:{}] Created new ping task", address, port);

    let registry_clone = registry.clone(); // spawn the ping task
    let key_clone = key.to_string();
    let server_str = format!("{}:{}", address, port);

    tokio::spawn(async move {
        run_shared_ping_task(
            registry_clone,
            key_clone,
            server_str,
            address,
            port,
            bedrock,
            srv_prefix,
            sender,
        )
        .await;
    });

    receiver
}

async fn run_shared_ping_task(
    registry: SharedRegistry, key: String, server_str: String, address: String, port: u16,
    bedrock: bool, srv_prefix: String, sender: broadcast::Sender<LatencyUpdate>,
) {
    let mut interval = tokio::time::interval(SUBSCRIBE_INTERVAL);
    let mut consecutive_errors: u32 = 0;

    loop {
        interval.tick().await;

        {
            // check if we still have subscribers, if not, stop the ping task
            let reg = registry.read().await;
            match reg.tasks.get(&key) {
                Some(task) if task.subscriber_count == 0 => {
                    println!("[{}] No subscribers, stopping ping task", server_str);
                    drop(reg);
                    registry.write().await.tasks.remove(&key);
                    return;
                }
                None => {
                    println!("[{}] Task removed, stopping", server_str);
                    return;
                }
                _ => {}
            }
        }

        let result = if bedrock {
            // perform the ping
            let pinger = BedrockPinger::new(&address, port, PING_TIMEOUT);
            pinger.ping().await
        } else {
            let pinger = JavaPinger::new(&address, port, PING_TIMEOUT, &srv_prefix);
            pinger.ping().await
        };

        let update = match result {
            Ok(data) => {
                consecutive_errors = 0;
                let latency = data.get("latency").and_then(|v| v.as_u64());
                let players = data.get("players").map(|p| Players {
                    online: p.get("online").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                    max: p.get("max").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                });

                LatencyUpdate { server: server_str.clone(), latency, players, error: None }
            }
            Err(e) => {
                consecutive_errors += 1;
                LatencyUpdate {
                    server: server_str.clone(),
                    latency: None,
                    players: None,
                    error: Some(e.to_string()),
                }
            }
        };

        let _ = sender.send(update); // broadcast to all subscribers (ignore send errors - means no receivers)

        if consecutive_errors > MAX_SUBSCRIBE_ERRORS {
            // stop if too many consecutive errors
            eprintln!("[{}] Too many consecutive errors, stopping ping task", server_str);
            registry.write().await.tasks.remove(&key);
            return;
        }
    }
}

async fn decrement_subscriber(registry: &SharedRegistry, key: &str) {
    let mut reg = registry.write().await;
    if let Some(task) = reg.tasks.get_mut(key) {
        task.subscriber_count = task.subscriber_count.saturating_sub(1);
        println!("[{}] Subscriber left (remaining: {})", key, task.subscriber_count);
    }
}

async fn do_ping(address: &str, port: u16, bedrock: bool, srv_prefix: &str) -> serde_json::Value {
    let result = if bedrock {
        let pinger = BedrockPinger::new(address, port, PING_TIMEOUT);
        pinger.ping().await
    } else {
        let pinger = JavaPinger::new(address, port, PING_TIMEOUT, srv_prefix);
        pinger.ping().await
    };

    match result {
        Ok(data) => data,
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}
