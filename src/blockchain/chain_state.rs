use super::{
    chainparams::ChainParams,
    chainstore::{ChainStore, DiskBlockHeader, KvChainStore},
    error::{BlockValidationErrors, BlockchainError},
    BlockchainInterface, BlockchainProviderInterface, Notification,
};
use crate::{read_lock, write_lock};
use async_std::channel::Sender;
use bitcoin::{
    blockdata::constants::genesis_block,
    consensus::{deserialize_partial, Decodable, Encodable},
    hashes::{sha256, Hash},
    util::uint::Uint256,
    Block, BlockHash, BlockHeader, Network, OutPoint, Transaction, TxOut,
};
use rustreexo::accumulator::{proof::Proof, stump::Stump};
use sha2::{Digest, Sha512_256};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
};
use std::{sync::RwLock, time::UNIX_EPOCH};
pub struct ChainStateInner<PersistedState: ChainStore> {
    /// The acc we use for validation.
    acc: Stump,
    /// All data is persisted here.
    chainstore: PersistedState,
    /// Best known block, cached in a specific field to faster access.
    best_block: BestChain,
    /// When one of our consumers tries to broadcast a transaction, this transaction gets
    /// writen to broadcast_queue, and the ChainStateBackend can use it's own logic to actually
    /// broadcast the tx.
    broadcast_queue: Vec<Transaction>,
    /// We may have more than one consumer, that access our data through [BlockchainInterface],
    /// they might need to be notified about new data coming in, like blocks. They do so by calling
    /// `subscribe` and passing a [async_std::channel::Sender]. We save all Senders here.
    subscribers: Vec<Sender<Notification>>,
    /// Fee estimation for 1, 10 and 20 blocks
    fee_estimation: (f64, f64, f64),
    /// Are we in Initial Block Download?
    ibd: bool,
    /// Global parameter of the chain we are in
    chain_params: ChainParams,
}

pub struct ChainState<PersistedState: ChainStore> {
    inner: RwLock<ChainStateInner<PersistedState>>,
}

impl<PersistedState: ChainStore> ChainState<PersistedState> {
    // TODO: Move to LeafData
    pub fn get_leaf_hashes(
        transaction: &Transaction,
        vout: u32,
        height: u32,
        block_hash: BlockHash,
    ) -> sha256::Hash {
        let header_code = height << 1;

        let mut ser_utxo = vec![];
        let utxo = transaction.output.get(vout as usize).unwrap();
        utxo.consensus_encode(&mut ser_utxo).unwrap();
        let header_code = if transaction.is_coin_base() {
            header_code | 1
        } else {
            header_code
        };

        let leaf_hash = Sha512_256::new()
            .chain_update(block_hash)
            .chain_update(transaction.txid())
            .chain_update(vout.to_le_bytes())
            .chain_update(header_code.to_le_bytes())
            .chain_update(ser_utxo)
            .finalize();
        sha256::Hash::from_slice(leaf_hash.as_slice())
            .expect("parent_hash: Engines shouldn't be Err")
    }
    pub fn verify_block_transactions(
        mut utxos: HashMap<OutPoint, TxOut>,
        transactions: &[Transaction],
    ) -> Result<bool, crate::error::Error> {
        for transaction in transactions {
            if !transaction.is_coin_base() {
                transaction.verify(|outpoint| utxos.remove(outpoint))?;
            }
        }
        Ok(true)
    }
    fn calc_next_work_required(
        last_block: &BlockHeader,
        first_block: &BlockHeader,
        params: ChainParams,
    ) -> u32 {
        let cur_target = last_block.target();

        let expected_timespan = Uint256::from_u64(params.pow_target_timespan).unwrap();
        let actual_timespan = last_block.time - first_block.time;

        let new_target = cur_target.mul_u32(actual_timespan);
        let new_target = new_target / expected_timespan;

        BlockHeader::compact_target_from_u256(&new_target)
    }
    fn get_next_required_work(&self, last_block: &BlockHeader, next_height: u32) -> Uint256 {
        let params = self.chain_params();
        let last_block_time = UNIX_EPOCH + std::time::Duration::from_secs(last_block.time as u64);
        // Special testnet rule, if a block takes more than 20 minutes to mine, we can
        // mine a block with diff 1
        if params.pow_allow_min_diff
            && last_block_time + std::time::Duration::from_secs(20 * 60)
                > std::time::SystemTime::now()
        {
            return params.max_target;
        }
        // Retarget
        // Regtest don't have retarget
        if !params.pow_allow_no_retarget && (next_height) % 2016 == 0 {
            // First block in this epoch
            let first_block = self.get_block_header_by_height(next_height - 2016);
            let last_block = self.get_block_header_by_height(next_height - 1);

            let next_bits =
                Self::calc_next_work_required(&last_block, &first_block, self.chain_params());
            let target = BlockHeader::u256_from_compact_target(next_bits);
            if target > params.max_target {
                return target;
            }
            return params.max_target;
        }
        last_block.target()
    }
    /// Returns the chain_params struct for the current network
    fn chain_params(&self) -> ChainParams {
        let inner = read_lock!(self);
        inner.chain_params.clone()
    }
    // This function should be only called if a block is guaranteed to be on chain
    fn get_block_header_by_height(&self, height: u32) -> BlockHeader {
        let block = self
            .get_block_hash(height)
            .expect("This block should be present");
        self.get_block_header(&block)
            .expect("This block should also be present")
    }
    pub fn update_acc(
        acc: &Stump,
        block: &Block,
        height: u32,
        proof: Proof,
        del_hashes: Vec<sha256::Hash>,
    ) -> Result<Stump, BlockchainError> {
        let block_hash = block.block_hash();
        let mut leaf_hashes = vec![];
        if !proof.verify(&del_hashes, acc)? {
            return Err(BlockchainError::InvalidProof);
        }
        let mut block_inputs = HashSet::new();
        for transaction in block.txdata.iter() {
            for input in transaction.input.iter() {
                block_inputs.insert((input.previous_output.txid, input.previous_output.vout));
            }
        }

        for transaction in block.txdata.iter() {
            for (i, output) in transaction.output.iter().enumerate() {
                if !output.script_pubkey.is_provably_unspendable()
                    && !block_inputs.contains(&(transaction.txid(), i as u32))
                {
                    leaf_hashes.push(Self::get_leaf_hashes(
                        transaction,
                        i as u32,
                        height,
                        block_hash,
                    ))
                }
            }
        }
        let acc = acc.modify(&leaf_hashes, &del_hashes, &proof)?.0;

        Ok(acc)
    }

