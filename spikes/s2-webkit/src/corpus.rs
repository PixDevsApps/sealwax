//! The synthetic reading corpus: newsletter-style HTML (nested tables, inline CSS, media
//! queries, `@font-face`, background images, `cid:` images and remote images that point at
//! the canary), simple HTML and plain text. Everything is generated here from fixed seeds; no
//! template or message comes from outside this file, and no real person or address appears.

use crate::png;
use std::fmt::Write as _;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Newsletter,
    SimpleHtml,
    PlainText,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub cid: String,
    pub content_type: &'static str,
    pub data: Arc<[u8]>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub kind: Kind,
    pub family: &'static str,
    pub sender: String,
    pub subject: String,
    pub body: String,
    pub parts: Vec<Part>,
}

/// Shared image and font bytes; every message references them under its own `cid:`s.
pub struct Assets {
    logo: Arc<[u8]>,
    hero: Arc<[u8]>,
    thumb: Arc<[u8]>,
    product: Arc<[u8]>,
    banner: Arc<[u8]>,
    icon: Arc<[u8]>,
    map: Arc<[u8]>,
    font: Option<Arc<[u8]>>,
}

impl Assets {
    /// `font` is a TrueType file for the `@font-face` samples (a system font read at run time).
    pub fn generate(font: Option<Vec<u8>>) -> Self {
        let image = |w, h, from, to| Arc::from(png::picture(w, h, from, to));
        Self {
            logo: image(180, 50, [20, 60, 120], [240, 140, 40]),
            hero: image(600, 260, [30, 90, 160], [250, 210, 120]),
            thumb: image(180, 120, [120, 40, 90], [240, 230, 200]),
            product: image(170, 170, [60, 140, 90], [230, 240, 250]),
            banner: image(600, 200, [80, 20, 30], [250, 120, 90]),
            icon: image(32, 32, [10, 10, 10], [200, 200, 200]),
            map: image(560, 220, [200, 220, 200], [90, 120, 160]),
            font: font.map(Arc::from),
        }
    }
}

/// Brand palettes: name, primary, accent, outer background.
const BRANDS: [(&str, &str, &str, &str); 10] = [
    ("Harbor & Pine", "#1b4d3e", "#e07a1f", "#f2efe9"),
    ("Lumen Weekly", "#23395b", "#d1495b", "#eef2f7"),
    ("Fjordline Travel", "#0b5563", "#f4a261", "#e9f5f5"),
    ("Copperleaf Market", "#7a3e1d", "#2a9d8f", "#faf3ea"),
    ("Northbound Digest", "#2b2d42", "#ef233c", "#edf2f4"),
    ("Kestrel Cycles", "#3d405b", "#81b29a", "#f4f1de"),
    ("Bramble Books", "#5f0f40", "#fb8b24", "#fdf6ec"),
    ("Tidewater Events", "#14213d", "#fca311", "#e5e5e5"),
    ("Orbit Coffee", "#432818", "#bb9457", "#f6eee3"),
    ("Quarry Supply", "#264653", "#e76f51", "#f1f5f4"),
];

const WORDS: [&str; 96] = [
    "the",
    "season",
    "garden",
    "notes",
    "weekend",
    "simple",
    "guide",
    "fresh",
    "morning",
    "coffee",
    "trail",
    "city",
    "small",
    "better",
    "routine",
    "update",
    "community",
    "new",
    "local",
    "design",
    "quiet",
    "ideas",
    "hands",
    "craft",
    "workshop",
    "light",
    "evening",
    "practical",
    "story",
    "market",
    "travel",
    "journey",
    "coast",
    "mountain",
    "river",
    "bread",
    "kitchen",
    "winter",
    "summer",
    "autumn",
    "spring",
    "planning",
    "careful",
    "honest",
    "useful",
    "reading",
    "list",
    "letters",
    "making",
    "repair",
    "tools",
    "build",
    "together",
    "slow",
    "clear",
    "bright",
    "steady",
    "open",
    "studio",
    "weekly",
    "favourite",
    "behind",
    "scenes",
    "process",
    "materials",
    "sources",
    "measure",
    "small-batch",
    "neighbours",
    "volunteer",
    "festival",
    "harbour",
    "village",
    "library",
    "station",
    "bicycle",
    "route",
    "picnic",
    "season's",
    "collection",
    "release",
    "preview",
    "member",
    "events",
    "schedule",
    "program",
    "careful",
    "patient",
    "work",
    "learn",
    "share",
    "walk",
    "visit",
    "return",
    "around",
    "through",
];

const CTAS: [&str; 6] = [
    "Read more",
    "Shop now",
    "Reserve a seat",
    "See the route",
    "Get the recipe",
    "View details",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }
}

struct Gen<'a> {
    rng: Rng,
    id: String,
    origin: &'a str,
    assets: &'a Assets,
    parts: Vec<Part>,
    brand: (&'static str, &'static str, &'static str, &'static str),
}

