//! Direct GSit MySQL storage using the MySQL v10 protocol over the host's WASI sockets.
use super::{FEATURES, Preference};
use crate::config::{MySqlConfig, TlsMode};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, StreamOwned,
    pki_types::{CertificateDer, ServerName, pem::PemObject},
};
use std::{
    error::Error,
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use turnloop_mysql::{Connection, Event, Outcome, RawValue, Value};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TIMEOUT: Duration = Duration::from_secs(5);

/// Applies the same operation deadline to each TCP read and write, including TLS handshake I/O.
struct Socket {
    tcp: TcpStream,
    deadline: Instant,
}

impl Socket {
    /// Returns the remaining time for the current command, including partial packets.
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "MySQL operation timed out"))
    }
}

impl Read for Socket {
    /// Bounds this read by the remaining time and returns bytes received from MySQL.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(self.remaining()?))?;
        self.tcp.read(buffer)
    }
}

impl Write for Socket {
    /// Bounds this write by the remaining time even when TLS sends multiple records.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(self.remaining()?))?;
        self.tcp.write(buffer)
    }

    /// Flushes the underlying TCP transport.
    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

/// A verified TLS stream or an explicitly configured plain MySQL connection.
enum Transport {
    Plain(Socket),
    Tls(Box<StreamOwned<ClientConnection, Socket>>),
}

trait Stream: Read + Write {}
impl Stream for Socket {}
impl Stream for StreamOwned<ClientConnection, Socket> {}

impl Transport {
    /// Sets the command deadline on the socket used by both plaintext and TLS I/O.
    fn set_deadline(&mut self, deadline: Instant) {
        match self {
            Self::Plain(tcp) => tcp.deadline = deadline,
            Self::Tls(tls) => tls.sock.deadline = deadline,
        }
    }

    /// Borrows the stream carrying MySQL frames after the current handshake stage.
    fn stream(&mut self) -> &mut dyn Stream {
        match self {
            Self::Plain(tcp) => tcp,
            Self::Tls(tls) => tls.as_mut(),
        }
    }
}

/// A fresh MySQL session. Dropping it releases all server-side statements.
struct Session<'a> {
    protocol: Connection,
    transport: Option<Transport>,
    cfg: &'a MySqlConfig,
    dir: &'a Path,
}

#[derive(Default)]
struct Reply {
    flags: Vec<bool>,
    affected_rows: Option<u64>,
    statement: Option<u32>,
}