    pub fn save_acc(&self) -> Result<(), bitcoin::consensus::encode::Error> {
        let inner = read_lock!(self);
        let mut ser_acc: Vec<u8> = vec![];
        inner.acc.leafs.consensus_encode(&mut ser_acc)?;
        #[allow(clippy::significant_drop_in_scrutinee)]
        for root in inner.acc.roots.iter() {
            ser_acc
                .write_all(root)
                .expect("String formatting should not err");
        }

        inner
            .chainstore
            .save_roots(ser_acc)
            .expect("Chain store is not working");
        Ok(())
    }
    #[allow(clippy::await_holding_lock)]
    async fn notify(&self, what: Notification) {
        //TODO: Use async-std::RwLock not std::RwLock
        let inner = self.inner.read().unwrap();
        let subs = inner.subscribers.iter();
        for client in subs {
            let _ = client.send(what.clone()).await;
        }
    }
    /// If we already hold a write lock to inner, we can't call self.save_acc() because it'd
    /// cause a deadlock. This method can be called from the already existing lock guard we acquired.
    /// It's intended to be used internally only.
    fn save_acc_inner(
        inner: &ChainStateInner<PersistedState>,
    ) -> Result<(), bitcoin::consensus::encode::Error> {
        let mut ser_acc: Vec<u8> = vec![];
        inner.acc.leafs.consensus_encode(&mut ser_acc)?;
        #[allow(clippy::significant_drop_in_scrutinee)]
        for root in inner.acc.roots.iter() {
            ser_acc
                .write_all(root)
                .expect("String formatting should not err");
        }

        inner
            .chainstore
            .save_roots(ser_acc)
            .expect("Chain store is not working");
        Ok(())
    }
    pub fn new(chainstore: KvChainStore, network: Network) -> ChainState<KvChainStore> {
        let genesis = genesis_block(network);
        chainstore
            .save_header(
                &super::chainstore::DiskBlockHeader::FullyValid(genesis.header, 0),
                0,
            )
            .expect("Error while saving genesis");
        ChainState {
            inner: RwLock::new(ChainStateInner {
                chainstore,
                acc: Stump::new(),
                best_block: BestChain {
                    best_block: genesis.block_hash(),
                    depth: 0,
                    validation_index: genesis.block_hash(),
                    rescan_index: None,
                    alternative_tips: vec![],
                },
                broadcast_queue: vec![],
                subscribers: vec![],
                fee_estimation: (1_f64, 1_f64, 1_f64),
                ibd: true,
                chain_params: network.into(),
            }),
        }
    }
    fn get_disk_block_header(&self, hash: &BlockHash) -> super::Result<DiskBlockHeader> {
        let inner = read_lock!(self);
        if let Some(header) = inner.chainstore.get_header(hash)? {
            return Ok(header);
        }
        Err(BlockchainError::BlockNotPresent)
    }

