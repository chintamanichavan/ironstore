use clap::{Parser, Subcommand};
use vektalite::network::client::Client;

#[derive(Parser)]
#[command(name = "vektalite-cli", about = "VektaLite CLI client")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:6380")]
    addr: String,

    /// Auth token (required when server has VEKTALITE_AUTH set)
    #[arg(long)]
    auth: Option<String>,

    /// Path to a PEM CA certificate for TLS verification
    #[arg(long)]
    tls_ca: Option<String>,

    /// Skip TLS certificate verification (dev mode only — MITM-vulnerable)
    #[arg(long)]
    tls_insecure: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Get { key: String },
    Set { key: String, value: String },
    Del { key: String },
    Stats,
    /// Store a vector embedding: set-vec <key> <f32,f32,...>
    SetVec {
        key: String,
        /// Comma-separated f32 values, e.g. "0.1,0.2,0.3"
        vector: String,
    },
    /// Search top-k similar vectors: search <k> <f32,f32,...>
    Search {
        k: u32,
        /// Comma-separated query vector, e.g. "0.1,0.2,0.3"
        query: String,
    },
}

fn parse_vec(s: &str) -> Result<Vec<f32>, String> {
    s.split(',')
        .map(|t| t.trim().parse::<f32>().map_err(|e| e.to_string()))
        .collect()
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let use_tls = cli.tls_ca.is_some() || cli.tls_insecure;
    let mut client = if use_tls {
        let tls_config = vektalite::tls::client_config(cli.tls_ca.as_deref(), cli.tls_insecure)
            .expect("TLS config failed");
        Client::connect_tls(&cli.addr, tls_config)
            .await
            .expect("TLS connect failed")
    } else {
        Client::connect(&cli.addr).await.expect("connect failed")
    };

    if let Some(token) = &cli.auth {
        client.auth(token).await.expect("AUTH failed");
    }

    match cli.command {
        Command::Get { key } => match client.get(&key).await.expect("GET failed") {
            Some(v) => println!("{v}"),
            None => println!("(nil)"),
        },
        Command::Set { key, value } => {
            client.set(&key, &value).await.expect("SET failed");
            println!("OK");
        }
        Command::Del { key } => {
            client.delete(&key).await.expect("DEL failed");
            println!("OK");
        }
        Command::Stats => {
            let s = client.stats().await.expect("STATS failed");
            println!("{s}");
        }
        Command::SetVec { key, vector } => {
            let vec = parse_vec(&vector).expect("invalid vector format");
            client.set_vec(&key, vec).await.expect("SET_VEC failed");
            println!("OK");
        }
        Command::Search { k, query } => {
            let vec = parse_vec(&query).expect("invalid query vector format");
            let results = client.search(vec, k).await.expect("SEARCH failed");
            if results.is_empty() {
                println!("(no results)");
            } else {
                for (key, score) in &results {
                    println!("{key}\t{score:.6}");
                }
            }
        }
    }
}
