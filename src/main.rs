// Written in 2022 by Davidson Souza.
// SPDX-License-Identifier: CC0-1.0

//! This is a modular-(ish) utreexo powered wallet backend and fully validating node, it's
//! developed as an experiment to showcase utreexo. This wallet also comes with an Electrum
//! server out-of-the-box, for people to try out with their favorite wallet.
//! This codebase consists of three main parts: a blockchain backend, that gets all information
//! we need from the network. An Electrum Server that talks full Electrum protocol and can be
//! used with any wallet that understands this protocol. Finally, it has the `AddressCache`,
//! a watch-only wallet that keeps track of your wallet's transactions.
//!
//! Right now, the blockchain backend uses a running utreexod's RPC to get needed data, this
//! is because Utreexo p2p messages are WIP, and we want to try out utreexo before that, so we use
//! a client-server base to test, but this is not final nor the goal.

// Coding conventions (lexicographically sorted)
#![deny(arithmetic_overflow)]
#![deny(clippy::absurd_extreme_comparisons)]
#![deny(clippy::almost_swapped)]
#![deny(clippy::assign_op_pattern)]
#![deny(clippy::await_holding_lock)]
#![deny(clippy::borrowed_box)]
#![deny(clippy::boxed_local)]
#![deny(clippy::drop_copy)]
// FIXME: Rethink enum variant naming
#![allow(clippy::enum_variant_names)]
#![deny(clippy::needless_lifetimes)]
#![deny(clippy::wildcard_imports)]
#![deny(clippy::while_let_loop)]
#![deny(missing_docs)]
#![deny(non_camel_case_types)]
#![deny(non_snake_case)]
#![deny(non_upper_case_globals)]
//#![deny(unused)]

mod address_cache;
mod blockchain;
mod cli;
mod config_file;
mod electrum;
mod error;
mod wallet_input;

use std::{path::PathBuf, process::exit, sync::Arc};

use address_cache::{kv_database::KvDatabase, AddressCache, AddressCacheDatabase};
use async_std::task::{self, block_on};

use bitcoin::Network;
use blockchain::{chain_state::ChainState, chainstore::KvChainStore};
use btcd_rpc::client::{BTCDClient, BTCDConfigs, BtcdRpc};
use clap::Parser;
use cli::{Cli, Commands};
use config_file::ConfigFile;
use log::{debug, error, info};
use miniscript::{Descriptor, DescriptorPublicKey};
use pretty_env_logger::env_logger::TimestampPrecision;
use std::str::FromStr;

#[cfg(not(feature = "experimental-p2p"))]
use crate::blockchain::cli_blockchain::UtreexodBackend;

#[cfg(feature = "experimental-p2p")]
use crate::blockchain::p2p_blockchain::{Mempool, UtreexoNode};

fn main() {
    // Setup global logger
    pretty_env_logger::formatted_timed_builder()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp(Some(TimestampPrecision::Seconds))
        .format_module_path(false)
        .init();

    let params = Cli::parse();
    let data = get_config_file(&params);
    match params.command {
        Commands::Run {
            data_dir,
            rpc_user,
            rpc_password,
            rpc_host,
            external_sync,
            use_external_sync,
            rpc_port,
            wallet_xpub,
        } => {
            // Catch user CTR + C for terminating our application
            let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
            signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))
                .expect("Could no register for SIGTERM");

            let data_dir = get_one_or_another(
                data_dir,
                dirs::home_dir().map(|x: PathBuf| {
                    x.to_str().unwrap_or_default().to_owned() + "/.utreexo_wallet/"
                }),
                "wallet".into(),
            );

            debug!("Loading wallet");
            let mut wallet = load_wallet(&data_dir);
            wallet.setup().expect("Could not initialize wallet");
            debug!("Done loading wallet");
            let result = setup_wallet(
                get_both_vec(wallet_xpub, data.wallet.xpubs),
                &mut wallet,
                params.network.clone(),
            );
            if let Err(e) = result {
                log::error!("Something went wrong while setting wallet up: {e}");
                return;
            }
            #[cfg(not(feature = "experimental-p2p"))]
            let rpc = create_rpc_connection(
                &get_one_or_another(rpc_host, data.rpc.rpc_host, "localhost".into()),
                get_one_or_another(rpc_port, data.rpc.rpc_port, 8332),
                Some(get_one_or_another(rpc_user, data.rpc.rpc_user, "".into())),
                Some(get_one_or_another(
                    rpc_password,
                    data.rpc.rpc_password,
                    "".into(),
                )),
            );

            #[cfg(not(feature = "experimental-p2p"))]
            if !test_rpc(&rpc) {
                info!("Unable to connect with rpc");
                return;
            }
            info!("Starting sync worker, this might take a while!");

            debug!("Loading database...");
            let blockchain_state = Arc::new(load_chain_state(&data_dir, get_net(&params.network)));

            debug!("Done loading wallet");

            #[cfg(not(feature = "experimental-p2p"))]
            let chain_provider = UtreexodBackend {
                chainstate: blockchain_state.clone(),
                rpc,
                external_sync_hostname: get_one_or_another(
                    external_sync,
                    data.misc.external_sync,
                    "".into(),
                ),
                use_external_sync,
                term: shutdown,
            };
            #[cfg(feature = "experimental-p2p")]
            let chain_provider = UtreexoNode::new(
                blockchain_state.clone(),
                Arc::new(async_std::sync::RwLock::new(Mempool)),
                Network::Signet,
            );
            info!("Starting server");
            // Create a new electrum server, we need to block_on because `ElectrumServer::new` is `async`
            // but our main isn't, so we can't `.await` on it.
            let electrum_server = block_on(electrum::electrum_protocol::ElectrumServer::new(
                "0.0.0.0:50001",
                wallet,
                blockchain_state,
            ))
            .expect("Could not create an Electrum Server");

            task::spawn(electrum::electrum_protocol::accept_loop(
                electrum_server
                    .listener
                    .clone()
                    .expect("Listener can't be none by this far"),
                electrum_server.notify_tx.clone(),
            ));
            task::spawn(electrum_server.main_loop());
            info!("Server running on: 0.0.0.0:50001");
            task::block_on(chain_provider.run());
        }
    }
}
/// Loads a config file from disk, returns default if some error happens
fn get_config_file(params: &cli::Cli) -> ConfigFile {
    let data = if let Some(file_name) = &params.config_file {
        ConfigFile::from_file(file_name)
    } else {
        // File not passed in, use default
        return ConfigFile::default();
    };
    if let Ok(data) = data {
        data
    } else {
        match data.unwrap_err() {
            error::Error::TomlParsingError(e) => {
                error!("Error while parsing config file, ignoring it");
                debug!("{e}");
                ConfigFile::default()
            }
            // Shouldn't be any other error
            _ => unreachable!(),
        }
    }
}
fn load_chain_state(data_dir: &String, network: Network) -> ChainState<KvChainStore> {
    let db = KvChainStore::new(data_dir.to_string()).expect("Could not read db");
    match ChainState::<KvChainStore>::load_chain_state(db, network) {
        Ok(chainstate) => chainstate,
        Err(err) => match err {
            blockchain::error::BlockchainError::ChainNotInitialized => {
                let db = KvChainStore::new(data_dir.to_string()).expect("Could not read db");

                ChainState::<KvChainStore>::new(db, network)
            }
            _ => unreachable!(),
        },
    }
}
fn load_wallet(data_dir: &String) -> AddressCache<KvDatabase> {
    let database = KvDatabase::new(data_dir.to_owned()).expect("Could not create a database");
    AddressCache::new(database)
}
fn create_rpc_connection(
    hostname: &String,
    rpc_port: u32,
    username: Option<String>,
    password: Option<String>,
) -> Arc<BTCDClient> {
    let config = BTCDConfigs::new(
        false,
        username,
        password,
        Some(hostname.to_owned()),
        Some(rpc_port as usize),
    );
    let connection = BTCDClient::new(config);
    if connection.is_err() {
        error!("Could not create RPC connection, check your configs");
        exit(1);
    }
    Arc::new(connection.expect("We checked this above, it's impossible to be Err"))
}