    pub fn load_chain_state(
        chainstore: KvChainStore,
        network: Network,
    ) -> Result<ChainState<KvChainStore>, BlockchainError> {
        let acc = Self::load_acc(&chainstore);

        let best_chain = chainstore.load_height()?;
        if best_chain.is_none() {
            return Err(BlockchainError::ChainNotInitialized);
        }

        let inner = ChainStateInner {
            acc,
            best_block: best_chain.unwrap(),
            broadcast_queue: Vec::new(),
            chainstore,
            fee_estimation: (1_f64, 1_f64, 1_f64),
            subscribers: Vec::new(),
            ibd: true,
            chain_params: network.into(),
        };

        Ok(ChainState {
            inner: RwLock::new(inner),
        })
    }

    pub fn load_acc<Storage: ChainStore>(data_storage: &Storage) -> Stump {
        let acc = data_storage
            .load_roots()
            .expect("load_acc: Could not read roots");
        if acc.is_none() {
            return Stump::new();
        }
        let mut acc = acc.unwrap();
        let leaves = acc.drain(0..8).collect::<Vec<u8>>();
        let (leaves, _) =
            deserialize_partial::<u64>(&leaves).expect("load_acc: Invalid num_leaves");
        let mut roots = vec![];
        while acc.len() >= 32 {
            // Since we only expect hashes after the num_leaves, it should always align with 32 bytes
            assert_eq!(acc.len() % 32, 0);
            let root = acc.drain(0..32).collect::<Vec<u8>>();
            let root = sha256::Hash::from_slice(&root).expect("Invalid hash");
            roots.push(root);
        }
        Stump {
            leafs: leaves,
            roots,
        }
    }
}

impl<PersistedState: ChainStore> BlockchainInterface for ChainState<PersistedState> {
    fn is_in_idb(&self) -> bool {
        self.inner.read().unwrap().ibd
    }
    fn get_block_hash(&self, height: u32) -> super::Result<bitcoin::BlockHash> {
        let inner = self.inner.read().expect("get_block_hash: Poisoned lock");
        if let Some(hash) = inner.chainstore.get_block_hash(height)? {
            return Ok(hash);
        }
        Err(BlockchainError::BlockNotPresent)
    }

    fn get_tx(&self, _txid: &bitcoin::Txid) -> super::Result<Option<bitcoin::Transaction>> {
        unimplemented!("This chainstate doesn't hold any tx")
    }

    fn get_height(&self) -> super::Result<u32> {
        let inner = read_lock!(self);
        Ok(inner.best_block.depth)
    }

    fn broadcast(&self, tx: &bitcoin::Transaction) -> super::Result<()> {
        let mut inner = write_lock!(self);
        inner.broadcast_queue.push(tx.clone());
        Ok(())
    }

    fn estimate_fee(&self, target: usize) -> super::Result<f64> {
        let inner = read_lock!(self);
        if target == 1 {
            Ok(inner.fee_estimation.0)
        } else if target == 10 {
            Ok(inner.fee_estimation.1)
        } else {
            Ok(inner.fee_estimation.2)
        }
    }

    fn get_block(&self, _hash: &BlockHash) -> super::Result<bitcoin::Block> {
        unimplemented!("This chainstate doesn't hold full blocks")
    }

    fn get_best_block(&self) -> super::Result<(u32, BlockHash)> {
        let inner = read_lock!(self);
        Ok((inner.best_block.depth, inner.best_block.best_block))
    }

    fn get_block_header(&self, hash: &BlockHash) -> super::Result<bitcoin::BlockHeader> {
        let inner = read_lock!(self);
        if let Some(header) = inner.chainstore.get_header(hash)? {
            return Ok(*header);
        }
        Err(BlockchainError::BlockNotPresent)
    }

