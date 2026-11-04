//! The hostile-HTML suite. Every case points its network vectors at the canary under
//! `/h/<case>/` and carries a script payload that would leave four traces if it ran: the
//! title changes to `pwned-<case>`, the root gets a `data-pwned` attribute, `alert()` raises
//! a script dialog, and an image beacon hits the canary. Cases with `click` are activated
//! from Mail's isolated world after load (a DOM `click()`, not synthetic input).

/// A part every case may load; it must render.
pub const OK_CID: &str = "ok@hostile.test";
/// A part that belongs to another message's view; it must not load in a case's view.
pub const FOREIGN_CID: &str = "secret@other-message.test";

#[derive(Debug, Clone)]
pub struct Case {
    pub id: &'static str,
    pub vector: &'static str,
    pub html: String,
    pub click: Option<&'static str>,
}

/// JavaScript that sets every script trace for `id`. It contains no double quotes, so it can
/// sit inside a double-quoted attribute.
pub fn payload(id: &str, origin: &str) -> String {
    format!(
        "try{{top.document.title='pwned-{id}'}}catch(e){{}};document.title='pwned-{id}';\
         document.documentElement.setAttribute('data-pwned','{id}');alert('{id}');\
         new Image().src='{origin}/h/{id}/js.gif'"
    )
}

fn document(id: &str, head: &str, body_attributes: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>hostile-{id}</title>{head}</head>\
         <body{body_attributes}><p>Hostile case {id}</p>\
         <img id=\"s2-ok\" src=\"cid:{OK_CID}\" width=\"16\" height=\"16\" alt=\"ok\">{body}</body></html>"
    )
}

