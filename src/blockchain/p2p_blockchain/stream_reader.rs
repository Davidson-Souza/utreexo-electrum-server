//! A simple async reader that reads data from a [Source] and builds a [Item] from it, assuming
//! item is [Decodable]. The main intent of this module is to read [RawNetworkMessages] from [TcpStream], because
//! we don't know how many bytes to read upfront, so we might read an incomplete message and try
//! to deserialize it, causing unrelated error. This module first reads the message reader
//! that has constant size (for RawNetworkMessage is 24). Then we look for payload size inside
//! this header. With payload size we can finally read the entire message and return a parsed
//! structure.

use async_std::net::TcpStream;
use async_std::{io::ReadExt, stream::Stream};
use bitcoin::consensus::{deserialize, deserialize_partial, Decodable};
use bitcoin::network::message::{RawNetworkMessage, MAX_MSG_SIZE};
use futures::future::ok;
use futures::AsyncRead;
use std::marker::PhantomData;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::task::Poll;

/// A simple type that wraps a stream and returns T, if T is [Decodable].
pub struct StreamReader<Source: Sync + Send + ReadExt + Unpin, Item: Decodable> {
    /// Were we read bytes from, usually a TcpStream
    source: Source,
    /// Item is what we return, since we don't actually hold any concrete type, just use a
    /// phantom data to bind a type.
    phantom: PhantomData<Item>,
}
impl<Source, Item> StreamReader<Source, Item>
where
    Item: Decodable,
    Source: Sync + Send + ReadExt + Unpin,
{
    /// Creates a new reader from a given stream
    pub fn new(stream: Source) -> Self {
        StreamReader {
            source: stream,
            phantom: PhantomData,
        }
    }
    /// Tries to read from a parsed [Item] from [Source]. Only returns on error or if we have
    /// a valid Item to return
    pub async fn next_message(&mut self) -> Result<Item, crate::error::Error> {
        let mut data: Vec<u8> = Vec::new();
        data.resize(24, 0);

        // Read the reader first, so learn the payload size
        self.source.read_exact(&mut *data).await?;
        let mut header: P2PMessageHeader = deserialize_partial(&mut *data)?.0;

        // Network Message too big
        if header.length + 24 > MAX_MSG_SIZE as u32 {
            return Err(crate::error::Error::WalletNotInitialized);
        }
        data.resize(24 + header.length as usize, 0);

        // Read everything else
        self.source.read_exact(&mut data[24..]).await?;
        let message = deserialize(&*data);

        Ok(message?)
    }
}
pub struct P2PMessageHeader {
    magic: u32,
    command: [u8; 12],
    length: u32,
    checksum: u32,
}
impl Decodable for P2PMessageHeader {
    fn consensus_decode<R: std::io::Read + ?Sized>(
        reader: &mut R,
    ) -> Result<Self, bitcoin::consensus::encode::Error> {
        let magic = u32::consensus_decode(reader)?;
        let command = <[u8; 12]>::consensus_decode(reader)?;
        let length = u32::consensus_decode(reader)?;
        let checksum = u32::consensus_decode(reader)?;
        Ok(Self {
            checksum,
            command,
            length,
            magic,
        })
    }
}
