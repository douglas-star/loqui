use crate::waiter::ResponseWaiter;
use crate::Config;
use bytesize::ByteSize;
use failure::{err_msg, Error};
use loqui_connection::find_encoding;
use loqui_connection::handler::{DelegatedFrame, Handler, Ready};
use loqui_connection::{IdSequence, LoquiError, ReaderWriter};
use loqui_protocol::frames::{
    Error as ErrorFrame, Frame, Hello, HelloAck, LoquiFrame, Push, Request, Response,
};
use loqui_protocol::upgrade::{Codec, UpgradeFrame};
use loqui_protocol::VERSION;
use std::collections::HashMap;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::prelude::*;
use tokio_codec::Framed;
use tokio_futures::compat::forward::IntoAwaitable;
use tokio_futures::stream::StreamExt;

pub enum InternalEvent {
    Request {
        payload: Vec<u8>,
        waiter: ResponseWaiter,
    },
    Push {
        payload: Vec<u8>,
    },
}

pub struct ConnectionHandler {
    waiters: HashMap<u32, ResponseWaiter>,
    config: Config,
}

impl ConnectionHandler {
    pub fn new(config: Config) -> Self {
        Self {
            waiters: HashMap::new(),
            config,
        }
    }
}

impl Handler for ConnectionHandler {
    type InternalEvent = InternalEvent;
    existential type UpgradeFuture: Send + Future<Output = Result<TcpStream, Error>>;
    existential type HandshakeFuture: Send
        + Future<
            Output = Result<(Ready, ReaderWriter), (Error, Option<ReaderWriter>)>,
        >;
    existential type HandleFrameFuture: Send + Future<Output = Result<Response, (Error, u32)>>;

    const SEND_GO_AWAY: bool = false;

    fn max_payload_size(&self) -> ByteSize {
        self.config.max_payload_size
    }

    fn upgrade(&self, tcp_stream: TcpStream) -> Self::UpgradeFuture {
        let max_payload_size = self.max_payload_size();
        async move {
            let framed_socket = Framed::new(tcp_stream, Codec::new(max_payload_size));
            let (mut writer, mut reader) = framed_socket.split();
            writer = match writer.send(UpgradeFrame::Request).into_awaitable().await {
                Ok(writer) => writer,
                Err(_e) => return Err(LoquiError::TcpStreamClosed.into()),
            };
            match reader.next().await {
                Some(Ok(UpgradeFrame::Response)) => Ok(writer.reunite(reader)?.into_inner()),
                Some(Ok(frame)) => Err(LoquiError::InvalidUpgradeFrame { frame }.into()),
                Some(Err(e)) => Err(e),
                None => Err(LoquiError::TcpStreamClosed.into()),
            }
        }
    }

    fn handshake(&mut self, mut reader_writer: ReaderWriter) -> Self::HandshakeFuture {
        let hello = self.make_hello();
        let supported_encodings = self.config.supported_encodings;
        async move {
            reader_writer = match reader_writer.write(hello).await {
                Ok(read_writer) => read_writer,
                Err(e) => return Err((e.into(), None)),
            };

            match reader_writer.reader.next().await {
                Some(Ok(frame)) => match Self::handle_handshake_frame(frame, supported_encodings) {
                    Ok(ready) => Ok((ready, reader_writer)),
                    Err(e) => Err((e, Some(reader_writer))),
                },
                Some(Err(e)) => Err((e, Some(reader_writer))),
                None => Err((LoquiError::TcpStreamClosed.into(), Some(reader_writer))),
            }
        }
    }

    fn handle_frame(
        &mut self,
        frame: DelegatedFrame,
        _encoding: &'static str,
    ) -> Option<Self::HandleFrameFuture> {
        match frame {
            DelegatedFrame::Response(response) => {
                self.handle_response(response);
                None
            }
            DelegatedFrame::Error(error) => {
                self.handle_error(error);
                None
            }
            DelegatedFrame::Push(_) | DelegatedFrame::Request(_) => Some(async move {
                Err((
                    LoquiError::InvalidOpcode {
                        actual: Request::OPCODE,
                        expected: None,
                    }
                    .into(),
                    0,
                ))
            }),
        }
    }

    fn handle_internal_event(
        &mut self,
        event: InternalEvent,
        id_sequence: &mut IdSequence,
    ) -> Option<LoquiFrame> {
        // Forward Request and Push events to the connection so it can send them to the server.
        match event {
            InternalEvent::Request { payload, waiter } => {
                let sequence_id = id_sequence.next();
                self.send_request(payload, sequence_id, waiter)
            }
            InternalEvent::Push { payload } => self.send_push(payload),
        }
    }

    fn on_ping_received(&mut self) {
        // Use to sweep dead waiters.
        let now = Instant::now();
        self.waiters
            .retain(|_sequence_id, waiter| waiter.deadline > now);
    }
}

impl ConnectionHandler {
    fn send_push(&mut self, payload: Vec<u8>) -> Option<LoquiFrame> {
        let push = Push { payload, flags: 0 };
        Some(push.into())
    }

