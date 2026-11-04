//! A deterministic synthetic mailbox: fictional people on reserved `.example` domains,
//! Norwegian, English, Chinese and Japanese text, conversations with reply chains, newsletters,
//! receipts and junk. Nothing here comes from real mail.

use crate::rng::Rng;
use crate::store::{FtsDoc, NewMessage, Writer, flags};

pub const DAY: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    No,
    En,
    Zh,
    Ja,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Conversation,
    Newsletter,
    Notice,
    Junk,
    Draft,
}

pub struct MailboxSpec {
    pub name: &'static str,
    pub role: Option<&'static str>,
}

pub const MAILBOXES: [MailboxSpec; 7] = [
    MailboxSpec {
        name: "INBOX",
        role: Some("inbox"),
    },
    MailboxSpec {
        name: "Sent",
        role: Some("sent"),
    },
    MailboxSpec {
        name: "Archive",
        role: Some("archive"),
    },
    MailboxSpec {
        name: "Lister",
        role: None,
    },
    MailboxSpec {
        name: "Junk",
        role: Some("junk"),
    },
    MailboxSpec {
        name: "Trash",
        role: Some("trash"),
    },
    MailboxSpec {
        name: "Drafts",
        role: Some("drafts"),
    },
];
const INBOX: usize = 0;
const SENT: usize = 1;
const ARCHIVE: usize = 2;
const LISTS: usize = 3;
const JUNK: usize = 4;
const TRASH: usize = 5;
const DRAFTS: usize = 6;

pub const ME_NAME: &str = "Alex Berg";
pub const ME_ADDR: &str = "alex@home.example";

#[derive(Clone, Debug)]
pub struct Person {
    pub name: String,
    pub addr: String,
    pub lang: Lang,
}

/// What it takes to regenerate a message body.
#[derive(Clone, Debug)]
pub struct BodySpec {
    pub seed: u64,
    pub lang: Lang,
    pub kind: Kind,
    pub length: usize,
    pub greet: String,
    pub signer: String,
    pub quote: Option<Quote>,
}

#[derive(Clone, Debug)]
pub struct Quote {
    pub seed: u64,
    pub lang: Lang,
    pub kind: Kind,
    pub from: String,
}

#[derive(Clone, Debug)]
pub struct SynthMessage {
    pub mailbox: usize,
    pub header: NewMessage,
    pub body: BodySpec,
    pub recipients: String,
}

impl SynthMessage {
    pub fn doc(&self, id: i64) -> FtsDoc {
        FtsDoc {
            id,
            subject: self.header.subject.clone(),
            sender: format!("{} {}", self.header.from_name, self.header.from_addr),
            recipients: self.recipients.clone(),
            body: body(&self.body, usize::MAX),
        }
    }
}

pub struct Corpus {
    pub now: i64,
    pub messages: Vec<SynthMessage>,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub messages: usize,
    pub seed: u64,
    pub now: i64,
    pub years: f64,
}

const NO_FIRST: &[&str] = &[
    "Ola", "Kari", "Ingrid", "Lars", "Sigrid", "Bjørn", "Åse", "Øystein", "Kjersti", "Håkon",
    "Siv", "Tor", "Ragnhild", "Eirik", "Solveig", "Jørgen", "Marit", "Pål", "Liv", "Ståle", "Nora",
    "Emil", "Ida", "Henrik", "Sofie", "Magnus", "Thea", "Sindre", "Maja", "Even",
];
const NO_LAST: &[&str] = &[
    "Hansen",
    "Johansen",
    "Olsen",
    "Larsen",
    "Andersen",
    "Pedersen",
    "Nilsen",
    "Kristiansen",
    "Jensen",
    "Karlsen",
    "Johnsen",
    "Pettersen",
    "Eriksen",
    "Berg",
    "Haugen",
    "Hagen",
    "Bakken",
    "Dahl",
    "Lie",
    "Strand",
    "Solberg",
    "Moen",
    "Lund",
    "Sæther",
    "Våge",
    "Grønli",
    "Øverland",
    "Bråten",
];
const EN_FIRST: &[&str] = &[
    "James", "Emma", "Olivia", "Liam", "Noah", "Ava", "Sophia", "Mason", "Lucas", "Mia", "Ethan",
    "Grace", "Leah", "Daniel", "Chloe", "Samuel", "Hannah", "Owen", "Ruby", "Isaac",
];
const EN_LAST: &[&str] = &[
    "Smith", "Brown", "Taylor", "Wilson", "Clark", "Walker", "Wright", "Hughes", "Turner", "Hill",
    "Green", "Baker", "Carter", "Mitchell", "Reed", "Cooper", "Kim", "Patel", "Nguyen", "Lopez",
];
const ZH_NAMES: &[(&str, &str)] = &[
    ("王伟", "wang.wei"),
    ("李娜", "li.na"),
    ("张敏", "zhang.min"),
    ("刘洋", "liu.yang"),
    ("陈静", "chen.jing"),
    ("杨帆", "yang.fan"),
    ("赵磊", "zhao.lei"),
    ("黄丽", "huang.li"),
    ("周杰", "zhou.jie"),
    ("吴芳", "wu.fang"),
];
const JA_NAMES: &[(&str, &str)] = &[
    ("佐藤 健", "sato.ken"),
    ("鈴木 花子", "suzuki.hanako"),
    ("高橋 翔", "takahashi.sho"),
    ("田中 美咲", "tanaka.misaki"),
    ("伊藤 大輔", "ito.daisuke"),
    ("渡辺 結衣", "watanabe.yui"),
    ("山本 蓮", "yamamoto.ren"),
    ("中村 陽菜", "nakamura.hina"),
];
const DOMAINS: &[&str] = &[
    "example.com",
    "example.org",
    "example.net",
    "firma.example",
    "kommune.example",
    "skole.example",
    "studio.example",
    "nordlys.example",
    "fjord.example",
    "mail.example",
];
const NEWSLETTERS: &[(&str, &str)] = &[
    ("Northwind Weekly", "news@northwind.example"),
    ("Fjordposten", "nyhetsbrev@fjordposten.example"),
    ("Tech Digest", "digest@techdigest.example"),
    ("Bokklubben", "post@bokklubben.example"),
    ("Design Letters", "letters@designletters.example"),
    ("Turlaget", "info@turlaget.example"),
    ("Rust Notes", "notes@rustnotes.example"),
    ("Matglede", "oppskrifter@matglede.example"),
    ("Hagenytt", "post@hagenytt.example"),
    ("Sykkelbladet", "redaksjon@sykkelbladet.example"),
];
const SENDERS: &[(&str, &str)] = &[
    ("Fjellbekk Regnskap", "regnskap@fjellbekk.example"),
    ("Nordlys Energi", "faktura@nordlys-energi.example"),
    ("Bølgen Mobil", "kundeservice@bolgen.example"),
    ("Acme Shop", "orders@acme-shop.example"),
    ("Ledger CI", "ci@ledger.example"),
    ("Aker Bibliotek", "noreply@akerbibliotek.example"),
    ("Kvist Reiser", "booking@kvist-reiser.example"),
    ("Fjord Cycling Club", "rides@fjordcycling.example"),
    ("Northwind Parcel", "tracking@northwind-parcel.example"),
    ("Tindra Forsikring", "post@tindra.example"),
    ("Lumen Cloud", "billing@lumen-cloud.example"),
    ("Sentrum Tannklinikk", "time@sentrum-tann.example"),
];
const JUNK_SENDERS: &[(&str, &str)] = &[
    ("Prize Center", "winner@prize-center.example"),
    ("Super Deals", "offers@super-deals.example"),
    ("Konto Varsel", "varsel@konto-varsel.example"),
    ("Crypto Daily", "alpha@crypto-daily.example"),
];

