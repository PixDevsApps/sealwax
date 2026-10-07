use super::*;
use proptest::prelude::*;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// S7's hostile clipboard (`spikes/s7-composer/src/canary.rs`), the canary at a closed port.
fn hostile() -> String {
    let url = |path: &str| format!("http://127.0.0.1:2/paste/{path}");
    format!(
        r#"<html><head><meta charset="utf-8"><title>clip</title><style>p {{ color: red; }}</style>
<link rel="stylesheet" href="{link}"></head><body>
<!--[if gte mso 9]><xml><o:OfficeDocumentSettings></o:OfficeDocumentSettings></xml><![endif]-->
<b style="font-weight:normal;" id="docs-internal-guid-1234"><p dir="ltr" style="line-height:1.38;margin-top:0pt"><span style="font-size:11pt;font-family:Arial;color:#000000;font-weight:700;">Bold from Docs</span> <span style="font-style:italic;">italic from Docs</span></p></b>
<p class="MsoNormal" style="mso-margin-top-alt:auto">Word paragraph<o:p></o:p></p>
<script>document.title = "pasted-script";</script>
<img src="{img}" onerror="alert(1)" width="10" alt="remote">
<img src="data:image/png;base64,{PNG}" alt="tiny">
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

/// Reads the serialized output tag by tag: every tag is on the allowlist, every attribute is
/// one its tag may have, with a value the rules allow; everything else is text.
fn assert_inert(html: &str) {
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        assert!(!rest.starts_with("!--"), "a comment in {html}");
        let closing = rest.starts_with('/');
        if closing {
            rest = &rest[1..];
        }
        let name_end = rest.find([' ', '>']).expect("tag ends");
        let name = &rest[..name_end];
        assert!(KEEP.contains(&name), "<{name}> in {html}");
        rest = &rest[name_end..];
        loop {
            rest = rest.trim_start_matches(' ');
            if let Some(after) = rest.strip_prefix('>') {
                rest = after;
                break;
            }
            assert!(!closing, "attributes on </{name}> in {html}");
            let equals = rest.find('=').expect("attribute value");
            let attribute = &rest[..equals];
            assert_eq!(&rest[equals + 1..equals + 2], "\"", "{html}");
            let value_end = rest[equals + 2..].find('"').expect("value ends") + equals + 2;
            let value = rest[equals + 2..value_end].replace("&amp;", "&");
            assert!(
                allowed_value(name, attribute, &value),
                "{name} {attribute}={value} in {html}"
            );
            if attribute == "class" {
                assert_eq!(value, SIGNATURE_CLASS);
            }
            if attribute == "href" || attribute == "src" {
                assert!(!value.to_ascii_lowercase().contains("script:"), "{html}");
            }
            rest = &rest[value_end + 1..];
        }
    }
}

#[test]
fn hostile_paste_keeps_only_the_allowlist() {
    let cleaned = clean(&hostile());
    let html = &cleaned.html;
    assert_inert(html);
    for kept in [
        "<b>Bold from Docs</b>",
        "<i>italic from Docs</i>",
        "<p>Word paragraph</p>",
        "<a href=\"https://example.org/ok\">good link</a>",
        "<a>bad link</a>",
        "<div>styled div</div>",
        "<td colspan=\"2\">cell</td>",
        "<ul><li>list item</li></ul>",
        "<u><s>under-strike</s></u>",
        "font tag",
        "alt=\"tiny\"",
    ] {
        assert!(html.contains(kept), "{kept} missing from {html}");
    }
    assert!(html.contains(&format!("src=\"data:image/png;base64,{PNG}\"")));
    assert!(
        !html.contains("alt=\"remote\""),
        "the http: image goes: {html}"
    );
    assert!(!html.contains("pasted-script") && !html.contains("clip"));
    assert_eq!(cleaned.content_ids, Vec::<String>::new());
    assert_eq!(cleaned.remote_images, 0);
}

#[test]
fn docs_wrapper_unwraps_and_strong_em_become_b_i() {
    let html = clean(
        "<b style=\"font-weight:normal\"><span style=\"font-weight:700;font-style:italic\">x</span></b>\
         <strong>s</strong><em>e</em><b style=\"font-weight:bold\">kept</b>\
         <p style=\"font: italic bold 12px Arial\">p</p><del>d</del><strike>k</strike>",
    )
    .html;
    assert_eq!(
        html,
        "<b><i>x</i></b><b>s</b><i>e</i><b>kept</b><p><b><i>p</i></b></p><del>d</del><strike>k</strike>"
    );
}

#[test]
fn blocks_become_divs_and_quotes_take_the_composer_form() {
    let html = clean(
        "<section><article>a</article></section><center>c</center>\
         <blockquote class=\"gmail_quote\" style=\"margin:0 0 0 .8ex;border-left:1px #ccc solid\">q\
         <blockquote type=\"cite\">nested</blockquote></blockquote>\
         <blockquote style=\"margin: 0 0 0 40px; border: none; padding: 0px;\">indent</blockquote>",
    )
    .html;
    assert_eq!(
        html,
        format!(
            "<div><div>a</div></div><div>c</div>\
             <blockquote type=\"cite\" style=\"{QUOTE_STYLE}\">q\
             <blockquote type=\"cite\" style=\"{QUOTE_STYLE}\">nested</blockquote></blockquote>\
             <blockquote style=\"{INDENT_STYLE}\">indent</blockquote>"
        )
    );
}

#[test]
fn images_cid_https_and_data_only() {
    let cleaned = clean(
        "<img src=\"cid:part1.abc@example.test\" width=\"120\" height=\"x\">\
         <img src=\"cid:a%25b@example.test\">\
         <img src=\"https://images.example.test/logo.png\" alt=\"Logo\" srcset=\"https://x.example.test/2x.png 2x\">\
         <img src=\"http://images.example.test/plain.png\">\
         <img src=\"data:image/svg+xml;base64,PHN2Zz4=\">\
         <img src=\"data:image/png;base64,AAAA\" onload=\"x()\">\
         <img src=\"//images.example.test/relative.png\"><img src=\"file:///etc/passwd\"><img>",
    );
    assert_eq!(
        cleaned.html,
        "<img src=\"cid:part1.abc@example.test\" width=\"120\">\
         <img src=\"cid:a%25b@example.test\">\
         <img src=\"https://images.example.test/logo.png\" alt=\"Logo\">\
         <img src=\"data:image/png;base64,AAAA\">"
    );
    assert_eq!(
        cleaned.content_ids,
        ["part1.abc@example.test", "a%b@example.test"]
    );
    assert_eq!(cleaned.remote_images, 1);
    let only_known = clean_with(
        "<p>a<img src=\"cid:known@example.test\"><img src=\"cid:gone@example.test\">b</p>",
        |id| id == "known@example.test",
    );
    assert_eq!(
        only_known.html,
        "<p>a<img src=\"cid:known@example.test\">b</p>"
    );
    assert_eq!(only_known.content_ids, ["known@example.test"]);
}

/// Found by fuzzing (ML-093): a `<p>` inside `<svg>` ends the SVG for the allowlist's parser
/// but not for the first pass, which let an image through without `keep_cid`; an image's
/// source written in capitals, after white space or in text was counted wrongly.
#[test]
fn every_image_the_output_shows_is_counted_and_checked() {
    let escaped = clean_with(
        "<svg><p><img src=\"cid:gone@example.test\"><img src=\"cid:known@example.test\"></svg>",
        |id| id == "known@example.test",
    );
    assert_eq!(escaped.content_ids, ["known@example.test"]);
    assert!(!escaped.html.contains("gone@"), "{}", escaped.html);
    let counted = clean(
        "<img src=\" CID:Upper@example.test\"><img src=\"\u{c}https://example.test/a.png\">\
         <p>src=\"cid:text@example.test\"</p>",
    );
    assert_eq!(counted.content_ids, ["Upper@example.test"]);
    assert_eq!(counted.remote_images, 1);
}

#[test]
fn links_web_and_mail_only() {
    let html = clean(
        "<a href=\"HTTPS://example.org/a?b=1&amp;c=2\">1</a><a href=\"mailto:a@example.org\">2</a>\
         <a href=\" javascript:alert(1)\">3</a><a href=\"data:text/html,x\">4</a>\
         <a href=\"cid:x@example.org\">5</a><a href=\"/relative\">6</a><a href=\"ftp://example.org\">7</a>",
    )
    .html;
    assert_eq!(
        html,
        "<a href=\"HTTPS://example.org/a?b=1&amp;c=2\">1</a><a href=\"mailto:a@example.org\">2</a>\
         <a>3</a><a>4</a><a>5</a><a>6</a><a>7</a>"
    );
}

#[test]
fn raw_text_and_odd_markup_stay_inert() {
    for input in [
        "<script>var s = \"<b>hi</b>\";</script>after",
        "<style>p{}</style><p>x</p>",
        "<textarea>&lt;/textarea&gt;&lt;script&gt;alert(1)&lt;/script&gt;</textarea>after",
        "<title><b>t</b></title>after",
        "<noscript><img src=x onerror=alert(1)></noscript>after",
        "<svg><style>x</style><foreignObject><p onclick=x()>in svg</p></foreignObject></svg>after",
        "<math><mi>x</mi><mglyph><style><img src=x onerror=alert(1)></style></mglyph></math>after",
        "<xmp><script>alert(1)</script></xmp>after",
        "<plaintext><script>alert(1)</script>",
        "<a href=\"java&#x09;script:alert(1)\">x</a>",
        "<a href=\"&#106;avascript:alert(1)\">x</a>",
        "<img src=\"java\nscript:alert(1)\">",
        "<div style=\"behavior:url(x.htc);font-weight:bold\">b</div>",
        "<p>unclosed <span style=\"font-weight:bold\">bold <div>block</p>after",
        "</b></i></div>stray ends<b>",
        "<b><i>misnested</b></i>",
        "<!--><script>alert(1)</script>-->",
        "<![CDATA[<script>alert(1)</script>]]>",
        "<base href=\"https://evil.example/\"><a href=\"x\">rel</a>",
        "<meta http-equiv=\"refresh\" content=\"0;url=https://evil.example\">",
        "<object data=\"x.swf\"><param name=a value=b><embed src=x></object>",
    ] {
        assert_inert(&clean(input).html);
    }
    for (input, output) in [
        ("<script>var s = \"<b>hi</b>\";</script>after", "after"),
        ("<style>p{}</style><p>x</p>", "<p>x</p>"),
        (
            "<textarea>&lt;/textarea&gt;&lt;script&gt;alert(1)&lt;/script&gt;</textarea>after",
            "after",
        ),
        ("<title><b>t</b></title>after", "after"),
        (
            "<noscript><img src=x onerror=alert(1)></noscript>after",
            "after",
        ),
        ("<xmp><script>alert(1)</script></xmp>after", "after"),
        ("<plaintext><script>alert(1)</script>", ""),
        ("<a href=\"&#106;avascript:alert(1)\">x</a>", "<a>x</a>"),
        (
            "<div style=\"behavior:url(x.htc);font-weight:bold\">b</div>",
            "<div><b>b</b></div>",
        ),
        ("<b><i>misnested</b></i>", "<b><i>misnested</i></b>"),
        (
            "<base href=\"https://evil.example/\"><a href=\"x\">rel</a>",
            "<a>rel</a>",
        ),
        // HTML reads CDATA outside SVG as a bogus comment ending at the first `>`: the rest is
        // text, as a browser shows it.
        ("<![CDATA[<script>alert(1)</script>]]>", "alert(1)]]&gt;"),
    ] {
        assert_eq!(clean(input).html, output, "{input}");
    }
}

#[test]
fn signature_class_only() {
    let html = clean(
        "<div class=\"sw-signature other\">-- <br>Ada</div><div class=\"gmail_signature\">g</div>\
         <p class=\"sw-signature\">p</p>",
    )
    .html;
    assert_eq!(
        html,
        "<div class=\"sw-signature\">-- <br>Ada</div><div>g</div><p>p</p>"
    );
}

#[test]
fn debug_prints_no_content() {
    let cleaned = clean("<p>Secret plans</p><img src=\"cid:plans@example.test\">");
    let printed = format!("{cleaned:?}");
    assert!(
        !printed.contains("Secret") && !printed.contains("plans@"),
        "{printed}"
    );
    assert!(printed.contains("content_ids: 1"));
}

proptest! {
    #[test]
    fn whatever_comes_in_nothing_unsafe_comes_out(
        parts in prop::collection::vec(
            prop_oneof![
                Just("<script>alert(1)</script>".to_owned()),
                Just("<img src=x onerror=alert(1)>".to_owned()),
                Just("<a href=\"javascript:alert(1)\">".to_owned()),
                Just("<svg onload=alert(1)>".to_owned()),
                Just("<style>".to_owned()),
                Just("</style>".to_owned()),
                Just("<b style=\"font-weight:normal\">".to_owned()),
                Just("<span style=\"font-weight:700\">".to_owned()),
                Just("<blockquote style=\"background:url(http://127.0.0.1:2/)\">".to_owned()),
                Just("</b>".to_owned()),
                Just("</span>".to_owned()),
                Just("<textarea>".to_owned()),
                Just("<!--".to_owned()),
                Just("-->".to_owned()),
                Just("<iframe src=http://127.0.0.1:2/>".to_owned()),
                "[a-z<>/=\" ]{0,12}",
            ],
            0..24,
        )
    ) {
        assert_inert(&clean(&parts.concat()).html);
    }

    #[test]
    fn cleaning_twice_changes_nothing(text in "[a-zA-Z <>/=\"bpiu]{0,60}") {
        let once = clean(&text).html;
        prop_assert_eq!(clean(&once).html, once);
    }
}