    fn subscribe(&self, tx: Sender<Notification>) {
        let mut inner = self.inner.write().expect("get_block_hash: Poisoned lock");
        inner.subscribers.push(tx);
    }
}
impl<PersistedState: ChainStore> BlockchainProviderInterface for ChainState<PersistedState> {
    fn get_block_locator(&self) -> Result<Vec<BlockHash>, BlockchainError> {
        let top_height = self.get_height()?;
        let mut indexes = vec![];
        let mut step = 1;
        let mut index = top_height;

        while index > 0 {
            if indexes.len() >= 10 {
                step *= 2;
            }
            indexes.push(index);
            if index > step {
                index -= step;
            } else {
                break;
            }
        }
        indexes.push(0);
        let hashes = indexes
            .iter()
            .map(|idx| self.get_block_hash(*idx).unwrap())
            .collect();

        Ok(hashes)
    }
    fn toggle_ibd(&self, is_ibd: bool) {
        let mut inner = write_lock!(self);
        inner.ibd = is_ibd;
    }
    fn connect_block(
        &self,
        block: &Block,
        proof: Proof,
        inputs: HashMap<OutPoint, TxOut>,
        del_hashes: Vec<sha256::Hash>,
    ) -> super::Result<()> {
        let header = self.get_disk_block_header(&block.block_hash())?;

        let height = match header {
            // If it's valid or orphan, we don't validate
            DiskBlockHeader::FullyValid(_, _) => {
                self.inner
                    .write()
                    .unwrap()
                    .best_block
                    .valid_block(block.block_hash());
                return Ok(());
            }
            DiskBlockHeader::Orphan(_) => return Ok(()),
            DiskBlockHeader::HeadersOnly(_, height) => height,
        };
        let inner = self.inner.read().unwrap();
        let acc = Self::update_acc(&inner.acc, block, height, proof, del_hashes)?;

        if !block.check_merkle_root() {
            return Err(BlockchainError::BlockValidationError(
                BlockValidationErrors::BadMerkleRoot,
            ));
        }
        if !block.check_witness_commitment() {
            return Err(BlockchainError::BlockValidationError(
                BlockValidationErrors::BadWitnessCommitment,
            ));
        }
        let prev_block = inner.best_block.validation_index;
        if block.header.prev_blockhash != prev_block {
            return Err(BlockchainError::BlockValidationError(
                BlockValidationErrors::PrevBlockNotFound(block.header.prev_blockhash),
            ));
        }
        let prev_block = self.get_block_header(&prev_block)?;

        // Check pow
        let target = self.get_next_required_work(&prev_block, height);
        let hash = block.header.validate_pow(&target).map_err(|_| {
            BlockchainError::BlockValidationError(BlockValidationErrors::NotEnoughPow)
        })?;

        Self::verify_block_transactions(inputs, &block.txdata)
            .map_err(|_| BlockchainError::BlockValidationError(BlockValidationErrors::InvalidTx))?;

        // ... If we came this far, we consider this block valid ...

        // Notify others we have a new block
        async_std::task::block_on(self.notify(Notification::NewBlock((block.to_owned(), height))));
        inner.chainstore.save_header(
            &super::chainstore::DiskBlockHeader::FullyValid(block.header, height),
            height,
        )?;
        inner.chainstore.save_height(&inner.best_block)?;
        // Drop this lock because we need a write lock to inner, if we hold this lock this will
        // cause a deadlock.
        drop(inner);
        // Updates our local view of the network
        let mut inner = self.inner.write().unwrap();
        inner.acc = acc;
        inner.best_block.valid_block(hash);

        Self::save_acc_inner(&*inner)?;
        Ok(())
    }

    fn handle_reorg(&self) -> super::Result<()> {
        todo!()
    }

    fn handle_transaction(&self) -> super::Result<()> {
        unimplemented!("This chain_state has no mempool")
    }

    fn flush(&self) -> super::Result<()> {
        self.save_acc()?;
        let inner = read_lock!(self);
        inner.chainstore.flush()?;
        inner.chainstore.save_height(&inner.best_block)?;
        Ok(())
    }
    fn get_unbroadcasted(&self) -> Vec<Transaction> {
        let mut inner = write_lock!(self);
        inner.broadcast_queue.drain(..).collect()
    }

    fn accept_header(&self, header: BlockHeader) -> super::Result<()> {
        // We already know this block
        if self.get_block_header(&header.block_hash()).is_ok() {
            return Ok(());
        }
        let inner = self.inner.read().unwrap();
        let best_block = &inner.best_block;
        // Update our current tip
        if header.prev_blockhash == best_block.best_block {
            let height = best_block.depth + 1;

            let prev_block = self.get_block_header(&best_block.best_block)?;
            // Check pow
            let target = self.get_next_required_work(&prev_block, height);
            let _ = header.validate_pow(&target).map_err(|_| {
                BlockchainError::BlockValidationError(BlockValidationErrors::NotEnoughPow)
            })?;
            inner.chainstore.save_header(
                &super::chainstore::DiskBlockHeader::HeadersOnly(header, height),
                height,
            )?;
            drop(inner);
            let mut inner = self.inner.write().unwrap();
            inner.best_block.new_block(header.block_hash(), height);
        } else {
            unimplemented!();
        }
        Ok(())
    }