impl Gen<'_> {
    fn words(&mut self, count: u64) -> String {
        (0..count)
            .map(|_| WORDS[self.rng.below(WORDS.len() as u64) as usize])
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn title(&mut self) -> String {
        let count = self.rng.range(3, 7);
        capitalize(&self.words(count))
    }

    fn sentence(&mut self) -> String {
        let count = self.rng.range(8, 18);
        format!("{}.", capitalize(&self.words(count)))
    }

    fn paragraph(&mut self) -> String {
        let count = self.rng.range(2, 5);
        (0..count)
            .map(|_| self.sentence())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn link(&mut self) -> String {
        format!("https://example.com/{}/{}", self.id, self.rng.below(10_000))
    }

    fn cta(&mut self) -> &'static str {
        CTAS[self.rng.below(CTAS.len() as u64) as usize]
    }

    fn cid(&mut self, name: &str, data: Arc<[u8]>, content_type: &'static str) -> String {
        let cid = format!("{name}@{}.corpus.test", self.id);
        self.parts.push(Part {
            cid: cid.clone(),
            content_type,
            data,
        });
        format!("cid:{cid}")
    }

    fn image_cid(&mut self, name: &str, which: fn(&Assets) -> &Arc<[u8]>) -> String {
        let data = Arc::clone(which(self.assets));
        self.cid(name, data, "image/png")
    }

    fn remote(&self, name: &str) -> String {
        format!("{}/nl/{}/{name}", self.origin, self.id)
    }

    fn head(&mut self, title: &str, extra_css: &str) -> String {
        let mut head = String::from(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">\n\
             <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:v=\"urn:schemas-microsoft-com:vml\" xmlns:o=\"urn:schemas-microsoft-com:office:office\">\n<head>\n\
             <meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n\
             <meta name=\"x-apple-disable-message-reformatting\">\n",
        );
        let _ = writeln!(head, "<title>{title}</title>");
        head.push_str(
            "<!--[if mso]><xml><o:OfficeDocumentSettings><o:PixelsPerInch>96</o:PixelsPerInch></o:OfficeDocumentSettings></xml><![endif]-->\n\
             <style type=\"text/css\">\n\
             body, table, td, a { -webkit-text-size-adjust: 100%; -ms-text-size-adjust: 100%; }\n\
             table, td { mso-table-lspace: 0pt; mso-table-rspace: 0pt; }\n\
             img { -ms-interpolation-mode: bicubic; border: 0; outline: none; text-decoration: none; }\n\
             body { margin: 0 !important; padding: 0 !important; width: 100% !important; }\n\
             a[x-apple-data-detectors] { color: inherit !important; text-decoration: none !important; }\n",
        );
        head.push_str(extra_css);
        head.push_str(
            "\n@media screen and (max-width: 620px) {\n\
             .container { width: 100% !important; }\n\
             .stack { display: block !important; width: 100% !important; }\n\
             .hide-mobile { display: none !important; }\n\
             .pad { padding-left: 16px !important; padding-right: 16px !important; }\n}\n\
             </style>\n</head>\n",
        );
        head
    }

    fn preheader(&mut self) -> String {
        let text = self.sentence();
        format!(
            "<div style=\"display:none;font-size:1px;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden;mso-hide:all;\">{text}</div>\n"
        )
    }

    fn button(&mut self, background: &str) -> String {
        let link = self.link();
        let label = self.cta();
        format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
             <td bgcolor=\"{background}\" style=\"border-radius:4px;\">\
             <a href=\"{link}\" target=\"_blank\" style=\"display:inline-block;padding:12px 24px;font:bold 15px Arial,Helvetica,sans-serif;color:#ffffff;text-decoration:none;border-radius:4px;\">{label}</a>\
             </td></tr></table>"
        )
    }

    fn footer(&mut self) -> String {
        let (brand, ..) = self.brand;
        let mut icons = String::new();
        for network in ["social-a", "social-b", "social-c"] {
            let link = self.link();
            let src = self.remote(&format!("{network}.png"));
            let _ = write!(
                icons,
                "<a href=\"{link}\" style=\"display:inline-block;margin:0 6px;\"><img src=\"{src}\" width=\"24\" height=\"24\" alt=\"{network}\" style=\"display:block;\"></a>"
            );
        }
        let (unsubscribe, preferences) = (self.link(), self.link());
        let pixel = self.remote("open.gif");
        format!(
            "<tr><td align=\"center\" style=\"padding:24px;background-color:#f0f0f0;font:12px/18px Arial,Helvetica,sans-serif;color:#777777;\">\
             {icons}\
             <p style=\"margin:12px 0 4px;\">{brand} &middot; 1 Example Street &middot; Exampleville</p>\
             <p style=\"margin:0;\">You are receiving this because you subscribed on example.com.<br>\
             <a href=\"{unsubscribe}\" style=\"color:#777777;\">Unsubscribe</a> &middot; <a href=\"{preferences}\" style=\"color:#777777;\">Preferences</a></p>\
             <img src=\"{pixel}\" width=\"1\" height=\"1\" alt=\"\" style=\"display:block;border:0;\">\
             </td></tr>"
        )
    }

    fn article(&mut self, serif: bool) -> String {
        let (_, primary, accent, _) = self.brand;
        let title = self.title();
        let paragraph = self.paragraph();
        let button = self.button(accent);
        let font = if serif {
            "Georgia,'Times New Roman',serif"
        } else {
            "Helvetica,Arial,sans-serif"
        };
        format!(
            "<tr><td class=\"pad\" style=\"padding:24px 32px;font-family:{font};\">\
             <h2 style=\"margin:0 0 12px;font-size:22px;line-height:28px;color:{primary};\">{title}</h2>\
             <p style=\"margin:0 0 16px;font-size:16px;line-height:24px;color:#333333;\">{paragraph}</p>{button}</td></tr>\n"
        )
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

type Family = fn(&mut Gen, usize) -> (String, String);

const FAMILIES: [(&str, Family); 11] = [
    ("classic", classic),
    ("two-column", two_column),
    ("product-grid", product_grid),
    ("receipt", receipt),
    ("digest", digest),
    ("event", event),
    ("dark-promo", dark_promo),
    ("responsive", responsive),
    ("legacy", legacy),
    ("long-article", long_article),
    ("quirky", quirky),
];

/// Variants per family; 11 families give 55 newsletters.
pub const VARIANTS: usize = 5;

/// The newsletters. `origin` is the canary origin that remote images point at.
pub fn newsletters(origin: &str, assets: &Assets) -> Vec<Message> {
    let mut messages = Vec::new();
    for (family_index, (family, build)) in FAMILIES.iter().enumerate() {
        for variant in 0..VARIANTS {
            let number = family_index * VARIANTS + variant + 1;
            let id = format!("nl-{number:02}");
            let brand = BRANDS[(family_index * 3 + variant) % BRANDS.len()];
            let mut generator = Gen {
                rng: Rng(
                    0x9e37_79b9_7f4a_7c15 ^ (number as u64).wrapping_mul(0x2545_f491_4f6c_dd1d)
                ),
                id: id.clone(),
                origin,
                assets,
                parts: Vec::new(),
                brand,
            };
            let (subject, body) = build(&mut generator, variant);
            messages.push(Message {
                id,
                kind: Kind::Newsletter,
                family,
                sender: brand.0.to_owned(),
                subject,
                body,
                parts: generator.parts,
            });
        }
    }
    messages
}

fn classic(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, _, _, background) = g.brand;
    let subject = format!("{} — {}", brand, g.title());
    let mut html = g.head(&subject, "h2 { mso-line-height-rule: exactly; }");
    let _ = writeln!(
        html,
        "<body style=\"margin:0;padding:0;background-color:{background};\">"
    );
    html.push_str(&g.preheader());
    let logo = g.image_cid("logo", |a| &a.logo);
    let hero = if variant.is_multiple_of(2) {
        g.image_cid("hero", |a| &a.hero)
    } else {
        g.remote("hero.jpg")
    };
    let browser_link = g.link();
    let _ = write!(
        html,
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" bgcolor=\"{background}\">\n\
         <tr><td align=\"center\" style=\"padding:24px 0;\">\n\
         <table role=\"presentation\" class=\"container\" width=\"600\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"background-color:#ffffff;border-radius:6px;\">\n\
         <tr><td align=\"right\" style=\"padding:12px 24px;font:12px Arial,Helvetica,sans-serif;color:#888888;\"><a href=\"{browser_link}\" style=\"color:#888888;\">View in browser</a></td></tr>\n\
         <tr><td align=\"center\" style=\"padding:16px 24px;\"><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\" style=\"display:block;\"></td></tr>\n\
         <tr><td><img src=\"{hero}\" width=\"600\" height=\"260\" alt=\"\" style=\"display:block;width:100%;max-width:600px;height:auto;\"></td></tr>\n"
    );
    for _ in 0..(2 + variant) {
        html.push_str(&g.article(variant % 2 == 1));
    }
    html.push_str(&g.footer());
    html.push_str("\n</table>\n</td></tr>\n</table>\n</body>\n</html>\n");
    (subject, html)
}