const NO_WORDS: &[&str] = &[
    "og",
    "i",
    "det",
    "som",
    "en",
    "på",
    "er",
    "til",
    "med",
    "for",
    "av",
    "at",
    "ikke",
    "har",
    "de",
    "den",
    "jeg",
    "vi",
    "om",
    "et",
    "men",
    "så",
    "kan",
    "var",
    "fra",
    "vil",
    "seg",
    "skal",
    "han",
    "hun",
    "også",
    "eller",
    "bare",
    "når",
    "etter",
    "ut",
    "opp",
    "hva",
    "hvor",
    "alle",
    "noe",
    "nå",
    "må",
    "ble",
    "får",
    "mer",
    "her",
    "der",
    "godt",
    "bra",
    "takk",
    "møte",
    "møtet",
    "prosjekt",
    "prosjektet",
    "rapport",
    "rapporten",
    "budsjett",
    "budsjettet",
    "avtale",
    "avtalen",
    "kontrakt",
    "kontrakten",
    "faktura",
    "fakturaen",
    "betaling",
    "frist",
    "fristen",
    "uke",
    "uken",
    "dag",
    "dagen",
    "morgen",
    "fredag",
    "mandag",
    "tirsdag",
    "onsdag",
    "torsdag",
    "lørdag",
    "søndag",
    "kontoret",
    "kunde",
    "kunden",
    "teamet",
    "leder",
    "styret",
    "plan",
    "planen",
    "forslag",
    "forslaget",
    "endring",
    "endringer",
    "oppdatering",
    "utkast",
    "vedlegg",
    "vedlagt",
    "dokument",
    "dokumentet",
    "tilbakemelding",
    "spørsmål",
    "svar",
    "beskjed",
    "informasjon",
    "kommentar",
    "kommentarer",
    "gjennomgang",
    "status",
    "neste",
    "steg",
    "tid",
    "tidspunkt",
    "sted",
    "lokale",
    "rom",
    "pause",
    "lunsj",
    "kaffe",
    "sommer",
    "vinter",
    "høst",
    "vår",
    "ferie",
    "tur",
    "hytta",
    "fjellet",
    "sjøen",
    "båt",
    "været",
    "regn",
    "snø",
    "sol",
    "kald",
    "varm",
    "glad",
    "fin",
    "flott",
    "enkel",
    "viktig",
    "nødvendig",
    "mulig",
    "klar",
    "ferdig",
    "ny",
    "nye",
    "gammel",
    "stor",
    "liten",
    "lang",
    "kort",
    "rask",
    "sen",
    "tidlig",
    "sammen",
    "alene",
    "gjerne",
    "kanskje",
    "sikkert",
    "egentlig",
    "likevel",
    "derfor",
    "fordi",
    "hvis",
    "mens",
    "før",
    "siden",
    "gang",
    "ganger",
    "år",
    "måned",
    "uker",
    "timer",
    "minutter",
    "penger",
    "kroner",
    "pris",
    "kostnad",
    "kostnader",
    "regning",
    "kvittering",
    "bestilling",
    "levering",
    "pakke",
    "adresse",
    "telefon",
    "nummer",
    "lenke",
    "nettside",
    "system",
    "løsning",
    "problem",
    "feil",
    "test",
    "versjon",
    "oppgave",
    "oppgaver",
    "ansvar",
    "ansvarlig",
    "avdeling",
    "kollega",
    "kolleger",
    "familie",
    "barna",
    "skole",
    "barnehage",
    "trening",
    "kamp",
    "konsert",
    "middag",
    "bursdag",
    "gave",
    "invitasjon",
    "påmelding",
    "bekreftelse",
    "påminnelse",
    "avlyst",
    "utsatt",
    "flyttet",
    "gjennomført",
    "sendt",
    "mottatt",
    "lest",
    "skrevet",
    "sjekket",
    "avklart",
    "godkjent",
    "signert",
    "betalt",
    "levert",
    "ønsker",
    "trenger",
    "lurer",
    "håper",
    "tror",
    "mener",
    "synes",
    "vet",
    "husker",
    "glemte",
    "ser",
    "hører",
    "kommer",
    "går",
    "tar",
    "gir",
    "sier",
    "gjør",
    "lager",
    "finner",
    "bruker",
    "prøver",
    "jobber",
    "jobb",
    "arbeidet",
    "søknad",
    "intervju",
    "lønn",
    "prosjektplan",
    "fremdrift",
    "leveranse",
    "avvik",
    "risiko",
    "tiltak",
    "rutiner",
];
const EN_WORDS: &[&str] = &[
    "the",
    "of",
    "and",
    "to",
    "a",
    "in",
    "is",
    "it",
    "you",
    "that",
    "for",
    "on",
    "with",
    "as",
    "are",
    "this",
    "be",
    "at",
    "have",
    "from",
    "or",
    "by",
    "we",
    "an",
    "not",
    "but",
    "can",
    "will",
    "all",
    "your",
    "if",
    "about",
    "would",
    "there",
    "our",
    "more",
    "when",
    "which",
    "they",
    "one",
    "so",
    "what",
    "up",
    "out",
    "just",
    "also",
    "please",
    "thanks",
    "meeting",
    "project",
    "update",
    "report",
    "budget",
    "contract",
    "invoice",
    "payment",
    "deadline",
    "week",
    "today",
    "tomorrow",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "office",
    "client",
    "customer",
    "team",
    "manager",
    "board",
    "plan",
    "proposal",
    "change",
    "changes",
    "draft",
    "attachment",
    "attached",
    "document",
    "feedback",
    "question",
    "answer",
    "note",
    "information",
    "comment",
    "comments",
    "review",
    "status",
    "next",
    "step",
    "steps",
    "time",
    "schedule",
    "place",
    "room",
    "call",
    "lunch",
    "coffee",
    "summer",
    "winter",
    "holiday",
    "trip",
    "travel",
    "flight",
    "hotel",
    "weather",
    "great",
    "good",
    "quick",
    "simple",
    "important",
    "possible",
    "ready",
    "done",
    "new",
    "old",
    "big",
    "small",
    "long",
    "short",
    "early",
    "late",
    "together",
    "maybe",
    "sure",
    "actually",
    "still",
    "because",
    "while",
    "before",
    "since",
    "years",
    "month",
    "hours",
    "minutes",
    "money",
    "price",
    "cost",
    "costs",
    "bill",
    "receipt",
    "order",
    "delivery",
    "package",
    "address",
    "phone",
    "number",
    "link",
    "website",
    "system",
    "solution",
    "problem",
    "issue",
    "bug",
    "test",
    "version",
    "task",
    "tasks",
    "owner",
    "department",
    "colleague",
    "family",
    "kids",
    "school",
    "training",
    "game",
    "concert",
    "dinner",
    "birthday",
    "gift",
    "invitation",
    "registration",
    "confirmation",
    "reminder",
    "cancelled",
    "postponed",
    "moved",
    "completed",
    "sent",
    "received",
    "read",
    "written",
    "checked",
    "approved",
    "signed",
    "paid",
    "delivered",
    "want",
    "need",
    "wonder",
    "hope",
    "think",
    "know",
    "remember",
    "forgot",
    "see",
    "hear",
    "come",
    "go",
    "take",
    "give",
    "say",
    "make",
    "find",
    "use",
    "try",
    "work",
    "job",
    "application",
    "interview",
    "release",
    "launch",
    "support",
    "ticket",
    "account",
    "access",
    "server",
    "backup",
    "migration",
    "database",
    "design",
    "mockup",
    "roadmap",
    "quarter",
    "revenue",
    "sales",
    "analysis",
    "numbers",
    "chart",
    "slides",
    "agenda",
    "minutes",
    "milestone",
    "estimate",
    "scope",
    "risk",
    "priority",
];
const RARE_WORDS: &[&str] = &[
    "blåbærsyltetøy",
    "kanelboller",
    "hyttenøkkelen",
    "brunostskiver",
    "fjellvettreglene",
    "sølvbryllup",
    "skøytebanen",
    "dugnadsplakaten",
    "kumquat",
    "zeppelin",
    "quokka",
    "marzipan",
    "harpsichord",
    "sextant",
];
const NO_TOPICS: &[&str] = &[
    "budsjettet for neste år",
    "nettsiden",
    "kontrakten med leverandøren",
    "styremøtet",
    "dugnaden i borettslaget",
    "hytteturen",
    "skolestarten",
    "lønnsoppgjøret",
    "flyttingen",
    "søknaden",
    "kvartalsrapporten",
    "fakturarutinene",
    "kantina",
    "hjemmekontor",
    "oppussingen av badet",
    "bursdagsfeiringen",
    "kursopplegget",
    "strømavtalen",
    "bilservicen",
    "julebordet",
    "sommerferien",
    "fotballtreningen",
    "foreldremøtet",
    "det nye kontoret",
    "prosjektplanen",
];
const EN_TOPICS: &[&str] = &[
    "the Q3 budget",
    "the website redesign",
    "the contract renewal",
    "the onboarding plan",
    "release 2.4",
    "the database migration",
    "travel plans",
    "the quarterly report",
    "API access",
    "the conference",
    "the offsite",
    "the hiring plan",
    "support tickets",
    "the product launch",
    "the design review",
    "the vendor contract",
    "the security audit",
    "the new office",
    "the holiday schedule",
    "the board meeting",
    "the project update",
];
const ZH_TOPICS: &[&str] = &[
    "预算",
    "网站改版",
    "合同续签",
    "季度报告",
    "新员工培训",
    "产品发布",
    "数据库迁移",
    "年度计划",
    "客户反馈",
    "办公室搬迁",
];
const JA_TOPICS: &[&str] = &[
    "予算",
    "ウェブサイトの改修",
    "契約更新",
    "四半期報告",
    "新入社員研修",
    "製品リリース",
    "データベース移行",
    "年間計画",
    "お客様からのフィードバック",
    "オフィス移転",
];
const ZH_PHRASES: &[&str] = &[
    "关于下周的会议安排",
    "请查看附件中的文件",
    "我们需要尽快确认",
    "项目进度基本正常",
    "客户对新版本很满意",
    "预算需要重新评估",
    "请在周五之前回复",
    "会议改到下午三点",
    "感谢大家的辛苦工作",
    "如果有问题请随时联系我",
    "我已经更新了文档",
    "测试结果比预期好",
    "我们下周再讨论细节",
    "新的设计稿已经完成",
    "合同条款还需要修改",
    "数据迁移计划在月底完成",
    "请大家准时参加",
    "这个问题已经解决了",
];
const JA_PHRASES: &[&str] = &[
    "お世話になっております",
    "来週の会議についてご連絡します",
    "添付の資料をご確認ください",
    "プロジェクトの進捗は順調です",
    "金曜日までにご返信をお願いします",
    "会議の時間を午後三時に変更しました",
    "ご不明な点がございましたらお知らせください",
    "新しいデザイン案ができました",
    "テストの結果は予想より良好でした",
    "詳細は来週あらためて相談させてください",
    "契約の条件について確認が必要です",
    "データ移行は月末に完了する予定です",
    "引き続きよろしくお願いいたします",
    "資料を更新しましたのでご確認ください",
    "ご協力ありがとうございます",
];