impl<'a> Session<'a> {
    /// Connects to the configured address and performs MySQL authentication.
    /// Initialization can connect without a schema to create a missing legacy database.
    fn open(cfg: &'a MySqlConfig, dir: &'a Path, use_database: bool) -> Result<Self> {
        let mut last_error = None;
        let deadline = Instant::now() + TIMEOUT;
        for addr in (cfg.host.as_str(), cfg.port).to_socket_addrs()? {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            // WASI can report a completed nonblocking connect with a pending socket error.
            let tcp = match TcpStream::connect_timeout(&addr, remaining).and_then(|tcp| {
                match tcp.take_error()? {
                    Some(error) => Err(error),
                    None => Ok(tcp),
                }
            }) {
                Ok(tcp) => tcp,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            let protocol = Connection::new(turnloop_mysql::Config {
                user: cfg.user.clone(),
                password: cfg.password.as_bytes().to_vec(),
                database: use_database.then(|| cfg.database.clone()),
                tls: matches!(cfg.tls, TlsMode::VerifyIdentity),
                max_buffer: 1024 * 1024,
                ..Default::default()
            })?;
            let mut session = Self {
                protocol,
                transport: Some(Transport::Plain(Socket { tcp, deadline })),
                cfg,
                dir,
            };
            session.receive()?;
            return Ok(session);
        }
        Err(last_error.map_or_else(|| "no reachable MySQL address".into(), Into::into))
    }

    /// Sends pending protocol output, acknowledging bytes only after a successful write.
    fn flush(&mut self) -> Result<()> {
        let output = self.protocol.output();
        if !output.is_empty() {
            let stream = self.transport.as_mut().expect("session transport").stream();
            stream.write_all(output)?;
            stream.flush()?;
            self.protocol.consume_output(output.len())?;
        }
        Ok(())
    }

    /// Switches to verified TLS before the protocol can send the authentication response.
    fn upgrade_tls(&mut self) -> Result<()> {
        self.flush()?;
        let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(ca) = &self.cfg.ca_file {
            let certificates = std::fs::read(self.dir.join(ca))?;
            let mut count = 0;
            for cert in CertificateDer::pem_slice_iter(&certificates) {
                roots.add(cert?)?;
                count += 1;
            }
            if count == 0 {
                return Err("the configured CA file contains no PEM certificates".into());
            }
        }
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name = ServerName::try_from(self.cfg.host.clone())?;
        let mut tls = ClientConnection::new(Arc::new(config), name)?;
        let Some(Transport::Plain(mut tcp)) = self.transport.take() else {
            return Err("unexpected repeated MySQL TLS negotiation".into());
        };
        while tls.is_handshaking() {
            tls.complete_io(&mut tcp)?;
        }
        self.transport = Some(Transport::Tls(Box::new(StreamOwned::new(tls, tcp))));
        self.protocol.tls_established()?;
        Ok(())
    }

    /// Drives the protocol through authentication or one command; SELECT statements return schema or preference flags.
    fn receive(&mut self) -> Result<Reply> {
        let mut reply = Reply::default();
        let deadline = Instant::now() + TIMEOUT;
        self.transport
            .as_mut()
            .expect("session transport")
            .set_deadline(deadline);
        let mut buffer = [0; 4096];
        loop {
            if Instant::now() >= deadline {
                return Err("MySQL operation timed out".into());
            }
            self.flush()?;
            match self.protocol.next_event()? {
                Some(
                    Event::Connected { .. }
                    | Event::Completed {
                        outcome: Outcome::Success,
                        ..
                    },
                ) => return Ok(reply),
                Some(Event::Error { error, .. }) => {
                    return Err(format!(
                        "server error {} ({})",
                        error.errno,
                        error.code().unwrap_or("unknown MySQL error")
                    )
                    .into());
                }
                Some(Event::Closed { reason }) => return Err(reason.into()),
                Some(Event::Completed { .. }) => {
                    return Err("MySQL command did not complete".into());
                }
                Some(Event::UpgradeTls) => self.upgrade_tls()?,
                Some(Event::RsaSeedNeeded) => {
                    let mut seed = [0; 20];
                    rustls::crypto::aws_lc_rs::default_provider()
                        .secure_random
                        .fill(&mut seed)
                        .map_err(|_| "secure randomness unavailable for MySQL authentication")?;
                    self.protocol.rsa_seed(seed)?;
                }
                Some(Event::Prepared { statement, .. }) => reply.statement = Some(statement.id),
                Some(Event::Ok { packet, .. }) => {
                    reply.affected_rows = Some(packet.affected_rows())
                }
                Some(Event::Row { row, .. }) => {
                    for column in row {
                        reply.flags.push(match column? {
                            RawValue::Bytes(b"1")
                            | RawValue::Scalar(Value::Int(1) | Value::UInt(1)) => true,
                            RawValue::Bytes(b"0")
                            | RawValue::Scalar(Value::Int(0) | Value::UInt(0)) => false,
                            _ => return Err("expected a MySQL boolean result".into()),
                        });
                    }
                }
                None => {
                    let stream = self.transport.as_mut().expect("session transport").stream();
                    let n = stream.read(&mut buffer)?;
                    if n == 0 {
                        return Err("MySQL server closed the connection".into());
                    }
                    self.protocol.receive(&buffer[..n])?;
                }
                Some(_) => {}
            }
        }
    }

    /// Executes SQL generated from fixed table names or a quoted schema identifier.
    fn query(&mut self, sql: &str) -> Result<Reply> {
        self.protocol.query(0, sql, None)?;
        self.receive()
    }

    /// Binds text parameters without interpolating UUIDs, table names or credentials as values.
    fn execute<const N: usize>(&mut self, sql: &str, parameters: [&str; N]) -> Result<Reply> {
        self.protocol.prepare(0, sql, None)?;
        let statement = self
            .receive()?
            .statement
            .ok_or("MySQL did not prepare the statement")?;
        self.protocol.execute(
            0,
            statement,
            &parameters.map(|value| Value::Bytes(value.as_bytes().to_vec())),
            None,
        )?;
        self.receive()
    }
}

impl Drop for Session<'_> {
    /// Ends completed sessions with COM_QUIT so each preference read is a clean disconnect.
    fn drop(&mut self) {
        if self.protocol.is_ready() && self.protocol.quit().is_ok() {
            let _ = self.flush();
        }
    }
}