fn two_column(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, accent, background) = g.brand;
    let subject = format!("This week at {brand}");
    let mut html = g.head(&subject, ".col h3 { margin: 8px 0; }");
    let _ = writeln!(html, "<body bgcolor=\"{background}\" style=\"margin:0;\">");
    html.push_str(&g.preheader());
    let logo = g.image_cid("logo", |a| &a.logo);
    let _ = write!(
        html,
        "<center><table role=\"presentation\" width=\"100%\" bgcolor=\"{background}\" cellpadding=\"0\" cellspacing=\"0\"><tr><td align=\"center\">\n\
         <table class=\"container\" width=\"600\" bgcolor=\"#ffffff\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\n\
         <tr><td style=\"padding:20px;border-bottom:4px solid {accent};\"><table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
         <td><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\"></td>\
         <td align=\"right\" style=\"font:13px Arial,sans-serif;color:{primary};\">Issue {}</td></tr></table></td></tr>\n",
        100 + variant
    );
    for row in 0..(2 + variant) {
        html.push_str("<tr><td style=\"padding:16px 20px;\"><table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\n");
        for column in 0..2 {
            let thumb = g.image_cid(&format!("thumb-{row}-{column}"), |a| &a.thumb);
            let title = g.title();
            let text = g.sentence();
            let link = g.link();
            let _ = writeln!(
                html,
                "<td class=\"stack col\" width=\"280\" valign=\"top\" style=\"padding:0 {}px 0 {}px;\">\
                 <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr><td>\
                 <a href=\"{link}\"><img src=\"{thumb}\" width=\"270\" height=\"180\" alt=\"\" style=\"display:block;width:100%;height:auto;border-radius:4px;\"></a></td></tr>\
                 <tr><td style=\"font:15px/22px Helvetica,Arial,sans-serif;color:#444444;\"><h3 style=\"color:{primary};font-size:18px;\">{title}</h3>{text}</td></tr></table></td>",
                if column == 0 { 10 } else { 0 },
                if column == 1 { 10 } else { 0 },
            );
        }
        html.push_str("</tr></table></td></tr>\n");
    }
    html.push_str(&g.footer());
    html.push_str("</table></td></tr></table></center>\n</body></html>\n");
    (subject, html)
}

fn product_grid(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, accent, background) = g.brand;
    let subject = format!("{brand}: new arrivals inside");
    let mut html = g.head(
        &subject,
        ".price { font-weight: bold; } .hero-text { text-shadow: 0 1px 2px rgba(0,0,0,.4); }",
    );
    let _ = writeln!(html, "<body style=\"margin:0;background:{background};\">");
    html.push_str(&g.preheader());
    let banner = g.image_cid("banner", |a| &a.banner);
    let logo = g.image_cid("logo", |a| &a.logo);
    let headline = g.title();
    let _ = write!(
        html,
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:{background};\"><tr><td align=\"center\" style=\"padding:20px 0;\">\n\
         <table class=\"container\" width=\"600\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#ffffff;\">\n\
         <tr><td align=\"center\" style=\"padding:18px;\"><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\"></td></tr>\n\
         <tr><td background=\"{banner}\" bgcolor=\"{primary}\" valign=\"middle\" style=\"background-image:url('{banner}');background-size:cover;background-position:center;padding:70px 24px;\" align=\"center\">\n\
         <!--[if gte mso 9]><v:rect xmlns:v=\"urn:schemas-microsoft-com:vml\" fill=\"true\" stroke=\"false\" style=\"width:600px;height:200px;\"><v:fill type=\"tile\" src=\"{banner}\" color=\"{primary}\" /><v:textbox inset=\"0,0,0,0\"><![endif]-->\n\
         <div class=\"hero-text\" style=\"font:bold 30px/36px Helvetica,Arial,sans-serif;color:#ffffff;\">{headline}</div>\n\
         <!--[if gte mso 9]></v:textbox></v:rect><![endif]-->\n</td></tr>\n"
    );
    for row in 0..(1 + variant.min(3)) {
        html.push_str("<tr><td style=\"padding:12px;\"><table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\n");
        for column in 0..3 {
            let image = if (row + column + variant).is_multiple_of(4) {
                g.remote(&format!("product-{row}-{column}.jpg"))
            } else {
                g.image_cid(&format!("product-{row}-{column}"), |a| &a.product)
            };
            let name = capitalize(&g.words(2));
            let price = 10 + g.rng.below(190);
            let button = g.button(accent);
            let _ = writeln!(
                html,
                "<td class=\"stack\" width=\"33%\" valign=\"top\" align=\"center\" style=\"padding:8px;font:14px Helvetica,Arial,sans-serif;color:#333333;\">\
                 <img src=\"{image}\" width=\"170\" height=\"170\" alt=\"{name}\" style=\"display:block;margin:0 auto 8px;\">\
                 <div>{name}</div><div class=\"price\" style=\"color:{primary};margin:4px 0 10px;\">{price}.00 kr</div>{button}</td>"
            );
        }
        html.push_str("</tr></table></td></tr>\n");
    }
    html.push_str(&g.footer());
    html.push_str("</table></td></tr></table>\n</body></html>\n");
    (subject, html)
}