fn ascii_fold(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            'æ' | 'Æ' => "ae".chars().collect::<Vec<_>>(),
            'ø' | 'Ø' => vec!['o'],
            'å' | 'Å' => vec!['a'],
            c => c.to_lowercase().collect(),
        })
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

fn people(rng: &mut Rng) -> Vec<Person> {
    let mut people = Vec::new();
    for index in 0..1400 {
        let (first, last, lang) = if index % 5 == 4 {
            (*rng.pick(EN_FIRST), *rng.pick(EN_LAST), Lang::En)
        } else {
            (*rng.pick(NO_FIRST), *rng.pick(NO_LAST), Lang::No)
        };
        let domain = rng.pick(DOMAINS);
        let addr = format!(
            "{}.{}{}@{domain}",
            ascii_fold(first),
            ascii_fold(last),
            index % 37
        );
        people.push(Person {
            name: format!("{first} {last}"),
            addr,
            lang,
        });
    }
    for (index, (name, local)) in ZH_NAMES.iter().enumerate() {
        people.push(Person {
            name: (*name).to_owned(),
            addr: format!("{local}@{}", DOMAINS[index % DOMAINS.len()]),
            lang: Lang::Zh,
        });
    }
    for (index, (name, local)) in JA_NAMES.iter().enumerate() {
        people.push(Person {
            name: (*name).to_owned(),
            addr: format!("{local}@{}", DOMAINS[(index + 3) % DOMAINS.len()]),
            lang: Lang::Ja,
        });
    }
    people
}

