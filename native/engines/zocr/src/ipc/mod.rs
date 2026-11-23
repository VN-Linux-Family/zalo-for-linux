mod handshake;
pub mod protocol;
pub mod schema;
mod unix_socket;
use crate::{config::Config, engine::Engine, image};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use unix_socket::SocketServer;
pub type Output = Arc<Mutex<io::Stdout>>;
pub fn reply(writer: &mut impl Write, _kind: u8, value: &Value) -> Result<()> {
    reply_generation(writer, 0, value)
}
fn reply_generation(writer: &mut impl Write, generation: u64, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let mut payload = generation.to_le_bytes().to_vec();
    payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&bytes);
    protocol::write_reply(writer, protocol::OK, &payload)?;
    Ok(())
}
pub fn event(output: &Output, kind: u8, pid: u32, detail: &str) -> Result<()> {
    let mut payload = vec![kind];
    payload.extend_from_slice(&pid.to_le_bytes());
    payload.extend_from_slice(&protocol::string_payload(detail));
    let mut writer = output
        .lock()
        .map_err(|_| anyhow::anyhow!("stdout mutex poisoned"))?;
    protocol::write_reply(&mut *writer, protocol::PIPE_EVENT, &payload)?;
    Ok(())
}
struct Stdin {
    stop: Arc<AtomicBool>,
}
impl Read for Stdin {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "shutdown"));
            }
            let stdin_fd = rustix::stdio::stdin();
            let mut fds = [rustix::event::PollFd::new(
                &stdin_fd,
                rustix::event::PollFlags::IN,
            )];
            let timeout = rustix::time::Timespec {
                tv_sec: 0,
                tv_nsec: 50_000_000,
            };
            match rustix::event::poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Ok(_) => {}
                Err(e) => return Err(io::Error::from_raw_os_error(e.raw_os_error())),
            }
            return match rustix::io::read(stdin_fd, &mut *buffer) {
                Ok(n) => Ok(n),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => Err(io::Error::from_raw_os_error(e.raw_os_error())),
            };
        }
    }
}
pub fn run(
    engine: &Arc<Mutex<Engine>>,
    cfg: &Config,
    stop: &Arc<AtomicBool>,
    socket_directory: &Path,
) -> Result<()> {
    let mut stdin = Stdin {
        stop: Arc::clone(stop),
    };
    if !handshake::verify_control(&mut stdin, cfg.auth_token.as_bytes())? {
        bail!("handshake token mismatch");
    }
    eprintln!("OCR engine ready");
    let output = Arc::new(Mutex::new(io::stdout()));
    let mut server: Option<SocketServer> = None;
    while !stop.load(Ordering::Relaxed) {
        let frame = match protocol::read_envelope(&mut stdin) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_) if stop.load(Ordering::Relaxed) => break,
            Err(error) => return Err(error.into()),
        };
        match frame.kind {
            protocol::PIPE_OPEN => {
                let result = (|| -> Result<Vec<u8>> {
                    let token = protocol::pipe_token(&frame.payload)?.to_owned();
                    if server.is_some() {
                        bail!("pipe is already open");
                    }
                    let socket = SocketServer::start(
                        Arc::clone(engine),
                        (*cfg).clone(),
                        Arc::clone(stop),
                        socket_directory,
                        token,
                        output.clone(),
                    )?;
                    let mut payload = protocol::string_payload(&socket.path.to_string_lossy());
                    payload.extend_from_slice(&std::process::id().to_le_bytes());
                    server = Some(socket);
                    Ok(payload)
                })();
                let mut writer = output
                    .lock()
                    .map_err(|_| anyhow::anyhow!("stdout mutex poisoned"))?;
                match result {
                    Ok(payload) => {
                        protocol::write_reply(&mut *writer, protocol::PIPE_READY, &payload)?;
                    }
                    Err(error) => protocol::write_reply(
                        &mut *writer,
                        protocol::PIPE_ERR,
                        &protocol::string_payload(&format!("{error:#}")),
                    )?,
                }
            }
            protocol::PIPE_CLOSE => {
                if !frame.payload.is_empty() {
                    bail!("PIPE_CLOSE payload must be empty");
                }
                server = None;
                event(&output, 4, 0, "")?;
            }
            protocol::PATH_REQUEST => {
                let value = (|| -> Result<Value> {
                    let path = PathBuf::from(protocol::image_path(&frame.payload)?);
                    let (image, timings) = image::read_profiled(&path)?;
                    let mut value = engine
                        .lock()
                        .map_err(|_| anyhow::anyhow!("engine mutex poisoned"))?
                        .recognize(&image)?;
                    input_profile(&mut value, cfg.benchmark_profile, &timings);
                    Ok(value)
                })()
                .unwrap_or_else(|error| json!({"ok":false,"error":format!("{error:#}")}));
                let mut writer = output
                    .lock()
                    .map_err(|_| anyhow::anyhow!("stdout mutex poisoned"))?;
                reply(&mut *writer, protocol::OK, &value)?;
            }
            _ => bail!("unsupported command type {}", frame.kind),
        }
    }
    drop(server);
    Ok(())
}

pub(crate) fn input_profile(value: &mut Value, enabled: bool, timings: &image::InputTimings) {
    if !enabled {
        return;
    }
    let stages = &mut value["result"]["meta"]["stages"];
    stages["readMicros"] = json!(timings.read_micros);
    stages["imageDecodeMicros"] = json!(timings.decode_micros);
    stages["colorConvertMicros"] = json!(timings.color_micros);
}

#[cfg(test)]
mod tests {
    #[test]
    fn pipe_read_does_not_over_read() {
        let (reader, writer) = rustix::pipe::pipe().unwrap();
        rustix::io::write(&writer, &b"abcd"[..]).unwrap();
        let mut one = [0u8; 1];
        assert_eq!(rustix::io::read(&reader, &mut one).unwrap(), 1);
        assert_eq!(one[0], b'a');
        let mut rest = [0u8; 8];
        assert_eq!(rustix::io::read(&reader, &mut rest).unwrap(), 3);
        assert_eq!(&rest[..3], b"bcd");
    }
}