fn receipt(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, _, background) = g.brand;
    let order = 40_000 + g.rng.below(50_000);
    let subject = format!("Your {brand} order #{order}");
    let mut html = g.head(
        &subject,
        ".num { font-family: 'Courier New', Courier, monospace; text-align: right; white-space: nowrap; }",
    );
    let logo = g.image_cid("logo", |a| &a.logo);
    let _ = write!(
        html,
        "<body style=\"margin:0;background-color:{background};font-family:Helvetica,Arial,sans-serif;\">\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"{background}\"><tr><td align=\"center\" style=\"padding:30px 10px;\">\n\
         <table class=\"container\" width=\"560\" cellpadding=\"0\" cellspacing=\"0\" style=\"background-color:#ffffff;border:1px solid #dddddd;\">\n\
         <tr><td style=\"padding:24px;\"><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\"></td></tr>\n\
         <tr><td style=\"padding:0 24px;color:#222222;\"><h1 style=\"font-size:22px;color:{primary};\">Thanks for your order</h1>\
         <p style=\"font-size:15px;line-height:22px;\">Order <b>#{order}</b> is confirmed. We will send another note when it ships.</p></td></tr>\n\
         <tr><td style=\"padding:12px 24px;\"><table width=\"100%\" cellpadding=\"6\" cellspacing=\"0\" style=\"border-collapse:collapse;font-size:14px;color:#333333;\">\n\
         <tr style=\"background-color:#f5f5f5;\"><th align=\"left\">Item</th><th>Qty</th><th class=\"num\">Price</th></tr>\n"
    );
    let mut total = 0;
    for line in 0..(3 + variant * 2) {
        let name = capitalize(&g.words(3));
        let quantity = 1 + g.rng.below(3);
        let price = 49 + g.rng.below(900);
        total += quantity * price;
        let shade = if line % 2 == 1 { "#fafafa" } else { "#ffffff" };
        let _ = writeln!(
            html,
            "<tr style=\"background-color:{shade};border-bottom:1px solid #eeeeee;\"><td>{name}</td><td align=\"center\">{quantity}</td><td class=\"num\">{price}.00</td></tr>"
        );
    }
    let icon = g.image_cid("card-icon", |a| &a.icon);
    let _ = write!(
        html,
        "<tr><td colspan=\"2\" align=\"right\"><b>Subtotal</b></td><td class=\"num\">{total}.00</td></tr>\
         <tr><td colspan=\"2\" align=\"right\">Shipping</td><td class=\"num\">0.00</td></tr>\
         <tr><td colspan=\"2\" align=\"right\" style=\"font-size:16px;\"><b>Total</b></td><td class=\"num\" style=\"font-size:16px;color:{primary};\"><b>{total}.00 kr</b></td></tr>\n\
         </table></td></tr>\n\
         <tr><td style=\"padding:12px 24px;\"><table width=\"100%\"><tr>\
         <td class=\"stack\" width=\"50%\" valign=\"top\" style=\"font-size:13px;line-height:19px;color:#555555;\"><b>Ship to</b><br>A. Customer<br>1 Example Road<br>0000 Exampleville</td>\
         <td class=\"stack\" width=\"50%\" valign=\"top\" style=\"font-size:13px;line-height:19px;color:#555555;\"><img src=\"{icon}\" width=\"16\" height=\"16\" alt=\"\" style=\"vertical-align:middle;\"> <b>Payment</b><br>Card ending 0000<br>Paid in full</td></tr></table></td></tr>\n"
    );
    html.push_str(&g.footer());
    html.push_str("</table></td></tr></table>\n</body></html>\n");
    (subject, html)
}

fn digest(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, accent, background) = g.brand;
    let subject = format!("{brand} daily digest");
    let mut html = g.head(
        &subject,
        "a.item:hover { text-decoration: underline !important; }",
    );
    let _ = writeln!(
        html,
        "<body style=\"margin:0;padding:0;background-color:{background};\">"
    );
    html.push_str(&g.preheader());
    let logo = g.image_cid("logo", |a| &a.logo);
    let _ = write!(
        html,
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr><td align=\"center\">\n\
         <table class=\"container\" width=\"620\" cellpadding=\"0\" cellspacing=\"0\" style=\"background:#ffffff;margin:16px 0;\">\n\
         <tr><td style=\"padding:16px 24px;\"><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\"></td></tr>\n\
         <tr><td style=\"padding:20px 24px;background-color:{primary};color:#ffffff;font:bold 20px Helvetica,Arial,sans-serif;\">{brand}<span style=\"float:right;font-weight:normal;font-size:13px;color:{accent};\">Top stories</span></td></tr>\n"
    );
    for item in 0..(18 + variant * 6) {
        let title = g.title();
        let link = g.link();
        let source = capitalize(&g.words(1));
        let minutes = 2 + g.rng.below(14);
        let shade = if item % 2 == 0 { "#ffffff" } else { "#f7f8fa" };
        let _ = writeln!(
            html,
            "<tr><td bgcolor=\"{shade}\" style=\"padding:10px 24px;border-bottom:1px solid #eeeeee;font:15px/21px Helvetica,Arial,sans-serif;\">\
             <a class=\"item\" href=\"{link}\" style=\"color:{primary};font-weight:bold;text-decoration:none;\">{title}</a><br>\
             <span style=\"color:#777777;font-size:13px;\">{source} &middot; {minutes} min read</span></td></tr>"
        );
    }
    html.push_str(&g.footer());
    html.push_str("</table></td></tr></table>\n</body></html>\n");
    (subject, html)
}