/// Zipf sampler over a word list (exponent 1.0).
struct Zipf {
    cdf: Vec<f64>,
}

impl Zipf {
    fn new(n: usize) -> Self {
        let mut total = 0.0;
        let mut cdf = Vec::with_capacity(n);
        for rank in 1..=n {
            total += 1.0 / rank as f64;
            cdf.push(total);
        }
        for value in &mut cdf {
            *value /= total;
        }
        Self { cdf }
    }

    fn sample(&self, rng: &mut Rng) -> usize {
        let u = rng.unit();
        self.cdf
            .partition_point(|value| *value < u)
            .min(self.cdf.len() - 1)
    }
}

fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

/// Writes a body until it reaches `spec.length` or `limit` bytes.
pub fn body(spec: &BodySpec, limit: usize) -> String {
    let target = spec.length.min(limit);
    let mut out = String::with_capacity(target.min(1 << 16) + 64);
    let mut rng = Rng::new(spec.seed);
    let no = Zipf::new(NO_WORDS.len());
    let en = Zipf::new(EN_WORDS.len());
    let greet = match first_name(&spec.greet) {
        "" => String::new(),
        name => format!(" {name}"),
    };
    match spec.lang {
        Lang::No => out.push_str(&format!("Hei{greet},\n\n")),
        Lang::En => out.push_str(&format!("Hi{greet},\n\n")),
        Lang::Zh => out.push_str(&format!("{}您好！\n\n", spec.greet)),
        Lang::Ja => out.push_str(&format!("{}様\n\n", spec.greet)),
    }
    let body_target = target.saturating_sub(spec.quote.as_ref().map_or(0, |_| target / 3));
    while out.len() < body_target {
        let sentences = rng.range(2, 6);
        for _ in 0..sentences {
            match spec.lang {
                Lang::No => sentence(&mut rng, NO_WORDS, &no, NO_TOPICS, &mut out),
                Lang::En => sentence(&mut rng, EN_WORDS, &en, EN_TOPICS, &mut out),
                Lang::Zh => cjk_sentence(&mut rng, ZH_PHRASES, ZH_TOPICS, "，", "。", &mut out),
                Lang::Ja => cjk_sentence(&mut rng, JA_PHRASES, JA_TOPICS, "、", "。", &mut out),
            }
            if out.len() >= body_target {
                break;
            }
        }
        out.push_str("\n\n");
    }
    match spec.lang {
        Lang::No => out.push_str(&format!("Hilsen\n{}\n", first_name(&spec.signer))),
        Lang::En => out.push_str(&format!("Best regards,\n{}\n", spec.signer)),
        Lang::Zh => out.push_str(&format!("谢谢！\n{}\n", spec.signer)),
        Lang::Ja => out.push_str(&format!("よろしくお願いいたします。\n{}\n", spec.signer)),
    }
    if let Some(quote) = &spec.quote
        && out.len() < target
    {
        match spec.lang {
            Lang::No => out.push_str(&format!("\n{} skrev:\n", quote.from)),
            Lang::Zh | Lang::Ja => out.push_str(&format!("\n{}:\n", quote.from)),
            Lang::En => out.push_str(&format!("\n{} wrote:\n", quote.from)),
        }
        let remaining = target.saturating_sub(out.len()).max(80);
        let quoted = body(
            &BodySpec {
                seed: quote.seed,
                lang: quote.lang,
                kind: quote.kind,
                length: remaining,
                greet: String::new(),
                signer: quote.from.clone(),
                quote: None,
            },
            remaining,
        );
        for line in quoted.lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.len() > limit {
        let mut end = limit;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

fn sentence(rng: &mut Rng, words: &[&str], zipf: &Zipf, topics: &[&str], out: &mut String) {
    let length = rng.range(5, 18);
    for index in 0..length {
        if index > 0 {
            out.push(' ');
        }
        let roll = rng.unit();
        let word: String = if roll < 0.0002 {
            (*rng.pick(RARE_WORDS)).to_owned()
        } else if roll < 0.03 {
            rng.range(2, 99_999).to_string()
        } else if roll < 0.04 {
            (*rng.pick(topics)).to_owned()
        } else if roll < 0.045 {
            format!("INV-{}", rng.range(10_000, 99_999))
        } else if roll < 0.05 {
            format!(
                "https://www.{}/{}/{}",
                rng.pick(DOMAINS),
                rng.pick(words),
                rng.range(100, 99_999)
            )
        } else {
            words[zipf.sample(rng)].to_owned()
        };
        if index == 0 {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        } else {
            out.push_str(&word);
        }
    }
    out.push_str(match rng.below(10) {
        0 => "? ",
        1 => "! ",
        _ => ". ",
    });
}

fn cjk_sentence(
    rng: &mut Rng,
    phrases: &[&str],
    topics: &[&str],
    comma: &str,
    stop: &str,
    out: &mut String,
) {
    let parts = rng.range(1, 4);
    for index in 0..parts {
        if index > 0 {
            out.push_str(comma);
        }
        if rng.chance(0.2) {
            out.push_str(rng.pick(topics));
        }
        out.push_str(rng.pick(phrases));
    }
    out.push_str(stop);
}

fn snippet_of(body: &str) -> String {
    const CHARS: usize = 200;
    let mut snippet = String::with_capacity(CHARS * 2);
    'lines: for line in body.lines() {
        if line.starts_with('>') {
            continue;
        }
        for word in line.split_whitespace() {
            if snippet.chars().count() >= CHARS {
                break 'lines;
            }
            if !snippet.is_empty() {
                snippet.push(' ');
            }
            snippet.push_str(word);
        }
    }
    match snippet.char_indices().nth(CHARS) {
        Some((end, _)) => snippet[..end].to_owned(),
        None => snippet,
    }
}

fn subject(rng: &mut Rng, lang: Lang, kind: Kind) -> String {
    let topic = |rng: &mut Rng| -> &'static str {
        match lang {
            Lang::No => rng.pick(NO_TOPICS),
            Lang::En => rng.pick(EN_TOPICS),
            Lang::Zh => rng.pick(ZH_TOPICS),
            Lang::Ja => rng.pick(JA_TOPICS),
        }
    };
    let t = topic(rng);
    let n = rng.range(100, 9_999);
    let rare = if rng.chance(0.01) {
        format!(" ({})", rng.pick(RARE_WORDS))
    } else {
        String::new()
    };
    let text = match (kind, lang) {
        (Kind::Conversation | Kind::Draft, Lang::No) => match rng.below(12) {
            0 => format!("Møte om {t}"),
            1 => format!("Oppdatering: {t}"),
            2 => format!("Spørsmål om {t}"),
            3 => format!("Ang. {t}"),
            4 => format!("Utkast til gjennomgang – {t}"),
            5 => format!("Påminnelse: {t}"),
            6 => format!("Referat fra møtet om {t}"),
            7 => "Kort spørsmål".to_owned(),
            8 => format!("Planer for {t}"),
            9 => format!("Tilbakemelding på {t}"),
            10 => format!("Status for {t}"),
            _ => format!("Innspill til {t}"),
        },
        (Kind::Conversation | Kind::Draft, Lang::En) => match rng.below(11) {
            0 => format!("Meeting about {t}"),
            1 => format!("Quick question on {t}"),
            2 => format!("Update: {t}"),
            3 => format!("Notes from the call on {t}"),
            4 => format!("Draft for review – {t}"),
            5 => format!("Reminder: {t}"),
            6 => format!("Feedback on {t}"),
            7 => format!("Status of {t}"),
            8 => format!("Next steps for {t}"),
            9 => "Checking in".to_owned(),
            _ => format!("Thoughts on {t}"),
        },
        (Kind::Conversation | Kind::Draft, Lang::Zh) => match rng.below(5) {
            0 => format!("关于{t}的会议安排"),
            1 => format!("{t}进度更新"),
            2 => format!("关于{t}的问题"),
            3 => format!("提醒：{t}"),
            _ => format!("{t}会议纪要"),
        },
        (Kind::Conversation | Kind::Draft, Lang::Ja) => match rng.below(5) {
            0 => format!("{t}についてのご相談"),
            1 => format!("{t}の進捗について"),
            2 => format!("{t}に関するご質問"),
            3 => format!("【リマインダー】{t}"),
            _ => format!("{t}の議事録"),
        },
        (Kind::Newsletter, Lang::No) => match rng.below(3) {
            0 => format!("Ukens nyheter: {t}"),
            1 => format!("Nytt fra oss – utgave {n}"),
            _ => format!("{} tips om {t}", rng.range(3, 12)),
        },
        (Kind::Newsletter, _) => match rng.below(3) {
            0 => format!("Weekly digest #{n}: {}", topic_en(rng)),
            1 => format!("Issue {n}: {}", topic_en(rng)),
            _ => format!(
                "{} things to know about {}",
                rng.range(3, 12),
                topic_en(rng)
            ),
        },
        (Kind::Notice, Lang::No) => match rng.below(5) {
            0 => format!("Faktura {n} forfaller snart"),
            1 => format!("Kvittering for bestilling {n}"),
            2 => format!("Pakken din er på vei ({n})"),
            3 => format!("Timebekreftelse {n}"),
            _ => "Ny melding fra oss".to_owned(),
        },
        (Kind::Notice, _) => match rng.below(5) {
            0 => format!("Invoice INV-{n}"),
            1 => format!("Your order {n} has shipped"),
            2 => format!("Payment received – INV-{n}"),
            3 => format!("Build #{n} passed"),
            _ => format!("Build #{n} failed"),
        },
        (Kind::Junk, _) => match rng.below(4) {
            0 => "Gratulerer! Du har vunnet".to_owned(),
            1 => format!("Limited offer: {}% off everything", rng.range(20, 90)),
            2 => "Your account needs attention".to_owned(),
            _ => "Exclusive investment opportunity".to_owned(),
        },
    };
    format!("{text}{rare}")
}

fn topic_en(rng: &mut Rng) -> &'static str {
    rng.pick(EN_TOPICS)
}

