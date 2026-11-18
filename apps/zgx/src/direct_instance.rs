//! Connection profile for a portable ZGX build paired to one Linux engine.

use serde::Deserialize;
#[cfg(windows)]
use serde::Serialize;
#[cfg(windows)]
use serde_json::{Value as Json, json};
#[cfg(windows)]
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::OnceLock};
#[cfg(windows)]
use std::{net::TcpStream, time::Duration};
#[cfg(windows)]
use tungstenite::{Connector, Message, client::IntoClientRequest, handshake::HandshakeError};

use crate::engine::rpc::{self, DialError, Socket};

pub const ENABLED: bool = cfg!(windows) && option_env!("ZERON_ZGX_DIRECT_INSTANCE").is_some();

#[derive(Deserialize)]
struct Instance {
    address: SocketAddr,
    #[cfg(windows)]
    ca: Vec<u8>,
}

#[cfg(windows)]
#[derive(Serialize, Deserialize)]
struct Credential {
    token: String,
    instance_id: String,
    address: SocketAddr,
    ca_sha256: [u8; 32],
}

#[cfg(windows)]
impl Instance {
    fn ca_sha256(&self) -> [u8; 32] {
        Sha256::digest(&self.ca).into()
    }

    fn accepts(&self, credential: &Credential) -> bool {
        credential.address == self.address && credential.ca_sha256 == self.ca_sha256()
    }
}

