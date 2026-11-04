//! A local HTTP canary on 127.0.0.1: any request that reaches it is a remote load that leaked.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

pub struct Canary {
    pub port: u16,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Canary {
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let log = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || serve(stream, &log));
            }
        });
        Ok(Self { port, hits })
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    pub fn hits(&self, prefix: &str) -> Vec<String> {
        let wanted = format!("/{prefix}");
        self.hits
            .lock()
            .map(|hits| {
                hits.iter()
                    .filter(|path| path.starts_with(&wanted))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn total(&self) -> usize {
        self.hits.lock().map(|hits| hits.len()).unwrap_or(0)
    }
}

fn serve(mut stream: TcpStream, log: &Mutex<Vec<String>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buffer = [0u8; 4096];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let head = String::from_utf8_lossy(&buffer[..read]);
    let path = head.split_whitespace().nth(1).unwrap_or("?").to_string();
    if let Ok(mut hits) = log.lock() {
        hits.push(path);
    }
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            GIF.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(GIF);
}

/// A page that tries every passive way to load from the canary, plus markup scripts.
pub fn hostile_page(canary: &Canary, prefix: &str) -> String {
    let url = |path: &str| canary.url(&format!("{prefix}/{path}"));
    format!(
        r#"<!DOCTYPE html><html><head><title>canary</title>
<link rel="stylesheet" href="{stylesheet}">
<link rel="preconnect" href="{preconnect}">
<link rel="prefetch" href="{prefetch}">
<link rel="icon" href="{icon}">
<style>@import url("{import}"); body {{ background: url("{background}"); }}
@font-face {{ font-family: canary; src: url("{font}"); }} p {{ font-family: canary; }}</style>
<meta http-equiv="refresh" content="0; url={refresh}">
<script src="{script_src}"></script>
<script>document.title = "script-ran"; new Image().src = "{inline_script}";</script>
</head><body>
<p>canary page</p>
<img src="{img}" alt="">
<img srcset="{srcset} 1x" alt="">
<picture><source srcset="{picture}"><img src="data:," alt=""></picture>
<div style="background-image:url({inline_style})">styled</div>
<iframe src="{iframe}"></iframe>
<object data="{object}"></object>
<embed src="{embed}">
<video poster="{poster}" src="{video}"></video>
<audio src="{audio}" autoplay></audio>
<svg width="10" height="10"><image href="{svg}" width="10" height="10"/></svg>
<input type="image" src="{input}" alt="">
<table background="{table}"><tr><td>cell</td></tr></table>
<img src="x" onerror="new Image().src='{onerror}'" alt="">
</body></html>"#,
        stylesheet = url("stylesheet.css"),
        preconnect = url("preconnect"),
        prefetch = url("prefetch"),
        icon = url("icon.png"),
        import = url("import.css"),
        background = url("css-background.png"),
        font = url("font.woff"),
        refresh = url("meta-refresh"),
        script_src = url("script.js"),
        inline_script = url("inline-script"),
        img = url("img.png"),
        srcset = url("srcset.png"),
        picture = url("picture.png"),
        inline_style = url("inline-style.png"),
        iframe = url("iframe"),
        object = url("object"),
        embed = url("embed"),
        poster = url("poster.png"),
        video = url("video.mp4"),
        audio = url("audio.mp3"),
        svg = url("svg-image.png"),
        input = url("input-image.png"),
        table = url("table-background.png"),
        onerror = url("onerror"),
    )
}

/// Clipboard HTML in the shapes real sources produce (Google Docs wrapper, Word, a web page)
/// with every kind of hostile content mixed in.
pub fn hostile_paste(canary: &Canary, tiny_png_base64: &str) -> String {
    let url = |path: &str| canary.url(&format!("paste/{path}"));
    format!(
        r#"<html><head><meta charset="utf-8"><title>clip</title><style>p {{ color: red; }}</style>
<link rel="stylesheet" href="{link}"></head><body>
<!--[if gte mso 9]><xml><o:OfficeDocumentSettings></o:OfficeDocumentSettings></xml><![endif]-->
<b style="font-weight:normal;" id="docs-internal-guid-1234"><p dir="ltr" style="line-height:1.38;margin-top:0pt"><span style="font-size:11pt;font-family:Arial;color:#000000;font-weight:700;">Bold from Docs</span> <span style="font-style:italic;">italic from Docs</span></p></b>
<p class="MsoNormal" style="mso-margin-top-alt:auto">Word paragraph<o:p></o:p></p>
<script>document.title = "pasted-script";</script>
<img src="{img}" onerror="alert(1)" width="10" alt="remote">
<img src="data:image/png;base64,{tiny_png_base64}" alt="tiny">
<p><a href="javascript:alert(1)">bad link</a> and <a href="https://example.org/ok" onclick="steal()" target="_blank" rel="noopener">good link</a></p>
<iframe src="{iframe}"></iframe>
<form action="{form}"><input name="q"><button>go</button></form>
<div style="background:url({background})" onmouseover="steal()">styled div</div>
<svg onload="alert(1)"><circle r="5"/></svg>
<table background="{table}"><tr><td colspan="2" style="border:1px solid">cell</td></tr></table>
<ul><li>list item</li></ul>
<p><span style="text-decoration: underline line-through">under-strike</span> <font face="Comic Sans MS" color="red">font tag</font></p>
<video src="{video}" poster="{poster}"></video>
</body></html>"#,
        link = url("stylesheet.css"),
        img = url("img.png"),
        iframe = url("iframe"),
        form = url("form"),
        background = url("background.png"),
        table = url("table.png"),
        video = url("video.mp4"),
        poster = url("poster.png"),
    )
}