fn reply_prefix(rng: &mut Rng, lang: Lang) -> &'static str {
    match lang {
        Lang::No if rng.chance(0.6) => "SV: ",
        Lang::Zh => "回复：",
        _ => "Re: ",
    }
}

/// Dates over `years`, denser towards `now`, fewer at weekends, mostly in working hours (UTC).
fn date(rng: &mut Rng, now: i64, years: f64) -> i64 {
    let span = (years * 365.25 * DAY as f64) as i64;
    loop {
        let u = rng.unit();
        let x = (-1.0 + (1.0 + 15.0 * u).sqrt()) / 3.0;
        let day_start = now - span + (x * span as f64) as i64;
        let day_start = day_start - day_start.rem_euclid(DAY);
        let weekday = (day_start / DAY + 3).rem_euclid(7);
        if weekday >= 5 && !rng.chance(0.35) {
            continue;
        }
        let hour = if rng.chance(0.78) {
            rng.range(7, 18)
        } else {
            rng.range(0, 24)
        };
        let moment = day_start + hour * 3600 + rng.range(0, 3600);
        if moment <= now {
            return moment;
        }
    }
}

fn thread_size(rng: &mut Rng) -> usize {
    let u = rng.unit();
    if u < 0.45 {
        1
    } else if u < 0.65 {
        2
    } else if u < 0.77 {
        3
    } else if u < 0.87 {
        rng.range(4, 6) as usize
    } else if u < 0.95 {
        rng.range(6, 11) as usize
    } else if u < 0.99 {
        rng.range(11, 26) as usize
    } else {
        rng.range(26, 61) as usize
    }
}

