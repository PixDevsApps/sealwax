| # | Domain | Group | Result (IMAP / SMTP, username plan) | Won by | Strict | E1 | E1+E2 | Final ms |
|---|---|---|---|---|---|---|---|---|
| 1 | gmail.com | big | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 2 | googlemail.com | big | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 3 | outlook.com | big | outlook.office365.com:993 TLS<br>smtp-mail.outlook.com:587 STARTTLS (addr, OAuth) | preset | yes | yes | yes | 0 |
| 4 | hotmail.com | big | outlook.office365.com:993 TLS<br>smtp-mail.outlook.com:587 STARTTLS (addr, OAuth) | preset | yes | yes | yes | 0 |
| 5 | live.com | big | outlook.office365.com:993 TLS<br>smtp-mail.outlook.com:587 STARTTLS (addr, OAuth) | preset | yes | yes | yes | 0 |
| 6 | msn.com | big | outlook.office365.com:993 TLS<br>smtp-mail.outlook.com:587 STARTTLS (addr, OAuth) | preset | yes | yes | yes | 0 |
| 7 | icloud.com | big | imap.mail.me.com:993 TLS<br>smtp.mail.me.com:587 STARTTLS (addr→local) | preset | yes | yes | yes | 0 |
| 8 | me.com | big | imap.mail.me.com:993 TLS<br>smtp.mail.me.com:587 STARTTLS (addr→local) | preset | yes | yes | yes | 0 |
| 9 | yahoo.com | big | imap.mail.yahoo.com:993 TLS<br>smtp.mail.yahoo.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 10 | aol.com | big | imap.aol.com:993 TLS<br>smtp.aol.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 11 | fastmail.com | big | imap.fastmail.com:993 TLS<br>smtp.fastmail.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 12 | proton.me | big | unsupported: Proton Mail (BridgeOnly) | preset | yes | yes | yes | 0 |
| 13 | gmx.com | big | imap.gmx.com:993 TLS<br>mail.gmx.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 14 | gmx.net | big | imap.gmx.net:993 TLS<br>mail.gmx.net:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 15 | web.de | big | imap.web.de:993 TLS<br>smtp.web.de:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 16 | mail.com | big | imap.mail.com:993 TLS<br>smtp.mail.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 17 | zoho.com | big | imap.zoho.com:993 TLS<br>smtp.zoho.com:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 18 | yandex.com | big | imap.yandex.com:993 TLS<br>smtp.yandex.com:465 TLS (local→addr) | preset | yes | yes | yes | 0 |
| 19 | mail.ru | big | imap.mail.ru:993 TLS<br>smtp.mail.ru:465 TLS (addr→local) | preset | yes | yes | yes | 0 |
| 20 | mailbox.org | big | imap.mailbox.org:993 TLS<br>smtp.mailbox.org:465 TLS (addr→local) | autoconfig | yes | yes | yes | 207 |
| 21 | posteo.de | big | posteo.de:993 TLS<br>posteo.de:465 TLS (addr→local) | autoconfig | yes | yes | yes | 157 |
| 22 | tutanota.com | big | unsupported: Tuta (NoImap) | preset | yes | yes | yes | 0 |
| 23 | online.no | norway | imap.online.no:993 TLS<br>smtp.online.no:465 TLS (addr→local) | own-host | **no**: username Address wrong | yes | yes | 1762 |
| 24 | altibox.no | norway | imap.altibox.no:993 TLS<br>smtp.altibox.no:465 TLS (addr→local) | own-host | yes | yes | yes | 448 |
| 25 | lyse.net | norway | imap.altibox.no:993 TLS<br>smtp.altibox.no:587 STARTTLS (addr→local, confirm) | srv | **no**: SMTP smtp.altibox.no:587 StartTls not accepted | **no**: SMTP smtp.altibox.no:587 StartTls not accepted | **no**: SMTP smtp.altibox.no:587 StartTls not accepted | 350 |
| 26 | broadpark.no | norway | imap.broadpark.no:993 TLS<br>smtp.broadpark.no:587 STARTTLS (addr→local) | srv | yes | yes | yes | 450 |
| 27 | tussa.com | norway | imap.tussa.com:993 TLS<br>smtp.tussa.com:465 TLS (addr→local) | own-host | **no**: SMTP smtp.tussa.com:465 Tls not accepted | **no**: SMTP smtp.tussa.com:465 Tls not accepted | **no**: SMTP smtp.tussa.com:465 Tls not accepted | 149 |
| 28 | domeneshop.no | norway | imap.domeneshop.no:993 TLS<br>smtp.domeneshop.no:587 STARTTLS (addr→local) | autoconfig | **no**: username Address wrong | **no**: username Address wrong | **no**: username Address wrong | 51 |
| 29 | birdlife.no | norway | imap.domeneshop.no:993 TLS<br>smtp.domeneshop.no:465 TLS (addr→local, confirm) | mx-probe | **no**: username Address wrong | **no**: username Address wrong | **no**: username Address wrong | 6508 |
| 30 | piratpartiet.no | norway | imap.domeneshop.no:993 TLS<br>smtp.domeneshop.no:465 TLS (addr→local, confirm) | mx-probe | **no**: username Address wrong | **no**: username Address wrong | **no**: username Address wrong | 5086 |
| 31 | one.com | norway | imap.one.com:993 TLS<br>send.one.com:465 TLS (addr→local) | ispdb | yes | yes | yes | 54 |
| 32 | hotmail.no | norway | outlook.office365.com:993 TLS<br>smtp-mail.outlook.com:587 STARTTLS (addr, OAuth) | ispdb | yes | yes | yes | 70 |
| 33 | c2i.net | norway | nothing found | - | yes | yes | yes | 4599 |
| 34 | chello.no | norway | no mail | mx-preset | yes | yes | yes | 291 |
| 35 | tele2.no | norway | nothing found | - | yes | yes | yes | 84 |
| 36 | mozilla.org | workspace | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 61 |
| 37 | finn.no | workspace | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 59 |
| 38 | nrk.no | workspace | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 75 |
| 39 | letsencrypt.org | workspace | imap.gmail.com:993 TLS<br>smtp.gmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 62 |
| 40 | vy.no | m365 | outlook.office365.com:993 TLS<br>smtp.office365.com:587 STARTTLS (addr, OAuth) | mx-preset | yes | yes | yes | 64 |
| 41 | uib.no | m365 | outlook.office365.com:993 TLS<br>smtp.office365.com:587 STARTTLS (addr, OAuth) | mx-preset | yes | yes | yes | 59 |
| 42 | oslomet.no | m365 | outlook.office365.com:993 TLS<br>smtp.office365.com:587 STARTTLS (addr, OAuth) | mx-preset | yes | yes | yes | 55 |
| 43 | stortinget.no | m365 | outlook.office365.com:993 TLS<br>smtp.office365.com:587 STARTTLS (addr, OAuth) | mx-preset | yes | yes | yes | 137 |
| 44 | eff.org | m365 | outlook.office365.com:993 TLS<br>smtp.office365.com:587 STARTTLS (addr, OAuth) | mx-preset | yes | yes | yes | 182 |
| 45 | linux.org.au | fastmail | imap.fastmail.com:993 TLS<br>smtp.fastmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 654 |
| 46 | digitalrightswatch.org.au | fastmail | imap.fastmail.com:993 TLS<br>smtp.fastmail.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 1020 |
| 47 | cyrusimap.org | fastmail | imap.fastmail.com:993 TLS<br>smtp.fastmail.com:465 TLS (addr→local) | srv | yes | yes | yes | 708 |
| 48 | gitea.io | zoho | imappro.zoho.com:993 TLS<br>smtppro.zoho.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 62 |
| 49 | pine64.org | zoho | imappro.zoho.com:993 TLS<br>smtppro.zoho.com:465 TLS (addr→local) | mx-preset | yes | yes | yes | 104 |
| 50 | fosstodon.org | zoho | imappro.zoho.eu:993 TLS<br>smtppro.zoho.eu:465 TLS (addr→local) | mx-preset | yes | yes | yes | 86 |
