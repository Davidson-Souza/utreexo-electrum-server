use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_std::{channel::Sender, sync::RwLock};

use crate::blockchain::p2p_blockchain::{peer::Peer, NodeNotification};

pub enum PingState {
    Pending { since: Instant },
    Idle { since: Instant },
}
pub struct PingManager {
    peers: Vec<(u64, PingState)>,
    ping_interval: Duration,
    ping_timeout: Duration,
    node_tx: Sender<NodeNotification>,
}

impl PingManager {
    fn new_peer(&mut self, peer: u64) {
        self.peers.push((
            peer,
            PingState::Idle {
                since: Instant::now(),
            },
        ))
    }
    /// Runs periodically to check pings
    async fn handle_ping(&mut self) {
        for (peer, state) in self.peers.iter() {
            match state {
                PingState::Idle { since } => {
                    if *since + self.ping_interval > Instant::now() {
                        self.node_tx.send(NodeNotification::TryPing(*peer)).await;
                    }
                }
                PingState::Pending { since } => {
                    if *since + self.ping_timeout > Instant::now() {
                        // timeout
                        self.node_tx
                            .send(NodeNotification::PingTimeout(*peer))
                            .await;
                    }
                }
            }
        }
    }
}