fn pick_lang(rng: &mut Rng, kind: Kind) -> Lang {
    let u = rng.unit();
    match kind {
        Kind::Junk => {
            if u < 0.7 {
                Lang::En
            } else if u < 0.9 {
                Lang::No
            } else {
                Lang::Zh
            }
        }
        _ => {
            if u < 0.55 {
                Lang::No
            } else if u < 0.9 {
                Lang::En
            } else if u < 0.95 {
                Lang::Zh
            } else {
                Lang::Ja
            }
        }
    }
}

struct Builder<'a> {
    rng: Rng,
    now: i64,
    years: f64,
    people: &'a [Person],
    messages: Vec<SynthMessage>,
    sequence: u64,
}

struct Draft {
    mailbox: usize,
    kind: Kind,
    lang: Lang,
    date: i64,
    from: (String, String),
    to: Vec<(String, String)>,
    cc: Vec<(String, String)>,
    subject: String,
    parent: Option<usize>,
    references: Vec<String>,
    flags: i64,
    has_attachments: bool,
}

impl Builder<'_> {
    fn message_id(&mut self, addr: &str) -> String {
        self.sequence += 1;
        let domain = addr.rsplit('@').next().unwrap_or("mail.example");
        format!(
            "{:x}.{:08x}@{domain}",
            self.sequence,
            self.rng.next_u64() as u32
        )
    }

    fn person_for(&mut self, lang: Lang) -> Person {
        match lang {
            Lang::Zh | Lang::Ja => {
                let wanted: Vec<&Person> = self.people.iter().filter(|p| p.lang == lang).collect();
                (*self.rng.pick(&wanted)).clone()
            }
            _ => {
                let person = self.rng.pick(self.people).clone();
                if matches!(person.lang, Lang::Zh | Lang::Ja) && !self.rng.chance(0.2) {
                    self.rng.pick(&self.people[..1400]).clone()
                } else {
                    person
                }
            }
        }
    }

    fn unread_chance(mailbox: usize, date: i64, now: i64) -> f64 {
        let recent = now - date < 30 * DAY;
        match mailbox {
            INBOX if recent => 0.75,
            INBOX => 0.5,
            LISTS => 0.8,
            JUNK => 0.85,
            ARCHIVE => 0.08,
            TRASH => 0.15,
            _ => 0.0,
        }
    }

    fn push(&mut self, draft: Draft) -> usize {
        let index = self.messages.len();
        let message_id = self.message_id(&draft.from.1);
        let mut message_flags = draft.flags;
        if !self
            .rng
            .chance(Self::unread_chance(draft.mailbox, draft.date, self.now))
        {
            message_flags |= flags::SEEN;
        }
        if matches!(draft.mailbox, INBOX | ARCHIVE) && self.rng.chance(0.03) {
            message_flags |= flags::FLAGGED;
        }
        if self.rng.chance(0.01) {
            message_flags |= flags::FORWARDED;
        }
        let quote = draft.parent.and_then(|parent| {
            let parent = &self.messages[parent];
            self.rng.chance(0.5).then(|| Quote {
                seed: parent.body.seed,
                lang: parent.body.lang,
                kind: parent.body.kind,
                from: parent.header.from_name.clone(),
            })
        });
        let length = match draft.kind {
            Kind::Newsletter => self.rng.log_normal(5_000.0, 0.6),
            Kind::Notice => self.rng.log_normal(900.0, 0.5),
            Kind::Junk => self.rng.log_normal(1_200.0, 0.6),
            Kind::Draft => self.rng.log_normal(400.0, 0.7),
            Kind::Conversation => self.rng.log_normal(1_100.0, 0.9),
        };
        let length = (length as usize + if quote.is_some() { 600 } else { 0 }).clamp(120, 64_000);
        let spec = BodySpec {
            seed: self.rng.next_u64(),
            lang: draft.lang,
            kind: draft.kind,
            length,
            greet: draft
                .to
                .first()
                .map(|(name, _)| name.clone())
                .unwrap_or_default(),
            signer: draft.from.0.clone(),
            quote,
        };
        let snippet = snippet_of(&body(&spec, 600));
        let summary = |list: &[(String, String)]| {
            list.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let recipients = draft
            .to
            .iter()
            .chain(draft.cc.iter())
            .map(|(name, addr)| format!("{name} {addr}"))
            .collect::<Vec<_>>()
            .join(", ");
        let attachment_size = if draft.has_attachments {
            self.rng.log_normal(250_000.0, 1.3).min(25_000_000.0) as i64
        } else {
            0
        };
        let in_reply_to = draft
            .parent
            .and_then(|parent| self.messages[parent].header.message_id.clone());
        let body_state = if self.now - draft.date < 90 * DAY {
            2
        } else if self.rng.chance(0.4) {
            1
        } else {
            0
        };
        let blob_key = (body_state == 2).then(|| {
            let mut key = [0u8; 32];
            for chunk in key.chunks_mut(8) {
                chunk.copy_from_slice(&self.rng.next_u64().to_le_bytes());
            }
            key
        });
        let header = NewMessage {
            mailbox_id: 0,
            uid: 0,
            modseq: 0,
            message_id: Some(message_id),
            in_reply_to,
            references: draft.references,
            date: draft.date,
            sent_date: draft.date - self.rng.range(1, 120),
            from_name: draft.from.0,
            from_addr: draft.from.1,
            to_summary: summary(&draft.to),
            cc_summary: summary(&draft.cc),
            subject: draft.subject,
            snippet,
            flags: message_flags,
            has_attachments: draft.has_attachments,
            size: self.rng.range(1_800, 6_000) + length as i64 + attachment_size,
            body_state,
            blob_key,
        };
        self.messages.push(SynthMessage {
            mailbox: draft.mailbox,
            header,
            body: spec,
            recipients,
        });
        index
    }

    fn conversation(&mut self) {
        let lang = pick_lang(&mut self.rng, Kind::Conversation);
        let others: Vec<Person> = (0..self.rng.range(1, 5))
            .map(|_| self.person_for(lang))
            .collect();
        let me = (ME_NAME.to_owned(), ME_ADDR.to_owned());
        let size = thread_size(&mut self.rng);
        let base_subject = subject(&mut self.rng, lang, Kind::Conversation);
        let has_files = self.rng.chance(0.3);
        let mut date = date(&mut self.rng, self.now, self.years);
        let mut from_me = self.rng.chance(0.15);
        let mut indices: Vec<usize> = Vec::new();
        for position in 0..size {
            if position > 0 {
                date += (self.rng.log_normal(3.0 * 3600.0, 1.4) as i64).clamp(60, 20 * DAY);
                if date > self.now {
                    break;
                }
                from_me = if from_me { false } else { self.rng.chance(0.4) };
            }
            let other = self.rng.pick(&others).clone();
            let other = (other.name, other.addr);
            let (from, to) = if from_me {
                (me.clone(), other)
            } else {
                (other, me.clone())
            };
            let cc: Vec<(String, String)> = others
                .iter()
                .filter(|person| person.addr != from.1 && person.addr != to.1)
                .map(|person| (person.name.clone(), person.addr.clone()))
                .collect();
            let parent = match indices.as_slice() {
                [] => None,
                [.., last] if self.rng.chance(0.85) => Some(*last),
                earlier => Some(*self.rng.pick(earlier)),
            };
            let references = parent.map_or_else(Vec::new, |parent| {
                if self.rng.chance(0.08) {
                    return Vec::new();
                }
                let parent_message = &self.messages[parent].header;
                let mut chain = parent_message.references.clone();
                chain.extend(parent_message.message_id.clone());
                if chain.len() > 8 {
                    let tail = chain.split_off(chain.len() - 4);
                    chain.truncate(1);
                    chain.extend(tail);
                }
                chain
            });
            let subject = if position == 0 {
                base_subject.clone()
            } else {
                format!("{}{base_subject}", reply_prefix(&mut self.rng, lang))
            };
            let mailbox = if from_me {
                SENT
            } else if self.now - date > 120 * DAY && self.rng.chance(0.45) {
                ARCHIVE
            } else if self.rng.chance(0.01) {
                TRASH
            } else {
                INBOX
            };
            let attachments = has_files && (position == 0 || self.rng.chance(0.5));
            if let Some(parent) = parent
                && from_me
            {
                self.messages[parent].header.flags |= flags::ANSWERED;
            }
            let index = self.push(Draft {
                mailbox,
                kind: Kind::Conversation,
                lang,
                date,
                from,
                to: vec![to],
                cc,
                subject,
                parent,
                references,
                flags: if from_me { flags::SEEN } else { 0 },
                has_attachments: attachments,
            });
            indices.push(index);
        }
    }

    fn single(&mut self, kind: Kind) {
        let lang = pick_lang(&mut self.rng, kind);
        let date = date(&mut self.rng, self.now, self.years);
        let me = (ME_NAME.to_owned(), ME_ADDR.to_owned());
        let (mailbox, from, to, attachments, extra_flags) = match kind {
            Kind::Newsletter => {
                let (name, addr) = *self.rng.pick(NEWSLETTERS);
                let mailbox = if self.rng.chance(0.4) { LISTS } else { INBOX };
                (
                    mailbox,
                    (name.to_owned(), addr.to_owned()),
                    me,
                    self.rng.chance(0.02),
                    0,
                )
            }
            Kind::Notice => {
                let (name, addr) = *self.rng.pick(SENDERS);
                let mailbox = if self.now - date > 120 * DAY && self.rng.chance(0.5) {
                    ARCHIVE
                } else if self.rng.chance(0.03) {
                    TRASH
                } else {
                    INBOX
                };
                (
                    mailbox,
                    (name.to_owned(), addr.to_owned()),
                    me,
                    self.rng.chance(0.75),
                    0,
                )
            }
            Kind::Junk => {
                let (name, addr) = *self.rng.pick(JUNK_SENDERS);
                (
                    JUNK,
                    (name.to_owned(), addr.to_owned()),
                    me,
                    self.rng.chance(0.1),
                    flags::JUNK,
                )
            }
            Kind::Draft | Kind::Conversation => {
                let other = self.person_for(lang);
                (
                    DRAFTS,
                    me,
                    (other.name, other.addr),
                    self.rng.chance(0.1),
                    flags::DRAFT | flags::SEEN,
                )
            }
        };
        let subject = subject(&mut self.rng, lang, kind);
        self.push(Draft {
            mailbox,
            kind,
            lang,
            date,
            from,
            to: vec![to],
            cc: Vec::new(),
            subject,
            parent: None,
            references: Vec::new(),
            flags: extra_flags,
            has_attachments: attachments,
        });
    }
}