    fn get_validation_index(&self) -> super::Result<u32> {
        let inner = self.inner.read().unwrap();
        let validation = inner.best_block.validation_index;
        let header = self.get_disk_block_header(&validation)?;
        match header {
            DiskBlockHeader::HeadersOnly(_, height) => Ok(height),
            DiskBlockHeader::FullyValid(_, height) => Ok(height),
            DiskBlockHeader::Orphan(_) => unreachable!(),
        }
    }
}
#[macro_export]
/// Grabs a RwLock for reading
macro_rules! read_lock {
    ($obj: ident) => {
        $obj.inner.read().expect("get_block_hash: Poisoned lock")
    };
}
#[macro_export]
/// Grabs a RwLock for writing
macro_rules! write_lock {
    ($obj: ident) => {
        $obj.inner.write().expect("get_block_hash: Poisoned lock")
    };
}
/// Internal representation of the chain we are in
pub struct BestChain {
    /// Hash of the last block in the chain we believe has more work on
    best_block: BlockHash,
    /// How many blocks are pilled on this chain?
    depth: u32,
    /// We actually validated blocks up to this point
    validation_index: BlockHash,
    /// We may rescan even after we validate all blocks, this index saves the position
    /// we are while re-scanning
    rescan_index: Option<BlockHash>,
    /// Blockchains are not fast-forward only, they might have "forks", sometimes it's useful
    /// to keep track of them, in case they become the best one. This keeps track of some
    /// tips we know about, but are not the best one. We don't keep tips that are too deep
    /// or has too little work if compared to our best one
    alternative_tips: Vec<BlockHash>,
}
impl BestChain {
    fn new_block(&mut self, block_hash: BlockHash, height: u32) {
        self.best_block = block_hash;
        self.depth = height;
    }
    fn valid_block(&mut self, block_hash: BlockHash) {
        self.validation_index = block_hash;
    }
}
impl Encodable for BestChain {
    fn consensus_encode<W: std::io::Write + ?Sized>(
        &self,
        writer: &mut W,
    ) -> Result<usize, std::io::Error> {
        let mut len = 0;
        len += self.best_block.consensus_encode(writer)?;
        len += self.depth.consensus_encode(writer)?;
        len += self.validation_index.consensus_encode(writer)?;

        match self.rescan_index {
            Some(hash) => len += hash.consensus_encode(writer)?,
            None => len += BlockHash::all_zeros().consensus_encode(writer)?,
        }
        len += self.alternative_tips.consensus_encode(writer)?;
        Ok(len)
    }
}
impl Decodable for BestChain {
    fn consensus_decode<R: std::io::Read + ?Sized>(
        reader: &mut R,
    ) -> Result<Self, bitcoin::consensus::encode::Error> {
        let best_block = BlockHash::consensus_decode(reader)?;
        let depth = u32::consensus_decode(reader)?;
        let validation_index = BlockHash::consensus_decode(reader)?;
        let rescan_index = BlockHash::consensus_decode(reader)?;
        let rescan_index = if rescan_index == BlockHash::all_zeros() {
            None
        } else {
            Some(rescan_index)
        };
        let alternative_tips = <Vec<BlockHash>>::consensus_decode(reader)?;
        Ok(Self {
            alternative_tips,
            best_block,
            depth,
            rescan_index,
            validation_index,
        })
    }
}
#[cfg(test)]
mod test {
    use bitcoin::{consensus::deserialize, hashes::hex::FromHex, BlockHeader, Network};

    use crate::blockchain::{chainparams::ChainParams, chainstore::KvChainStore};

    #[test]
    fn test_calc_next_work_required() {
        let first_block = Vec::from_hex("0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a008f4d5fae77031e8ad22203").unwrap();
        let first_block: BlockHeader = deserialize(&first_block).unwrap();

        let last_block = Vec::from_hex("00000020dec6741f7dc5df6661bcb2d3ec2fceb14bd0e6def3db80da904ed1eeb8000000d1f308132e6a72852c04b059e92928ea891ae6d513cd3e67436f908c804ec7be51df535fae77031e4d00f800").unwrap();
        let last_block = deserialize(&last_block).unwrap();

        let next_target = super::ChainState::<KvChainStore>::calc_next_work_required(
            &last_block,
            &first_block,
            ChainParams::from(Network::Bitcoin),
        );

        assert_eq!(0x1e012fa7, next_target);
    }
}