fn event(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, accent, background) = g.brand;
    let subject = format!("You're invited: {}", g.title());
    let font = g
        .assets
        .font
        .clone()
        .map(|font| g.cid("display-font", font, "font/ttf"));
    let mut css = String::new();
    if let Some(font) = &font {
        let _ = write!(
            css,
            "@font-face {{ font-family: 'S2 Display'; src: url('{font}') format('truetype'); font-weight: normal; }}\n\
             .display {{ font-family: 'S2 Display', Georgia, serif; }}\n"
        );
    }
    let _ = write!(
        css,
        "@import url('{}');\n.agenda td {{ border-top: 1px solid #e3e3e3; }}",
        g.remote("fonts.css")
    );
    let mut html = g.head(&subject, &css);
    let map = g.image_cid("map", |a| &a.map);
    let logo = g.image_cid("logo", |a| &a.logo);
    let day = 1 + g.rng.below(28);
    let intro = g.paragraph();
    let _ = write!(
        html,
        "<body style=\"margin:0;background-color:{background};\">\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"{background}\"><tr><td align=\"center\" style=\"padding:24px 8px;\">\n\
         <table class=\"container\" width=\"600\" cellpadding=\"0\" cellspacing=\"0\" style=\"background-color:#ffffff;border-top:6px solid {accent};\">\n\
         <tr><td style=\"padding:24px;\"><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\"></td></tr>\n\
         <tr><td style=\"padding:0 24px;\"><table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
         <td width=\"120\" align=\"center\" valign=\"top\" style=\"background-color:{primary};color:#ffffff;padding:16px 0;\">\
         <div class=\"display\" style=\"font-size:48px;line-height:52px;\">{day}</div><div style=\"font:13px Arial,sans-serif;letter-spacing:2px;\">OCTOBER</div></td>\
         <td valign=\"top\" style=\"padding-left:20px;font:16px/24px Georgia,serif;color:#333333;\"><h1 class=\"display\" style=\"margin:0 0 8px;font-size:30px;color:{primary};font-weight:normal;\">{subject}</h1>{intro}</td>\
         </tr></table></td></tr>\n\
         <tr><td style=\"padding:20px 24px;\"><table class=\"agenda\" width=\"100%\" cellpadding=\"8\" cellspacing=\"0\" style=\"font:14px Helvetica,Arial,sans-serif;color:#444444;\">\n"
    );
    for slot in 0..(4 + variant) {
        let topic = g.title();
        let _ = writeln!(
            html,
            "<tr><td width=\"70\" style=\"color:{primary};font-weight:bold;\">{:02}:{:02}</td><td>{topic}</td></tr>",
            9 + slot,
            if slot % 2 == 0 { 0 } else { 30 }
        );
    }
    let button = g.button(accent);
    let _ = write!(
        html,
        "</table></td></tr>\n\
         <tr><td style=\"padding:0 24px 20px;\"><img src=\"{map}\" width=\"552\" height=\"217\" alt=\"Map\" style=\"display:block;width:100%;height:auto;border:1px solid #dddddd;\"></td></tr>\n\
         <tr><td align=\"center\" style=\"padding:0 24px 28px;\">{button}</td></tr>\n"
    );
    html.push_str(&g.footer());
    html.push_str("</table></td></tr></table>\n</body></html>\n");
    (subject, html)
}

fn dark_promo(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, _, accent, _) = g.brand;
    let subject = format!("{brand} after dark: {}", g.title());
    let mut html = g.head(
        &subject,
        "@media (prefers-color-scheme: dark) { .invert { color: #ffffff !important; } }\n.glow { letter-spacing: 3px; }",
    );
    let banner = g.image_cid("banner", |a| &a.banner);
    let headline = g.title();
    let _ = write!(
        html,
        "<body bgcolor=\"#101014\" text=\"#eeeeee\" style=\"margin:0;background-color:#101014;color:#eeeeee;\">\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"#101014\"><tr><td align=\"center\">\n\
         <table class=\"container\" width=\"600\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"#18181f\">\n\
         <tr><td align=\"center\" class=\"glow\" style=\"padding:22px;font:bold 14px Helvetica,Arial,sans-serif;color:{accent};\">{}</td></tr>\n\
         <tr><td><img src=\"{banner}\" width=\"600\" height=\"200\" alt=\"\" style=\"display:block;width:100%;height:auto;\"></td></tr>\n\
         <tr><td align=\"center\" class=\"invert\" style=\"padding:26px 30px;font:bold 28px/34px Helvetica,Arial,sans-serif;color:#f5f5f5;\">{headline}</td></tr>\n",
        brand.to_uppercase()
    );
    for _ in 0..(2 + variant) {
        let text = g.paragraph();
        let button = g.button(accent);
        let _ = writeln!(
            html,
            "<tr><td style=\"padding:12px 30px 24px;font:16px/25px Helvetica,Arial,sans-serif;color:#cfcfd6;\">{text}<br><br>{button}</td></tr>"
        );
    }
    let pixel = g.remote("dark-open.gif");
    let _ = write!(
        html,
        "<tr><td align=\"center\" style=\"padding:24px;font:12px Arial,sans-serif;color:#8a8a99;\">{brand} &middot; Exampleville<br><a href=\"https://example.com/u\" style=\"color:#8a8a99;\">Unsubscribe</a>\
         <img src=\"{pixel}\" width=\"1\" height=\"1\" alt=\"\"></td></tr>\n</table></td></tr></table>\n</body></html>\n"
    );
    (subject, html)
}