pub fn generate(config: Config) -> Corpus {
    let mut rng = Rng::new(config.seed);
    let people = people(&mut rng);
    let mut builder = Builder {
        rng: rng.fork(),
        now: config.now,
        years: config.years,
        people: &people,
        messages: Vec::with_capacity(config.messages + 64),
        sequence: 0,
    };
    let duplicates = config.messages / 100;
    while builder.messages.len() < config.messages - duplicates {
        let u = builder.rng.unit();
        if u < 0.58 {
            builder.conversation();
        } else if u < 0.80 {
            builder.single(Kind::Newsletter);
        } else if u < 0.955 {
            builder.single(Kind::Notice);
        } else if u < 0.99 {
            builder.single(Kind::Junk);
        } else {
            builder.single(Kind::Draft);
        }
    }
    builder.messages.truncate(config.messages - duplicates);
    let inbox: Vec<usize> = builder
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.mailbox == INBOX)
        .map(|(index, _)| index)
        .collect();
    for _ in 0..if inbox.is_empty() { 0 } else { duplicates } {
        let source = *builder.rng.pick(&inbox);
        let mut copy = builder.messages[source].clone();
        copy.mailbox = ARCHIVE;
        copy.header.flags |= flags::SEEN;
        builder.messages.push(copy);
    }
    let mut messages = builder.messages;
    for mailbox in 0..MAILBOXES.len() {
        let mut members: Vec<usize> = (0..messages.len())
            .filter(|index| messages[*index].mailbox == mailbox)
            .collect();
        members.sort_by_key(|index| (messages[*index].header.date, *index));
        for (uid, index) in members.into_iter().enumerate() {
            messages[index].header.uid = uid as i64 + 1;
            messages[index].header.modseq = uid as i64 + 2;
        }
    }
    messages.sort_by(|a, b| {
        a.mailbox
            .cmp(&b.mailbox)
            .then(b.header.date.cmp(&a.header.date))
            .then(b.header.uid.cmp(&a.header.uid))
    });
    Corpus {
        now: config.now,
        messages,
    }
}