fn get_net(net: &cli::Network) -> Network {
    match net {
        cli::Network::Bitcoin => Network::Bitcoin,
        cli::Network::Signet => Network::Signet,
        cli::Network::Testnet => Network::Testnet,
        cli::Network::Regtest => Network::Regtest,
    }
}
fn setup_wallet<D: AddressCacheDatabase>(
    xpubs: Vec<String>,
    wallet: &mut AddressCache<D>,
    network: cli::Network,
) -> Result<(), crate::error::Error> {
    if xpubs.is_empty() {
        return Ok(());
    }
    for key in xpubs {
        // Don't cache a descriptor twice
        if wallet.is_cached(&key)? {
            continue;
        }
        // Parses the descriptor and get an external and change descriptors
        let xpub = wallet_input::extended_pub_key::from_wif(key.as_str());
        if let Err(error) = xpub {
            log::error!("Invalid xpub provided: {key} \nReason: {:?}", error);
            exit(0);
        }
        let xpub = xpub.expect("We checked this above, should not be Err");
        let main_desc = format!("wpkh({xpub}/0/*)");
        let change_desc = format!("wpkh({xpub}/1/*)");
        // Saves our descriptors on disk for further derivations
        wallet.push_descriptor(&main_desc)?;
        wallet.push_descriptor(&change_desc)?;
        // Derives a bunch of addresses to keep track of
        derive_addresses(main_desc, wallet, &network);
        derive_addresses(change_desc, wallet, &network);
    }

    info!("Wallet setup completed!");
    Ok(())
}
fn derive_addresses<D: AddressCacheDatabase>(
    descriptor: String,
    wallet: &mut AddressCache<D>,
    network: &cli::Network,
) {
    let desc = Descriptor::<DescriptorPublicKey>::from_str(descriptor.as_str())
        .expect("Error while parsing descriptor");
    for index in 0..100 {
        let address = desc
            .at_derivation_index(index)
            .address(get_net(network))
            .expect("Error while deriving address. Is this an active descriptor?");
        wallet.cache_address(address.script_pubkey());
    }
}
/// Finds out whether our RPC works or not
fn test_rpc(rpc: &BTCDClient) -> bool {
    if rpc.getbestblock().is_ok() {
        return true;
    }
    false
}
/// Returns the value that is defined, if a is defined, return b. If b is defined and and
/// a not, returns b. If a and b is defined, returns a, etc.
fn get_one_or_another<A, B, Return>(a: Option<A>, b: Option<B>, default: Return) -> Return
where
    A: Into<Return>,
    B: Into<Return>,
{
    if let Some(a) = a {
        return a.into();
    }
    if let Some(b) = b {
        return b.into();
    }

    default
}

fn get_both_vec<T>(a: Option<Vec<T>>, b: Option<Vec<T>>) -> Vec<T> {
    let mut result: Vec<T> = vec![];
    if let Some(a) = a {
        result.extend(a.into_iter());
    }
    if let Some(b) = b {
        result.extend(b.into_iter());
    }
    result
}
