use clap::{arg, command, Parser, Subcommand, ValueEnum};

#[derive(Clone, Debug, ValueEnum)]
pub enum Network {
    Bitcoin,
    Signet,
    Testnet,
    Regtest,
}
impl Default for Network {
    fn default() -> Self {
        Network::Bitcoin
    }
}
impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Network::Bitcoin => write!(f, "bitcoin"),
            Network::Signet => write!(f, "signet"),
            Network::Testnet => write!(f, "testnet"),
            Network::Regtest => write!(f, "regtest"),
        }
    }
}
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE")]
    pub config_file: Option<String>,
    /// Which network should we use
    #[arg(short, long, default_value_t=Network::Bitcoin)]
    pub network: Network,
    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub debug: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    #[cfg(not(feature = "experimental-p2p"))]
    /// Starts your wallet and server
    Run {
        /// Where should we store data
        #[arg(long)]
        data_dir: Option<String>,
        /// Add a xpub to our wallet
        #[arg(long)]
        wallet_xpub: Option<Vec<String>>,
        /// Your rpc user, as set in Utreexod
        #[arg(long)]
        rpc_user: Option<String>,
        /// Your rpc password, as set in Utreexod
        #[arg(long)]
        rpc_password: Option<String>,
        /// The hostname:port of Utreexod
        #[arg(short, long)]
        rpc_host: Option<String>,
        #[arg(long)]
        rpc_port: Option<u32>,
        /// Whether or not we want to sync with a external provider
        #[arg(long)]
        #[arg(default_value_t = false)]
        use_external_sync: bool,
        /// If use_external_sync is set, this option provides which server we use
        #[arg(long)]
        external_sync: Option<String>,
    },
    #[cfg(feature = "experimental-p2p")]
    /// Starts your wallet and server
    Run {
        /// Where should we store data
        #[arg(long)]
        data_dir: Option<String>,
        /// Add a xpub to our wallet
        #[arg(long)]
        wallet_xpub: Option<Vec<String>>,
        /// Your rpc user, as set in Utreexod
        #[arg(long)]
        rpc_user: Option<String>,
        /// Your rpc password, as set in Utreexod
        #[arg(long)]
        rpc_password: Option<String>,
        /// The hostname:port of Utreexod
        #[arg(short, long)]
        rpc_host: Option<String>,
        #[arg(long)]
        rpc_port: Option<u32>,
    },
}
