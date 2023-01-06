#![allow(unused)]
use self::peer_utils::make_pong;

use super::{stream_reader::StreamReader, Mempool, NodeNotification, NodeRequest};
use crate::blockchain::{
    chain_state::ChainState, chainstore::KvChainStore, error::BlockchainError, udata::LeafData,
    BlockchainInterface, BlockchainProviderInterface,
};
use async_std::{
    channel::{Receiver, Sender},
    io::{Read, ReadExt, WriteExt},
    net::{TcpStream, ToSocketAddrs},
    stream::{Stream, StreamExt},
    sync::RwLock,
};
use bitcoin::{
    consensus::{deserialize, deserialize_partial, serialize, Decodable},
    hashes::{hex::FromHex, sha256},
    network::{
        constants::ServiceFlags,
        message::{NetworkMessage, RawNetworkMessage, MAX_MSG_SIZE},
        message_network::VersionMessage,
    },
    Network, OutPoint,
};
use btcd_rpc::{
    client::{BTCDClient, BtcdRpc},
    json_types::blockchain::GetUtreexoProofResult,
};
use clap::builder::TypedValueParser;
use rustreexo::accumulator::proof::Proof;
use std::{collections::HashMap, io::BufReader};
use std::{sync::Arc, time::Instant};
#[derive(PartialEq)]
enum State {
    None,
    VersionSent,
    RemoteVerack,
    VersionReceived,
    Connected,
}
pub struct Peer {
    stream: TcpStream,
    network: Network,
    chain: Arc<ChainState<KvChainStore>>,
    mempool: Arc<RwLock<Mempool>>,
    blocks_only: bool,
    services: ServiceFlags,
    user_agent: String,
    current_best_block: i32,
    last_ping: Instant,
    id: u32,
    node_tx: Sender<NodeNotification>,
    state: State,
    send_headers: bool,
    node_requests: Receiver<NodeRequest>,
    rpc: Arc<BTCDClient>, // For proofs
}
impl Peer {
    pub fn get_proof<T: BtcdRpc>(
        rpc: &T,
        hash: &String,
    ) -> Result<(Proof, Vec<sha256::Hash>, Vec<LeafData>), BlockchainError> {
        let proof = rpc.getutreexoproof(hash.to_string(), true)?.get_verbose();
        Self::process_proof(proof)
    }
    fn process_proof(
        proof: GetUtreexoProofResult,
    ) -> Result<(Proof, Vec<sha256::Hash>, Vec<LeafData>), BlockchainError> {
        let preimages: Vec<_> = proof
            .target_preimages
            .iter()
            .map(|preimage| {
                deserialize_partial::<LeafData>(&Vec::from_hex(preimage).unwrap())
                    .unwrap()
                    .0
            })
            .collect();

        let proof_hashes: Vec<_> = proof
            .proofhashes
            .iter()
            .map(|hash| sha256::Hash::from_hex(hash).unwrap())
            .collect();
        let targets = proof.prooftargets;

        let targethashes: Vec<_> = proof
            .targethashes
            .iter()
            .map(|hash| sha256::Hash::from_hex(hash).unwrap())
            .collect();
        let proof = Proof::new(targets, proof_hashes);

        Ok((proof, targethashes, preimages))
    }

