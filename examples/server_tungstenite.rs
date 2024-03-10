use std::net::TcpListener;
use std::thread::spawn;
use tungstenite::accept;
use tungstenite::protocol::frame::{Frame, FrameHeader};

/// A WebSocket echo server
fn main() {
    let server = TcpListener::bind("192.168.1.196:4443").unwrap();
    for stream in server.incoming() {
        spawn(move || {
            println!("got client");
            let mut websocket = accept(stream.unwrap()).unwrap();
            println!("accepted handshake");
            loop {
                websocket
                    .write(tungstenite::Message::Frame(Frame::from_payload(
                        FrameHeader {
                            is_final: false,
                            opcode: tungstenite::protocol::frame::coding::OpCode::Data(
                                tungstenite::protocol::frame::coding::Data::Text,
                            ),
                            mask: None,
                            ..Default::default()
                        },
                        String::from("hello").into_bytes(),
                    )))
                    .unwrap();
                websocket
                    .write(tungstenite::Message::Frame(Frame::from_payload(
                        FrameHeader {
                            is_final: true,
                            opcode: tungstenite::protocol::frame::coding::OpCode::Data(
                                tungstenite::protocol::frame::coding::Data::Continue,
                            ),
                            mask: None,
                            ..Default::default()
                        },
                        String::from("world").into_bytes(),
                    )))
                    .unwrap();
                websocket.flush().unwrap();
                println!("sent hello");

                let msg = websocket.read().unwrap();

                // We do not want to send back ping/pong messages.
                if msg.is_binary() || msg.is_text() {
                    websocket.send(msg).unwrap();
                }
            }
        });
    }
}
