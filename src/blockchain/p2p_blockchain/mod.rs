#![allow(unused)]

use self::peer::Peer;
use super::{chain_state::ChainState, chainstore::KvChainStore};
use async_std::{
    channel::{self, Receiver, Sender},
    sync::RwLock,
    task::{sleep, spawn},
};
use bitcoin::{BlockHash, Network};
use btcd_rpc::client::{BTCDClient, BTCDConfigs};
use std::{
    collections::HashMap,
    net::{SocketAddr, SocketAddrV4},
    process::exit,
    sync::Arc,
    time::Duration,
};

mod peer;
mod protocol;
mod stream_reader;

/// Messages passed from different modules to the main node to process. They should minimal
/// and only if it requires global states, everything else should be handled by the module
/// itself.
pub enum NodeNotification {
    /// This means our peer timed out, and we should disconnect it
    PingTimeout(u64),
    /// It's been a while since we last ping this peer, so send a ping to make sure they
    /// are still there
    TryPing(u64),
    /// A new block just arrived, we should ask for it and update our chain
    NewBlock(BlockHash),
    /// Also a new block, but our connection is a compact blocks one, so we should handle
    /// a compact blocs communication, not a explicit block request
    NewCompactBlock(BlockHash),
}
pub struct Mempool;
impl Mempool {
    pub fn accept_to_mempool(&self) {}
}
#[allow(unused)]
pub struct UtreexoNode {
    peer_id_count: u32,
    network: Network,
    peers: HashMap<u32, Arc<RwLock<Peer>>>,
    chain: Arc<ChainState<KvChainStore>>,
    mempool: Arc<RwLock<Mempool>>,
    node_rx: Receiver<NodeNotification>,
    node_tx: Sender<NodeNotification>,
    rpc: Arc<BTCDClient>,
}

impl UtreexoNode {
    pub fn new(
        chain: Arc<ChainState<KvChainStore>>,
        mempool: Arc<RwLock<Mempool>>,
        network: Network,
        rpc: Arc<BTCDClient>,
    ) -> Self {
        let (node_tx, node_rx) = channel::bounded(1024);
        UtreexoNode {
            chain,
            mempool,
            network,
            node_rx,
            node_tx,
            peer_id_count: 0,
            peers: HashMap::new(),
            rpc,
        }
    }
    pub async fn run(mut self) {
        let seed_node = "178.128.221.177:38333";
        self.create_connection(seed_node).await;
        loop {
            sleep(Duration::from_secs(10)).await;
        }
    }
    async fn create_connection(&mut self, peer: &str) {
        let peer = Peer::create_outbound_connection(
            self.chain.clone(),
            self.peer_id_count,
            peer,
            self.mempool.clone(),
            self.network,
            self.node_tx.clone(),
            self.rpc.clone(),
        )
        .await;

        if let Ok(peer) = peer {
            spawn(peer.read_loop());
        } else {
            exit(0);
        }
        self.peer_id_count += 1;
    }
}
