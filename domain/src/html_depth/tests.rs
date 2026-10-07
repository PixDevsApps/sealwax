use super::*;

/// The deepest [`Nesting::depth`] over `html`, with no limit.
fn counted(html: &str) -> usize {
    struct Deepest {
        nesting: RefCell<Nesting>,
        deepest: Cell<usize>,
    }

    impl TokenSink for Deepest {
        type Handle = ();

        fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
            let mut nesting = self.nesting.borrow_mut();
            let tag = match token {
                Token::TagToken(tag) => tag,
                Token::CharacterTokens(_) => {
                    nesting.text();
                    return TokenSinkResult::Continue;
                }
                _ => return TokenSinkResult::Continue,
            };
            let result = match tag.kind {
                TagKind::StartTag => {
                    assert!(nesting.start(&tag));
                    nesting
                        .raw_text(&tag.name)
                        .unwrap_or(TokenSinkResult::Continue)
                }
                TagKind::EndTag => {
                    nesting.end(&tag);
                    TokenSinkResult::Continue
                }
            };
            self.deepest.set(self.deepest.get().max(nesting.depth()));
            result
        }
    }

    let tokenizer = Tokenizer::new(
        Deepest {
            nesting: RefCell::new(Nesting::new(usize::MAX, false)),
            deepest: Cell::new(0),
        },
        TokenizerOpts::default(),
    );
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.deepest.get()
}

/// The depth of the deepest element html5ever builds below `body`, as html2text parses.
fn built(html: &str) -> usize {
    let dom = html2text::config::plain()
        .parse_html(html.as_bytes())
        .unwrap();
    let mut deepest = 0;
    let mut pending = vec![(dom.document.clone(), 0usize)];
    while let Some((node, level)) = pending.pop() {
        if let Element {
            ref template_contents,
            ..
        } = node.data
        {
            deepest = deepest.max(level);
            if let Some(contents) = template_contents.borrow().as_ref() {
                pending.push((contents.clone(), level));
            }
        }
        for child in node.children.borrow().iter() {
            pending.push((child.clone(), level + 1));
        }
    }
    deepest.saturating_sub(2)
}

fn nested(open: &str, close: &str, depth: usize) -> String {
    format!("{}dypt{}", open.repeat(depth), close.repeat(depth))
}

/// Mail as it comes, sloppy included: what the tree builder closes by itself must not add up.
fn normal() -> Vec<(&'static str, String)> {
    vec![
        ("unclosed paragraphs", "<p>linje\n".repeat(2_000)),
        (
            "unclosed list items",
            format!("<ul>{}</ul>", "<li>punkt\n".repeat(2_000)),
        ),
        (
            "unclosed definitions",
            format!("<dl>{}</dl>", "<dt>ord<dd>forklaring\n".repeat(1_000)),
        ),
        (
            "rows and cells without end tags",
            format!("<table>{}</table>", "<tr><td>a<td>b<th>c\n".repeat(1_000)),
        ),
        (
            "font soup",
            "<p><font face=\"Arial\" size=2>tekst\n".repeat(2_000),
        ),
        ("bold across paragraphs", "<b><p>fet</b></p>\n".repeat(2_000)),
        (
            "unclosed spans in closed divs",
            "<div><span><span>x</div>\n".repeat(2_000),
        ),
        (
            "unclosed options",
            format!("<select>{}</select>", "<option>valg\n".repeat(2_000)),
        ),
        ("headings without end tags", "<h1>a<h2>b<h3>c\n".repeat(1_000)),
        (
            "a newsletter's nested tables",
            nested(
                "<table width=\"100%\"><tr><td align=\"center\">",
                "</td></tr></table>",
                12,
            )
            .repeat(200),
        ),
        (
            "outlook paragraphs",
            "<p class=MsoNormal><span style='font-size:11.0pt'>Hei<o:p></o:p></span></p>\n"
                .repeat(2_000),
        ),
        (
            "a quoted thread",
            nested("<blockquote type=\"cite\"><div>", "</div></blockquote>", 30),
        ),
        (
            "links in links",
            "<a href=\"https://example.test/\">lenke <a href=\"https://example.test/2\">to\n"
                .repeat(2_000),
        ),
        ("unclosed forms", "<form><input name=q>\n".repeat(2_000)),
        (
            "fonts between table tags",
            "<table><font face=\"Arial\"><tr><td><font size=2>tekst</font></td></tr></font></table>\n"
                .repeat(1_000),
        ),
        (
            "nested tables with fonts between their tags",
            nested(
                "<table><font face=\"Arial\"><tr><td><font size=2>",
                "</font></td></tr></font></table>",
                12,
            )
            .repeat(100),
        ),
        (
            "unclosed fonts in cells",
            format!(
                "<table>{}</table>",
                "<tr><td><font face=\"Arial\"><b>celle\n".repeat(1_000)
            ),
        ),
        (
            "inline svg",
            format!("<svg>{}</svg>", "<path d=\"M0 0\"/><g><rect/></g>".repeat(2_000)),
        ),
    ]
}