    fn send_request(
        &mut self,
        payload: Vec<u8>,
        sequence_id: u32,
        waiter: ResponseWaiter,
    ) -> Option<LoquiFrame> {
        if waiter.deadline <= Instant::now() {
            waiter.notify(Err(LoquiError::RequestTimeout.into()));
            return None;
        }

        // Store the waiter so we can notify it when we get a response.
        self.waiters.insert(sequence_id, waiter);
        let request = Request {
            payload,
            sequence_id,
            flags: 0,
        };
        Some(request.into())
    }

    fn handle_response(&mut self, response: Response) {
        let Response {
            flags: _flags,
            sequence_id,
            payload,
        } = response;
        match self.waiters.remove(&sequence_id) {
            Some(waiter) => {
                waiter.notify(Ok(payload));
            }
            None => {
                debug!("No waiter for sequence_id. sequence_id={:?}", sequence_id);
            }
        }
    }

    fn handle_error(&mut self, error: ErrorFrame) {
        let ErrorFrame {
            sequence_id,
            payload,
            ..
        } = error;
        match self.waiters.remove(&sequence_id) {
            Some(waiter) => {
                // payload is always a string
                let result = String::from_utf8(payload)
                    .map_err(Error::from)
                    .and_then(|reason| Err(err_msg(reason)));
                waiter.notify(result);
            }
            None => {
                debug!("No waiter for sequence_id. sequence_id={:?}", sequence_id);
            }
        }
    }

    fn make_hello(&self) -> Hello {
        Hello {
            flags: 0,
            version: VERSION,
            encodings: self
                .config
                .supported_encodings
                .to_owned()
                .into_iter()
                .map(String::from)
                .collect(),
            // compression not supported
            compressions: vec![],
        }
    }

    fn handle_handshake_frame(
        frame: LoquiFrame,
        supported_encodings: &'static [&'static str],
    ) -> Result<Ready, Error> {
        match frame {
            LoquiFrame::HelloAck(hello_ack) => {
                Self::handle_handshake_hello_ack(hello_ack, supported_encodings)
            }
            LoquiFrame::GoAway(go_away) => Err(LoquiError::ToldToGoAway { go_away }.into()),
            frame => Err(LoquiError::InvalidOpcode {
                actual: frame.opcode(),
                expected: Some(HelloAck::OPCODE),
            }
            .into()),
        }
    }

    fn handle_handshake_hello_ack(
        hello_ack: HelloAck,
        supported_encodings: &'static [&'static str],
    ) -> Result<Ready, Error> {
        // Validate the settings and convert them to &'static str.
        let encoding = match find_encoding(hello_ack.encoding, supported_encodings) {
            Some(encoding) => encoding,
            None => return Err(LoquiError::InvalidEncoding.into()),
        };

        // compression not supported
        if hello_ack.compression.is_some() {
            return Err(LoquiError::InvalidCompression.into());
        };
        let ping_interval = Duration::from_millis(u64::from(hello_ack.ping_interval_ms));
        Ok(Ready {
            ping_interval,
            encoding,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::future_utils::block_on_all;

    const ENCODING: &str = "identity";

    fn make_handler() -> ConnectionHandler {
        let config = Config {
            max_payload_size: ByteSize::b(5000),
            request_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(10),
            supported_encodings: &[ENCODING],
        };

        ConnectionHandler::new(config)
    }

    #[test]
    fn it_handles_request_response() {
        let mut handler = make_handler();
        let mut id_sequence = IdSequence::default();
        let (waiter, awaitable) = ResponseWaiter::new(Duration::from_secs(5));
        let payload = b"hello".to_vec();
        let request = handler
            .handle_internal_event(
                InternalEvent::Request {
                    payload: payload.clone(),
                    waiter,
                },
                &mut id_sequence,
            )
            .expect("no request");
        match request {
            LoquiFrame::Request(request) => {
                let response = Response {
                    sequence_id: request.sequence_id,
                    flags: 0,
                    payload: payload.clone(),
                };
                let frame = handler.handle_frame(response.into(), ENCODING);
                assert!(frame.is_none())
            }
            _other => panic!("request not returned"),
        }
        let result = block_on_all(async { awaitable.await }).unwrap();
        assert_eq!(result, payload)
    }

    #[test]
    fn it_handles_request_response_diff_sequence_id() {
        let mut handler = make_handler();
        let mut id_sequence = IdSequence::default();
        let (waiter, awaitable) = ResponseWaiter::new(Duration::from_secs(1));
        let _request = handler
            .handle_internal_event(
                InternalEvent::Request {
                    payload: vec![],
                    waiter,
                },
                &mut id_sequence,
            )
            .expect("no request");
        let response = Response {
            sequence_id: id_sequence.next(),
            flags: 0,
            payload: vec![],
        };
        let _frame = handler.handle_frame(response.into(), ENCODING);
        let result = block_on_all(async { awaitable.await });
        assert!(result.is_err())
    }

}