    pub async fn read_loop(mut self) -> Result<(), BlockchainError> {
        let read_stream = self.stream.clone();
        let mut buff = vec![0_u8; MAX_MSG_SIZE];
        let mut stream: StreamReader<_, RawNetworkMessage> = StreamReader::new(read_stream);
        while let Ok(message) = stream.next_message().await {
            println!("{:?}", message);
            if self.state != State::Connected
                && !(message.payload.cmd() == "version" || message.payload.cmd() != "verack")
            {
                return Ok(());
            }
            match message.payload {
                bitcoin::network::message::NetworkMessage::Version(version) => {
                    self.handle_version(version).await;
                    self.state = State::VersionReceived;
                }
                bitcoin::network::message::NetworkMessage::Verack => {
                    self.state = State::Connected;
                }
                bitcoin::network::message::NetworkMessage::Addr(_) => todo!(),
                bitcoin::network::message::NetworkMessage::Inv(inv) => {
                    for inv_entry in inv {
                        match inv_entry {
                            bitcoin::network::message_blockdata::Inventory::Error => {}
                            bitcoin::network::message_blockdata::Inventory::Transaction(_) => {}
                            bitcoin::network::message_blockdata::Inventory::Block(block_hash) => {
                                self.node_tx
                                    .send(NodeNotification::NewBlock(block_hash))
                                    .await;
                            }
                            bitcoin::network::message_blockdata::Inventory::CompactBlock(_) => {}
                            bitcoin::network::message_blockdata::Inventory::WTx(_) => todo!(),
                            bitcoin::network::message_blockdata::Inventory::WitnessTransaction(
                                _,
                            ) => {}
                            bitcoin::network::message_blockdata::Inventory::WitnessBlock(_) => {}
                            bitcoin::network::message_blockdata::Inventory::Unknown {
                                inv_type,
                                hash,
                            } => {}
                        }
                    }
                }
                bitcoin::network::message::NetworkMessage::GetData(_) => todo!(),
                bitcoin::network::message::NetworkMessage::NotFound(_) => todo!(),
                bitcoin::network::message::NetworkMessage::GetBlocks(_) => todo!(),
                bitcoin::network::message::NetworkMessage::GetHeaders(_) => todo!(),
                bitcoin::network::message::NetworkMessage::MemPool => todo!(),
                bitcoin::network::message::NetworkMessage::Tx(_) => {
                    // self.mempool.write().await.accept_to_mempool();
                }
                bitcoin::network::message::NetworkMessage::Block(block) => {
                    let (proof, del_hashes, leaf_data) =
                        Self::get_proof(&*self.rpc, &block.block_hash().to_string())?;
                    let height = self.chain.get_height()? + 1;
                    let mut inputs = HashMap::new();
                    for tx in block.txdata.iter() {
                        for (vout, out) in tx.output.iter().enumerate() {
                            inputs.insert(
                                OutPoint {
                                    txid: tx.txid(),
                                    vout: vout as u32,
                                },
                                out.clone(),
                            );
                        }
                    }
                    for leaf in leaf_data {
                        inputs.insert(leaf.prevout, leaf.utxo);
                    }
                    self.chain.connect_block(&block, proof, inputs, del_hashes);
                }
                bitcoin::network::message::NetworkMessage::Headers(_) => todo!(),
                bitcoin::network::message::NetworkMessage::SendHeaders => {
                    self.send_headers = true;
                }
                bitcoin::network::message::NetworkMessage::GetAddr => {}
                bitcoin::network::message::NetworkMessage::Ping(nonce) => {
                    self.handle_ping(nonce).await;
                }
                bitcoin::network::message::NetworkMessage::Pong(_) => {}
                bitcoin::network::message::NetworkMessage::MerkleBlock(_) => todo!(),
                bitcoin::network::message::NetworkMessage::FilterLoad(_) => todo!(),
                bitcoin::network::message::NetworkMessage::FilterAdd(_) => todo!(),
                bitcoin::network::message::NetworkMessage::FilterClear => todo!(),
                bitcoin::network::message::NetworkMessage::GetCFilters(_) => todo!(),
                bitcoin::network::message::NetworkMessage::CFilter(_) => todo!(),
                bitcoin::network::message::NetworkMessage::GetCFHeaders(_) => todo!(),
                bitcoin::network::message::NetworkMessage::CFHeaders(_) => todo!(),
                bitcoin::network::message::NetworkMessage::GetCFCheckpt(_) => todo!(),
                bitcoin::network::message::NetworkMessage::CFCheckpt(_) => todo!(),
                bitcoin::network::message::NetworkMessage::SendCmpct(_) => {}
                bitcoin::network::message::NetworkMessage::CmpctBlock(_) => todo!(),
                bitcoin::network::message::NetworkMessage::GetBlockTxn(_) => todo!(),
                bitcoin::network::message::NetworkMessage::BlockTxn(_) => todo!(),
                bitcoin::network::message::NetworkMessage::Alert(msg) => {}
                bitcoin::network::message::NetworkMessage::Reject(_) => todo!(),
                bitcoin::network::message::NetworkMessage::FeeFilter(_) => todo!(),
                bitcoin::network::message::NetworkMessage::WtxidRelay => {}
                bitcoin::network::message::NetworkMessage::AddrV2(_) => todo!(),
                bitcoin::network::message::NetworkMessage::SendAddrV2 => {}
                bitcoin::network::message::NetworkMessage::Unknown { command, payload } => todo!(),
            }
        }
        Ok(())
    }
}
impl Peer {
    pub async fn write(&self, msg: NetworkMessage) -> Result<(), crate::error::Error> {
        let mut data = &mut RawNetworkMessage {
            magic: self.network.magic(),
            payload: msg,
        };
        let data = serialize(&data);
        (&self.stream).write_all(&data).await?;
        Ok(())
    }

