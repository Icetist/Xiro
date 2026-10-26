//! Real SChannel handshake against `tls_acceptor`, with a client that trusts only the
//! local CA (no built-in roots, no "accept invalid" shortcuts).

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use xiro_net::{CertBundle, ensure_certificates, tls_acceptor};

const PING: &[u8] = b"ping from the iPad";
const PONG: &[u8] = b"pong from the PC";

fn connector(bundle: &CertBundle) -> tokio_native_tls::TlsConnector {
    let root = native_tls::Certificate::from_der(&bundle.ca_der).expect("CA DER");
    let connector = native_tls::TlsConnector::builder()
        .add_root_certificate(root)
        .disable_built_in_roots(true)
        .build()
        .expect("connector");
    tokio_native_tls::TlsConnector::from(connector)
}

async fn serve_one(listener: &TcpListener, acceptor: &tokio_native_tls::TlsAcceptor) -> Result<(), String> {
    let (tcp, _) = listener.accept().await.map_err(|e| format!("accept: {e}"))?;
    let mut tls = acceptor.accept(tcp).await.map_err(|e| format!("server handshake: {e}"))?;
    let mut buf = vec![0u8; PING.len()];
    tls.read_exact(&mut buf).await.map_err(|e| format!("server read: {e}"))?;
    if buf != PING {
        return Err("unexpected request bytes".to_owned());
    }
    tls.write_all(PONG).await.map_err(|e| format!("server write: {e}"))?;
    tls.shutdown().await.map_err(|e| format!("server shutdown: {e}"))
}

async fn serve(listener: TcpListener, acceptor: tokio_native_tls::TlsAcceptor, connections: usize) -> Vec<Result<(), String>> {
    let mut outcomes = Vec::with_capacity(connections);
    for _ in 0..connections {
        outcomes.push(serve_one(&listener, &acceptor).await);
    }
    outcomes
}

async fn exchange(bundle: &CertBundle, addr: std::net::SocketAddr, domain: &str) -> Result<Vec<u8>, String> {
    let tcp = TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
    let mut tls = connector(bundle).connect(domain, tcp).await.map_err(|e| format!("client handshake: {e}"))?;
    tls.write_all(PING).await.map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    tls.read_to_end(&mut reply).await.map_err(|e| e.to_string())?;
    Ok(reply)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_with_only_the_generated_ca_trusted() {
    let temp = tempfile::tempdir().expect("temp dir");
    let sans: Vec<String> = ["localhost", "127.0.0.1", "mypc.local"].iter().map(|s| (*s).to_owned()).collect();
    let bundle = ensure_certificates(temp.path(), &sans).expect("certificates");
    assert!(bundle.ca_created && bundle.renewed);
    let acceptor = tls_acceptor(&bundle).expect("acceptor");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(serve(listener, acceptor, 3));

    let localhost = exchange(&bundle, addr, "localhost").await;
    let mdns = exchange(&bundle, addr, "mypc.local").await;
    // A name the certificate does not cover must be refused: verification is real.
    let wrong = exchange(&bundle, addr, "other.local").await;
    let outcomes = server.await.expect("server task");

    assert_eq!(localhost.as_deref(), Ok(PONG), "localhost; server saw {outcomes:?}");
    assert_eq!(mdns.as_deref(), Ok(PONG), "mypc.local; server saw {outcomes:?}");
    assert!(wrong.is_err(), "hostname mismatch must fail, got {wrong:?}");
    assert!(outcomes[0].is_ok() && outcomes[1].is_ok() && outcomes[2].is_err(), "{outcomes:?}");
}