/// Deep in the ways found (N1, N2) and in ways that fool a simpler count.
fn hostile(depth: usize) -> Vec<(&'static str, String)> {
    vec![
        ("nested lists", nested("<ul><li>", "</li></ul>", depth)),
        ("open lists", nested("<ul>", "", depth)),
        ("nested strikeouts", nested("<s>", "", depth)),
        ("nested divs", nested("<div>", "</div>", depth)),
        (
            "nested tables",
            nested("<table><tr><td>", "</td></tr></table>", depth),
        ),
        ("nested quotes", nested("<blockquote>", "", depth)),
        ("nested spans", nested("<span>", "</span>", depth)),
        (
            "formatting opened again in closed divs",
            "<div><b></div>x".repeat(depth),
        ),
        (
            "distinct formatting closed by paragraphs",
            (0..depth)
                .map(|n| format!("<p><b class=c{n}>"))
                .collect::<String>()
                + "<p>x",
        ),
        (
            "end tags out of scope",
            "<ul><table><td></ul>".repeat(depth),
        ),
        (
            "lists inside formatting end tags",
            "<b><ul></b>".repeat(depth),
        ),
        ("items in items", nested("<li><ul>", "", depth)),
        (
            "definitions in lists",
            nested("<dl><dd><ul><li>", "", depth),
        ),
    ]
}

#[test]
fn normal_mail_counts_as_built() {
    for (name, html) in normal() {
        let (counted, built) = (counted(&html), built(&html));
        assert!(
            built <= counted + 4 && counted <= built + built / 4 + 4,
            "{name}: counted {counted}, built {built}"
        );
        assert!(counted < 100, "{name}: counted {counted}");
        assert!(!too_deep(&html), "{name}");
    }
}

#[test]
fn deep_mail_counts_at_least_as_deep_as_built() {
    for (name, html) in hostile(300) {
        let (counted, built) = (counted(&html), built(&html));
        assert!(
            counted + 2 >= built,
            "{name}: counted {counted}, built {built}"
        );
        assert!(built >= 250, "{name}: built {built}");
    }
    for (name, html) in hostile(600) {
        assert!(too_deep(&html), "{name}");
    }
}

const SOUP: [&str; 60] = [
    "<div>",
    "</div>",
    "<p>",
    "</p>",
    "<li>",
    "</li>",
    "<ul>",
    "</ul>",
    "<ol>",
    "<b>",
    "</b>",
    "<i>",
    "</i>",
    "<a href=x>",
    "</a>",
    "<span>",
    "</span>",
    "<table>",
    "</table>",
    "<tr>",
    "</tr>",
    "<td>",
    "</td>",
    "<th>",
    "<tbody>",
    "<blockquote>",
    "</blockquote>",
    "<h1>",
    "</h2>",
    "<dl>",
    "<dd>",
    "<dt>",
    "<s>",
    "<font face=x>",
    "</font>",
    "<em>",
    "<option>",
    "<select>",
    "</select>",
    "<button>",
    "<form>",
    "</form>",
    "<pre>",
    "<svg>",
    "</svg>",
    "<math>",
    "<br>",
    "<img>",
    "<hr>",
    "<caption>",
    "<col>",
    "<center>",
    "<nobr>",
    "<code>",
    "<object>",
    "</object>",
    "<u>",
    "<ruby><rt>",
    "x",
    "<b class=y>",
];