pub fn cases(origin: &str) -> Vec<Case> {
    let o = origin;
    let host = o.trim_start_matches("http://");
    let port = host.rsplit_once(':').map_or("80", |(_, port)| port);
    let mut cases = Vec::new();
    let mut add = |id: &'static str,
                   vector: &'static str,
                   head: String,
                   body_attributes: String,
                   body: String,
                   click: Option<&'static str>| {
        cases.push(Case {
            id,
            vector,
            html: document(id, &head, &body_attributes, &body),
            click,
        });
    };
    let p = |id: &str| payload(id, o);
    let none = String::new;

    add(
        "img-pixel",
        "tracking pixel",
        none(),
        none(),
        format!(
            "<img src=\"{o}/h/img-pixel/p.gif\" width=\"1\" height=\"1\" alt=\"\"><img src=\"{o}/h/img-pixel/hidden.gif\" style=\"display:none\">"
        ),
        None,
    );
    add(
        "css-inline-bg",
        "CSS url() background in a style attribute",
        none(),
        none(),
        format!("<div style=\"background:url('{o}/h/css-inline-bg/bg.png');height:20px\">bg</div>"),
        None,
    );
    add(
        "css-style-bg",
        "CSS url() background in a style block",
        format!(
            "<style>body{{background-image:url({o}/h/css-style-bg/bg.png)}} .x{{background:url(\"{o}/h/css-style-bg/x.png\")}}</style>"
        ),
        none(),
        "<div class=\"x\">x</div>".into(),
        None,
    );
    add(
        "css-import",
        "@import (plain and CSS-escaped)",
        format!(
            "<style>@import url(\"{o}/h/css-import/a.css\");</style><style>@\\69mport url({o}/h/css-import/escaped.css);</style>"
        ),
        none(),
        none(),
        None,
    );
    add(
        "link-stylesheet",
        "<link rel=stylesheet>, also a data: stylesheet that imports",
        format!(
            "<link rel=\"stylesheet\" href=\"{o}/h/link-stylesheet/s.css\"><link rel=\"stylesheet\" href=\"data:text/css,@import url({o}/h/link-stylesheet/data-import.css);\">"
        ),
        none(),
        none(),
        None,
    );
    add(
        "css-misc-urls",
        "list-style-image, cursor, content, border-image, mask, filter, shape-outside, image-set, var()",
        format!(
            "<style>.c::before{{content:url({o}/h/css-misc-urls/content.png)}}\
             :root{{--u:url({o}/h/css-misc-urls/var.png)}} .v{{background:var(--u)}}</style>"
        ),
        none(),
        format!(
            "<ul style=\"list-style-image:url({o}/h/css-misc-urls/list.png)\"><li>a</li></ul>\
             <div style=\"cursor:url({o}/h/css-misc-urls/cursor.png),auto\">cursor</div>\
             <div style=\"border:8px solid;border-image:url({o}/h/css-misc-urls/border.png) 30 round\">border</div>\
             <div style=\"-webkit-mask-image:url({o}/h/css-misc-urls/wmask.png);mask-image:url({o}/h/css-misc-urls/mask.png)\">mask</div>\
             <div class=\"c\"></div><div class=\"v\">var</div>\
             <div style=\"filter:url({o}/h/css-misc-urls/filter.svg#f)\">filter</div>\
             <div style=\"float:left;width:10px;height:10px;shape-outside:url({o}/h/css-misc-urls/shape.png)\"></div>\
             <div style=\"background-image:-webkit-image-set(url({o}/h/css-misc-urls/wset.png) 1x);background-image:image-set(url({o}/h/css-misc-urls/set.png) 1x)\">set</div>"
        ),
        None,
    );
    add(
        "font-face-remote",
        "remote @font-face",
        format!(
            "<style>@font-face{{font-family:Leak;src:url({o}/h/font-face-remote/f.woff2) format('woff2'),url({o}/h/font-face-remote/f.ttf)}} p.f{{font-family:Leak}}</style>"
        ),
        none(),
        "<p class=\"f\">Remote font text</p>".into(),
        None,
    );
    add(
        "meta-refresh",
        "meta refresh to the canary",
        format!("<meta http-equiv=\"refresh\" content=\"0;url={o}/h/meta-refresh/go\">"),
        none(),
        none(),
        None,
    );
    add(
        "meta-refresh-js",
        "meta refresh to a javascript: URL",
        format!(
            "<meta http-equiv=\"refresh\" content=\"0;url=javascript:{}\">",
            p("meta-refresh-js")
        ),
        none(),
        none(),
        None,
    );
    add(
        "form-autosubmit",
        "auto-submitting form (script and onload)",
        none(),
        " onload=\"document.forms[0].submit()\"".to_string(),
        format!(
            "<form id=\"f\" action=\"{o}/h/form-autosubmit/post\" method=\"post\"><input name=\"a\" value=\"b\"></form>\
             <script>document.getElementById('f').submit();{}</script>",
            p("form-autosubmit")
        ),
        None,
    );
    add(
        "form-click",
        "form submitted by a click",
        none(),
        none(),
        format!(
            "<form action=\"{o}/h/form-click/post\" method=\"get\"><input name=\"q\" value=\"x\"><input type=\"submit\" id=\"s2-click\" value=\"Go\"></form>"
        ),
        Some("#s2-click"),
    );
    add(
        "iframe-remote",
        "iframe to the canary",
        none(),
        none(),
        format!(
            "<iframe src=\"{o}/h/iframe-remote/frame.html\" width=\"100\" height=\"50\"></iframe>"
        ),
        None,
    );
    add(
        "iframe-srcdoc",
        "srcdoc iframe with an image and a script",
        none(),
        none(),
        format!(
            "<iframe srcdoc=\"<img src='{o}/h/iframe-srcdoc/i.png'><script>{}</script>\" width=\"100\" height=\"50\"></iframe>",
            p("iframe-srcdoc")
        ),
        None,
    );
    add(
        "iframe-data-js",
        "data: and javascript: iframes",
        none(),
        none(),
        format!(
            "<iframe src=\"data:text/html,<img src='{o}/h/iframe-data-js/data.png'><script>{}</script>\"></iframe>\
             <iframe src=\"javascript:'<img src={o}/h/iframe-data-js/js-frame.png>'\"></iframe>",
            p("iframe-data-js")
        ),
        None,
    );
    add(
        "object-embed",
        "object and embed",
        none(),
        none(),
        format!(
            "<object data=\"{o}/h/object-embed/o.swf\" type=\"application/x-shockwave-flash\"></object>\
             <object data=\"{o}/h/object-embed/o.html\" type=\"text/html\" width=\"50\" height=\"20\"></object>\
             <embed src=\"{o}/h/object-embed/e.swf\"><embed src=\"{o}/h/object-embed/e.svg\" type=\"image/svg+xml\">\
             <object data=\"data:text/html,<script>{}</script>\"></object>",
            p("object-embed")
        ),
        None,
    );
    add(
        "svg-script",
        "inline SVG with script and onload",
        none(),
        none(),
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\" onload=\"{}\"><script>{}</script></svg>",
            p("svg-script"),
            p("svg-script")
        ),
        None,
    );
    add(
        "svg-external",
        "SVG image, use and feImage",
        none(),
        none(),
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"20\" height=\"20\">\
             <image href=\"{o}/h/svg-external/image.png\" width=\"10\" height=\"10\"/>\
             <use xlink:href=\"{o}/h/svg-external/use.svg#x\"/>\
             <filter id=\"f\"><feImage href=\"{o}/h/svg-external/fe.png\"/></filter><rect width=\"5\" height=\"5\" filter=\"url(#f)\"/>\
             <style>@import url({o}/h/svg-external/style.css);</style></svg>"
        ),
        None,
    );
    add(
        "svg-data-image",
        "SVG in a data: image with a script and an external image",
        none(),
        none(),
        format!(
            "<img src=\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><image href='{o}/h/svg-data-image/i.png' width='10' height='10'/><script>{}</script></svg>\">",
            p("svg-data-image")
        ),
        None,
    );
    add(
        "a-ping",
        "<a ping> activated by a click",
        none(),
        none(),
        format!(
            "<a id=\"s2-click\" href=\"{o}/h/a-ping/target\" ping=\"{o}/h/a-ping/ping\">ping link</a>"
        ),
        Some("#s2-click"),
    );
    add(
        "dns-prefetch",
        "dns-prefetch, x-dns-prefetch-control on, anchors to other hosts",
        "<meta http-equiv=\"x-dns-prefetch-control\" content=\"on\">\
         <link rel=\"dns-prefetch\" href=\"//dns-prefetch.s2-canary.invalid\">"
            .into(),
        none(),
        "<a href=\"https://anchor-prefetch.s2-canary.invalid/\">anchor</a>".into(),
        None,
    );
    add(
        "preconnect",
        "<link rel=preconnect> (a connection without a request)",
        format!(
            "<link rel=\"preconnect\" href=\"{o}\"><link rel=\"preconnect\" href=\"{o}/h/preconnect/\" crossorigin>"
        ),
        none(),
        none(),
        None,
    );
    add(
        "prefetch-preload",
        "prefetch, preload, modulepreload, prerender, icon, manifest",
        format!(
            "<link rel=\"prefetch\" href=\"{o}/h/prefetch-preload/prefetch.html\">\
             <link rel=\"preload\" as=\"image\" href=\"{o}/h/prefetch-preload/preload.png\">\
             <link rel=\"preload\" as=\"style\" href=\"{o}/h/prefetch-preload/preload.css\">\
             <link rel=\"preload\" as=\"font\" href=\"{o}/h/prefetch-preload/preload.woff2\" crossorigin>\
             <link rel=\"modulepreload\" href=\"{o}/h/prefetch-preload/module.js\">\
             <link rel=\"prerender\" href=\"{o}/h/prefetch-preload/prerender.html\">\
             <link rel=\"icon\" href=\"{o}/h/prefetch-preload/favicon.ico\">\
             <link rel=\"apple-touch-icon\" href=\"{o}/h/prefetch-preload/touch.png\">\
             <link rel=\"manifest\" href=\"{o}/h/prefetch-preload/manifest.json\">"
        ),
        none(),
        none(),
        None,
    );
    add(
        "base-href",
        "<base href> with relative URLs",
        format!("<base href=\"{o}/h/base-href/\"><link rel=\"stylesheet\" href=\"rel.css\">"),
        none(),
        "<img src=\"rel.png\" width=\"5\" height=\"5\"><a id=\"s2-click\" href=\"rel-link\">relative link</a>".into(),
        Some("#s2-click"),
    );
    add(
        "srcset",
        "img srcset and sizes",
        none(),
        none(),
        format!(
            "<img src=\"cid:{OK_CID}\" srcset=\"{o}/h/srcset/1x.png 1x, {o}/h/srcset/2x.png 2x\" alt=\"\">\
             <img srcset=\"{o}/h/srcset/w600.png 600w\" sizes=\"100vw\" alt=\"\">"
        ),
        None,
    );
    add(
        "picture-source",
        "<picture><source>",
        none(),
        none(),
        format!(
            "<picture><source srcset=\"{o}/h/picture-source/a.webp\" type=\"image/webp\">\
             <source srcset=\"{o}/h/picture-source/b.png\" media=\"(min-width: 1px)\"><img src=\"cid:{OK_CID}\" alt=\"\"></picture>"
        ),
        None,
    );
    add(
        "media-poster",
        "video poster, video and audio sources, track",
        none(),
        none(),
        format!(
            "<video poster=\"{o}/h/media-poster/poster.png\" src=\"{o}/h/media-poster/v.mp4\" autoplay muted preload=\"auto\" width=\"60\" height=\"40\"></video>\
             <audio src=\"{o}/h/media-poster/a.mp3\" autoplay preload=\"auto\"></audio>\
             <video preload=\"auto\"><source src=\"{o}/h/media-poster/s.webm\" onerror=\"{}\"><track src=\"{o}/h/media-poster/t.vtt\" default></video>",
            p("media-poster")
        ),
        None,
    );
    add(
        "javascript-url",
        "javascript: link activated by a click",
        none(),
        none(),
        format!(
            "<a id=\"s2-click\" href=\"javascript:{}\">js link</a>",
            p("javascript-url")
        ),
        Some("#s2-click"),
    );
    add(
        "inline-handlers",
        "inline event handlers (onerror, onload, ontoggle, onfocus, onbegin)",
        none(),
        format!(" onload=\"{}\"", p("inline-handlers")),
        format!(
            "<img src=\"cid:missing@hostile.test\" onerror=\"{0}\">\
             <details open ontoggle=\"{0}\"><summary>s</summary>d</details>\
             <input autofocus onfocus=\"{0}\">\
             <svg width=\"5\" height=\"5\"><animate onbegin=\"{0}\" attributeName=\"x\" dur=\"1s\"/></svg>",
            p("inline-handlers")
        ),
        None,
    );
    add(
        "script-inline",
        "inline <script>",
        none(),
        none(),
        format!("<script>{}</script>", p("script-inline")),
        None,
    );
    add(
        "script-remote",
        "remote, module and deferred scripts",
        format!("<script src=\"{o}/h/script-remote/head.js\"></script>"),
        none(),
        format!(
            "<script src=\"{o}/h/script-remote/s.js\"></script><script type=\"module\" src=\"{o}/h/script-remote/m.js\"></script>\
             <script defer src=\"{o}/h/script-remote/d.js\"></script>"
        ),
        None,
    );
    add(
        "script-data-url",
        "script from a data: URL",
        none(),
        none(),
        format!(
            "<script src=\"data:text/javascript,{}\"></script>",
            p("script-data-url")
        ),
        None,
    );
    add(
        "window-open",
        "window.open and a target=_blank link click",
        none(),
        none(),
        format!(
            "<script>window.open('{o}/h/window-open/popup.html');{}</script>\
             <a id=\"s2-click\" target=\"_blank\" href=\"{o}/h/window-open/blank.html\">new window</a>",
            p("window-open")
        ),
        Some("#s2-click"),
    );
    add(
        "input-image",
        "<input type=image>",
        none(),
        none(),
        format!("<input type=\"image\" src=\"{o}/h/input-image/i.png\" alt=\"go\">"),
        None,
    );
    add(
        "legacy-background",
        "body/table/td background attributes",
        none(),
        format!(" background=\"{o}/h/legacy-background/body.png\""),
        format!(
            "<table background=\"{o}/h/legacy-background/table.png\"><tr><td background=\"{o}/h/legacy-background/td.png\">x</td></tr></table>"
        ),
        None,
    );
    add(
        "meta-csp-relax",
        "a page CSP that tries to allow everything",
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src * 'unsafe-inline' 'unsafe-eval' data: blob:; img-src *; script-src * 'unsafe-inline'\">".into(),
        none(),
        format!("<img src=\"{o}/h/meta-csp-relax/i.png\"><script>{}</script>", p("meta-csp-relax")),
        None,
    );
    add(
        "noscript",
        "<noscript> content (parsed as markup when scripting is off)",
        none(),
        none(),
        format!(
            "<noscript><img src=\"{o}/h/noscript/i.png\"><link rel=\"stylesheet\" href=\"{o}/h/noscript/n.css\"></noscript>"
        ),
        None,
    );
    add(
        "obfuscated-urls",
        "entity-encoded, upper-case, short, hex, decimal and backslash IP URLs",
        none(),
        none(),
        format!(
            "<img src=\"&#104;ttp://127.0.0.1:{port}/h/obfuscated-urls/entity.png\">\
             <img src=\"HTTP://127.0.0.1:{port}/h/obfuscated-urls/upper.png\">\
             <img src=\"http://127.1:{port}/h/obfuscated-urls/short.png\">\
             <img src=\"http://0x7f.0.0.1:{port}/h/obfuscated-urls/hex.png\">\
             <img src=\"http://2130706433:{port}/h/obfuscated-urls/decimal.png\">\
             <img src=\" http://127.0.0.1:{port}/h/obfuscated-urls/space.png\">\
             <img src=\"http:\\\\127.0.0.1:{port}\\h\\obfuscated-urls\\backslash.png\">\
             <img src=\"//127.0.0.1:{port}/h/obfuscated-urls/scheme-relative.png\">"
        ),
        None,
    );
    add(
        "cid-foreign",
        "cid: part that belongs to another message",
        none(),
        none(),
        format!(
            "<img id=\"s2-foreign\" src=\"cid:{FOREIGN_CID}\" width=\"16\" height=\"16\" alt=\"foreign\">"
        ),
        None,
    );
    add(
        "cid-navigation",
        "link to a cid: part activated by a click",
        none(),
        none(),
        format!("<a id=\"s2-click\" href=\"cid:{OK_CID}\">open part</a>"),
        Some("#s2-click"),
    );
    add(
        "download-link",
        "<a download> activated by a click",
        none(),
        none(),
        format!(
            "<a id=\"s2-click\" download=\"x.bin\" href=\"{o}/h/download-link/file.bin\">download</a>"
        ),
        Some("#s2-click"),
    );

    add(
        "lazy-image",
        "lazy-loaded image far below the fold",
        none(),
        none(),
        format!(
            "<div style=\"height:3000px\"></div><img loading=\"lazy\" src=\"{o}/h/lazy-image/i.png\" width=\"10\" height=\"10\">"
        ),
        None,
    );
    add(
        "link-click-remote",
        "plain link activated by a click",
        none(),
        none(),
        format!("<a id=\"s2-click\" href=\"{o}/h/link-click-remote/target\">plain link</a>"),
        Some("#s2-click"),
    );
    add(
        "xml-stylesheet",
        "XSLT and xml-stylesheet processing instructions",
        none(),
        none(),
        format!(
            "<?xml-stylesheet type=\"text/xsl\" href=\"{o}/h/xml-stylesheet/x.xsl\"?><?xml-stylesheet href=\"{o}/h/xml-stylesheet/x.css\"?>"
        ),
        None,
    );
    add(
        "http-equiv-misc",
        "meta Link header, set-cookie, refresh in the body",
        format!(
            "<meta http-equiv=\"Link\" content=\"<{o}/h/http-equiv-misc/link.css>; rel=stylesheet\"><meta http-equiv=\"set-cookie\" content=\"s2=1\">"
        ),
        none(),
        format!(
            "<meta http-equiv=\"refresh\" content=\"0; url={o}/h/http-equiv-misc/body-refresh\">"
        ),
        None,
    );
    add(
        "vh-feedback",
        "content sized in viewport units (grows with every resize)",
        "<style>.grow{height:150vh;background:#ccc}</style>".into(),
        none(),
        "<div class=\"grow\">tall</div>".into(),
        None,
    );
    add(
        "huge-height",
        "a 200000 px tall body",
        none(),
        none(),
        "<div style=\"height:200000px\">huge</div>".into(),
        None,
    );
    cases.push(Case {
        id: "frameset",
        vector: "frameset document",
        html: format!(
            "<!DOCTYPE html><html><head><title>hostile-frameset</title></head>\
             <frameset cols=\"50%,50%\"><frame src=\"{o}/h/frameset/a.html\"><frame src=\"{o}/h/frameset/b.html\">\
             <noframes><img src=\"cid:{OK_CID}\"></noframes></frameset></html>"
        ),
        click: None,
    });
    cases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_is_large_and_ids_are_unique() {
        let cases = cases("http://127.0.0.1:9");
        assert!(cases.len() >= 25, "{} cases", cases.len());
        let mut ids: Vec<_> = cases.iter().map(|case| case.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), cases.len());
    }

    #[test]
    fn network_vectors_stay_under_their_case_prefix() {
        for case in cases("http://127.0.0.1:9") {
            for (index, _) in case.html.match_indices("/h/") {
                let rest = &case.html[index + 3..];
                assert!(
                    rest.starts_with(case.id),
                    "{}: {}",
                    case.id,
                    &rest[..rest.len().min(40)]
                );
            }
        }
    }

    #[test]
    fn payload_fits_in_a_double_quoted_attribute() {
        let js = payload("x", "http://127.0.0.1:9");
        assert!(!js.contains('"') && !js.contains('#') && !js.contains('%'));
    }
}
