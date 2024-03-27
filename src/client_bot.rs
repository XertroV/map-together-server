#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::os::unix::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use rand::Rng;
    use rand::prelude::SliceRandom;
    use tokio::fs::{read, read_dir};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::mpsc::{Sender, Receiver, channel};
    use tokio::sync::{oneshot, OnceCell};
    use tokio::time;

    use crate::msgs::{PlayerCamCursor, INIT_MSG_ROOM_CREATE};
    use crate::mt_codec::MTEncode;
    use crate::{read_lp_string, read_lp_string_owh, write_lp_string, StreamErr};

    struct ClientBot {
        stream: TcpStream,
        shutdown: Option<oneshot::Receiver<()>>,
        shutdown_flag: Arc<OnceCell<bool>>,
    }

    impl ClientBot {
        async fn new(addr: String) -> Result<(Self, oneshot::Sender<()>), Box<dyn Error>> {
            let stream = TcpStream::connect(addr).await?;
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            Ok((ClientBot {
                stream,
                shutdown: Some(shutdown_rx),
                shutdown_flag: OnceCell::new().into(),
            }, shutdown_tx))
        }

        async fn init_connection(&mut self, join: bool) -> Result<(), Box<dyn Error>> {
            write_lp_string(&mut self.stream, "test_token").await.unwrap();
            if join {

            } else {
                self.stream.write_u8(INIT_MSG_ROOM_CREATE).await.unwrap();
                write_lp_string(&mut self.stream, "test_pw").await.unwrap();
                self.stream.write_u32_le(0).await.unwrap();

                let mut buf = [0u8; 3];
                self.stream.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf, b"OK_");
            }
            let room_id = read_lp_string(&mut self.stream).await.unwrap();
            assert_eq!(room_id.len(), 6);
            println!("Room ID: {}", room_id);
            Ok(())
        }

        async fn run(mut self, join: bool) -> Result<(), Box<dyn std::error::Error>> {
            let mut rng = rand::thread_rng();
            let mut shutdown = self.shutdown.take().unwrap();
            self.init_connection(join).await?;


            let mut mb_dir = read_dir("./mb-samples").await?;
            // load all files in the mb-samples directory
            let mut mbs: Vec<Vec<u8>> = vec![];
            while let Some(entry) = mb_dir.next_entry().await? {
                if entry.file_type().await?.is_file() && entry.path().extension().unwrap_or_default() == ".mb_spec" {
                    let mut buf = read(entry.path()).await?;
                    let len: u32 = u32::from_le_bytes(buf[1..5].try_into().unwrap());
                    let _ = buf.split_off(5 + len as usize);
                    mbs.push(buf);
                }
            };

            let (mut r, mut w) = self.stream.into_split();
            let shutdown_flag2 = self.shutdown_flag.clone();
            tokio::spawn(async move {
                loop {
                    let x = r.read_u8().await?;
                    let len = r.read_u32_le().await?;
                    let mut buf = vec![0u8; len as usize];
                    r.read_exact(&mut buf).await?;
                    let _id = read_lp_string_owh(&mut r).await;
                    let _ts = r.read_u64_le().await?;
                    if shutdown_flag2.initialized() {
                        break;
                    }
                }
                Ok::<(), StreamErr>(())
            });



            loop {
                tokio::select! {
                    _ = time::sleep(Duration::from_millis(rng.gen_range(100..600))) => {
                        let cam: PlayerCamCursor = rng.gen();
                        let buf = cam.encode();
                        w.write_all(&buf).await?;
                        // 25% chance to send a block
                        if rng.gen::<usize>() % 4 == 0 {
                            let mb = mbs.choose(&mut rand::thread_rng()).unwrap();
                            w.write_all(&mb).await?;
                        }
                    }
                }
                if shutdown.try_recv().is_ok() {
                    self.shutdown_flag.set(true).unwrap();
                    w.shutdown().await?;
                    break;
                }
            }

            Ok(())
        }
    }
}