fn responsive(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, accent, background) = g.brand;
    let subject = format!("{brand} notes: {}", g.title());
    let mut css = format!(
        ".wrap {{ max-width: 600px; margin: 0 auto; background: #ffffff; }}\n\
         .row {{ font-size: 0; }}\n\
         .col {{ display: inline-block; width: 50%; vertical-align: top; font-size: 16px; box-sizing: border-box; padding: 12px 20px; }}\n\
         .tag {{ display: inline-block; padding: 2px 8px; border-radius: 10px; background: {accent}; color: #ffffff; font-size: 12px; }}\n\
         h1, h2 {{ color: {primary}; }}\n\
         @media only screen and (max-width: 600px) {{ .col {{ display: block !important; width: 100% !important; }} }}\n"
    );
    if variant.is_multiple_of(2) {
        let _ = write!(css, "@import url('{}');", g.remote("modern.css"));
    }
    let mut html = g.head(&subject, &css);
    let logo = g.image_cid("logo", |a| &a.logo);
    let hero = g.image_cid("hero", |a| &a.hero);
    let lead = g.paragraph();
    let _ = write!(
        html,
        "<body style=\"margin:0;padding:24px 0;background:{background};font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#2d2d2d;\">\n\
         <div class=\"wrap\" role=\"article\" aria-label=\"{subject}\">\n\
         <div style=\"padding:20px;\"><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\"></div>\n\
         <img src=\"{hero}\" alt=\"\" style=\"display:block;width:100%;height:auto;\">\n\
         <div style=\"padding:8px 20px;\"><h1 style=\"font-size:26px;\">{subject}</h1><p style=\"line-height:1.6;\">{lead}</p></div>\n"
    );
    for _ in 0..(2 + variant) {
        html.push_str("<div class=\"row\">");
        for _ in 0..2 {
            let title = g.title();
            let text = g.paragraph();
            let tag = capitalize(&g.words(1));
            let link = g.link();
            let _ = write!(
                html,
                "<div class=\"col\"><span class=\"tag\">{tag}</span><h2 style=\"font-size:19px;margin:8px 0;\">{title}</h2><p style=\"line-height:1.55;margin:0 0 8px;\">{text}</p><a href=\"{link}\" style=\"color:{accent};\">Continue &rarr;</a></div>"
            );
        }
        html.push_str("</div>\n");
    }
    let pixel = g.remote("r.gif");
    let _ = write!(
        html,
        "<div style=\"padding:20px;font-size:12px;color:#888888;border-top:1px solid #eeeeee;\">{brand}, Exampleville. <a href=\"https://example.com/unsubscribe\" style=\"color:#888888;\">Unsubscribe</a></div>\n\
         </div>\n<img src=\"{pixel}\" width=\"1\" height=\"1\" alt=\"\">\n</body></html>\n"
    );
    (subject, html)
}

fn legacy(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, ..) = g.brand;
    let subject = format!("{brand} Newsletter No. {}", 7 + variant);
    let logo = g.image_cid("logo", |a| &a.logo);
    let spacer = g.remote("spacer.gif");
    let mut html = format!(
        "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\"><title>{subject}</title></head>\n\
         <body bgcolor=\"#ffffee\" text=\"#000000\" link=\"#0000cc\" vlink=\"#551a8b\" alink=\"#ff0000\" leftmargin=\"0\" topmargin=\"0\" marginwidth=\"0\" marginheight=\"0\">\n\
         <!--[if gte mso 9]><style>td {{ font-family: Arial, sans-serif; }}</style><![endif]-->\n\
         <center><table width=\"600\" border=\"1\" cellpadding=\"4\" cellspacing=\"0\" bordercolor=\"#999966\">\n\
         <tr><td bgcolor=\"#333366\" align=\"center\"><img src=\"{logo}\" width=\"180\" height=\"50\" border=\"0\" alt=\"{brand}\"><br>\
         <font face=\"Verdana, Arial\" size=\"2\" color=\"#ffffcc\"><b>{}</b></font></td></tr>\n",
        brand.to_uppercase()
    );
    for section in 0..(3 + variant) {
        let title = g.title();
        let text = g.paragraph();
        let link = g.link();
        let _ = writeln!(
            html,
            "<tr><td><img src=\"{spacer}\" width=\"1\" height=\"8\" alt=\"\"><br>\
             <font face=\"Verdana, Arial\" size=\"3\" color=\"#333366\"><b>{}. {title}</b></font><br>\
             <font face=\"Verdana, Arial\" size=\"2\">{text} <a href=\"{link}\">More&hellip;</a></font><o:p></o:p></td></tr>",
            section + 1
        );
    }
    html.push_str(
        "<tr><td align=\"center\"><hr width=\"80%\" size=\"1\" noshade><font face=\"Verdana\" size=\"1\" color=\"#666666\">\
         To stop receiving this newsletter reply with UNSUBSCRIBE.</font></td></tr>\n</table></center>\n</body></html>\n",
    );
    (subject, html)
}

