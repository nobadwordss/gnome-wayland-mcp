use std::{cell::Cell, future::Future, io, os::fd::BorrowedFd, path::Path};

use tokio::net::{UnixStream, unix::OwnedWriteHalf};
use zbus::{
    Connection,
    connection::socket::{BoxedSplit, WriteHalf},
};

use super::DBUS_TIMEOUT;

tokio::task_local! { static WRITE_STARTED: Cell<bool>; }

pub(super) async fn connect(path: &Path) -> zbus::Result<Connection> {
    let (read, write) = UnixStream::connect(path).await?.into_split();
    let socket = BoxedSplit::new(Box::new(read), Box::new(InputWriter(write)));
    zbus::connection::Builder::socket(socket)
        .method_timeout(DBUS_TIMEOUT)
        .build()
        .await
}

pub(super) async fn send(
    interrupted: impl Future<Output = ()>,
    call: impl Future<Output = zbus::Result<()>>,
) -> (bool, zbus::Result<()>) {
    WRITE_STARTED
        .scope(Cell::new(false), async {
            tokio::pin!(call);
            let reply = tokio::select! {
                biased;
                _ = interrupted => {
                    if !WRITE_STARTED.with(Cell::get) {
                        return (false, Err(zbus::Error::Failure("pointer input interrupted before transmission".into())));
                    }
                    // A partial message must be finished on the shared D-Bus socket.
                    call.await
                }
                reply = &mut call => reply,
            };
            (WRITE_STARTED.with(Cell::get), reply)
        })
        .await
}

#[derive(Debug)]
struct InputWriter(OwnedWriteHalf);

#[async_trait::async_trait]
impl WriteHalf for InputWriter {
    async fn sendmsg(&mut self, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
        // Entering a zbus call can still leave it waiting for the writer lock or
        // a writable socket. Only a successful sendmsg commits the input.
        let count = self.0.sendmsg(bytes, fds).await?;
        if count > 0 {
            let _ = WRITE_STARTED.try_with(|started| started.set(true));
        }
        Ok(count)
    }

    async fn close(&mut self) -> io::Result<()> {
        self.0.close().await
    }

    fn can_pass_unix_fd(&self) -> bool {
        self.0.can_pass_unix_fd()
    }

    async fn peer_credentials(&mut self) -> io::Result<zbus::fdo::ConnectionCredentials> {
        self.0.peer_credentials().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn disconnect_while_the_first_socket_write_is_blocked_sends_no_press() -> io::Result<()> {
        let (socket, mut peer) = UnixStream::pair()?;
        socket.writable().await?;
        let filler = [0u8; 4096];
        let mut filled = 0;
        loop {
            match socket.try_write(&filler) {
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        let mut writer = InputWriter(socket.into_split().1);
        let interrupted = CancellationToken::new();
        let press = send(interrupted.cancelled(), async {
            writer.sendmsg(b"press", &[]).await?;
            Ok(())
        });
        tokio::pin!(press);
        assert!(futures_util::poll!(&mut press).is_pending());
        interrupted.cancel();
        let (started, reply) = press.await;
        assert!(!started && reply.is_err());
        let mut received = vec![0; filled];
        peer.read_exact(&mut received).await?;
        assert!(received.iter().all(|&byte| byte == 0));
        assert_eq!(
            peer.try_read(&mut [0u8]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_after_the_first_write_allows_the_message_to_finish() -> zbus::Result<()> {
        let (socket, mut peer) = UnixStream::pair()?;
        let mut writer = InputWriter(socket.into_split().1);
        let interrupted = CancellationToken::new();
        let (started, reply) = send(interrupted.cancelled(), async {
            assert_eq!(writer.sendmsg(b"a", &[]).await?, 1);
            interrupted.cancel();
            tokio::task::yield_now().await;
            assert_eq!(writer.sendmsg(b"b", &[]).await?, 1);
            Ok(())
        })
        .await;
        assert!(started);
        reply?;
        let mut received = [0; 2];
        peer.read_exact(&mut received).await?;
        assert_eq!(&received, b"ab");
        Ok(())
    }
}
