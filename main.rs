use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
};

fn handle(mut stream: TcpStream) {
    let mut buffer = [0_u8; 2048];
    let size = stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..size]);
    let path = request.split_whitespace().nth(1).unwrap_or("/");

    let (file, content_type) = match path {
        "/" | "/index.html" => ("static/index.html", "text/html; charset=utf-8"),
        "/game.js" => ("static/game.js", "application/javascript; charset=utf-8"),
        _ => ("", "text/plain; charset=utf-8"),
    };

    let (status, body) = if file.is_empty() {
        ("404 Not Found", b"Not Found".to_vec())
    } else {
        match fs::read(file) {
            Ok(body) => ("200 OK", body),
            Err(error) => (
                "500 Internal Server Error",
                format!("Unable to read {file}: {error}").into_bytes(),
            ),
        }
    };

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

fn main() -> std::io::Result<()> {
    let address = "127.0.0.1:8080";
    let listener = TcpListener::bind(address)?;
    println!("BREACH PROTOCOL 已启动：http://{address}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || handle(stream));
            }
            Err(error) => eprintln!("连接错误：{error}"),
        }
    }
    Ok(())
}