/// Initializes the upstream schema. All existing tables are checked before issuing MySQL's
/// implicitly committed DDL; missing tables can be created again safely after an interruption.
pub(super) fn initialize(cfg: &MySqlConfig, data_dir: &Path) -> std::result::Result<(), String> {
    let initialize = || -> Result<()> {
        let mut session = Session::open(cfg, data_dir, false)?;
        let schema = format!("`{}`", cfg.database.replace('`', "``"));
        let exists = session.execute(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = ?)",
            [&cfg.database],
        )?;
        if exists.flags == [false] {
            session.query(&format!("CREATE DATABASE IF NOT EXISTS {schema}"))?;
        }
        session.query(&format!("USE {schema}"))?;
        let mut missing = Vec::new();
        for feature in FEATURES {
            let table = feature.table();
            let layout = session.execute(
                "SELECT t.table_type = 'BASE TABLE' AND
                 (SELECT COUNT(*) FROM information_schema.columns c WHERE
                  c.table_schema = t.table_schema AND c.table_name = t.table_name) = 1 AND
                 EXISTS(SELECT 1 FROM information_schema.columns c WHERE
                  c.table_schema = t.table_schema AND c.table_name = t.table_name AND
                  c.column_name = 'uuid' AND c.data_type = 'text' AND c.generation_expression = '')
                 FROM information_schema.tables t WHERE t.table_schema = DATABASE() AND t.table_name = ?",
                [table],
            )?;
            match layout.flags.as_slice() {
                [] => missing.push(table),
                [true] => {}
                _ => {
                    return Err(format!("{table}: expected one writable uuid TEXT column").into());
                }
            }
        }
        for table in missing {
            session.query(&format!("CREATE TABLE IF NOT EXISTS {table} (uuid TEXT)"))?;
        }
        Ok(())
    };
    initialize().map_err(|error| format!("MySQL: {error}"))
}

/// Fetches one consistent snapshot with one authentication and one prepared SELECT.
pub(super) fn read<const N: usize>(
    cfg: &MySqlConfig,
    dir: &Path,
    sql: &str,
    key: &str,
) -> std::result::Result<[bool; N], String> {
    let read = || -> Result<[bool; N]> {
        Session::open(cfg, dir, true)?
            .execute(sql, [key; N])?
            .flags
            .try_into()
            .map_err(|_| "unexpected number of MySQL preference flags".into())
    };
    read().map_err(|error| format!("MySQL preferences: {error}"))
}

/// Applies the original INSERT or DELETE on one session. Closing an uncommitted InnoDB
/// transaction rolls it back if the second half of an implicit toggle fails.
pub(super) fn change(
    cfg: &MySqlConfig,
    dir: &Path,
    feature: Preference,
    key: &str,
    exception: Option<bool>,
) -> std::result::Result<bool, String> {
    let write = || -> Result<bool> {
        let mut session = Session::open(cfg, dir, true)?;
        if let Some(exception) = exception {
            session.execute(&feature.write_sql(exception), [key])?;
            return Ok(exception);
        }
        session.query("START TRANSACTION")?;
        let removed = session
            .execute(&feature.write_sql(false), [key])?
            .affected_rows
            .ok_or("missing MySQL delete count")?;
        if removed == 0 {
            session.execute(&feature.write_sql(true), [key])?;
        }
        session.query("COMMIT")?;
        Ok(removed == 0)
    };
    write().map_err(|error| format!("MySQL {}: {error}", feature.table()))
}
