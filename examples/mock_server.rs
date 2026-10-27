use std::{convert::Infallible, net::SocketAddr, time::Duration};

use bytes::Bytes;
use clap::Parser;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:3000")]
    addr: SocketAddr,
    #[arg(long, default_value = "0")]
    delay_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let listener = TcpListener::bind(args.addr).await?;
    eprintln!("Mock server listening on http://{}", listener.local_addr()?);
    loop {
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;
        let delay = Duration::from_millis(args.delay_ms);
        tokio::spawn(async move {
            let service = service_fn(move |req: Request<Incoming>| async move {
                let mut body = req.into_body();
                while let Some(frame) = body.frame().await {
                    if frame.is_err() {
                        break;
                    }
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                    br#"{"ok":true}"#,
                ))))
            });
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