fn instance() -> Result<&'static Instance, String> {
    static INSTANCE: OnceLock<Result<Instance, String>> = OnceLock::new();
    INSTANCE
        .get_or_init(|| {
            let input = option_env!("ZERON_ZGX_DIRECT_INSTANCE")
                .ok_or("This build needs an instance address")?;
            serde_json::from_str(input).map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub fn address() -> String {
    instance()
        .map(|instance| instance.address.to_string())
        .unwrap_or_default()
}

pub fn paired() -> bool {
    #[cfg(windows)]
    {
        credential().is_ok()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub async fn pair(key: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("zeron-pairing".into())
            .spawn(move || {
                let result = (|| {
                    let key = key.trim();
                    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err("Paste the one-time key from the Linux host.".into());
                    }
                    let instance = instance()?;
                    let mut socket = dial(&format!("Pair {key}")).map_err(dial_error)?;
                    let name =
                        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "ZGX on Windows".into());
                    let paired = request(&mut socket, "Pair", json!({"name": name}))?;
                    let credential = Credential {
                        token: paired["token"]
                            .as_str()
                            .ok_or("The instance returned no device token")?
                            .into(),
                        instance_id: paired["instanceId"]
                            .as_str()
                            .ok_or("The instance returned no identity")?
                            .into(),
                        address: instance.address,
                        ca_sha256: instance.ca_sha256(),
                    };
                    storage::save(&credential)
                })();
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        receiver.await.map_err(|error| error.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        Err("Direct pairing is available in the portable Windows build.".into())
    }
}

pub async fn connect() -> Result<rpc::Rpc, DialError> {
    rpc::connect_with(dial_paired).await
}

#[cfg(windows)]
fn credential() -> Result<Credential, String> {
    let credential = storage::load()?;
    // Check the saved identity before putting the token on a connection.
    if !instance()?.accepts(&credential) {
        return Err("This build needs a new pairing.".into());
    }
    Ok(credential)
}

pub fn dial_paired() -> Result<Socket, DialError> {
    #[cfg(windows)]
    {
        let credential = credential().map_err(DialError::Other)?;
        let mut socket = dial(&format!("Bearer {}", credential.token))?;
        let identity = request(&mut socket, "DirectHello", json!({})).map_err(DialError::Other)?;
        if identity["instanceId"].as_str() != Some(&credential.instance_id)
            || identity["desktop"].as_bool() != Some(true)
        {
            return Err(DialError::Other(
                "This gateway no longer matches this Zeron pairing.".into(),
            ));
        }
        Ok(socket)
    }
    #[cfg(not(windows))]
    {
        Err(DialError::Refused)
    }
}

#[cfg(windows)]
fn dial_error(error: DialError) -> String {
    match error {
        DialError::Unauthorized => {
            "The pairing key is expired or already used. Generate a new key on Linux.".into()
        }
        DialError::Refused => {
            "Could not reach Linux. Check that both computers are on the LAN.".into()
        }
        DialError::Other(error) => error,
    }
}

#[cfg(windows)]
fn dial(authorization: &str) -> Result<Socket, DialError> {
    let instance = instance().map_err(DialError::Other)?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            instance.ca.as_slice(),
        ))
        .map_err(|error| DialError::Other(format!("Invalid instance certificate: {error}")))?;
    let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| DialError::Other(error.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let timeout = Some(Duration::from_secs(8));
    let stream = TcpStream::connect_timeout(&instance.address, timeout.unwrap())
        .map_err(|_| DialError::Refused)?;
    stream
        .set_read_timeout(timeout)
        .map_err(|error| DialError::Other(error.to_string()))?;
    stream
        .set_write_timeout(timeout)
        .map_err(|error| DialError::Other(error.to_string()))?;
    let _ = stream.set_nodelay(true);
    let mut request = format!("wss://{}/rpc", instance.address)
        .into_client_request()
        .map_err(|error| DialError::Other(error.to_string()))?;
    request.headers_mut().insert(
        tungstenite::http::header::AUTHORIZATION,
        authorization.parse().map_err(|_| DialError::Unauthorized)?,
    );
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(64 * 1024 * 1024))
        .max_frame_size(Some(64 * 1024 * 1024));
    tungstenite::client_tls_with_config(
        request,
        stream,
        Some(config),
        Some(Connector::Rustls(std::sync::Arc::new(tls))),
    )
    .map(|(socket, _)| socket)
    .map_err(|error| match error {
        HandshakeError::Failure(tungstenite::Error::Http(response))
            if response.status().as_u16() == 401 =>
        {
            DialError::Unauthorized
        }
        other => DialError::Other(format!("Could not connect to the Linux gateway. {other}")),
    })
}

#[cfg(windows)]
fn request(socket: &mut Socket, method: &str, params: Json) -> Result<Json, String> {
    socket
        .send(Message::text(
            json!({"id": 1, "method": method, "params": params}).to_string(),
        ))
        .map_err(|error| error.to_string())?;
    loop {
        let message = socket.read().map_err(|error| error.to_string())?;
        if message.is_close() {
            return Err("The Linux gateway closed the connection.".into());
        }
        let Message::Text(raw) = message else {
            continue;
        };
        let value: Json = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        if value["id"].as_u64() == Some(1) {
            return value.get("ok").cloned().ok_or_else(|| {
                value["err"]["message"]
                    .as_str()
                    .unwrap_or("The gateway rejected this connection.")
                    .into()
            });
        }
    }
}

#[cfg(windows)]
mod storage {
    use super::Credential;
    use std::{fs, io, path::PathBuf, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };

    fn path() -> Result<PathBuf, String> {
        zgx_script::app_data_dir(crate::APP_ID)
            .map(|dir| dir.join("instance-pairing.dat"))
            .ok_or_else(|| "The Windows user data directory is unavailable.".into())
    }

    pub fn load() -> Result<Credential, String> {
        let encrypted = fs::read(path()?).map_err(|error| error.to_string())?;
        let plaintext = crypt(encrypted, false).map_err(|error| error.to_string())?;
        serde_json::from_slice(&plaintext).map_err(|error| error.to_string())
    }

    pub fn save(credential: &Credential) -> Result<(), String> {
        let path = path()?;
        let plaintext = serde_json::to_vec(credential).map_err(|error| error.to_string())?;
        let encrypted = crypt(plaintext, true).map_err(|error| error.to_string())?;
        fs::create_dir_all(path.parent().expect("pairing directory"))
            .map_err(|error| error.to_string())?;
        let pending = path.with_extension("pending");
        fs::write(&pending, encrypted).map_err(|error| error.to_string())?;
        fs::rename(pending, path).map_err(|error| error.to_string())
    }

    fn crypt(mut bytes: Vec<u8>, encrypt: bool) -> io::Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: bytes.len().try_into().map_err(io::Error::other)?,
            pbData: bytes.as_mut_ptr(),
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: ptr::null_mut(),
        };
        // DPAPI scopes the saved token to the Windows user.
        unsafe {
            let ok = if encrypt {
                CryptProtectData(
                    &input,
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let bytes = if output.cbData == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
            };
            LocalFree(output.pbData.cast());
            Ok(bytes)
        }
    }
}