fn long_article(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, accent, _) = g.brand;
    let subject = g.title();
    let mut html = g.head(
        &subject,
        ".post p { margin: 0 0 18px; } .post blockquote { margin: 24px 0; padding: 0 0 0 18px; border-left: 4px solid #d8d8d8; font-style: italic; }",
    );
    let author = capitalize(&g.words(2));
    let _ = write!(
        html,
        "<body style=\"margin:0;background-color:#ffffff;\">\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr><td align=\"center\">\n\
         <table class=\"container post\" width=\"620\" cellpadding=\"0\" cellspacing=\"0\" style=\"font:18px/29px Georgia,'Times New Roman',serif;color:#242424;\">\n\
         <tr><td style=\"padding:32px 20px 8px;\"><div style=\"font:bold 13px Helvetica,Arial,sans-serif;color:{accent};text-transform:uppercase;\">{brand}</div>\
         <h1 style=\"font-size:34px;line-height:40px;margin:10px 0;color:{primary};\">{subject}</h1>\
         <div style=\"font:14px Helvetica,Arial,sans-serif;color:#757575;\">By {author} &middot; {} min read</div></td></tr>\n<tr><td style=\"padding:16px 20px;\">\n",
        12 + variant * 4
    );
    for paragraph in 0..(30 + variant * 10) {
        if paragraph % 9 == 4 {
            let quote = g.sentence();
            let _ = writeln!(html, "<blockquote>{quote}</blockquote>");
        } else if paragraph % 13 == 7 {
            let image = g.image_cid(&format!("figure-{paragraph}"), |a| &a.hero);
            let caption = g.sentence();
            let _ = writeln!(
                html,
                "<figure style=\"margin:24px 0;\"><img src=\"{image}\" width=\"580\" height=\"251\" alt=\"\" style=\"width:100%;height:auto;\"><figcaption style=\"font:13px Helvetica,Arial,sans-serif;color:#757575;\">{caption}</figcaption></figure>"
            );
        }
        let text = g.paragraph();
        let _ = writeln!(html, "<p>{text}</p>");
    }
    html.push_str("</td></tr>\n");
    html.push_str(&g.footer());
    html.push_str("</table></td></tr></table>\n</body></html>\n");
    (subject, html)
}

fn quirky(g: &mut Gen, variant: usize) -> (String, String) {
    let (brand, primary, accent, background) = g.brand;
    let subject = format!("{brand}: {}", g.title());
    let css = match variant {
        0 => "html, body { height: 100%; } .full { height: 100%; }".to_owned(),
        1 => "body { min-height: 100vh; } .hero { min-height: 40vh; }".to_owned(),
        2 => ".clear-none:after { content: none; } .float { float: left; width: 46%; margin: 2%; }"
            .to_owned(),
        3 => ".badge { position: absolute; top: 12px; right: 12px; } .pull { margin-top: -20px; }"
            .to_owned(),
        _ => "pre { white-space: pre; font-size: 12px; }".to_owned(),
    };
    let mut html = g.head(&subject, &css);
    let logo = g.image_cid("logo", |a| &a.logo);
    let _ = write!(
        html,
        "<body style=\"margin:0;background:{background};\">\n\
         <table class=\"full\" role=\"presentation\" width=\"100%\" height=\"100%\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"{background}\"><tr><td align=\"center\" valign=\"top\">\n\
         <div style=\"position:relative;max-width:600px;background:#ffffff;text-align:left;\">\n\
         <div style=\"padding:18px;\"><img src=\"{logo}\" width=\"180\" height=\"50\" alt=\"{brand}\"></div>\n"
    );
    match variant {
        1 => {
            let title = g.title();
            let _ = writeln!(
                html,
                "<div class=\"hero\" style=\"background:{primary};color:#ffffff;padding:30px;font:bold 26px Helvetica,sans-serif;\">{title}</div>"
            );
        }
        2 => {
            html.push_str("<div class=\"clear-none\" style=\"overflow:hidden;\">");
            for _ in 0..4 {
                let text = g.paragraph();
                let _ = write!(
                    html,
                    "<div class=\"float\" style=\"font:15px/22px Helvetica,sans-serif;color:#333333;\">{text}</div>"
                );
            }
            html.push_str("</div>\n");
        }
        3 => {
            let _ = writeln!(
                html,
                "<div class=\"badge\" style=\"background:{accent};color:#ffffff;padding:4px 10px;border-radius:12px;font:bold 12px Helvetica,sans-serif;\">NEW</div>"
            );
        }
        4 => {
            let wide = (0..6).map(|_| g.words(16)).collect::<Vec<_>>().join("\n");
            let _ = writeln!(
                html,
                "<pre style=\"background:#f6f6f6;color:#333333;padding:12px;overflow:auto;\">{wide}</pre>"
            );
        }
        _ => {}
    }
    for _ in 0..3 {
        let title = g.title();
        let text = g.paragraph();
        let _ = writeln!(
            html,
            "<div class=\"pull\" style=\"padding:20px;font:16px/24px Helvetica,Arial,sans-serif;color:#333333;\"><h2 style=\"color:{primary};\">{title}</h2>{text}</div>"
        );
    }
    let pixel = g.remote("q.gif");
    let _ = write!(
        html,
        "<div style=\"padding:16px;font:12px Arial,sans-serif;color:#888888;\">{brand} &middot; <a href=\"https://example.com/u\" style=\"color:#888888;\">Unsubscribe</a><img src=\"{pixel}\" width=\"1\" height=\"1\" alt=\"\"></div>\n\
         </div></td></tr></table>\n</body></html>\n"
    );
    (subject, html)
}

const PEOPLE: [&str; 6] = [
    "Alex Example",
    "Sam Sample",
    "Robin Test",
    "Kim Placeholder",
    "Jo Fixture",
    "Max Mock",
];