/// Random tag soup from a fixed seed: the count is never more than a few levels short of the
/// tree html5ever builds (the leaf a void element adds included).
#[test]
fn tag_soup_counts_within_a_few_levels_of_built() {
    let mut state: u64 = 0x5EA1_0A11;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..3_000 {
        let len = (next() % 200) as usize;
        let html: String = (0..len)
            .map(|_| SOUP[(next() % SOUP.len() as u64) as usize])
            .collect();
        let (counted, built) = (counted(&html), built(&html));
        assert!(
            built <= counted + 4,
            "{html}: counted {counted}, built {built}"
        );
    }
}

/// The soups the count misses most (found by shrinking random soup), repeated: what it misses
/// grows slowly, well inside the backstop's margin ([`MAX_TREE_DEPTH`] is twice the limit).
#[test]
fn what_the_count_misses_grows_slowly() {
    for soup in [
        "<a href=x><b><table><a href=x><b></table><select><a href=x><object>",
        "<form><pre></form><em><hr>",
        "<form><dl></form><form><h1></form><pre><form>",
        "<select><select><form><a href=x></form><select><form><a href=x><code>",
    ] {
        let many = soup.repeat(200);
        let (counted, built) = (counted(&many), built(&many));
        assert!(
            built <= 2 * counted,
            "{soup}: counted {counted}, built {built}"
        );
    }
}

/// html2text renders the same text from [`render`] as from `string_from_read` (the old call) for
/// the fuzz corpus and normal mail.
#[test]
fn render_is_string_from_read_below_the_limit() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fuzz/corpus/html");
    let corpus = std::fs::read_dir(root).unwrap().map(|entry| {
        let path = entry.unwrap().path();
        let data = std::fs::read(&path).unwrap();
        (
            path.display().to_string(),
            String::from_utf8_lossy(&data).into_owned(),
        )
    });
    let named = normal()
        .into_iter()
        .map(|(name, html)| (name.to_owned(), html));
    for (name, html) in corpus.chain(named).filter(|(_, html)| !too_deep(html)) {
        for width in [78, 200] {
            let old = html2text::config::plain()
                .string_from_read(html.as_bytes(), width)
                .map_err(|error| error.to_string());
            let new =
                render(html2text::config::plain(), &html, width).map_err(|error| error.to_string());
            assert_eq!(old, new, "{name} at {width}");
        }
    }
}

#[test]
fn flatten_keeps_the_text_below_the_depth_in_order() {
    let html = format!(
        "<p>a</p>{}<p>b</p>",
        nested("<div><span>c</span>", "</div>", 50)
    );
    let dom = html2text::config::plain()
        .parse_html(html.as_bytes())
        .unwrap();
    flatten(&dom, 10);
    let mut deepest = 0;
    let mut text = String::new();
    let mut pending = vec![(dom.document.clone(), 0usize)];
    while let Some((node, level)) = pending.pop() {
        deepest = deepest.max(level);
        match node.data {
            Element { .. } | html2text::Document => {}
            _ => {
                let mut serialized = Vec::new();
                node.serialize(&mut serialized).unwrap();
                text.push_str(&String::from_utf8(serialized).unwrap());
            }
        }
        for child in node.children.borrow().iter().rev() {
            pending.push((child.clone(), level + 1));
        }
    }
    assert!(deepest <= 11, "{deepest}");
    assert_eq!(text, format!("a{}dyptb", "c".repeat(50)));
}
