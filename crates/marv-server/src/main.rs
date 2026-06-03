//! The `marv-server` binary: the agent protocol over stdio (`spec/03` §1).
//!
//! Reads line-delimited JSON-RPC 2.0 requests from stdin and writes one response
//! object per line to stdout. This is the default transport an agent attaches
//! to; the protocol logic lives in [`marv_server`].

use std::io::{self, BufReader};

fn main() -> io::Result<()> {
    let mut server = marv_server::Server::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    marv_server::serve(&mut server, BufReader::new(stdin.lock()), &mut stdout)
}