/// Personal-style HTML with no colours of its own: the kind that takes the theme.
pub fn simple_html() -> Vec<Message> {
    let mut rng = Rng(0x51_3ac0_ffee);
    let mut gen_text = |count: u64| {
        (0..count)
            .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut bodies: Vec<(String, String)> = Vec::new();
    bodies.push((
        "Plans for Saturday".into(),
        format!(
            "<div dir=\"ltr\">Hi,<br><br>{}.<br><br>Does <b>ten</b> work? The route is <a href=\"https://example.com/route\">here</a>.<br><br>Alex</div>\
             <br><div class=\"gmail_quote\"><div dir=\"ltr\" class=\"gmail_attr\">On Fri, Sam Sample wrote:<br></div>\
             <blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px solid;padding-left:1ex\">{}.<br>{}.</blockquote></div>",
            capitalize(&gen_text(20)),
            capitalize(&gen_text(14)),
            capitalize(&gen_text(9))
        ),
    ));
    bodies.push((
        "Meeting notes".into(),
        format!(
            "<html><body><p>Notes from today:</p><ul><li>{}</li><li>{}</li><li>{}</li></ul>\
             <ol><li>{}</li><li>{}</li></ol><p>Next meeting in two weeks.</p><p>-- <br>Robin</p></body></html>",
            capitalize(&gen_text(8)),
            capitalize(&gen_text(11)),
            capitalize(&gen_text(6)),
            capitalize(&gen_text(9)),
            capitalize(&gen_text(7))
        ),
    ));
    bodies.push((
        "Numbers for the report".into(),
        format!(
            "<p>Here are the figures:</p><table border=\"1\" cellpadding=\"4\"><tr><th>Quarter</th><th>Visits</th><th>Orders</th></tr>\
             <tr><td>Q1</td><td>1200</td><td>87</td></tr><tr><td>Q2</td><td>1340</td><td>96</td></tr><tr><td>Q3</td><td>1515</td><td>120</td></tr></table>\
             <p>{}.</p>",
            capitalize(&gen_text(25))
        ),
    ));
    bodies.push((
        "Re: build error".into(),
        format!(
            "<div>The error comes from this part:</div><pre>fn main() {{\n    let value = compute();\n    println!(\"{{value}}\");\n}}</pre>\
             <div>{}.</div><blockquote type=\"cite\"><div>{}.</div></blockquote>",
            capitalize(&gen_text(18)),
            capitalize(&gen_text(12))
        ),
    ));
    bodies.push((
        "Photos".into(),
        format!(
            "<div>{}.</div><div><br></div><div>{}.</div><div><br></div><div>Kim</div>",
            capitalize(&gen_text(30)),
            capitalize(&gen_text(16))
        ),
    ));
    bodies.push((
        "Long reply".into(),
        (0..8)
            .map(|_| format!("<p>{}.</p>", capitalize(&gen_text(40))))
            .collect::<String>(),
    ));
    bodies.push((
        "Quick question".into(),
        format!(
            "<p>{}?</p><hr><p><small>Sent from a phone</small></p>",
            capitalize(&gen_text(12))
        ),
    ));
    bodies.push((
        "Reading list".into(),
        format!(
            "<h3>Reading list</h3><p>{}.</p><p><a href=\"https://example.org/a\">First</a> &middot; <a href=\"https://example.org/b\">Second</a></p><p><i>{}</i></p>",
            capitalize(&gen_text(22)),
            capitalize(&gen_text(10))
        ),
    ));
    bodies
        .into_iter()
        .enumerate()
        .map(|(index, (subject, body))| Message {
            id: format!("simple-{:02}", index + 1),
            kind: Kind::SimpleHtml,
            family: "simple",
            sender: PEOPLE[index % PEOPLE.len()].to_owned(),
            subject,
            body,
            parts: Vec::new(),
        })
        .collect()
}

/// Plain-text messages (approach A shows them in a GTK text view, not a WebView).
pub fn plain_text() -> Vec<Message> {
    let mut rng = Rng(0x7e57_7e57);
    let mut sentence = || {
        let count = rng.range(8, 20);
        let words: Vec<_> = (0..count)
            .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
            .collect();
        format!("{}.", capitalize(&words.join(" ")))
    };
    (0..6)
        .map(|index| {
            let mut body = format!("Hi,\n\n{} {}\n\n", sentence(), sentence());
            for _ in 0..(1 + index) {
                body.push_str(&format!("{} {} {}\n\n", sentence(), sentence(), sentence()));
            }
            if index % 2 == 1 {
                body.push_str(&format!(
                    "On Tuesday, Sam Sample wrote:\n> {}\n> {}\n>\n> {}\n\n",
                    sentence(),
                    sentence(),
                    sentence()
                ));
            }
            body.push_str("See https://example.com/notes for the rest.\n\n-- \nRobin Test\n");
            Message {
                id: format!("plain-{:02}", index + 1),
                kind: Kind::PlainText,
                family: "plain",
                sender: PEOPLE[(index + 2) % PEOPLE.len()].to_owned(),
                subject: format!("Notes {}", index + 1),
                body,
                parts: Vec::new(),
            }
        })
        .collect()
}

/// A very tall plain HTML body (for the texture-size and scrolling checks).
pub fn tall_message(paragraphs: usize) -> Message {
    let mut rng = Rng(0x7a11);
    let mut body = String::from("<html><body>");
    for index in 0..paragraphs {
        let count = rng.range(30, 60);
        let words: Vec<_> = (0..count)
            .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
            .collect();
        let _ = write!(body, "<p>{index}. {}.</p>", capitalize(&words.join(" ")));
    }
    body.push_str("</body></html>");
    Message {
        id: format!("tall-{paragraphs}"),
        kind: Kind::SimpleHtml,
        family: "tall",
        sender: "Max Mock".into(),
        subject: "A very long message".into(),
        body,
        parts: Vec::new(),
    }
}

/// A body exactly `height` CSS px tall with a gradient, text at both ends.
pub fn sized_message(height: u32) -> Message {
    Message {
        id: format!("height-{height}"),
        kind: Kind::SimpleHtml,
        family: "sized",
        sender: "Max Mock".into(),
        subject: format!("A body {height} px tall"),
        body: format!(
            "<!DOCTYPE html><html><body style=\"margin:0\"><div style=\"height:{height}px;\
             background:linear-gradient(#f4a261,#2a9d8f,#264653);font:20px sans-serif;\
             display:flex;flex-direction:column;justify-content:space-between\">\
             <p>Top of {height}</p><p>Bottom of {height}</p></div></body></html>"
        ),
        parts: Vec::new(),
    }
}