/// One new incoming message for live inserts (the "new mail arrives" path).
pub fn incoming(seed: u64, date: i64, reply_to: Option<(&str, &str)>) -> (NewMessage, BodySpec) {
    let mut rng = Rng::new(seed);
    let people = people(&mut rng.fork());
    let mut builder = Builder {
        rng,
        now: date,
        years: 0.01,
        people: &people,
        messages: Vec::new(),
        sequence: (1 << 40) | seed,
    };
    let lang = pick_lang(&mut builder.rng, Kind::Conversation);
    let from = builder.person_for(lang);
    let subject = match reply_to {
        Some((_, subject)) => format!("Re: {subject}"),
        None => subject(&mut builder.rng, lang, Kind::Conversation),
    };
    builder.push(Draft {
        mailbox: INBOX,
        kind: Kind::Conversation,
        lang,
        date,
        from: (from.name, from.addr),
        to: vec![(ME_NAME.to_owned(), ME_ADDR.to_owned())],
        cc: Vec::new(),
        subject,
        parent: None,
        references: reply_to
            .map(|(id, _)| vec![id.to_owned()])
            .unwrap_or_default(),
        flags: 0,
        has_attachments: false,
    });
    let mut message = builder.messages.pop().expect("one message");
    message.header.flags &= !flags::SEEN;
    if let Some((id, _)) = reply_to {
        message.header.in_reply_to = Some(id.to_owned());
    }
    (message.header, message.body)
}

pub fn mailbox_index(name: &str) -> Option<usize> {
    MAILBOXES.iter().position(|spec| spec.name == name)
}

/// Creates the synthetic account and its mailboxes; returns the mailbox ids by `MAILBOXES` index.
pub fn create_mailboxes(writer: &mut Writer) -> anyhow::Result<Vec<i64>> {
    let account = writer.add_account("Personal", ME_ADDR)?;
    MAILBOXES
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            writer.add_mailbox(account, spec.name, spec.role, 1_700_000_000 + index as i64)
        })
        .collect()
}

impl Corpus {
    pub fn assign_mailboxes(&mut self, ids: &[i64]) {
        for message in &mut self.messages {
            message.header.mailbox_id = ids[message.mailbox];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_well_formed() {
        let config = Config {
            messages: 2_000,
            seed: 11,
            now: 1_790_000_000,
            years: 5.0,
        };
        let a = generate(config);
        let b = generate(config);
        assert_eq!(a.messages.len(), 2_000);
        assert_eq!(a.messages.len(), b.messages.len());
        for (x, y) in a.messages.iter().zip(&b.messages) {
            assert_eq!(x.header.message_id, y.header.message_id);
            assert_eq!(x.header.snippet, y.header.snippet);
            assert!(x.header.date <= config.now);
            assert!(x.header.uid > 0);
            assert!(x.header.snippet.chars().count() <= 200);
        }
        let first = &a.messages[0];
        assert_eq!(body(&first.body, usize::MAX), body(&first.body, usize::MAX));
        assert!(
            a.messages
                .iter()
                .any(|m| m.body.lang == Lang::Zh || m.body.lang == Lang::Ja)
        );
        let reserved = |addr: &str| {
            let domain = addr.rsplit('@').next().unwrap_or_default();
            domain.ends_with(".example")
                || ["example.com", "example.org", "example.net"].contains(&domain)
        };
        assert!(a.messages.iter().all(|m| reserved(&m.header.from_addr)));
    }
}
