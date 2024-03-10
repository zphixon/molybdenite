use anyhow::Result;

fn main() -> Result<()> {
    let (mut ws, _) = tungstenite::connect("ws://192.168.1.196:4443/").unwrap();
    ws.close(None).unwrap();
    Ok(())
}