    pub async fn create_outbound_connection<A: ToSocketAddrs>(
        chain: Arc<ChainState<KvChainStore>>,
        id: u32,
        address: A,
        mempool: Arc<RwLock<Mempool>>,
        network: Network,
        node_tx: Sender<NodeNotification>,
        rpc: Arc<BTCDClient>,
        node_requests: Receiver<NodeRequest>,
    ) -> Result<Peer, std::io::Error> {
        let stream = TcpStream::connect(address).await?;

        let mut peer = Peer {
            blocks_only: false,
            chain,
            current_best_block: -1,
            id,
            last_ping: Instant::now(),
            mempool,
            network,
            node_tx,
            services: ServiceFlags::NONE,
            stream,
            user_agent: "".into(),
            state: State::None,
            send_headers: false,
            rpc,
            node_requests,
        };
        let version = peer_utils::build_version_message();
        // send a version
        peer.write(version).await;
        Ok(peer)
    }
    async fn handle_ping(&mut self, nonce: u64) {
        self.last_ping = Instant::now();
        let pong = make_pong(nonce);
        self.write(pong).await;
    }
    async fn handle_version(&mut self, version: VersionMessage) {
        self.user_agent = version.user_agent;
        self.blocks_only = !version.relay;
        self.current_best_block = version.start_height;
        self.services = version.services;
        let verack = NetworkMessage::Verack;
        self.write(verack).await;
    }
}

pub(super) mod peer_utils {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{SystemTime, UNIX_EPOCH},
    };

    use bitcoin::{
        network::{
            address, constants,
            message::{self, NetworkMessage},
            message_network,
        },
        secp256k1::Secp256k1,
    };
    pub(super) fn make_pong(nonce: u64) -> NetworkMessage {
        NetworkMessage::Pong(nonce)
    }
    pub(super) fn build_version_message() -> message::NetworkMessage {
        // Building version message, see https://en.bitcoin.it/wiki/Protocol_documentation#version
        let my_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);

        // "bitfield of features to be enabled for this connection"
        let services = constants::ServiceFlags::NETWORK;

        // "standard UNIX timestamp in seconds"
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time error")
            .as_secs();

        // "The network address of the node receiving this message"
        let addr_recv = address::Address::new(&my_address, constants::ServiceFlags::NONE);

        // "The network address of the node emitting this message"
        let addr_from = address::Address::new(&my_address, constants::ServiceFlags::NONE);

        // "Node random nonce, randomly generated every time a version packet is sent. This nonce is used to detect connections to self."
        let nonce: u64 = 1;

        // "User Agent (0x00 if string is 0 bytes long)"
        let user_agent = String::from("rust-example");

        // "The last block received by the emitting node"
        let start_height: i32 = 0;

        // Construct the message
        message::NetworkMessage::Version(message_network::VersionMessage {
            services,
            timestamp: timestamp as i64,
            receiver: addr_recv,
            sender: addr_from,
            nonce,
            user_agent,
            start_height,
            relay: false,
            version: 70016,
        })
    }
}
